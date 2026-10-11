use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use tabtrail::app::{self, Request, Shell};
use tabtrail::cache::Cache;
use tabtrail::config::{Config, Dirs, HelpProbes, TrustList};
use tabtrail::discovery::{self, Env, Policy};
use tabtrail::install::{self, Paths};
use tabtrail::resolve::resolve;

/// Prints one diagnostic line with control/bidi characters escaped, since
/// it may contain paths, names or help-derived text.
macro_rules! say {
    ($($arg:tt)*) => {
        println!("{}", tabtrail::sanitize::escape_display(&format!($($arg)*)))
    };
}

macro_rules! warn_line {
    ($($arg:tt)*) => {
        eprintln!("{}", tabtrail::sanitize::escape_display(&format!($($arg)*)))
    };
}

const USAGE: &str = "\
tabtrail - discovery-based Tab completion for Bash, Zsh and Fish

Usage:
  tabtrail install [--shell bash|zsh|fish]... [--dry-run]
  tabtrail uninstall [--shell bash|zsh|fish]... [--dry-run]
  tabtrail init <bash|zsh|fish>          Print the integration script
  tabtrail trust <command>...            Allow running `<command> --help` for discovery
  tabtrail untrust <command>...
  tabtrail trusted                       List trusted executables
  tabtrail discover <command> [sub...]   Discover metadata now (policy applies)
  tabtrail explain [--shell S] <line>    Explain how <line> would be completed
  tabtrail status                        Show configuration and cache locations
  tabtrail cache clear                   Remove all cached metadata
  tabtrail --version

Everyday use needs no commands: press Tab in your shell.
Without --shell, install/uninstall act on the shell named by $SHELL.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("complete") => complete_cmd(&args[1..]),
        Some("__discover") => worker_cmd(&args[1..]),
        Some("init") => init_cmd(&args[1..]),
        Some("install") => install_cmd(&args[1..], true),
        Some("uninstall") => install_cmd(&args[1..], false),
        Some("trust") => trust_cmd(&args[1..], true),
        Some("untrust") => trust_cmd(&args[1..], false),
        Some("trusted") => trusted_cmd(),
        Some("discover") => discover_cmd(&args[1..]),
        Some("explain") => explain_cmd(&args[1..]),
        Some("status") => status_cmd(),
        Some("cache") => cache_cmd(&args[1..]),
        Some("--version" | "-V" | "version") => {
            println!("tabtrail {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help" | "-h" | "help") | None => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            warn_line!("tabtrail: unknown command `{other}`");
            eprint!("\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn fail(msg: impl std::fmt::Display) -> ExitCode {
    warn_line!("tabtrail: {msg}");
    ExitCode::FAILURE
}

/// `--key value` pairs in command-line order.
type Opts = Vec<(String, String)>;

/// Parses `--key value` options; returns (options, positionals).
fn options(args: &[String], with_value: &[&str]) -> Result<(Opts, Vec<String>), String> {
    let mut opts = Vec::new();
    let mut pos = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            pos.extend(args[i + 1..].iter().cloned());
            break;
        }
        if let Some(name) = a.strip_prefix("--") {
            if let Some((k, v)) = name.split_once('=') {
                opts.push((k.to_string(), v.to_string()));
            } else if with_value.contains(&name) {
                let v = args.get(i + 1).ok_or_else(|| format!("--{name} needs a value"))?;
                opts.push((name.to_string(), v.clone()));
                i += 1;
            } else {
                opts.push((name.to_string(), String::new()));
            }
        } else {
            pos.push(a.clone());
        }
        i += 1;
    }
    Ok((opts, pos))
}

fn opt<'a>(opts: &'a [(String, String)], key: &str) -> Option<&'a str> {
    opts.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

/// Internal: called by the shell adapters on every Tab press.
fn complete_cmd(args: &[String]) -> ExitCode {
    let parsed = options(args, &["shell", "before", "after", "wordbreaks", "command-kind"]);
    let Ok((opts, _)) = parsed else {
        return ExitCode::from(2);
    };
    let Some(shell) = opt(&opts, "shell").and_then(Shell::parse) else {
        return ExitCode::from(2);
    };
    let req = Request {
        shell,
        before: opt(&opts, "before").unwrap_or_default().to_string(),
        wordbreaks: opt(&opts, "wordbreaks").map(String::from),
        command_kind: opt(&opts, "command-kind").filter(|k| !k.is_empty()).map(String::from),
        cwd: cwd(),
    };
    // Whatever happens, answer with a well-formed (possibly empty) response
    // so the shell falls back to its own completion.
    let out = std::panic::catch_unwind(|| app::complete(&req)).unwrap_or_else(|_| {
        let ev = app::Evaluation {
            context: tabtrail::context::Context::NoCompletion("internal error"),
            note: "internal error",
            resolved: None,
            cache: "-",
            policy: None,
            outcome: tabtrail::completion::Outcome {
                files: true,
                ..Default::default()
            },
            provider: None,
            discovery_requested: None,
        };
        app::render(&req, &ev)
    });
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(&out);
    let _ = stdout.flush();
    ExitCode::SUCCESS
}

