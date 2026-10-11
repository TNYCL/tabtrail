# Architecture

Tabtrail is one Rust package (library + `tabtrail` binary) plus three static
shell adapters. There is no daemon, database or async runtime.

```text
src/
  shellword.rs   tokenizer for the text before the cursor (Bash/Zsh/Fish dialects)
  context.rs     current simple command: assignments, keywords, wrappers, redirections
  resolve.rs     PATH resolution + executable identity (dev, inode, size, mtime)
  schema.rs      versioned, serializable command schema
  help.rs        heuristic help-text parser (also used for rendered man pages)
  man.rs         manual page lookup, bounded gzip reading, roff/mdoc → text
  completion.rs  pure engine: schema + context → candidates, file intent, discovery need
  providers.rs   explicit dynamic value providers (Git branches, Docker containers)
  render.rs      shell-specific output encoding (protocol tabtrail1)
  cache.rs       per-user schema cache, markers, lock pool
  discovery.rs   policy, help probes, man source, background worker
  probe.rs       bounded subprocess runner (deadline, output cap, process-group kill)
  install.rs     rc-file block editing, init scripts
  config.rs      XDG directories, config file, trust list
  sanitize.rs    escape/control-character handling for untrusted text
  fsutil.rs      atomic writes, non-blocking bounded reads, hashing
  app.rs         foreground request pipeline and `explain`
shell/           tabtrail.bash, tabtrail.zsh, tabtrail.fish (embedded into the binary)
```

Parsing, resolution, schema traversal, candidate generation and rendering are
separate modules with their own unit tests; only `probe`, `discovery`,
`cache`, `install` and `app` touch the file system or processes.

## Foreground path (every Tab)

1. The adapter passes the text **before the cursor** (`--before`), the shell
   (`--shell`), Bash's `COMP_WORDBREAKS`, and what the shell knows about the
   command word (`alias`/`function`/`builtin`/`keyword`).
2. `context::parse` tokenizes without evaluating anything. Expansions are
   kept literally and flagged; a cursor inside an unterminated substitution,
   heredoc or comment yields "no completion".
3. The command word is resolved like the shell would (PATH order, empty PATH
   entries = current directory, explicit paths relative to the working
   directory). Paths that are not valid UTF-8 or contain control characters
   are not supported.
4. The cache entry for the resolved path is validated (below). Only a fresh,
   matching entry is used.
5. `completion::complete` walks the schema along the typed arguments
   (subcommands, `--flag value`, `--flag=value`, short clusters, `--`,
   repeated flags) and produces candidates, the replacement start, whether
   file completion is appropriate, and whether discovery is needed.
6. A provider may add values (see `providers.rs`); it runs only for a trusted
   executable in a supported context.
7. If discovery is needed and permitted, a detached worker is spawned
   (stdio closed, own process group) and the answer is returned immediately.

The helper never writes to stderr in normal operation, and any internal
panic is caught and turned into a well-formed empty answer.

## Protocol `tabtrail1`

Request (command-line arguments of `tabtrail complete`):

| Argument | Meaning |
| --- | --- |
| `--shell bash\|zsh\|fish` | dialect and output encoding |
| `--before TEXT` | command line up to the cursor, as text (no offsets) |
| `--wordbreaks W` | Bash `COMP_WORDBREAKS` |
| `--command-kind K` | `alias`, `function`, `builtin`, `keyword`, `file` or empty |

Passing the text before the cursor avoids byte/character offset conversion
in the shells. Each adapter produces it with the shell's own primitives:

* Bash: `${COMP_LINE:0:COMP_POINT}`. `COMP_POINT` counts **characters**
  (verified on Bash 5.2.21, 5.2.37 and 5.3.9), and `${var:0:n}` slices by
  characters, so the slice is correct for multibyte text.
* Zsh: `$LBUFFER`. Inside an open quote, compsys gives widgets a dequoted
  `LBUFFER` whose current word equals `$PREFIX`; the adapter re-inserts the
  opening quote (`$compstate[quote]`) so the helper sees the typed text.
* Fish: `commandline -cp` (current process up to the cursor).

Internally all offsets are **byte offsets into `--before`**, and every
logical (dequoted) character of the current word records the byte offset it
came from (`shellword::Word::offsets`).

Responses never contain shell code:

* **Bash**: NUL-terminated fields:
  `tabtrail1`, `FLAGS`, `COUNT`, then `COUNT` reply strings. Replies are
  already quoted for the current quoting context and trimmed to Readline's
  word (`COMP_WORDBREAKS`), because Readline inserts `COMPREPLY` verbatim.
  Candidates that cannot be represented safely (control characters, `!`
  inside double quotes, …) are dropped.
