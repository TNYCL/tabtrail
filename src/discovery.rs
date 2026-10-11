//! Discovery: turning trusted help output or installed manual pages into a
//! cached schema. Runs only in the explicit `tabtrail discover` command or in
//! a detached background worker, never on the foreground Tab path.
//!
//! Policy
//! * Help probes (`<cmd> [sub...] --help`, then `-h`) run only for
//!   executables whose canonical path is on the user's trust list, and only
//!   when `help_probes` is not `off`.
//! * Manual pages are read for any command when `man_pages` is on; this
//!   executes nothing.
//! * Subcommands are probed lazily, one path at a time, and only when the
//!   parent's metadata lists that exact subcommand (evidence), never by
//!   enumerating every listed subcommand.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::cache::{COLLECTION_LOCALE, Cache, Lock, Lookup, Marker};
use crate::config::{Config, Dirs, HelpProbes, TrustList};
use crate::fsutil::{create_private_dir, fnv1a64, now_unix};
use crate::help::{PARSER_VERSION, is_valid_word, parse_help};
use crate::man;
use crate::probe::{self, ProbeSpec};
use crate::resolve::{Resolved, current_identity};
use crate::schema::{CommandNode, NodeStatus, Provenance, SCHEMA_VERSION, SchemaDocument, SourceKind};

pub const MAX_DEPTH: usize = 4;
pub const MAX_PROBE_OUTPUT: usize = 512 * 1024;
pub const JOB_DEADLINE: Duration = Duration::from_secs(20);
/// Do not retry a discovery that a worker actually started for the same
/// executable identity, path and policy within this window.
pub const RETRY_AFTER: Duration = Duration::from_secs(600);
/// Do not spawn another worker for the same request within this window,
/// even if the previous one could not start (all slots busy). Bounds the
/// number of processes repeated Tab presses can create.
pub const SPAWN_THROTTLE: Duration = Duration::from_secs(3);
pub const WORKER_SLOTS: usize = 2;

pub use crate::schema::CollectionPolicy as Policy;

impl Policy {
    pub fn decide(config: &Config, trust: &TrustList, canonical: &str) -> Policy {
        Policy {
            help: config.help_probes == HelpProbes::Trusted && trust.contains(canonical),
            man: config.man_pages,
            man_roots: 0,
        }
    }

    /// Records which manual page roots are in effect for `exe_path` under
    /// the given MANPATH/PATH, so documents built from other roots are
    /// recognised as stale.
    pub fn with_man_roots(
        mut self,
        exe_path: &str,
        man_path: Option<&std::ffi::OsStr>,
        path_var: Option<&std::ffi::OsStr>,
    ) -> Policy {
        self.man_roots = if self.man {
            man::roots_fingerprint(&man::man_roots(Path::new(exe_path), man_path, path_var))
        } else {
            0
        };
        self
    }

    pub fn any(self) -> bool {
        self.help || self.man
    }
}

pub struct Env {
    pub path_var: Option<OsString>,
    pub man_path: Option<OsString>,
    /// Upper bound for a single probe.
    pub probe_timeout: Duration,
    /// Upper bound for a whole job; every probe is clamped to what remains.
    pub job_budget: Duration,
    pub probe_cwd: PathBuf,
}

