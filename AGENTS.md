# Tabtrail — Agent Guide

## 1. Purpose and current authorization

Tabtrail is a Rust-based, open-source project for dynamic completion in an existing Linux shell. Repository: `TNYCL/tabtrail`.

The user installs Tabtrail and enables the relevant shell integration once on the machine where the shell runs. Afterwards, pressing the ordinary Tab key completes commands, nested subcommands, flags, and discoverable argument values. This includes normal interactive SSH sessions on a configured remote host. No replacement terminal, chat UI, command prefix, LLM, account, or cloud service is required.

Current authorization: implementation, verification, CI and the release mechanism are approved. Tuna explicitly authorized development with Claude Code using Opus 5.5 at high effort, including GitHub Actions CI and a release mechanism. Track the work in Linear TNY-14. Use the exact requested model and effort; do not silently substitute another model. Do not keep asking for approval for ordinary in-scope development steps.

Publication through existing access is authorized after independent verification of the exact revision: commits, pushes to the default branch, tags, GitHub releases and registry publication that use already configured access. The coordinator performs these remote operations; implementation agents do not push. Not authorized without a new, specific owner approval: creating accounts, credentials, tokens or new registry/repository connections; changing repository secrets or settings; force-push or history rewrites; destructive operations; and any change to the owner's live shell or SSH configuration. Documentation or coding approval is not publication approval.

Licensing remains an owner decision: never select a license silently. The unresolved project license blocks any public release; `scripts/release-gate.sh` keeps publication fail-closed until the license (and package maintainer/contact metadata) is provided.

## 2. Product contract

- Integrate with the existing Tab completion mechanism; do not build a new shell or terminal.
- Target Bash, Zsh, and Fish on Linux. Implementing one shell first is a development order, not a reduction of the final target.
- Discover command structure rather than requiring users to install a separate handwritten definition for every executable.
- Reuse existing native completions when possible. Preserve their behavior and the shell's filename/directory fallback.
- Support nested subcommands, flags, positional arguments, option values, and context-dependent candidates where a reliable source exists.
- Resolve the actual installed executable and its context; never assume an online/latest version matches it.
- Work locally and offline by default. Network-backed providers must be explicit opt-ins, never an invisible prerequisite.
- Handle unsupported or ambiguous programs honestly. Return partial or no candidates with safe fallback instead of inventing flags.
- Keep completion responsive. Discovery must not block the foreground Tab path.
- Installation and removal must be explicit, idempotent, reversible, and must not overwrite unrelated shell configuration.

Target examples, not claims of working functionality:

```text
claude --allow<Tab>          -> matching flags from the installed command's available metadata
git switch <Tab>             -> branches in the current repository
docker exec <Tab>            -> running container names, with a trusted local provider
some-tool deploy <Tab>       -> discovered nested options and arguments
some-tool deploy --env <Tab> -> documented enum values or an explicitly supported dynamic source
```

Universal integration is the goal; perfect knowledge of undocumented commands is not a promise. Do not hardcode these examples and call the engine general-purpose.

## 3. Target architecture

The following boundaries are the working design, not implemented components. Keep them modular inside a simple Rust package initially; split crates only when dependencies or testing justify it. Do not introduce a daemon, database server, plugin VM, or async runtime merely to fill the diagram.

```text
interactive shell + Tab
    -> thin shell adapter
    -> completion request / shell-aware cursor context
    -> resolver + command schema + completion engine
    -> cached metadata / bounded trusted value provider
    -> structured candidates
    -> shell-native insertion, escaping, and display

separate discovery / refresh path
    -> trusted metadata sources / permitted probes
    -> parsers + normalization + provenance
    -> versioned schema cache
```

### Shell adapters

Capture the line, cursor position, shell identity, working directory, and only the minimum necessary environment context. Preserve each shell's completion semantics, word boundaries, quoting, replacement range, and spacing behavior. The adapter owns shell-specific insertion/escaping; the core must not manufacture shell code for evaluation.

Existing native completion takes precedence. Design fallback and delegation explicitly for each shell; prevent recursive calls back into Tabtrail. Do not assume Bash, Zsh, and Fish share one completion ABI. A helper executable behind Tab is fine; requiring the user to manually invoke it for normal completion is not.

### Context and resolution

Parse incomplete input without executing it. Handle the cursor in the middle of a token, spaces, Unicode, quoted/escaped paths, `--flag=value`, separate option values, short flags, repeated options, and the end-of-options marker `--`. Interpret pipes, redirects, assignments, aliases, shell functions, and command wrappers conservatively; unsupported syntax must fall back without rewriting the user's command. Never run command substitutions to understand a line.