* **Zsh**: NUL-terminated fields:
  `tabtrail1`, `FLAGS`, `RAW_STRIP`, `LOGICAL_STRIP`, `COUNT`, then
  `COUNT × (INSERT, DISPLAY, NOSPACE)`. The adapter moves `RAW_STRIP`
  (or, inside quotes, `LOGICAL_STRIP`) characters into `IPREFIX` with
  `compset -p` and passes unquoted inserts to `compadd`, which quotes them.
* **Fish**: lines; first `tabtrail1<TAB>FLAGS`, then
  `candidate<TAB>description` (the `complete -a` contract). Fields never
  contain tabs, newlines or control characters. Fish quotes on insertion.

`FLAGS` letters: `f` = file completion is appropriate here; `s` = no trailing
space (Bash).

## Shell adapters

* **Bash** registers `complete -D` (default completion). If bash-completion's
  lazy loader (`_completion_loader` / `_comp_complete_load`) is the current
  default, it is called first; Tabtrail only replaces the generic
  "minimal" fallback spec it registers for commands without a completion
  file. Any other pre-existing default (`-W`, `-C`, options …) is left
  untouched and Tabtrail stays inactive for Bash (`$__tabtrail_inactive`
  says why). Spec options `-o bashdefault -o default` keep file completion.
* **Zsh** handles the `-default-` compsys context and calls the previous
  `-default-` handler (normally `_default`) for files. It registers when
  sourced and again at the first prompt, so it works whether the block is
  before or after `compinit`.
* **Fish** has no default-completion hook, so a wildcard path completion
  (`complete -p '*'`) is consulted for every command; it returns nothing
  for commands that have Fish completions of their own. A second wildcard
  entry with `-f` and a condition suppresses file names where Tabtrail knows
  they are wrong. Each entry queries the helper itself.

## Schema and cache

`SchemaDocument` (schema version 2) stores the executable identity, parser
version, collection locale (`C`), the collection policy (help/man allowed,
man-root fingerprint) and a tree of `CommandNode`s with flags (aliases,
value arity, optional values, repeatability, choices, value kind),
subcommands, positionals, a status (`described`, `listed`, `no_separate_help`,
`unavailable`) and provenance (help argv or man page path, parser version,
output hash, man page file identity).

Cache layout under `$XDG_CACHE_HOME/tabtrail/v1/`:

| Path | Content | Bound |
| --- | --- | --- |
| `schemas/<fnv64(resolved path)>.json` | one document per resolved executable path | 512 entries, 2 MiB each |
| `attempts/`, `spawns/` | discovery cooldown / spawn throttle markers | 2048 each |
| `locks/` | `worker-slot-0..1`, `job-00..15` (fixed set) | 18 files |
| `probe-cwd/` | empty working directory for help probes | – |

An entry is used only if schema version, parser version, locale, executable
identity, man-root fingerprint and the identity of every man page it was
built from all match. Anything else is "stale" (refreshed in the
background, never served). Writes are atomic (unique temp file + rename),
reads are non-blocking and bounded (FIFOs and other special files are
rejected), corrupt entries are removed. No command lines, environment
values, history or provider results are stored.

## Discovery jobs

* Triggered by the foreground when an entry is missing/stale, a listed
  subcommand is being completed, or the policy widened (e.g. after
  `tabtrail trust`).
* Throttle: one spawn per (executable, subcommand path, policy) per 3 s; a
  worker that actually started blocks repeats for 10 min unless the entry is
  found stale.
* Concurrency: 2 worker slots, 16 job lock stripes (non-blocking `flock`).
* Budget: 20 s per job; each probe gets `min(probe_timeout, remaining)`,
  512 KiB output cap, null stdin, cleared environment (PATH, HOME, USER,
  LOGNAME, `LC_ALL=C`, `TERM=dumb`, `NO_COLOR=1`, pagers set to `cat`),
  private working directory, own process group killed on exit or timeout.
* Probes: `<cmd> [path...] --help`, then `-h` if the first yields nothing.
  Subcommands are probed one level at a time and only when the parent's
  metadata lists them. Output identical to the parent's marks the node as
  having no separate help.
* Cancellation: if the executable changes during a job, nothing is written.

## Error handling

Malformed external data (help output, man pages, cache files, provider
output, configuration) produces recoverable errors or empty results; the
foreground always answers with a well-formed frame.
