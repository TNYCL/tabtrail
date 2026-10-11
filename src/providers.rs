//! Explicit dynamic value providers for genuine gaps in static metadata.
//!
//! Providers complement the general engine; they never replace it.
//!
//! Trust: a provider only ever runs the executable the user is completing,
//! exactly as the shell resolves it (no second PATH lookup), and only when
//! that executable's canonical path is on the trust list (`tabtrail trust
//! git`). Untrusted commands never run, whatever their name.
//!
//! Each run uses a fixed, structured argv (the user's line is never passed
//! to a shell), a cleared environment, a deadline and an output cap. Values are not cached across invocations, so repository- or
//! daemon-specific results can never leak between directories or contexts.
//! Any failure simply yields no values.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use crate::completion::{SlotKind, ValueSlot};
use crate::config::Config;
use crate::config::TrustList;
use crate::context::ArgWord;
use crate::probe::{self, ProbeSpec};
use crate::resolve::Resolved;
use crate::sanitize::is_safe_candidate;

const MAX_OUTPUT: usize = 256 * 1024;
const MAX_VALUES: usize = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderId {
    GitLocalBranches,
    DockerContainers { all: bool },
}

impl ProviderId {
    pub fn name(self) -> &'static str {
        match self {
            ProviderId::GitLocalBranches => "git",
            ProviderId::DockerContainers { .. } => "docker",
        }
    }
}

const GIT_BRANCH_SUBCOMMANDS: &[&str] = &["switch", "checkout", "merge", "rebase"];
const DOCKER_RUNNING: &[&str] = &[
    "exec", "stop", "restart", "kill", "attach", "top", "pause", "port", "stats",
];
const DOCKER_ANY: &[&str] = &["logs", "inspect", "rm", "start", "unpause", "wait"];

/// The subcommand path and positional index used to select a provider.
/// When the schema resolved a subcommand path, the engine's slot is exact.
/// Otherwise (no schema, or a schema that does not list the subcommand -
/// help output is often incomplete) only the simple shape `cmd sub <word>`
/// is accepted, since the arity of unknown flags is unknown.
pub fn effective_slot(
    slot: Option<&ValueSlot>,
    has_schema: bool,
    args: &[ArgWord],
    prefix: &str,
) -> Option<(Vec<String>, usize)> {
    if has_schema {
        let s = slot?;
        match s.kind {
            SlotKind::Positional(i) if !s.path.is_empty() => return Some((s.path.clone(), i)),
            SlotKind::Positional(_) => {} // subcommand not in the schema: fall through
            SlotKind::FlagValue(_) => return None,
        }
    }
    if args.iter().any(|a| a.expansion || a.value.starts_with('-')) || prefix.starts_with('-') {
        return None;
    }
    let (first, rest) = args.split_first()?;
    let mut path = vec![first.value.clone()];
    // `docker container exec` style management groups
    let mut rest = rest;
    if first.value == "container" {
        let (second, r) = rest.split_first()?;
        path = vec![second.value.clone()];
        rest = r;
    }
    Some((path, rest.len()))
}