Resolve executable identity using the current PATH, explicit paths, and shell-provided context. Aliases and functions cannot simply be treated as files. Keep parsing, name resolution, schema traversal, candidate generation, and shell rendering separately testable.

### Discovery

Prefer compatible existing/native completions and structured metadata; then use local help/man information when available and safe. Preserve source provenance, executable identity, and uncertainty. Help parsing is heuristic, not a universal CLI specification.

Discover nested command structure only from evidence. Do not recursively execute every candidate subcommand with `--help`: some programs ignore help switches or perform work before printing help. Treat `--version` probes with the same execution policy. Support a generic discovery path and optional command-specific providers for genuine gaps; providers must not replace the general engine.

On a cold cache, return existing shell/file completion or no additional candidates immediately. Trigger only policy-permitted, deduplicated background discovery, or report that explicit discovery is needed. The foreground request must not wait for it. Define job lifetimes, cancellation, resource limits, and refresh scheduling before implementing background workers. A permanent daemon is optional, not required.

### Command schema and protocol

Define a versioned, serializable internal schema before scattering parsing logic through the code. It should represent commands/subcommands, flag aliases, value arity and types, positional arguments, finite values, repeatability, documented constraints, descriptions, sources, and uncertainty. Absence of metadata is not proof that an option does not exist.

Completion requests and responses need a versioned contract: line/cursor/context in; candidate insertion text, display text, description, kind, replacement span, and spacing intent out. Specify byte-versus-character offsets and test the conversions at shell boundaries. Use a safe framing/encoding format, not whitespace splitting or shell `eval`. Keep diagnostics out of the candidate stream.

### Dynamic providers

Use explicit providers for values such as local Git branches or local container names. Pass structured arguments to a resolved executable; never interpolate the user's line into `sh -c`. Bound runtime, output, recursion, and concurrency. Provider failure, missing tools, missing permissions, or an unavailable service must not break completion.

Separate command-schema caching from short-lived, context-dependent value caching. Do not reuse repository-specific branches, container contexts, or environment-specific values across unrelated directories or users. Docker or Git invocations may target remote services depending on configuration; honor the offline policy rather than assuming every CLI call is local.

### Cache

Use per-user storage following XDG conventions. Cache keys must distinguish resolved executable identity, relevant version/fingerprint, parser/schema version, and relevant environment/locale. Support invalidation when an executable is replaced or PATH resolution changes. Avoid a subprocess version probe on every Tab.

Use bounded storage, atomic replacement, and concurrency-safe writes. Corruption, missing entries, stale schemas, and interrupted writers must fail safely. Do not store full command history, secret values, tokens, or unrelated environment variables. Choose a simple storage format based on measured needs, not habit.

## 4. Execution and trust boundary

- Tab must never execute the command the user is composing or accept a suggestion on the user's behalf.
- Distinguish typed-command execution from narrowly permitted discovery/provider subprocesses; both need explicit design, not ambiguous promises of 'no execution'.
- Do not automatically execute arbitrary binaries discovered on PATH or in the current directory. Define executable/provider trust and user opt-in before enabling probes.
- Do not source newly discovered completion scripts or project-local provider/config code without an explicit trust decision. Existing user-installed shell completions remain subject to the shell's existing trust boundary.
- Timeouts and process limits are robustness controls, not a security sandbox. If a command cannot be safely inspected under the available policy, skip execution and use passive metadata or fallback.
- Enforce subprocess deadlines, output caps, process-tree cleanup, noninteractive stdin, and minimum required environment. No sudo, privileged access, or broad filesystem scans during completion/discovery.
- Treat help/man output, executable filenames, and completion descriptions as untrusted data. Remove terminal control sequences from display text and safely handle malicious candidate strings.
- Never evaluate candidate text, help output, aliases, or substitutions as shell code. Never request credentials from a completion attempt.
- No telemetry or background uploads by default. Do not include personal server paths, credentials, production data, or private project details in this public repository.

## 5. Rust engineering approach

- Prefer stable Rust, small focused modules, explicit types, and a small dependency footprint. MSRV: Rust 1.89 (`rust-version` in `Cargo.toml`, tested in CI); release binaries are static musl builds.
- Keep deterministic parsing/completion logic separate from filesystem/process/clock interfaces so tests can isolate side effects.
- Use explicit error types at module boundaries. Malformed external data should produce recoverable errors, not panic.
- Avoid `unsafe` unless a concrete need, written justification, and dedicated review exist.
- Use English for public code, identifiers, and repository documentation. Communicate progress to the owner in Turkish.
- A likely module split is `context`, `schema`, `discovery`, `completion`, `providers`, `cache`, and `protocol`, with shell scripts under `shell/`. This is a proposed organization, not a requirement to generate empty scaffolding.
- Internal setup/diagnostic/refresh commands are allowed. Their exact interface is undecided; the everyday UX remains ordinary Tab.