fn worker_cmd(args: &[String]) -> ExitCode {
    let Ok((opts, _)) = options(args, &["path", "name", "sub"]) else {
        return ExitCode::from(2);
    };
    let (Some(path), Some(name), Some(dirs)) = (opt(&opts, "path"), opt(&opts, "name"), Dirs::from_env()) else {
        return ExitCode::from(2);
    };
    let sub: Vec<String> = opts
        .iter()
        .filter(|(k, _)| k == "sub")
        .map(|(_, v)| v.clone())
        .collect();
    let _ = discovery::worker_main(&dirs, path, name, &sub);
    ExitCode::SUCCESS
}

fn helper_path() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "tabtrail".into())
}

fn init_cmd(args: &[String]) -> ExitCode {
    let Some(shell) = args.first().and_then(|s| Shell::parse(s)) else {
        return fail("usage: tabtrail init <bash|zsh|fish>");
    };
    print!("{}", install::init_script(shell, &helper_path()));
    ExitCode::SUCCESS
}

fn shells_from(opts: &[(String, String)]) -> Result<Vec<Shell>, String> {
    let named: Vec<&str> = opts
        .iter()
        .filter(|(k, _)| k == "shell")
        .map(|(_, v)| v.as_str())
        .collect();
    if named.is_empty() {
        let sh = std::env::var("SHELL").unwrap_or_default();
        let base = sh.rsplit('/').next().unwrap_or("");
        return Shell::parse(base)
            .map(|s| vec![s])
            .ok_or_else(|| "cannot tell the shell from $SHELL; pass --shell bash|zsh|fish".to_string());
    }
    named
        .iter()
        .map(|n| Shell::parse(n).ok_or_else(|| format!("unsupported shell `{n}`")))
        .collect()
}

fn install_cmd(args: &[String], install_mode: bool) -> ExitCode {
    let (opts, _) = match options(args, &["shell"]) {
        Ok(v) => v,
        Err(e) => return fail(e),
    };
    let dry = opt(&opts, "dry-run").is_some();
    let shells = match shells_from(&opts) {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    let Some(dirs) = Dirs::from_env() else {
        return fail("HOME is not set");
    };
    let Some(paths) = Paths::from_env(&dirs.data) else {
        return fail("HOME is not set");
    };
    let bin = helper_path();
    let mut status = ExitCode::SUCCESS;
    for shell in shells {
        let result = if install_mode {
            install::install(&paths, shell, &bin, dry)
        } else {
            install::uninstall(&paths, shell, dry)
        };
        let prefix = if dry { "would " } else { "" };
        match result {
            Ok(c) => {
                for p in &c.written {
                    say!("{prefix}write   {}", p.display());
                }
                for p in &c.removed {
                    say!("{prefix}remove  {}", p.display());
                }
                for p in &c.unchanged {
                    say!("unchanged {}", p.display());
                }
                if install_mode && !dry {
                    say!(
                        "{}: start a new {} session to enable Tabtrail.",
                        shell.name(),
                        shell.name()
                    );
                }
            }
            Err(e) => {
                warn_line!("tabtrail: {e}");
                status = ExitCode::FAILURE;
            }
        }
    }
    status
}

fn trust_cmd(args: &[String], add: bool) -> ExitCode {
    if args.is_empty() {
        return fail("usage: tabtrail trust|untrust <command>...");
    }
    let Some(dirs) = Dirs::from_env() else {
        return fail("HOME is not set");
    };
    let mut trust = TrustList::load(&dirs);
    let path_var = std::env::var_os("PATH");
    for name in args {
        let Some(r) = resolve(name, path_var.as_deref(), &cwd()) else {
            if !add && trust.paths.remove(name.as_str()) {
                say!("untrusted {name}");
                continue;
            }
            return fail(format!("`{name}` is not an executable on PATH"));
        };
        let canonical = r.identity.canonical.clone();
        if add {
            if trust.paths.insert(canonical.clone()) {
                say!("trusted {canonical}");
                say!(
                    "  Tabtrail may now run `{} [subcommand] --help` in the background.",
                    r.name
                );
            } else {
                say!("already trusted {canonical}");
            }
        } else if trust.paths.remove(&canonical) {
            say!("untrusted {canonical}");
        } else {
            say!("not trusted {canonical}");
        }
    }
    match trust.save(&dirs) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(e),
    }
}

