# Security model

Tabtrail runs on every Tab press with the user's privileges. Its rules are
about what it *executes*, what it *reads* and what it *prints*.

## What Tabtrail executes

| Situation | Executed | Condition |
| --- | --- | --- |
| Typing / pressing Tab | **never** the command being composed | always |
| Help discovery | `<cmd> [subcommand…] --help`, then `-h` | `<cmd>`'s canonical path is on the trust list and `help_probes` is not `off`; in a detached background process |
| Git branches | `git -c core.fsmonitor=false for-each-ref --format=%(refname:short) refs/heads/` | `git` (as resolved for this command) is trusted, `providers` includes `git`, no global options, assignments or wrappers before the subcommand |
| Docker containers | `docker --host unix://… ps --format {{.Names}} [--all]` | `docker` (as resolved) is trusted, provider enabled, local socket, default context, no global options/assignments/wrappers |
| Explicit `tabtrail discover` | same as help discovery | only what the policy allows |

Everything else — man pages, cache, config — is only read. Nothing is ever
passed to a shell (`sh -c`); every subprocess gets a fixed argv.

Trust is per canonical executable path, never per name. Trusting
`/usr/bin/git` does not trust a `git` that appears earlier on `PATH`, and a
provider always runs exactly the executable the user's command resolves to.

Subprocess controls (robustness, **not** a sandbox): cleared environment
with a minimal allow-list, null stdin, private empty working directory, own
process group, wall-clock deadline that includes spawn retries and output
collection, output cap, SIGKILL of the whole process group afterwards. No
privilege changes, no `sudo`, no network access is initiated by Tabtrail
itself. A trusted program can still do anything when run with `--help`;
trust only programs you would run yourself.

## What Tabtrail reads

* The line before the cursor (passed by the shell), never stored.
* Installed manual pages under man roots derived from the executable's
  prefix, `MANPATH` and `PATH` (bounded size, gzip decompressed in process).
* Its own config, trust list and cache. All reads open files non-blocking,
  reject FIFOs/devices/directories by the opened descriptor's metadata, and
  are size-bounded.
* For the Docker provider only: the `currentContext` field of the Docker CLI
  configuration (`$DOCKER_CONFIG/config.json` or `~/.docker/config.json`,
  at most 1 MiB). Other fields, including credentials, are skipped by the
  deserializer and never stored or logged. An unreadable or malformed file
  disables the provider.

The cache holds only command metadata, an executable identity and a hash of
the effective man roots. It never stores command lines, environment values,
history or provider results.

## What Tabtrail prints

* Help/man text, executable names, provider output and descriptions are
  untrusted. Terminal escape sequences, control characters and bidirectional
  overrides are removed from display text; candidates containing them are
  dropped.
* Candidates are emitted as data. Bash replies are pre-quoted for the
  current quoting context (verified against `$(…)`, backticks, `;`, `~`,
  quotes); Zsh and Fish quote candidates themselves.
* Diagnostic commands (`explain`, `trust`, `discover`, `status`) escape
  control characters in every printed value.

## Installation

`tabtrail install` edits only a block delimited by exact marker lines, or
its own Fish file. It never overwrites unknown files, never follows dangling
symlinks, refuses ambiguous or embedded markers, and `uninstall` restores the
original bytes. Packages never modify shell configuration.

## Shell-native behaviour

Tabtrail adds no execution, but shells and their completion frameworks do
things of their own when Tab is pressed. Verified examples (with Tabtrail
uninstalled):

* **Zsh**: the default Tab widget `expand-or-complete` expands a command
  substitution in the word under the cursor, i.e. `cmd --x=$(touch f)<Tab>`
  runs `touch f`. Tabtrail additionally declines to hand such words to
  file completion, but it cannot prevent the shell's own expansion.
* **bash-completion 2.16** (Debian 13) ships fallback loaders that run some
  commands to obtain their completion: `_fx` runs `fx` as a `complete -C`
  completer, `_docker` runs `docker completion bash`. Because native
  completion takes precedence, these run before Tabtrail is consulted.

Tabtrail therefore makes no claim that pressing Tab executes nothing in
general — only that Tabtrail itself executes nothing beyond the table above.

## Reporting

There is no security contact yet; it will be added together with the
project's publication.