## 6. Acceptance and verification

Do not claim the product works based only on parser unit tests or printed JSON. Acceptance requires the actual shell integration.

- Unit/fixture tests: schema extraction, nested traversal, value arity, prefixes, quoting, `--`, cursor placement, malformed/ambiguous help, Unicode, and deterministic ordering.
- Generalization: test previously unseen synthetic CLI fixtures that the implementation does not special-case, alongside permitted real commands.
- Shell tests: isolated Bash/Zsh/Fish sessions with temporary HOME/config; drive the actual Tab key through a PTY and verify the resulting buffer/candidates. Cover fresh interactive startup, native completion coexistence, filenames, and removal. Never modify the owner's live dotfiles for tests.
- SSH behavior: validate on a separately authorized disposable/local SSH test setup before claiming end-to-end SSH coverage; do not modify SSH configuration or access remote hosts without permission.
- Safety: prove no execution of the composed command; test command substitution/injection strings, hostile display output, denied discovery, hanging probes, output floods, and unavailable providers. Use synthetic fixtures, not dangerous real commands.
- Cache: executable/PATH changes, context isolation, corrupt entries, simultaneous readers/writers, cancellation, and bounded cache growth.
- Performance: measure end-to-end warm Tab latency (including helper startup), cold-cache behavior, provider deadlines, memory, and cache footprint on declared hardware. Record percentile distributions and regressions. Agree on numeric budgets after the first baseline; do not invent measured timings or silently trade correctness for latency.
- Release readiness: reproducible documented install/uninstall, clean-machine smoke tests, supported shell versions/architectures, honest limitations, and an owner-approved license. Publication still requires approval.

Once a Cargo project exists, the expected checks are `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --all-targets --all-features`, plus the separate PTY/integration and benchmark commands established by the implementation. Feature combinations must remain compatible or have an explicitly documented test matrix. These are acceptance requirements, not claims of completed checks.

### CI and release automation (approved scope)

Implement GitHub Actions quality gates, shell integration tests, and release build/package checks. Do not limit meaningful verification merely to save Actions minutes; do bound retries and avoid duplicate or unbounded workflows. Use least-privilege workflow permissions and review current official action versions before selecting dependencies. Do not expose publishing credentials to pull-request code.

Prepare an explicitly approval-gated release path with version/tag consistency checks, platform artifacts, checksums, and crates.io packaging/publishing support. Build/test/package verification must work without registry credentials. Keep real publication fail-closed (`scripts/release-gate.sh`: license, runtime notices, maintainer metadata, `publish = false`) until the owner's license decision is made. Publication follows the authorization in section 1; do not create access tokens, accounts or new registry connections, or configure GitHub secrets/environments, without specific approval. The required one-time setup is documented in `RELEASING.md`. Local workflow validation is not proof that GitHub Actions ran.

## 7. Working process and handoff

The owner is Tuna. Orvis coordinates scope and reviews results; Claude Code is the selected implementation agent. This repository root (`code/tabtrail`) is the working directory for this standalone project; do not create a separate playground or duplicate checkout without a reason and approval.

Before changes, inspect current instructions and Git status. This is a shared checkout: preserve other people's work and do not run concurrent writers or change branches under another session. Use an agreed handoff or isolated worktree when needed.

After implementation is authorized, start by checking assumptions and unresolved decisions, then develop in testable steps toward the full product contract. Prefer tests for the specific behavior before implementation. Development sequencing is not permission to relabel partial delivery as the finished product.

Report what changed, exact checks run and their results, unverified behavior, and remaining blockers. A subagent saying 'done' is not evidence. Do not manufacture test logs, benchmarks, supported-command counts, or universal compatibility claims.

Implemented choices (documented in `docs/ARCHITECTURE.md` and `docs/SECURITY.md`, open to owner review): MSRV 1.89; protocol `tabtrail1` (NUL-framed for Bash/Zsh, line-based for Fish); JSON schema cache with identity/policy/man-source validation; detached, throttled background discovery workers; per-canonical-path trust list for help probes and value providers.

Open owner decisions: license; package maintainer/contact metadata; crates.io trusted-publishing connection (new registry setup); distribution targets (proposed: static musl x86_64/aarch64 tarballs, `.deb`, `.rpm`); minimum shell versions (declared Bash 4.4, Zsh 5 compsys, Fish 3.2; tested Bash 5.2–5.3, Zsh 5.9, Fish 3.7–4.0); default enablement of the Git/Docker providers; numeric performance budgets (baseline in `docs/TESTING.md`). Present consequential tradeoffs to the owner rather than treating suggestions in this guide as already approved detailed choices.