impl Env {
    pub fn from_process(config: &Config, cache: &Cache) -> Env {
        Env {
            path_var: std::env::var_os("PATH"),
            man_path: std::env::var_os("MANPATH"),
            probe_timeout: Duration::from_millis(config.probe_timeout_ms),
            job_budget: JOB_DEADLINE,
            probe_cwd: cache.root().join("probe-cwd"),
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub probes_run: Vec<String>,
    pub man_pages_read: Vec<String>,
    pub updated: bool,
    pub cancelled: Option<&'static str>,
}

/// Below this remaining budget a probe is not started at all.
const MIN_PROBE_BUDGET: Duration = Duration::from_millis(20);

fn help_probe(
    resolved: &Resolved,
    sub: &[String],
    env: &Env,
    deadline: Instant,
    report: &mut Report,
) -> Option<(String, String)> {
    create_private_dir(&env.probe_cwd).ok()?;
    for switch in ["--help", "-h"] {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining < MIN_PROBE_BUDGET {
            report.cancelled = Some("job deadline reached");
            return None;
        }
        let mut args: Vec<String> = sub.to_vec();
        args.push(switch.to_string());
        let argv_text = format!("{} {}", resolved.name, args.join(" "));
        report.probes_run.push(argv_text.clone());
        let result = probe::run(&ProbeSpec {
            program: Path::new(&resolved.identity.path),
            argv0: &resolved.name,
            args: &args,
            env: probe::probe_env(),
            cwd: &env.probe_cwd,
            timeout: env.probe_timeout.min(remaining),
            max_output: MAX_PROBE_OUTPUT,
        })
        .ok()?;
        if result.timed_out {
            return None; // a program that hangs on --help is not probed further
        }
        // Help goes to stdout by convention, but many tools use stderr.
        let text = if result.stdout.iter().any(|b| !b.is_ascii_whitespace()) {
            String::from_utf8_lossy(&result.stdout).into_owned()
        } else {
            String::from_utf8_lossy(&result.stderr).into_owned()
        };
        let mut path: Vec<&str> = vec![resolved.name.as_str()];
        path.extend(sub.iter().map(String::as_str));
        if parse_help(&text, &path).has_content() {
            return Some((text, argv_text));
        }
    }
    None
}

fn man_source(resolved: &Resolved, sub: &[String], env: &Env, report: &mut Report) -> Option<(String, PathBuf)> {
    let roots = man::man_roots(
        Path::new(&resolved.identity.path),
        env.man_path.as_deref(),
        env.path_var.as_deref(),
    );
    let mut name = resolved.name.clone();
    for s in sub {
        name.push('-');
        name.push_str(s);
    }
    let page = man::find_page(&roots, &name)?;
    let (roff, source) = man::read_page(&page)?;
    report.man_pages_read.push(page.display().to_string());
    Some((man::roff_to_text(&roff), source))
}

fn discover_node(
    resolved: &Resolved,
    sub: &[String],
    policy: Policy,
    env: &Env,
    deadline: Instant,
    report: &mut Report,
    parent_hash: Option<u64>,
) -> Option<CommandNode> {
    let mut path: Vec<&str> = vec![resolved.name.as_str()];
    path.extend(sub.iter().map(String::as_str));
    if policy.help
        && let Some((text, argv)) = help_probe(resolved, sub, env, deadline, report)
    {
        let hash = fnv1a64(text.as_bytes());
        if parent_hash == Some(hash) {
            return Some(CommandNode {
                status: NodeStatus::NoSeparateHelp,
                ..Default::default()
            });
        }
        let mut node = parse_help(&text, &path);
        node.source = Some(Provenance {
            kind: SourceKind::HelpOutput,
            detail: argv,
            parser_version: PARSER_VERSION,
            output_hash: Some(hash),
            source_identity: None,
        });
        return Some(node);
    }
    if policy.man
        && let Some((text, page)) = man_source(resolved, sub, env, report)
    {
        let mut node = parse_help(&text, &path);
        if node.has_content() {
            node.source = Some(Provenance {
                kind: SourceKind::ManPage,
                detail: page.to_string_lossy().into_owned(),
                parser_version: PARSER_VERSION,
                output_hash: None,
                source_identity: man::file_identity(&page),
            });
            return Some(node);
        }
    }
    None
}

/// Merges freshly discovered details into a node that was only listed,
/// keeping the listing's name, aliases and description.
fn merge_into(target: &mut CommandNode, found: CommandNode) {
    let name = std::mem::take(&mut target.name);
    let aliases = std::mem::take(&mut target.aliases);
    let description = target.description.take();
    *target = CommandNode {
        name,
        aliases,
        description: description.or(found.description.clone()),
        ..found
    };
    if !target.has_content() && target.status == NodeStatus::Described {
        target.status = NodeStatus::Unavailable;
    }
}

/// Runs one discovery job for `resolved` and the subcommand path `sub`.
/// `force` discards previously collected metadata (explicit discovery).
pub fn run_job(cache: &Cache, resolved: &Resolved, sub: &[String], policy: Policy, env: &Env, force: bool) -> Report {
    let deadline = Instant::now() + env.job_budget;
    let mut report = Report::default();
    if sub.len() > MAX_DEPTH || sub.iter().any(|s| !is_valid_word(s) || s.starts_with('-')) {
        report.cancelled = Some("invalid subcommand path");
        return report;
    }
    let mut doc = match cache.load(&resolved.identity, policy.man_roots) {
        Lookup::Fresh(doc) if !force && !policy.wider_than(doc.collected_with) => *doc,
        _ => SchemaDocument {
            schema_version: SCHEMA_VERSION,
            parser_version: PARSER_VERSION,
            executable: resolved.identity.clone(),
            locale: COLLECTION_LOCALE.to_string(),
            collected_at_unix: now_unix(),
            collected_with: policy,
            root: CommandNode {
                name: resolved.name.clone(),
                status: NodeStatus::Listed,
                ..Default::default()
            },
        },
    };

    if doc.root.status == NodeStatus::Listed {
        match discover_node(resolved, &[], policy, env, deadline, &mut report, None) {
            Some(found) => merge_into(&mut doc.root, found),
            None => doc.root.status = NodeStatus::Unavailable,
        }
        report.updated = true;
    }

    // Walk the requested path, discovering each listed level on demand.
    for depth in 1..=sub.len() {
        if Instant::now() >= deadline {
            report.cancelled = Some("job deadline reached");
            break;
        }
        let parent_path = &sub[..depth - 1];
        let Some(parent) = doc.root.descend(parent_path) else {
            break;
        };
        let parent_hash = parent.source.as_ref().and_then(|s| s.output_hash);
        let Some(node) = parent.subcommand(&sub[depth - 1]) else {
            break; // no evidence that this subcommand exists
        };
        if node.status != NodeStatus::Listed {
            continue;
        }
        let canonical_path: Vec<String> = {
            let mut p = parent_path.to_vec();
            p.push(node.name.clone());
            p
        };
        let found = discover_node(
            resolved,
            &canonical_path,
            policy,
            env,
            deadline,
            &mut report,
            parent_hash,
        );
        if report.cancelled.is_some() {
            break; // out of budget: leave the node listed for a later attempt
        }
        let Some(target) = doc.root.descend_mut(&canonical_path) else {
            break;
        };
        match found {
            Some(f) => merge_into(target, f),
            None => target.status = NodeStatus::Unavailable,
        }
        report.updated = true;
    }

    if report.updated {
        // Cancel if the executable changed while we were looking at it.
        if current_identity(&resolved.identity.path).as_ref() != Some(&resolved.identity) {
            report.cancelled = Some("executable changed during discovery");
            report.updated = false;
            return report;
        }
        doc.collected_at_unix = now_unix();
        if cache.store(&doc).is_err() {
            report.updated = false;
        }
    }
    report
}

/// Starts a detached background discovery unless one ran recently for the
/// same executable identity and path. Never waits for it. `stale` means the
/// cached entry was found outdated (changed manual page, corrupt file): the
/// long cooldown is then bypassed, but the short spawn throttle still
/// bounds how often a worker can start.
pub fn spawn_background(cache: &Cache, resolved: &Resolved, sub: &[String], policy: Policy, stale: bool) {
    let id = &resolved.identity;
    if !policy.any()
        || (!stale && cache.marker_recent(Marker::Attempt, id, sub, policy, RETRY_AFTER))
        || cache.marker_recent(Marker::Spawn, id, sub, policy, SPAWN_THROTTLE)
    {
        return;
    }
    cache.record_marker(Marker::Spawn, id, sub, policy);
    let Ok(exe) = std::env::current_exe() else { return };
    let mut cmd = Command::new(exe);
    cmd.arg("__discover")
        .arg("--path")
        .arg(&resolved.identity.path)
        .arg("--name")
        .arg(&resolved.name);
    for s in sub {
        cmd.arg("--sub").arg(s);
    }
    use std::os::unix::process::CommandExt;
    // Detach stdio so the shell's command substitution does not wait, and use
    // a separate process group so terminal signals do not reach it.
    let _ = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
}

#[derive(Debug, PartialEq, Eq)]
pub enum WorkerOutcome {
    /// All worker slots busy; the foreground retries after SPAWN_THROTTLE.
    NoSlot,
    /// The executable vanished or changed path safety.
    Vanished,
    /// Another worker holds this executable's job stripe.
    JobBusy,
    Ran(Report),
}

/// Entry point of the background worker process.
pub fn worker_main(dirs: &Dirs, path: &str, name: &str, sub: &[String]) -> WorkerOutcome {
    let cache = Cache::new(dirs);
    // Bounded concurrency: a fixed number of worker slots.
    let slot = (0..WORKER_SLOTS).find_map(|i| cache.try_lock(Lock::WorkerSlot(i)));
    let Some(_slot) = slot else {
        return WorkerOutcome::NoSlot;
    };
    let Some(identity) = current_identity(path) else {
        return WorkerOutcome::Vanished;
    };
    let resolved = Resolved {
        name: name.to_string(),
        identity,
    };
    let Some(_job) = cache.try_lock(Lock::job(&resolved.identity)) else {
        return WorkerOutcome::JobBusy;
    };
    let config = Config::load(dirs);
    let trust = TrustList::load(dirs);
    let env = Env::from_process(&config, &cache);
    let policy = Policy::decide(&config, &trust, &resolved.identity.canonical).with_man_roots(
        &resolved.identity.path,
        env.man_path.as_deref(),
        env.path_var.as_deref(),
    );
    // Only now has discovery really started: start the long cooldown.
    cache.record_marker(Marker::Attempt, &resolved.identity, sub, policy);
    WorkerOutcome::Ran(run_job(&cache, &resolved, sub, policy, &env, false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::resolve;
    use crate::testutil::{TempDir, write_script};

    struct Setup {
        _t: TempDir,
        cache: Cache,
        env: Env,
        bin: PathBuf,
        log: PathBuf,
    }

    fn setup(tag: &str) -> Setup {
        let t = TempDir::new(tag);
        let dirs = Dirs {
            config: t.path().join("config"),
            cache: t.path().join("cache"),
            data: t.path().join("data"),
        };
        let cache = Cache::new(&dirs);
        let bin = t.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let env = Env {
            path_var: Some(bin.clone().into_os_string()),
            man_path: Some(t.path().join("noman").into_os_string()),
            probe_timeout: Duration::from_millis(1500),
            job_budget: JOB_DEADLINE,
            probe_cwd: t.path().join("probe-cwd"),
        };
        let log = t.path().join("calls.log");
        Setup {
            _t: t,
            cache,
            env,
            bin,
            log,
        }
    }

    /// A fixture CLI that logs each invocation and prints help per path.
    fn fixture_cli(s: &Setup) -> Resolved {
        let script = format!(
            r#"echo "$*" >> '{log}'
case "$*" in
  "--help") printf 'Usage: fx [OPTIONS] <COMMAND>\n\nCommands:\n  deploy  Deploy\n  clean   Clean\n  danger  Never probe me\n\nOptions:\n  -v, --verbose  Verbose\n' ;;
  "deploy --help") printf 'Usage: fx deploy [OPTIONS]\n\nOptions:\n  --env <ENV>  Env [possible values: dev, prod]\n' ;;
  "clean --help") printf 'Usage: fx [OPTIONS] <COMMAND>\n\nCommands:\n  deploy  Deploy\n  clean   Clean\n  danger  Never probe me\n\nOptions:\n  -v, --verbose  Verbose\n' ;;
  *) echo "unexpected invocation" ;;
esac"#,
            log = s.log.display()
        );
        write_script(&s.bin.join("fx"), &script);
        resolve("fx", Some(s.bin.as_os_str()), &s.bin).unwrap()
    }