pub fn select(command: &str, path: &[String], positional: usize) -> Option<ProviderId> {
    let sub = path.last().map(String::as_str)?;
    match command {
        "git" if path.len() == 1 && positional == 0 && GIT_BRANCH_SUBCOMMANDS.contains(&sub) => {
            Some(ProviderId::GitLocalBranches)
        }
        "docker" if positional == 0 => {
            if DOCKER_RUNNING.contains(&sub) {
                Some(ProviderId::DockerContainers { all: false })
            } else if DOCKER_ANY.contains(&sub) {
                Some(ProviderId::DockerContainers { all: true })
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Local Docker endpoint, or None when the configured endpoint is remote
/// (remote endpoints would need an explicit network opt-in).
pub fn docker_socket(
    docker_host: Option<&str>,
    runtime_dir: Option<&Path>,
    exists: &dyn Fn(&Path) -> bool,
) -> Option<String> {
    match docker_host {
        Some(h) if !h.is_empty() => h.starts_with("unix://").then(|| h.to_string()),
        _ => {
            let system = Path::new("/var/run/docker.sock");
            if exists(system) {
                return Some(format!("unix://{}", system.display()));
            }
            let user = runtime_dir?.join("docker.sock");
            exists(&user).then(|| format!("unix://{}", user.display()))
        }
    }
}

fn base_env() -> Vec<(String, OsString)> {
    let mut env = Vec::new();
    for key in ["PATH", "HOME", "USER", "LOGNAME", "XDG_RUNTIME_DIR"] {
        if let Some(v) = std::env::var_os(key) {
            env.push((key.to_string(), v));
        }
    }
    env.push(("LC_ALL".into(), "C".into()));
    env
}

/// Why a provider did or did not run (for `tabtrail explain`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    Allowed,
    Disabled,
    Untrusted,
    /// The command may target a different repository/daemon than the one
    /// the provider would query; no values rather than wrong values.
    ContextUnsupported(&'static str),
}

pub fn permission(id: ProviderId, config: &Config, trust: &TrustList, exe: &Resolved) -> Permission {
    if !config.providers.contains(id.name()) {
        Permission::Disabled
    } else if !trust.contains(&exe.identity.canonical) {
        Permission::Untrusted
    } else {
        Permission::Allowed
    }
}

/// Environment lookup, injectable for tests.
pub type EnvLookup<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// Git variables that select the repository (or the refs) a command sees.
/// They are passed through unchanged so the provider looks at the same
/// repository as the user's command.
const GIT_REPO_SELECTORS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
];

/// Only the default Docker context is supported (its endpoint is known
/// locally). Reads at most 1 MiB of the CLI config and extracts nothing but
/// `currentContext`; other fields (including credentials) are skipped by the
/// deserializer and never stored or logged.
fn docker_context_is_default(env: EnvLookup<'_>) -> Result<(), &'static str> {
    #[derive(serde::Deserialize)]
    struct DockerCliConfig {
        #[serde(rename = "currentContext", default)]
        current_context: Option<String>,
    }
    let is_default = |c: &str| c.is_empty() || c == "default";
    if let Some(ctx) = env("DOCKER_CONTEXT") {
        return if is_default(&ctx.to_string_lossy()) {
            Ok(())
        } else {
            Err("non-default DOCKER_CONTEXT")
        };
    }
    let dir = match env("DOCKER_CONFIG") {
        Some(d) => std::path::PathBuf::from(d),
        None => match env("HOME") {
            Some(h) => std::path::PathBuf::from(h).join(".docker"),
            None => return Err("cannot locate the Docker CLI configuration"),
        },
    };
    let path = dir.join("config.json");
    let data = match crate::fsutil::read_regular(&path, 1024 * 1024) {
        Ok(Some(d)) => d,
        Ok(None) => return Ok(()),
        Err(_) => return Err("unreadable Docker CLI configuration"),
    };
    match serde_json::from_slice::<DockerCliConfig>(&data) {
        Ok(cfg) if cfg.current_context.as_deref().is_none_or(is_default) => Ok(()),
        Ok(_) => Err("non-default Docker currentContext"),
        Err(_) => Err("malformed Docker CLI configuration"),
    }
}

/// Decides whether the provider would query the same context as the
/// command being typed, and returns the extra environment to pass.
/// `args` are the command's arguments; the subcommand must come first:
/// global options before it (`git -C dir`, `git --git-dir=..`,
/// `docker -H ..`, `docker --context ..`) may change the context, so the
/// provider declines instead of guessing.
/// `command_is_first_word` is false when inline assignments
/// (`GIT_DIR=x git`) or wrappers (`env`, `sudo`) precede the command: they
/// change the environment or user the command runs with, so the provider
/// declines.
pub fn context(
    id: ProviderId,
    args: &[ArgWord],
    command_is_first_word: bool,
    env: EnvLookup<'_>,
) -> Result<Vec<(String, OsString)>, &'static str> {
    if !command_is_first_word {
        return Err("assignments or wrappers before the command change its environment");
    }
    if args.first().is_some_and(|a| a.value.starts_with('-')) {
        return Err("global options before the subcommand may select another context");
    }
    match id {
        ProviderId::GitLocalBranches => Ok(GIT_REPO_SELECTORS
            .iter()
            .filter_map(|k| env(k).map(|v| (k.to_string(), v)))
            .collect()),
        ProviderId::DockerContainers { .. } => {
            docker_context_is_default(env)?;
            let host = env("DOCKER_HOST").map(|h| h.to_string_lossy().into_owned());
            let runtime = env("XDG_RUNTIME_DIR").map(std::path::PathBuf::from);
            let sock = docker_socket(host.as_deref(), runtime.as_deref(), &|p| p.exists())
                .ok_or("Docker endpoint is not a local socket (network endpoints need an explicit opt-in)")?;
            let mut extra = vec![("TABTRAIL_DOCKER_HOST".to_string(), OsString::from(sock))];
            if let Some(cfg) = env("DOCKER_CONFIG") {
                extra.push(("DOCKER_CONFIG".into(), cfg));
            }
            Ok(extra)
        }
    }
}

/// Runs a provider with the user's own resolved executable and returns
/// sanitized values starting with `prefix`. Runs nothing unless
/// `permission` allows it and `context` accepts the command line.
#[allow(clippy::too_many_arguments)]
pub fn values(
    id: ProviderId,
    config: &Config,
    trust: &TrustList,
    exe: &Resolved,
    args_typed: &[ArgWord],
    command_is_first_word: bool,
    env_lookup: EnvLookup<'_>,
    cwd: &Path,
    prefix: &str,
) -> Vec<String> {
    if permission(id, config, trust, exe) != Permission::Allowed {
        return Vec::new();
    }
    let Ok(extra) = context(id, args_typed, command_is_first_word, env_lookup) else {
        return Vec::new();
    };
    let mut env = base_env();
    let mut docker_host = None;
    for (k, v) in extra {
        if k == "TABTRAIL_DOCKER_HOST" {
            docker_host = Some(v.to_string_lossy().into_owned());
        } else {
            env.push((k, v));
        }
    }
    let args: Vec<String> = match id {
        ProviderId::GitLocalBranches => {
            env.push(("GIT_TERMINAL_PROMPT".into(), "0".into()));
            env.push(("GIT_OPTIONAL_LOCKS".into(), "0".into()));
            vec![
                "-c".into(),
                "core.fsmonitor=false".into(),
                "for-each-ref".into(),
                "--format=%(refname:short)".into(),
                "refs/heads/".into(),
            ]
        }
        ProviderId::DockerContainers { all } => {
            let Some(sock) = docker_host else { return Vec::new() };
            let mut a = vec![
                "--host".into(),
                sock,
                "ps".into(),
                "--format".into(),
                "{{.Names}}".into(),
            ];
            if all {
                a.push("--all".into());
            }
            a
        }
    };
    let Ok(result) = probe::run(&ProbeSpec {
        program: Path::new(&exe.identity.path),
        argv0: &exe.name,
        args: &args,
        env,
        cwd,
        timeout: Duration::from_millis(config.provider_timeout_ms),
        max_output: MAX_OUTPUT,
    }) else {
        return Vec::new();
    };
    if result.timed_out || result.exit_code != Some(0) {
        return Vec::new();
    }
    // `docker ps --format {{.Names}}` joins multiple names with commas.
    parse_lines(
        &result.stdout,
        prefix,
        matches!(id, ProviderId::DockerContainers { .. }),
    )
}

/// One value per line; unsafe or non-matching lines are dropped.
pub fn parse_lines(out: &[u8], prefix: &str, split_commas: bool) -> Vec<String> {
    let text = String::from_utf8_lossy(out);
    let mut v: Vec<String> = text
        .lines()
        .flat_map(|l| {
            if split_commas {
                l.split(',').collect::<Vec<_>>()
            } else {
                vec![l]
            }
        })
        .map(str::trim)
        .filter(|l| is_safe_candidate(l) && l.starts_with(prefix))
        .map(String::from)
        .collect();
    v.sort();
    v.dedup();
    v.truncate(MAX_VALUES);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<ArgWord> {
        words
            .iter()
            .map(|w| ArgWord {
                value: w.to_string(),
                expansion: false,
            })
            .collect()
    }

    #[test]
    fn selection_rules() {
        let p = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(select("git", &p(&["switch"]), 0), Some(ProviderId::GitLocalBranches));
        assert_eq!(select("git", &p(&["switch"]), 1), None);
        assert_eq!(select("git", &p(&["log"]), 0), None);
        assert_eq!(
            select("docker", &p(&["exec"]), 0),
            Some(ProviderId::DockerContainers { all: false })
        );
        assert_eq!(
            select("docker", &p(&["rm"]), 0),
            Some(ProviderId::DockerContainers { all: true })
        );
        assert_eq!(
            select("docker", &p(&["exec"]), 1),
            None,
            "exec's second word is the command"
        );
        assert_eq!(select("other", &p(&["switch"]), 0), None);
    }

    #[test]
    fn schemaless_slot_only_for_simple_shapes() {
        assert_eq!(
            effective_slot(None, false, &args(&["switch"]), "ma"),
            Some((vec!["switch".into()], 0))
        );
        assert_eq!(
            effective_slot(None, false, &args(&["container", "exec"]), ""),
            Some((vec!["exec".into()], 0))
        );
        // unknown flag arity: refuse to guess
        assert_eq!(effective_slot(None, false, &args(&["exec", "-u", "root"]), ""), None);
        // a schema that does not list the subcommand behaves like no schema
        let root_slot = ValueSlot {
            path: vec![],
            kind: SlotKind::Positional(1),
            prefix: String::new(),
        };
        assert_eq!(
            effective_slot(Some(&root_slot), true, &args(&["checkout"]), ""),
            Some((vec!["checkout".into()], 0))
        );
        // a resolved path is used as is
        let sub_slot = ValueSlot {
            path: vec!["switch".into()],
            kind: SlotKind::Positional(0),
            prefix: String::new(),
        };
        assert_eq!(
            effective_slot(Some(&sub_slot), true, &args(&["-q", "switch"]), ""),
            Some((vec!["switch".into()], 0))
        );
        assert_eq!(effective_slot(None, false, &args(&[]), ""), None);
    }

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v))
    }

    #[test]
    fn global_options_before_the_subcommand_decline() {
        let e = env_of(&[]);
        for typed in [
            &["-C", "/other", "switch"][..],
            &["--git-dir=/x/.git", "switch"],
            &["-c", "a=b", "switch"],
        ] {
            assert!(
                context(ProviderId::GitLocalBranches, &args(typed), true, &e).is_err(),
                "{typed:?}"
            );
        }
        assert!(context(ProviderId::GitLocalBranches, &args(&["switch"]), true, &e).is_ok());
        assert!(context(ProviderId::GitLocalBranches, &args(&["switch"]), false, &e).is_err());
    }

    #[test]
    fn git_repo_selectors_are_preserved() {
        let e = env_of(&[
            ("GIT_DIR", "/repo-b/.git"),
            ("GIT_WORK_TREE", "/repo-b"),
            ("UNRELATED", "x"),
        ]);
        let extra = context(ProviderId::GitLocalBranches, &args(&["switch"]), true, &e).unwrap();
        let keys: Vec<&str> = extra.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["GIT_DIR", "GIT_WORK_TREE"]);
    }

    #[test]
    fn docker_contexts_fail_closed() {
        let t = crate::testutil::TempDir::new("prov-docker-ctx");
        let home = t.path().to_str().unwrap().to_string();
        let home: &'static str = Box::leak(home.into_boxed_str());
        let docker = ProviderId::DockerContainers { all: false };
        let typed = args(&["exec"]);
        let sock: &'static [(&str, &str)] = &[("DOCKER_HOST", "unix:///run/x.sock")];
        let _ = sock;
        // explicit env selectors
        let remote_env = env_of(Box::leak(Box::new([
            ("HOME", home),
            ("DOCKER_CONTEXT", "remote"),
            ("DOCKER_HOST", "unix:///x.sock"),
        ])));
        assert_eq!(
            context(docker, &typed, true, &remote_env).unwrap_err(),
            "non-default DOCKER_CONTEXT"
        );
        let tcp = env_of(Box::leak(Box::new([
            ("HOME", home),
            ("DOCKER_HOST", "tcp://10.1.2.3:2376"),
        ])));
        assert!(context(docker, &typed, true, &tcp).is_err());
        // CLI global options
        let ok_env = env_of(Box::leak(Box::new([("HOME", home), ("DOCKER_HOST", "unix:///x.sock")])));
        assert!(context(docker, &args(&["-H", "tcp://x", "exec"]), true, &ok_env).is_err());
        assert!(context(docker, &args(&["--context", "remote", "exec"]), true, &ok_env).is_err());
        assert!(
            context(docker, &typed, true, &ok_env).is_ok(),
            "no config file: default context"
        );
        // currentContext in the CLI config
        std::fs::create_dir_all(t.path().join(".docker")).unwrap();
        let cfg = t.path().join(".docker/config.json");
        std::fs::write(
            &cfg,
            r#"{"auths":{"r.example":{"auth":"c2VjcmV0"}},"currentContext":"remote"}"#,
        )
        .unwrap();
        assert_eq!(
            context(docker, &typed, true, &ok_env).unwrap_err(),
            "non-default Docker currentContext"
        );
        std::fs::write(&cfg, r#"{"currentContext":"default"}"#).unwrap();
        assert!(context(docker, &typed, true, &ok_env).is_ok());
        std::fs::write(&cfg, "{ not json").unwrap();
        assert!(
            context(docker, &typed, true, &ok_env).is_err(),
            "malformed config fails closed"
        );
        // DOCKER_CONTEXT=default overrides a remote currentContext
        std::fs::write(&cfg, r#"{"currentContext":"remote"}"#).unwrap();
        let def = env_of(Box::leak(Box::new([
            ("HOME", home),
            ("DOCKER_CONTEXT", "default"),
            ("DOCKER_HOST", "unix:///x.sock"),
        ])));
        assert!(context(docker, &typed, true, &def).is_ok());
    }

    #[test]
    fn docker_endpoint_policy() {
        let yes = |_: &Path| true;
        let no = |_: &Path| false;
        assert_eq!(
            docker_socket(None, None, &yes).as_deref(),
            Some("unix:///var/run/docker.sock")
        );
        assert_eq!(
            docker_socket(Some("unix:///x.sock"), None, &no).as_deref(),
            Some("unix:///x.sock")
        );
        assert_eq!(docker_socket(Some("tcp://10.0.0.1:2376"), None, &yes), None);
        assert_eq!(docker_socket(Some("ssh://host"), None, &yes), None);
        assert_eq!(docker_socket(None, Some(Path::new("/run/user/1")), &no), None);
    }

    #[test]
    fn provider_output_is_sanitized() {
        let out = b"main\nfeature/x\n\x1b[31mevil\nbad\x07bell\nmain\nfix\xff\n";
        assert_eq!(parse_lines(out, "", false), ["feature/x", "fix\u{fffd}", "main"]);
        assert_eq!(parse_lines(out, "fe", false), ["feature/x"]);
        // git branch names may contain commas; docker joins names with them
        assert_eq!(parse_lines(b"a,b\n", "", false), ["a,b"]);
        assert_eq!(parse_lines(b"a,b\n", "", true), ["a", "b"]);
    }

    fn fake(dir: &Path, name: &str, marker: &Path) -> Resolved {
        crate::testutil::write_script(
            &dir.join(name),
            &format!("touch '{}'; printf 'main\\nfeature\\n'", marker.display()),
        );
        crate::resolve::resolve(name, Some(dir.as_os_str()), dir).unwrap()
    }

    #[test]
    fn untrusted_or_disabled_providers_run_nothing() {
        let t = crate::testutil::TempDir::new("prov-trust");
        let marker = t.path().join("executed");
        let git = fake(t.path(), "git", &marker);
        let none = TrustList::default();
        let cfg = Config::default();
        assert_eq!(
            permission(ProviderId::GitLocalBranches, &cfg, &none, &git),
            Permission::Untrusted
        );
        let env = |k: &str| std::env::var_os(k);
        assert!(
            values(
                ProviderId::GitLocalBranches,
                &cfg,
                &none,
                &git,
                &[],
                true,
                &env,
                t.path(),
                ""
            )
            .is_empty()
        );
        assert!(!marker.exists(), "untrusted fake git was executed");
        let mut trust = TrustList::default();
        trust.paths.insert(git.identity.canonical.clone());
        let off = Config::parse("providers = none");
        assert!(
            values(
                ProviderId::GitLocalBranches,
                &off,
                &trust,
                &git,
                &[],
                true,
                &env,
                t.path(),
                ""
            )
            .is_empty()
        );
        assert!(!marker.exists(), "disabled provider was executed");
        let typed = args(&["switch"]);
        assert_eq!(
            values(
                ProviderId::GitLocalBranches,
                &cfg,
                &trust,
                &git,
                &typed,
                true,
                &env,
                t.path(),
                "f"
            ),
            ["feature"]
        );
        assert!(marker.exists());
        // inline assignments / wrappers: declined, nothing run
        std::fs::remove_file(&marker).unwrap();
        assert!(
            values(
                ProviderId::GitLocalBranches,
                &cfg,
                &trust,
                &git,
                &typed,
                false,
                &env,
                t.path(),
                ""
            )
            .is_empty()
        );
        assert!(!marker.exists());
    }
}
