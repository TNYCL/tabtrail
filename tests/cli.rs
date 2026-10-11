//! Binary-level tests: run the real `tabtrail` executable with an isolated
//! HOME and a fixture PATH, exactly as the shell adapters do.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_tabtrail");

struct Env {
    root: PathBuf,
    home: PathBuf,
    path: String,
}

impl Env {
    fn new(tag: &str) -> Env {
        let root = std::env::temp_dir().join(format!("tabtrail-cli-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(root.join("bin")).unwrap();
        Env {
            path: format!("{}:/usr/bin:/bin", root.join("bin").display()),
            root,
            home,
        }
    }

    fn script(&self, rel: &str, body: &str) -> PathBuf {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn run_with_path(&self, path: &str, args: &[&str]) -> Output {
        self.run_full(path, &[("DOCKER_HOST", "unix:///nonexistent.sock")], &self.home, args)
    }

    fn run_full(&self, path: &str, extra_env: &[(&str, &str)], cwd: &Path, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", path)
            .envs(extra_env.iter().copied())
            .current_dir(cwd)
            .output()
            .unwrap()
    }

    fn complete_in(&self, cwd: &Path, extra_env: &[(&str, &str)], line: &str) -> Vec<String> {
        let out = self.run_full(
            &self.path,
            extra_env,
            cwd,
            &["complete", "--shell", "bash", "--before", line],
        );
        assert!(out.status.success());
        let mut f: Vec<String> = out
            .stdout
            .split(|b| *b == 0)
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        f.pop();
        f.split_off(3)
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with_path(&self.path, args)
    }

    fn complete_bash(&self, line: &str) -> Vec<String> {
        self.complete_bash_with_path(&self.path, line)
    }

    fn complete_bash_with_path(&self, path: &str, line: &str) -> Vec<String> {
        let out = self.run_with_path(path, &["complete", "--shell", "bash", "--before", line]);
        assert!(out.status.success());
        let mut f: Vec<String> = out
            .stdout
            .split(|b| *b == 0)
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        assert_eq!(f.pop().as_deref(), Some(""), "frame must end with NUL");
        assert_eq!(f[0], "tabtrail1");
        f.split_off(3)
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn fake_git(env: &Env, rel: &str, marker: &str) -> PathBuf {
    env.script(
        rel,
        &format!(
            "touch \"$HOME/{marker}\"\ncase \"$*\" in *for-each-ref*) printf 'main\\nfeature-{marker}\\n' ;; esac"
        ),
    )
}

#[test]
fn untrusted_provider_executable_is_never_run() {
    let e = Env::new("prov-untrusted");
    fake_git(&e, "bin/git", "git-ran");
    e.script("bin/docker", "touch \"$HOME/docker-ran\"; echo web");
    assert!(e.complete_bash("git switch ").is_empty());
    assert!(e.complete_bash("docker exec ").is_empty());
    assert!(!e.home.join("git-ran").exists(), "untrusted fake git was executed");
    assert!(
        !e.home.join("docker-ran").exists(),
        "untrusted fake docker was executed"
    );
}

#[test]
fn trusted_provider_runs_the_resolved_executable_only() {
    let e = Env::new("prov-trusted");
    let trusted = fake_git(&e, "bin/git", "trusted-ran");
    assert!(e.run(&["trust", "git"]).status.success());
    let values = e.complete_bash("git switch ");
    assert_eq!(values, ["feature-trusted-ran", "main"]);
    assert!(e.home.join("trusted-ran").exists());

    // PATH now resolves `git` to a different, untrusted executable first:
    // it must not run, and the trusted one must not be substituted for it.
    fs::remove_file(e.home.join("trusted-ran")).unwrap();
    fake_git(&e, "other/git", "shadow-ran");
    let shadowed = format!("{}:{}", e.root.join("other").display(), e.path);
    assert!(e.complete_bash_with_path(&shadowed, "git switch ").is_empty());
    assert!(
        !e.home.join("shadow-ran").exists(),
        "untrusted PATH-first git was executed"
    );
    assert!(
        !e.home.join("trusted-ran").exists(),
        "trusted git was substituted for the resolved one"
    );

    // An explicit path to the trusted executable uses exactly that file.
    let line = format!("{} switch ", trusted.display());
    assert_eq!(
        e.complete_bash_with_path(&shadowed, &line),
        ["feature-trusted-ran", "main"]
    );
    assert!(!e.home.join("shadow-ran").exists());

    // Untrusting stops it again.
    assert!(e.run(&["untrust", "git"]).status.success());
    fs::remove_file(e.home.join("trusted-ran")).unwrap();
    assert!(e.complete_bash("git switch ").is_empty());
    assert!(!e.home.join("trusted-ran").exists());
}

#[test]
fn complete_never_runs_the_composed_command_or_untrusted_tools() {
    let e = Env::new("no-exec");
    e.script(
        "bin/tool",
        "echo \"$*\" >> \"$HOME/tool.log\"; printf 'Options:\\n  --alpha  A\\n'",
    );
    for line in ["tool ", "tool --al", "tool sub --x=$(touch PWN)", "tool `touch PWN2`"] {
        let _ = e.complete_bash(line);
    }
    // give a (wrongly) spawned background probe time to run
    std::thread::sleep(Duration::from_millis(300));
    assert!(!e.home.join("tool.log").exists(), "untrusted tool executed");
    assert!(!e.home.join("PWN").exists() && !e.home.join("PWN2").exists());
}

#[test]
fn background_discovery_after_trust_and_warm_completion() {
    let e = Env::new("bg");
    e.script(
        "bin/tool",
        "echo \"$*\" >> \"$HOME/tool.log\"\ncase \"$*\" in --help) printf 'Usage: tool <COMMAND>\\n\\nCommands:\\n  build  Build it\\n  bump   Bump\\n\\nOptions:\\n  --alpha  A\\n' ;; esac",
    );
    assert!(e.run(&["trust", "tool"]).status.success());
    // Cold: answers immediately (no waiting for discovery) with no candidates.
    let start = Instant::now();
    assert!(e.complete_bash("tool b").is_empty());
    assert!(start.elapsed() < Duration::from_millis(500));
    // The detached worker fills the cache shortly after.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got = Vec::new();
    while Instant::now() < deadline {
        got = e.complete_bash("tool b");
        if !got.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(got, ["build", "bump"]);
    let log = fs::read_to_string(e.home.join("tool.log")).unwrap();
    assert_eq!(log, "--help\n", "only the help probe ran, exactly once");
}

#[test]
fn replaced_executable_invalidates_cache() {
    let e = Env::new("replace");
    let help = |opt: &str| format!("case \"$*\" in --help) printf 'Options:\\n  --{opt}  X\\n' ;; esac");
    e.script("bin/tool", &help("old-flag"));
    assert!(e.run(&["trust", "tool"]).status.success());
    assert!(e.run(&["discover", "tool"]).status.success());
    assert_eq!(e.complete_bash("tool --"), ["--old-flag"]);
    // Replace the executable (new inode/size): the old schema must not be used.
    let tmp = e.script("bin/tool.new", &help("new-flag-longer"));
    fs::rename(&tmp, e.root.join("bin/tool")).unwrap();
    assert!(
        e.complete_bash("tool --").is_empty(),
        "stale schema used after replacement"
    );
    // `tabtrail trust` is by canonical path, so trust survives in-place replacement.
    assert!(e.run(&["discover", "tool"]).status.success());
    assert_eq!(e.complete_bash("tool --"), ["--new-flag-longer"]);
}

#[test]
fn malformed_requests_still_answer_well_formed() {
    let e = Env::new("malformed");
    for line in [
        "",
        "'",
        "\"",
        "$(",
        "a\\",
        "\u{1b}[31m",
        "x | | ;; &&",
        "tool \u{202e} --",
    ] {
        let _ = e.complete_bash(line);
    }
    let out = e.run(&["complete", "--shell", "nosuchshell", "--before", "x"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
}

#[test]
fn install_and_uninstall_round_trip() {
    let e = Env::new("install");
    let rc = e.home.join(".bashrc");
    fs::write(&rc, "# mine\n").unwrap();
    let out = e.run(&["install", "--shell", "bash"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(
        fs::read_to_string(&rc)
            .unwrap()
            .contains("# >>> tabtrail initialize >>>")
    );
    let script = e.home.join(".local/share/tabtrail/shell/tabtrail.bash");
    assert!(fs::read_to_string(&script).unwrap().contains("__tabtrail_bin="));
    assert!(e.run(&["uninstall", "--shell", "bash"]).status.success());
    assert_eq!(fs::read_to_string(&rc).unwrap(), "# mine\n");
    assert!(!script.exists());
    let _ = Path::new(BIN);
}

#[test]
fn trust_rejects_paths_that_could_inject_trust_lines() {
    let e = Env::new("trust-inject");
    // An executable whose path embeds "\n/usr/bin/env": a line-based trust
    // file must never gain the unrelated /usr/bin/env entry.
    let evil = e.script("trusted\n/usr/bin/env", "true");
    let out = e.run(&["trust", evil.to_str().unwrap()]);
    assert!(!out.status.success(), "trusting an unrepresentable path must fail");
    let trust_file = e.home.join(".config/tabtrail/trusted");
    let text = fs::read_to_string(&trust_file).unwrap_or_default();
    assert!(
        !text.lines().any(|l| l == "/usr/bin/env"),
        "injected trust entry: {text:?}"
    );
    assert!(!text.contains("trusted"), "{text:?}");
    // The diagnostic does not echo raw control characters.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("\\u{a}"),
        "control character should be shown escaped: {stderr:?}"
    );

    // Only the explicitly selected canonical path is trusted.
    let good = e.script("bin/good", "true");
    assert!(e.run(&["trust", good.to_str().unwrap()]).status.success());
    let text = fs::read_to_string(&trust_file).unwrap();
    let entries: Vec<&str> = text.lines().filter(|l| l.starts_with('/')).collect();
    assert_eq!(entries, [fs::canonicalize(&good).unwrap().to_str().unwrap()]);
}

#[test]
fn explain_escapes_hostile_values() {
    let e = Env::new("explain-esc");
    let out = e.run(&["explain", "tool \u{1b}]0;PWNED\u{7} --x"]);
    assert!(out.status.success());
    assert!(!out.stdout.contains(&0x1b), "raw ESC in diagnostic output");
    assert!(!out.stdout.contains(&0x07), "raw BEL in diagnostic output");
}

fn git(args: &[&str], cwd: &Path) {
    let st = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?}");
}

#[test]
fn git_provider_never_lists_another_repositorys_branches() {
    let Some(real_git) =
        std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("git")).find(|g| g.is_file()))
    else {
        eprintln!("git not installed; skipping");
        return;
    };
    let e = Env::new("git-ctx");
    let alpha = e.home.join("alpha");
    let beta = e.home.join("beta");
    for (repo, branch) in [(&alpha, "alpha-only"), (&beta, "beta-only")] {
        fs::create_dir_all(repo).unwrap();
        git(&["init", "-q", "-b", "main"], repo);
        git(&["commit", "-q", "--allow-empty", "-m", "x"], repo);
        git(&["branch", branch], repo);
    }
    // Use the real git through PATH and trust exactly that executable.
    let path = format!("{}:{}", real_git.parent().unwrap().display(), e.path);
    let env_path = |cwd: &Path, extra: &[(&str, &str)], line: &str| {
        let out = e.run_full(&path, extra, cwd, &["complete", "--shell", "bash", "--before", line]);
        let mut f: Vec<String> = out
            .stdout
            .split(|b| *b == 0)
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        f.pop();
        f.split_off(3)
    };
    assert!(e.run_full(&path, &[], &e.home, &["trust", "git"]).status.success());

    let here = env_path(&alpha, &[], "git switch ");
    assert!(here.contains(&"alpha-only".to_string()), "{here:?}");
    assert!(!here.contains(&"beta-only".to_string()), "{here:?}");

    // A repository selected through the environment is honoured, not replaced
    // by the current directory's repository.
    let beta_git = beta.join(".git");
    let beta_git = beta_git.to_str().unwrap();
    let via_env = env_path(&alpha, &[("GIT_DIR", beta_git)], "git switch ");
    assert!(via_env.contains(&"beta-only".to_string()), "{via_env:?}");
    assert!(!via_env.contains(&"alpha-only".to_string()), "{via_env:?}");

    // Context-changing syntax the provider does not model: no values at all.
    let beta_s = beta.to_str().unwrap();
    for line in [
        format!("git -C {beta_s} switch "),
        format!("git --git-dir={beta_git} switch "),
        format!("GIT_DIR={beta_git} git switch "),
        format!("env GIT_DIR={beta_git} git switch "),
        "sudo git switch ".to_string(),
    ] {
        let got = env_path(&alpha, &[], &line);
        assert!(
            !got.iter().any(|v| v == "alpha-only" || v == "beta-only"),
            "{line}: leaked {got:?}"
        );
    }
}

#[test]
fn docker_provider_declines_other_contexts() {
    let e = Env::new("docker-ctx");
    e.script(
        "bin/docker",
        "printf '%s\\n' \"$*\" >> \"$HOME/docker.log\"\ncase \"$*\" in *' ps '*) echo web-1 ;; esac",
    );
    assert!(e.run(&["trust", "docker"]).status.success());
    let sock = [("DOCKER_HOST", "unix:///run/test-docker.sock")];
    let ps_calls = || {
        fs::read_to_string(e.home.join("docker.log"))
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains(" ps "))
            .count()
    };
    assert_eq!(e.complete_in(&e.home, &sock, "docker exec "), ["web-1"]);
    assert_eq!(ps_calls(), 1);
    let cases: Vec<(Vec<(&str, &str)>, &str)> = vec![
        (sock.to_vec(), "docker -H tcp://10.0.0.1:2376 exec "),
        (sock.to_vec(), "docker --context remote exec "),
        (
            vec![
                ("DOCKER_HOST", "unix:///run/test-docker.sock"),
                ("DOCKER_CONTEXT", "remote"),
            ],
            "docker exec ",
        ),
        (vec![("DOCKER_HOST", "tcp://10.0.0.1:2376")], "docker exec "),
        (vec![("DOCKER_HOST", "ssh://user@host")], "docker exec "),
        (sock.to_vec(), "DOCKER_CONTEXT=remote docker exec "),
    ];
    for (env, line) in &cases {
        assert!(e.complete_in(&e.home, env, line).is_empty(), "{line} {env:?}");
    }
    // A non-default currentContext in the Docker CLI config.
    fs::create_dir_all(e.home.join(".docker")).unwrap();
    fs::write(e.home.join(".docker/config.json"), r#"{"currentContext":"remote"}"#).unwrap();
    assert!(e.complete_in(&e.home, &sock, "docker exec ").is_empty());
    assert_eq!(ps_calls(), 1, "no provider run for any other context");
}

fn mkfifo(p: &Path) {
    let st = Command::new("mkfifo").arg(p).status().unwrap();
    assert!(st.success());
}

fn timed_complete(e: &Env, extra: &[(&str, &str)], line: &str) -> (Duration, Vec<String>) {
    // A hung helper would block forever; run it on a thread with a bound.
    let (tx, rx) = std::sync::mpsc::channel();
    let path = e.path.clone();
    let home = e.home.clone();
    let extra: Vec<(String, String)> = extra.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let shown = line.to_string();
    let line = line.to_string();
    std::thread::spawn(move || {
        let start = Instant::now();
        let child = Command::new(BIN)
            .args(["complete", "--shell", "bash", "--before", &line])
            .env_clear()
            .env("HOME", &home)
            .env("PATH", &path)
            .envs(extra.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .current_dir(&home)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id();
        let _ = tx.send((pid, None));
        let out = child.wait_with_output().unwrap();
        let _ = tx.send((pid, Some((start.elapsed(), out.stdout))));
    });
    let (pid, _) = rx.recv().unwrap();
    match rx.recv_timeout(Duration::from_secs(3)) {
        Ok((_, Some((elapsed, stdout)))) => {
            let mut f: Vec<String> = stdout
                .split(|b| *b == 0)
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .collect();
            assert_eq!(f.pop().as_deref(), Some(""), "frame must end with NUL");
            assert_eq!(f[0], "tabtrail1");
            (elapsed, f.split_off(3))
        }
        _ => {
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
            panic!("`tabtrail complete` hung on {shown:?}");
        }
    }
}

#[test]
fn special_files_never_block_the_tab_path() {
    let e = Env::new("fifo");
    e.script(
        "bin/ttfifo",
        "case \"$*\" in --help) printf 'Options:\\n  --one  One\\n' ;; esac",
    );
    assert!(e.run(&["trust", "ttfifo"]).status.success());
    assert!(e.run(&["discover", "ttfifo"]).status.success());
    assert_eq!(timed_complete(&e, &[], "ttfifo --").1, ["--one"]);

    // Cache entry replaced by a FIFO, then by an oversized file.
    let schemas = e.home.join(".cache/tabtrail/v1/schemas");
    let entry = fs::read_dir(&schemas)
        .unwrap()
        .flatten()
        .find(|d| d.path().extension().is_some_and(|x| x == "json"))
        .unwrap()
        .path();
    fs::remove_file(&entry).unwrap();
    mkfifo(&entry);
    let (t, got) = timed_complete(&e, &[], "ttfifo --");
    assert!(got.is_empty());
    assert!(t < Duration::from_secs(1), "{t:?}");
    let _ = fs::remove_file(&entry);
    fs::write(&entry, vec![b' '; 3 * 1024 * 1024]).unwrap();
    let (t, got) = timed_complete(&e, &[], "ttfifo --");
    // A background refresh may already have replaced the entry: either
    // nothing or the correct value, never garbage, never a hang.
    assert!(got.is_empty() || got == ["--one"], "{got:?}");
    assert!(t < Duration::from_secs(1), "{t:?}");

    // Trust file and config file as FIFOs: nothing is trusted, nothing hangs.
    let trust = e.home.join(".config/tabtrail/trusted");
    fs::remove_file(&trust).unwrap();
    mkfifo(&trust);
    mkfifo(&e.home.join(".config/tabtrail/config"));
    let (t, _) = timed_complete(&e, &[], "ttfifo --");
    assert!(t < Duration::from_secs(1), "{t:?}");

    // Docker CLI config as a FIFO: provider context fails closed, quickly.
    e.script(
        "bin/docker",
        "printf '%s\\n' \"$*\" >> \"$HOME/docker.log\"; echo web-1",
    );
    fs::remove_file(&trust).unwrap();
    assert!(e.run(&["trust", "docker"]).status.success());
    fs::create_dir_all(e.home.join(".docker")).unwrap();
    mkfifo(&e.home.join(".docker/config.json"));
    let (t, got) = timed_complete(&e, &[("DOCKER_HOST", "unix:///run/x.sock")], "docker exec ");
    assert!(got.is_empty(), "{got:?}");
    assert!(t < Duration::from_secs(1), "{t:?}");
}

fn man_page(dir: &Path, name: &str, flag: &str) {
    fs::create_dir_all(dir.join("man1")).unwrap();
    fs::write(
        dir.join(format!("man1/{name}.1")),
        format!(".TH X 1\n.SH OPTIONS\n.TP\n.B \\-\\-{flag}\nThe {flag} option\n"),
    )
    .unwrap();
}

/// Waits for background discovery to produce `want` (or fails).
fn eventually(e: &Env, extra: &[(&str, &str)], line: &str, want: &[&str]) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let got = e.complete_in(&e.home, extra, line);
        if got == want || Instant::now() > deadline {
            return got;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn man_metadata_follows_manpath_and_page_changes() {
    let e = Env::new("manenv");
    e.script("bin/ttmanenv", "echo should-not-run >> \"$HOME/ran.log\"");
    fs::create_dir_all(e.home.join(".config/tabtrail")).unwrap();
    fs::write(e.home.join(".config/tabtrail/config"), "help_probes = off\n").unwrap();
    let alpha = e.root.join("alpha");
    let beta = e.root.join("beta");
    man_page(&alpha, "ttmanenv", "alpha");
    man_page(&beta, "ttmanenv", "beta");
    let a = [("MANPATH", alpha.to_str().unwrap())];
    let b = [("MANPATH", beta.to_str().unwrap())];
    assert!(
        e.run_full(&e.path, &a, &e.home, &["discover", "ttmanenv"])
            .status
            .success()
    );
    assert_eq!(e.complete_in(&e.home, &a, "ttmanenv --"), ["--alpha"]);

    // Switching MANPATH must never serve the other root's metadata.
    let first = e.complete_in(&e.home, &b, "ttmanenv --");
    assert!(!first.contains(&"--alpha".to_string()), "{first:?}");
    assert_eq!(eventually(&e, &b, "ttmanenv --", &["--beta"]), ["--beta"]);

    // An updated page invalidates the cached metadata, too.
    man_page(&beta, "ttmanenv", "gamma");
    let first = e.complete_in(&e.home, &b, "ttmanenv --");
    assert!(!first.contains(&"--beta".to_string()), "{first:?}");
    assert_eq!(eventually(&e, &b, "ttmanenv --", &["--gamma"]), ["--gamma"]);
    assert!(
        !e.home.join("ran.log").exists(),
        "help probes were off: nothing may run"
    );
}