    fn calls(s: &Setup) -> Vec<String> {
        std::fs::read_to_string(&s.log)
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    const TRUSTED: Policy = Policy {
        help: true,
        man: false,
        man_roots: 0,
    };

    #[test]
    fn untrusted_executable_is_never_run() {
        let s = setup("disc-untrusted");
        let r = fixture_cli(&s);
        let report = run_job(
            &s.cache,
            &r,
            &[],
            Policy {
                help: false,
                man: true,
                man_roots: 0,
            },
            &s.env,
            false,
        );
        assert!(report.probes_run.is_empty());
        assert!(calls(&s).is_empty(), "untrusted fixture was executed");
        match s.cache.load(&r.identity, 0) {
            Lookup::Fresh(doc) => assert_eq!(doc.root.status, NodeStatus::Unavailable),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn root_then_lazy_subcommand_probe_only_on_evidence() {
        let s = setup("disc-lazy");
        let r = fixture_cli(&s);
        run_job(&s.cache, &r, &[], TRUSTED, &s.env, false);
        assert_eq!(
            calls(&s),
            ["--help"],
            "only the root is probed, subcommands are not enumerated"
        );
        run_job(&s.cache, &r, &["deploy".into()], TRUSTED, &s.env, false);
        assert_eq!(calls(&s), ["--help", "deploy --help"]);
        let Lookup::Fresh(doc) = s.cache.load(&r.identity, 0) else {
            panic!()
        };
        let deploy = doc.root.subcommand("deploy").unwrap();
        assert_eq!(deploy.status, NodeStatus::Described);
        assert_eq!(deploy.description.as_deref(), Some("Deploy"));
        assert!(deploy.flag("--env").is_some());
        // A subcommand with no evidence in the parent listing is not probed.
        let report = run_job(&s.cache, &r, &["rm-rf".into()], TRUSTED, &s.env, false);
        assert!(report.probes_run.is_empty());
        assert_eq!(calls(&s).len(), 2);
        // Hostile path components are rejected outright.
        let report = run_job(&s.cache, &r, &["--evil".into()], TRUSTED, &s.env, false);
        assert!(report.cancelled.is_some());
    }

    #[test]
    fn subcommand_that_echoes_parent_help_is_marked() {
        let s = setup("disc-same");
        let r = fixture_cli(&s);
        run_job(&s.cache, &r, &["clean".into()], TRUSTED, &s.env, false);
        let Lookup::Fresh(doc) = s.cache.load(&r.identity, 0) else {
            panic!()
        };
        assert_eq!(doc.root.subcommand("clean").unwrap().status, NodeStatus::NoSeparateHelp);
        assert!(doc.root.subcommand("clean").unwrap().flags.is_empty());
    }

    #[test]
    fn hanging_help_probe_does_not_block_or_cache_garbage() {
        let s = setup("disc-hang");
        write_script(&s.bin.join("hang"), "sleep 30");
        let r = resolve("hang", Some(s.bin.as_os_str()), &s.bin).unwrap();
        let start = Instant::now();
        let mut env = s.env;
        env.probe_timeout = Duration::from_millis(300);
        let report = run_job(&s.cache, &r, &[], TRUSTED, &env, false);
        assert!(start.elapsed() < Duration::from_secs(2));
        assert_eq!(report.probes_run.len(), 1, "no second probe after a hang");
        let Lookup::Fresh(doc) = s.cache.load(&r.identity, 0) else {
            panic!()
        };
        assert_eq!(doc.root.status, NodeStatus::Unavailable);
    }

    #[test]
    fn every_probe_is_clamped_to_the_job_budget() {
        let s = setup("disc-budget");
        write_script(&s.bin.join("slow"), "sleep 30");
        let r = resolve("slow", Some(s.bin.as_os_str()), &s.bin).unwrap();
        let mut env = s.env;
        env.probe_timeout = Duration::from_secs(30); // the configurable maximum
        env.job_budget = Duration::from_millis(300);
        let start = Instant::now();
        let report = run_job(&s.cache, &r, &[], TRUSTED, &env, false);
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(300 + 300),
            "job overran its budget: {elapsed:?}"
        );
        assert_eq!(report.probes_run.len(), 1);
    }

    #[test]
    fn executable_replaced_during_discovery_is_not_cached() {
        let s = setup("disc-replace");
        let target = s.bin.join("morph");
        write_script(
            &target,
            &format!(
                "printf 'Options:\\n  --a  A\\n'; cp '{0}.next' '{0}.tmp'; mv '{0}.tmp' '{0}'",
                target.display()
            ),
        );
        write_script(
            &PathBuf::from(format!("{}.next", target.display())),
            "printf 'Options:\\n  --b  B\\n'; sleep 0",
        );
        let r = resolve("morph", Some(s.bin.as_os_str()), &s.bin).unwrap();
        let report = run_job(&s.cache, &r, &[], TRUSTED, &s.env, false);
        assert_eq!(report.cancelled, Some("executable changed during discovery"));
        assert_eq!(s.cache.load(&r.identity, 0), Lookup::Missing);
    }

    #[test]
    fn widening_the_policy_recollects() {
        let s = setup("disc-widen");
        let r = fixture_cli(&s);
        run_job(
            &s.cache,
            &r,
            &[],
            Policy {
                help: false,
                man: true,
                man_roots: 0,
            },
            &s.env,
            false,
        );
        assert!(calls(&s).is_empty());
        // After `tabtrail trust`, the narrower document is replaced.
        run_job(
            &s.cache,
            &r,
            &[],
            Policy {
                help: true,
                man: true,
                man_roots: 0,
            },
            &s.env,
            false,
        );
        assert_eq!(calls(&s), ["--help"]);
        let Lookup::Fresh(doc) = s.cache.load(&r.identity, 0) else {
            panic!()
        };
        assert_eq!(doc.root.status, NodeStatus::Described);
        // Same policy again: reused, nothing re-run.
        run_job(
            &s.cache,
            &r,
            &[],
            Policy {
                help: true,
                man: true,
                man_roots: 0,
            },
            &s.env,
            false,
        );
        assert_eq!(calls(&s).len(), 1);
        // Forced (explicit) discovery re-runs.
        run_job(
            &s.cache,
            &r,
            &[],
            Policy {
                help: true,
                man: true,
                man_roots: 0,
            },
            &s.env,
            true,
        );
        assert_eq!(calls(&s).len(), 2);
    }

    #[test]
    fn busy_worker_slots_do_not_start_the_long_cooldown() {
        let s = setup("disc-busy");
        let r = fixture_cli(&s);
        let dirs = Dirs {
            config: s._t.path().join("config"),
            cache: s._t.path().join("cache"),
            data: s._t.path().join("data"),
        };
        let mut trust = TrustList::default();
        trust.paths.insert(r.identity.canonical.clone());
        trust.save(&dirs).unwrap();
        // Same policy (including the man-root fingerprint) the worker derives
        // from its environment.
        let policy = Policy::decide(&Config::load(&dirs), &trust, &r.identity.canonical).with_man_roots(
            &r.identity.path,
            std::env::var_os("MANPATH").as_deref(),
            std::env::var_os("PATH").as_deref(),
        );
        let held: Vec<_> = (0..WORKER_SLOTS)
            .map(|i| s.cache.try_lock(Lock::WorkerSlot(i)).unwrap())
            .collect();
        assert_eq!(worker_main(&dirs, &r.identity.path, "fx", &[]), WorkerOutcome::NoSlot);
        assert!(calls(&s).is_empty());
        assert!(
            !s.cache
                .marker_recent(Marker::Attempt, &r.identity, &[], policy, RETRY_AFTER)
        );
        drop(held);
        // flock locks belong to the open file description: a child that
        // another test thread forked a moment ago holds a copy of the fd
        // until its exec closes it (O_CLOEXEC), so the slots can stay busy
        // for a few milliseconds. The foreground handles this by retrying
        // after SPAWN_THROTTLE; here we retry briefly.
        let deadline = Instant::now() + Duration::from_secs(3);
        let outcome = loop {
            match worker_main(&dirs, &r.identity.path, "fx", &[]) {
                WorkerOutcome::NoSlot if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
                other => break other,
            }
        };
        assert!(matches!(outcome, WorkerOutcome::Ran(_)), "{outcome:?}");
        assert_eq!(calls(&s), ["--help"], "{outcome:?}");
        assert!(
            s.cache
                .marker_recent(Marker::Attempt, &r.identity, &[], policy, RETRY_AFTER)
        );
    }

    #[test]
    fn policy_requires_trust_and_probes_enabled() {
        let mut trust = TrustList::default();
        trust.paths.insert("/usr/bin/x".into());
        let cfg = Config::default();
        assert!(Policy::decide(&cfg, &trust, "/usr/bin/x").help);
        assert!(!Policy::decide(&cfg, &trust, "/usr/bin/y").help);
        let off = Config::parse("help_probes = off\nman_pages = off");
        let p = Policy::decide(&off, &trust, "/usr/bin/x");
        assert!(!p.help && !p.man && !p.any());
    }
}
