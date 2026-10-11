# Testing

All tests use temporary HOME/XDG directories and synthetic fixtures; none
touch the invoking user's dotfiles.

## Rust tests

```sh
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked
```

* Unit tests (`src/**`): tokenizer, context, help/man parsing, engine,
  rendering/quoting, cache (corruption, FIFOs, bounds, concurrency), probes
  (deadlines, floods, process-group kill, ETXTBSY budget), discovery policy
  and budgets, providers (trust, context), installer (exact restoration,
  markers, symlinks, non-UTF-8 files).
* `tests/cli.rs`: the real binary with isolated HOME/PATH: no execution of
  untrusted tools or composed commands, trust injection, PATH shadowing,
  two real Git repositories with different branches (`GIT_DIR`, `-C`,
  `--git-dir`, assignments, wrappers), Docker context negatives, FIFO and
  oversized cache/config files, MANPATH switches and man page updates,
  background discovery, executable replacement, install round trip.
* `tests/generalization.rs`: help texts in styles not used during
  development. Results are recorded honestly:

  | Set | First (untuned) run | After general fixes |
  | --- | --- | --- |
  | `holdout/` | 16 / 22 | 22 / 22 |
  | `holdout_v2/` (written after the v1 fixes) | 10 / 20 | 20 / 20 |

  Both sets needed parser improvements, so expect new help layouts to need
  them too.

Minimum supported Rust version: 1.89 (`cargo +1.89 test --locked`).

## Shell integration (PTY)

`tests/shell/test_shells.py` installs Tabtrail with `tabtrail install` into a
temporary HOME, starts interactive shells on a pseudo-terminal, presses the
real Tab key and reads the line editor buffer back through a test-only key
binding. Covered: fresh startup, subcommands, nested flags, separate and
attached values, short flags, `--`, Unicode, quoting, mid-word cursor,
listings with descriptions, file fallback, native completion precedence
(including bash-completion's loader and pre-existing default specs),
substitution safety against a no-Tabtrail baseline, hostile help and
provider output, untrusted providers, fresh metadata on repeated Tab, Zsh
`compinit` ordering, and uninstall.

```sh
# host (any installed shells)
python3 tests/shell/test_shells.py --tabtrail target/debug/tabtrail -v
# disposable containers (all three shells, two distributions)
docker build -t tabtrail-test:ubuntu-24.04 --build-arg BASE=ubuntu:24.04 tests/docker
docker build -t tabtrail-test:debian-trixie --build-arg BASE=debian:trixie tests/docker
cargo build --release --target x86_64-unknown-linux-musl
docker run --rm --user tester -v "$PWD:/src:ro" tabtrail-test:ubuntu-24.04 \
  python3 /src/tests/shell/test_shells.py \
  --tabtrail /src/target/x86_64-unknown-linux-musl/release/tabtrail --require-all
```

Last local result (both images): 72 tests, OK.

## SSH

`tests/ssh/ssh_test.py` runs **only as root inside a disposable container**:
it starts an sshd on 127.0.0.1 inside the container, installs Tabtrail for a
container user and presses Tab in remote interactive Bash, Zsh and Fish over
`ssh -tt`. No host configuration, port or network is involved.

```sh
docker run --rm -v "$PWD:/src:ro" tabtrail-test:debian-trixie \
  python3 /src/tests/ssh/ssh_test.py \
  --tabtrail /src/target/x86_64-unknown-linux-musl/release/tabtrail
```

Last local result: 9/9 cases per image (Ubuntu 24.04, Debian 13), no
composed command executed.

## Release scripts and packages

```sh
scripts/test-release-scripts.sh   # gate/policy/version/packaging on fixture repos
scripts/check-release-policy.sh   # the real repository: github gate open, crates.io deferred
```

Covers the publication gate: license field vs. files, `license-file`,
`[package]` scoping; the project's `MIT OR Apache-2.0` passing for GitHub
but denied for crates.io by `publish = false`; either dual-license text
missing, empty, blank, holding the wrong license or an unfilled MIT
copyright line; absent, empty, malformed, multi-line or placeholder
maintainers; missing or empty runtime notices. Denials must exit 1 *and*
name the expected blocker. Also covered: the policy check, version/tag/
changelog checks, refusal to package without license or maintainer, and
that both full license texts (byte-identical), the third-party and
toolchain notices and the maintainer end up in the tarball, `.deb`
(`copyright`, `Maintainer`), `.rpm` (`%license`, `License`, `Packager`) and
the crate. Packaging under umask 027 and 077 (from a checkout created
under that umask) must succeed and produce byte-identical artifacts with
public payload and artifact modes. Failing package builds print their
output. Other fixtures use synthetic `LicenseRef-TestFixture-*`
identifiers. `scripts/smoke-test.sh` checks the same license texts and
metadata in the real artifacts.

The release workflow's clean-install job (checksums, native package
install, notices present in the package, `tabtrail install`/`uninstall`
round trip in a temporary HOME) was replicated locally on pristine
`ubuntu:24.04`, `debian:trixie` and `fedora:latest` containers: all passed.
GitHub Actions itself has not run yet.

## Benchmarks

```sh
python3 tests/bench/bench.py --tabtrail target/release/tabtrail \
  --iterations 1000 --pty-iterations 60 --json result.json
```

Measures helper latency against a process-spawn baseline: warm cache;
`no metadata, discovery denied` (untrusted command, man pages off: the
cheapest possible miss); `cold first Tab` (trusted command, fresh empty
cache — the call answers without metadata and starts background discovery;
the bench waits untimed until discovery has produced completions and fails
otherwise); the Git provider. Also peak RSS (via GNU `time`, when installed),
cache footprint, and the end-to-end time from pressing Tab to the completed
text appearing in Bash, Zsh and Fish. Samples where the completion is already
visible before Tab (autosuggestions, uncleared lines) abort the run.

Baseline (AMD Ryzen 9 9900X, 24 threads, 64 GB, Linux 7.0, static musl
release build with Rust 1.99.0, container runs; 1000 helper samples, 100
cold-first-Tab samples, 60 PTY samples per shell):

| Metric (p50 / p99) | Ubuntu 24.04 | Debian 13 |
| --- | --- | --- |
| helper, warm cache | 0.168 / 0.292 ms | 0.153 / 0.245 ms |
| helper, no metadata, discovery denied | 0.108 / 0.190 ms | 0.099 / 0.173 ms |
| helper, cold first Tab (starts discovery) | 4.67 / 8.64 ms | 4.68 / 12.19 ms |
| process spawn baseline (`/bin/true`) | 0.148 / 0.314 ms | 0.177 / 0.318 ms |
| helper + Git provider | 1.01 / 1.11 ms | 1.01 / 1.12 ms |
| Tab → inserted, Bash | 0.88 / 1.18 ms | 1.13 / 2.47 ms |
| Tab → inserted, Zsh | 1.78 / 2.17 ms | 1.78 / 2.84 ms |
| Tab → inserted, Fish | 1.45 / 1.89 ms | 1.29 / 1.59 ms |
| helper peak RSS | 1.1 MB | 1.1 MB |
| cache for 2 schemas | 7.2 KB | 8.4 KB |

The cold first Tab happens once per command (and again only after the
executable or its metadata sources change); the time it spends has not been
profiled yet. Numeric budgets have not been agreed; treat these as the first
baseline.