fn trusted_cmd() -> ExitCode {
    let Some(dirs) = Dirs::from_env() else {
        return fail("HOME is not set");
    };
    for p in TrustList::load(&dirs).paths {
        say!("{p}");
    }
    ExitCode::SUCCESS
}

fn discover_cmd(args: &[String]) -> ExitCode {
    let Some((name, sub)) = args.split_first() else {
        return fail("usage: tabtrail discover <command> [subcommand...]");
    };
    let Some(dirs) = Dirs::from_env() else {
        return fail("HOME is not set");
    };
    let Some(resolved) = resolve(name, std::env::var_os("PATH").as_deref(), &cwd()) else {
        return fail(format!("`{name}` is not an executable on PATH"));
    };
    let config = Config::load(&dirs);
    let trust = TrustList::load(&dirs);
    let policy = Policy::decide(&config, &trust, &resolved.identity.canonical).with_man_roots(
        &resolved.identity.path,
        std::env::var_os("MANPATH").as_deref(),
        std::env::var_os("PATH").as_deref(),
    );
    if !policy.help {
        if config.help_probes == HelpProbes::Off {
            say!("help probes are disabled in the configuration; using manual pages only");
        } else {
            say!(
                "{} is not trusted; using manual pages only (run `tabtrail trust {name}` to allow --help)",
                resolved.identity.canonical
            );
        }
    }
    let cache = Cache::new(&dirs);
    let env = Env::from_process(&config, &cache);
    let report = discovery::run_job(&cache, &resolved, sub, policy, &env, true);
    for p in &report.probes_run {
        say!("ran      {p}");
    }
    for m in &report.man_pages_read {
        say!("read     {m}");
    }
    if let Some(c) = report.cancelled {
        say!("cancelled: {c}");
    }
    match cache.load(&resolved.identity, policy.man_roots) {
        tabtrail::cache::Lookup::Fresh(doc) => {
            let node = doc.root.descend(sub).unwrap_or(&doc.root);
            say!(
                "result   {:?}: {} flags, {} subcommands, {} positionals",
                node.status,
                node.flags.len(),
                node.subcommands.len(),
                node.positionals.len()
            );
        }
        _ => say!("result   nothing cached"),
    }
    ExitCode::SUCCESS
}

fn explain_cmd(args: &[String]) -> ExitCode {
    let (opts, pos) = match options(args, &["shell"]) {
        Ok(v) => v,
        Err(e) => return fail(e),
    };
    let shell = opt(&opts, "shell").and_then(Shell::parse).unwrap_or(Shell::Bash);
    let Some(line) = pos.first() else {
        return fail("usage: tabtrail explain [--shell S] '<line>'");
    };
    let req = Request {
        shell,
        before: line.clone(),
        wordbreaks: None,
        command_kind: None,
        cwd: cwd(),
    };
    // Each line escapes untrusted values; keep the newlines between them.
    for line in app::explain(&req).lines() {
        say!("{line}");
    }
    ExitCode::SUCCESS
}

fn status_cmd() -> ExitCode {
    let Some(dirs) = Dirs::from_env() else {
        return fail("HOME is not set");
    };
    let config = Config::load(&dirs);
    let trust = TrustList::load(&dirs);
    let cache = Cache::new(&dirs);
    let entries = std::fs::read_dir(cache.root().join("schemas"))
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
                .count()
        })
        .unwrap_or(0);
    println!("version        {}", env!("CARGO_PKG_VERSION"));
    say!("config file    {}", dirs.config_file().display());
    say!(
        "trust file     {} ({} trusted)",
        dirs.trust_file().display(),
        trust.paths.len()
    );
    say!("cache          {} ({} schemas)", cache.root().display(), entries);
    say!("help probes    {:?}", config.help_probes);
    say!("man pages      {}", if config.man_pages { "on" } else { "off" });
    say!(
        "providers      {}",
        config.providers.iter().cloned().collect::<Vec<_>>().join(",")
    );
    ExitCode::SUCCESS
}

fn cache_cmd(args: &[String]) -> ExitCode {
    let Some(dirs) = Dirs::from_env() else {
        return fail("HOME is not set");
    };
    let cache = Cache::new(&dirs);
    match args.first().map(String::as_str) {
        Some("clear") => match cache.clear() {
            Ok(()) => {
                say!("removed {}", cache.root().display());
                ExitCode::SUCCESS
            }
            Err(e) => fail(e),
        },
        Some("path") => {
            say!("{}", cache.root().display());
            ExitCode::SUCCESS
        }
        _ => fail("usage: tabtrail cache clear|path"),
    }
}
