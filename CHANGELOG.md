# Changelog

All notable changes are documented here. Versions follow semantic versioning.

## [0.1.0] - unreleased

First implementation.

- Discovery-based completion for Bash (≥ 4.4), Zsh (compsys) and Fish (≥ 3.2)
  on Linux, integrated with the ordinary Tab key; native completions take
  precedence.
- Shell-aware tokenizer and context parser (quotes, escapes, `--flag=value`,
  short clusters, `--`, pipelines, redirections, assignments, wrappers);
  nothing on the line is ever executed.
- Heuristic help/man parser for common CLI frameworks, lazy evidence-based
  subcommand discovery, usage-line options.
- Trust model: passive man pages by default; `--help` probes and value
  providers only for explicitly trusted executables.
- Versioned per-user cache with identity, policy, man-root and man-page
  validation; bounded, atomic, non-blocking.
- Git branch and Docker container providers with context checks.
- `install`/`uninstall` with exact, reversible rc-file blocks.
- PTY, SSH, generalization and safety test suites; benchmarks.
