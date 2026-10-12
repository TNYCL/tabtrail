# Tabtrail

Tabtrail adds dynamic Tab completion to the shell you already use — Bash,
Zsh or Fish on Linux — for commands that have no completion of their own.
It discovers a command's subcommands, flags and documented values from
locally installed metadata, caches the result, and answers on the ordinary
Tab key. There is no new shell, terminal, prompt, account, network service or
AI involved.

> **Status: pre-release (0.1.1).** Tagged versions are distributed as
> GitHub releases (static binaries, `.deb`, `.rpm`) and on crates.io
> (`cargo install tabtrail`). See [Limitations](#limitations).

## How it works

```text
Tab ─► shell adapter (bash/zsh/fish, static script)
      ─► tabtrail complete   (≈0.15 ms helper, reads only local files)
          ├─ parse the line before the cursor (never executes it)
          ├─ resolve the executable on PATH, look up its cached schema
          └─ answer: candidates + "file completion OK?" (or nothing)
      ◄─ the shell inserts/lists candidates with its own quoting

background (never on the Tab path):
   man pages (read, never executed)  ──┐
   `<cmd> [sub] --help` for TRUSTED    ├─► parser ─► versioned schema cache
   executables only                   ─┘
```

* **Native completions win.** Commands that already have Bash, Zsh or Fish
  completions keep them; Tabtrail only fills gaps and otherwise falls back to
  the shell's normal file-name completion.
* **Nothing unknown is executed.** By default Tabtrail only *reads* installed
  manual pages. Running `<cmd> --help` requires an explicit
  `tabtrail trust <cmd>`. The command line being typed is never executed.
* **Nested discovery is lazy and evidence-based:** `tool deploy --<Tab>`
  probes `tool deploy --help` only after `deploy` was listed by `tool --help`.
* **First Tab on a new command may be empty**: discovery runs in a detached
  background process; the next Tab uses the cache.

## Install (from source)

Requirements: Linux, Rust 1.89 or newer.

```sh
cargo build --release --locked
install -m 0755 target/release/tabtrail ~/.local/bin/tabtrail   # or any directory on PATH
```

From crates.io (builds from source with your toolchain):

```sh
cargo install tabtrail --locked
```

Release archives, `.deb` and `.rpm` packages are produced by the release
workflow (see [RELEASING.md](RELEASING.md)); they install only the binary,
docs, license texts and notices, and never modify your shell configuration.
Verify downloads against the release's `SHA256SUMS`.

## Enable / disable

```sh
tabtrail install --dry-run          # show what would change
tabtrail install                    # for the shell in $SHELL
tabtrail install --shell zsh --shell fish
tabtrail uninstall [--shell ...]    # exact reverse
```

What `install` changes:

| Shell | Change |
| ----- | ------ |
| Bash | writes `~/.local/share/tabtrail/shell/tabtrail.bash`; adds a marked block to `~/.bashrc` that sources it if present |
| Zsh | writes `~/.local/share/tabtrail/shell/tabtrail.zsh`; adds a marked block to `${ZDOTDIR:-~}/.zshrc` |
| Fish | writes its own file `~/.config/fish/conf.d/tabtrail.fish` |

The block is delimited by the exact lines `# >>> tabtrail initialize >>>` and
`# <<< tabtrail initialize <<<`. Re-running `install` replaces it in place;
`uninstall` removes exactly those lines and restores the file byte for byte.
Symlinked rc files are edited at their target; dangling symlinks, files that
are not regular files, unreadable files and ambiguous markers are refused
without changing anything. Start a new shell after installing.

Alternatively, for manual setups: `eval "$(tabtrail init bash)"` (or `zsh`),
or `tabtrail init fish | source`.

Zsh needs the completion system (`compinit`); Tabtrail registers itself once
`compinit` has run, regardless of the order in `.zshrc`.

## Trust and discovery

```sh
tabtrail trust mytool       # allow `mytool [subcommand] --help` (and value providers)
tabtrail untrust mytool
tabtrail trusted            # list
tabtrail discover mytool deploy   # discover now, in the foreground
tabtrail explain 'mytool deploy --env '   # why a line completes the way it does
tabtrail status
tabtrail cache clear
```

Trust is recorded per **canonical executable path**: a different `mytool`
earlier on `PATH` is not trusted. Paths containing control characters or
invalid UTF-8 cannot be trusted.

Configuration (`~/.config/tabtrail/config`, all optional):

```ini
help_probes = trusted        # or: off  (never execute anything for discovery)
man_pages = on               # or: off
providers = git,docker       # or: none
probe_timeout_ms = 3000      # per help probe (100..30000), also bounded by the job budget
provider_timeout_ms = 500    # per provider call (50..5000)
```

### Dynamic values

Two built-in providers fill values that help text cannot describe. Both run
only for **trusted** executables (`tabtrail trust git`, `tabtrail trust docker`)
and only when the command targets the context they would query:

* `git switch|checkout|merge|rebase <Tab>` → local branches of the repository
  Git would use (`GIT_DIR`, `GIT_WORK_TREE`, … are honoured). Declined after
  global options such as `git -C dir` / `--git-dir`, after inline assignments
  (`GIT_DIR=x git …`) and after wrappers (`env`, `sudo`).
* `docker exec|stop|logs|… <Tab>` → container names from a **local** Docker
  socket. Declined for remote `DOCKER_HOST`s, any non-default Docker context
  (`DOCKER_CONTEXT` or `currentContext`), unreadable Docker configuration, and
  global options before the subcommand.

Usually Git and Docker have native completions, which take precedence; the
providers matter where those are missing.

## Supported and tested

Measured by the PTY test suite (real Tab key presses in isolated sessions):

| Environment | Bash | Zsh | Fish |
| ----------- | ---- | --- | ---- |
| Ubuntu 24.04 (container) | 5.2.21 + bash-completion 2.11 | 5.9 | 3.7.0 |
| Debian 13 "trixie" (container) | 5.2.37 + bash-completion 2.16 | 5.9 | 4.0.2 |
| Ubuntu 26.04 host | 5.3.9 (latency only) | – | – |
| SSH (`ssh -tt` to a container-local sshd) | ✓ | ✓ | ✓ |

Minimum versions declared (not all tested): Bash 4.4, Zsh 5.x with compsys,
Fish 3.2. Architectures: x86_64 tested; aarch64 is built by CI but has not
been run by the authors yet.

## Performance

Median end-to-end latency from pressing Tab to the completed text appearing,
with a warm cache, AMD Ryzen 9 9900X (Linux 7.0), container runs:

| | Bash | Zsh | Fish |
| --- | --- | --- | --- |
| Tab → inserted (p50 / p99) | 0.88–1.13 / 1.2–2.5 ms | 1.78 / 2.2–2.8 ms | 1.29–1.45 / 1.6–1.9 ms |

The helper process itself takes ≈0.15–0.17 ms warm (process spawn baseline
≈0.15–0.18 ms) with ≈1.1 MB peak RSS; a Git branch lookup adds ≈0.85 ms. The
first Tab for a newly trusted command (empty cache, starts background
discovery) takes ≈4.7 ms p50. Reproduce with `tests/bench/bench.py` (see
[docs/TESTING.md](docs/TESTING.md)). No latency budget has been agreed yet;
these are baselines, not guarantees.

## Limitations

* Help parsing is heuristic. It handles common styles (clap, argparse, click,
  Typer/rich, cobra, commander, yargs, picocli, docopt, Swift
  ArgumentParser, Go `flag`, gflags, GNU, man/mdoc) but new layouts still
  turn up: on two sets of fixtures written blind, the untuned parser passed
  16/22 and 10/20 checks before general fixes. Missing data means fewer
  candidates, never invented ones.
* Without trust, only commands with man pages get completions.
* Completion of the command name itself, of variables, globs and words
  containing substitutions is left to the shell.
* Aliases, functions and builtins are not completed by Tabtrail.
* Fish shows file names alongside values unless Tabtrail knows files are
  wrong at that position; Bash shows Tabtrail candidates *or* files.
* Value providers exist only for Git branches and Docker containers.
* Shell-native behaviour is out of Tabtrail's control — see
  [docs/SECURITY.md](docs/SECURITY.md#shell-native-behaviour) (e.g. Zsh's
  default Tab widget expands `$(...)` in the current word by itself).

## Development

```sh
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked
python3 tests/shell/test_shells.py --tabtrail target/debug/tabtrail --require-all
```

Details, container recipes, SSH and benchmark instructions:
[docs/TESTING.md](docs/TESTING.md). Design: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).
Security model: [docs/SECURITY.md](docs/SECURITY.md).

## License

Licensed under either of

* Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/license/mit>)

at your option (SPDX: `MIT OR Apache-2.0`).

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.

Release binaries also contain third-party code (Rust crates, the Rust
standard library, musl libc and LLVM runtime objects) under their own
licenses; their notices are shipped in `THIRD-PARTY-NOTICES.txt` and
`notices/` inside every archive and package.
