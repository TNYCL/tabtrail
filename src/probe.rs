//! Bounded subprocess execution for discovery probes and value providers.
//!
//! Robustness controls (not a sandbox): structured argv without a shell,
//! cleared environment, null stdin, private working directory, its own
//! process group, a wall-clock deadline, an output cap, and SIGKILL of the
//! whole process group when the child exits, times out or floods output.
//!
//! `ProbeSpec::timeout` is one overall budget: spawn retries, running time
//! and output collection all count against it. The only work after the
//! deadline is killing and reaping the process group plus a fixed
//! `COLLECT_GRACE` to drain pipes that the kill has just closed.

use std::ffi::OsString;
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};

/// Time allowed after the deadline (or exit) to collect already-closed pipes.
pub const COLLECT_GRACE: Duration = Duration::from_millis(20);

#[derive(Clone, Debug)]
pub struct ProbeSpec<'a> {
    pub program: &'a Path,
    pub argv0: &'a str,
    pub args: &'a [String],
    pub env: Vec<(String, OsString)>,
    pub cwd: &'a Path,
    pub timeout: Duration,
    pub max_output: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProbeResult {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub truncated: bool,
}

fn reader<R: Read + Send + 'static>(
    mut src: R,
    total: Arc<AtomicUsize>,
    overflow: Arc<AtomicBool>,
    max: usize,
    tx: mpsc::Sender<(u8, Vec<u8>)>,
    tag: u8,
) {
    thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match src.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let before = total.fetch_add(n, Ordering::SeqCst);
                    if before + n > max {
                        let room = max.saturating_sub(before);
                        buf.extend_from_slice(&chunk[..room.min(n)]);
                        overflow.store(true, Ordering::SeqCst);
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
            }
        }
        let _ = tx.send((tag, buf));
    });
}

/// Minimal environment for probes: enough for a program to locate its own
/// resources, with paging, colour and localisation disabled.
pub fn probe_env() -> Vec<(String, OsString)> {
    let mut env = Vec::new();
    for key in ["PATH", "HOME", "USER", "LOGNAME"] {
        if let Some(v) = std::env::var_os(key) {
            env.push((key.to_string(), v));
        }
    }
    for (k, v) in [
        ("LC_ALL", "C"),
        ("LANG", "C"),
        ("TERM", "dumb"),
        ("NO_COLOR", "1"),
        ("COLUMNS", "120"),
        ("PAGER", "cat"),
        ("MANPAGER", "cat"),
        ("GIT_PAGER", "cat"),
        ("TABTRAIL_PROBE", "1"),
    ] {
        env.push((k.to_string(), OsString::from(v)));
    }
    env
}

pub fn run(spec: &ProbeSpec<'_>) -> io::Result<ProbeResult> {
    let mut cmd = Command::new(spec.program);
    cmd.arg0(spec.argv0)
        .args(spec.args)
        .env_clear()
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .current_dir(spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let deadline = Instant::now() + spec.timeout;
    // ETXTBSY: another thread's fork may briefly hold a just-written
    // executable open for writing. Retry with backoff, but never past the
    // overall deadline.
    let mut backoff = Duration::from_millis(5);
    let mut child = loop {
        match cmd.spawn() {
            Ok(c) => break c,
            Err(e) if e.kind() == io::ErrorKind::ExecutableFileBusy => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(e);
                }
                thread::sleep(backoff.min(left));
                backoff = (backoff * 2).min(Duration::from_millis(40));
            }
            Err(e) => return Err(e),
        }
    };
    let pid = Pid::from_child(&child);
    let total = Arc::new(AtomicUsize::new(0));
    let overflow = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    if let Some(out) = child.stdout.take() {
        reader(out, total.clone(), overflow.clone(), spec.max_output, tx.clone(), 0);
    }
    if let Some(err) = child.stderr.take() {
        reader(err, total.clone(), overflow.clone(), spec.max_output, tx.clone(), 1);
    }
    drop(tx);

    let mut result = ProbeResult::default();
    let mut sleep = Duration::from_micros(200);
    loop {
        // Observe exit without reaping, so the process group id cannot be
        // reused before we kill the rest of the group.
        match waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        ) {
            Ok(Some(status)) => {
                result.exit_code = status.exit_status();
                break;
            }
            Ok(None) => {}
            Err(_) => break,
        }
        if overflow.load(Ordering::SeqCst) {
            result.truncated = true;
            break;
        }
        if Instant::now() >= deadline {
            result.timed_out = true;
            break;
        }
        thread::sleep(sleep);
        sleep = (sleep * 2).min(Duration::from_millis(5));
    }
    // Clean up the whole process tree that stayed in the group.
    let _ = kill_process_group(pid, Signal::KILL);
    let _ = child.wait();

    // Collect output; a descendant that escaped the group may hold a pipe
    // open, so never wait unboundedly.
    let collect_deadline = deadline.max(Instant::now() + COLLECT_GRACE);
    for _ in 0..2 {
        let left = collect_deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok((0, b)) => result.stdout = b,
            Ok((_, b)) => result.stderr = b,
            Err(_) => break,
        }
    }
    if overflow.load(Ordering::SeqCst) {
        result.truncated = true;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{TempDir, write_script};

    fn spec<'a>(program: &'a Path, args: &'a [String], cwd: &'a Path, timeout_ms: u64, max: usize) -> ProbeSpec<'a> {
        ProbeSpec {
            program,
            argv0: "fixture",
            args,
            env: probe_env(),
            cwd,
            timeout: Duration::from_millis(timeout_ms),
            max_output: max,
        }
    }

    #[test]
    fn captures_output_status_and_minimal_env() {
        let t = TempDir::new("probe");
        let p = t.path().join("tool");
        write_script(
            &p,
            "echo \"out:$1:$0\"; echo err >&2; echo \"secret=${SECRET_TOKEN:-unset} lc=$LC_ALL pwd=$(pwd)\"; exit 3",
        );
        // SECRET_TOKEN is never set by the probe env, whatever the parent has.
        let args = vec!["--help".to_string()];
        let cwd = t.path().join("cwd");
        std::fs::create_dir_all(&cwd).unwrap();
        let r = run(&spec(&p, &args, &cwd, 2000, 10_000)).unwrap();
        let out = String::from_utf8_lossy(&r.stdout);
        // ($0 of a #! script is the script path, so argv0 is not visible here.)
        assert!(out.contains("out:--help:"), "{out}");
        assert!(out.contains("secret=unset lc=C"), "{out}");
        assert!(out.contains(&format!("pwd={}", cwd.display())), "{out}");
        assert_eq!(r.stderr, b"err\n");
        assert_eq!(r.exit_code, Some(3));
        assert!(!r.timed_out && !r.truncated);
    }

    #[test]
    fn hanging_probe_and_its_children_are_killed_at_deadline() {
        let t = TempDir::new("probe-hang");
        let p = t.path().join("hang");
        let marker = t.path().join("child-alive");
        write_script(
            &p,
            &format!("(sleep 2; touch '{}') & echo started; sleep 30", marker.display()),
        );
        let start = Instant::now();
        let r = run(&spec(&p, &[], t.path(), 300, 10_000)).unwrap();
        let elapsed = start.elapsed();
        assert!(r.timed_out);
        // One overall budget: timeout + kill/reap + collection grace.
        assert!(elapsed < Duration::from_millis(300 + 150), "took {elapsed:?}");
        assert_eq!(r.stdout, b"started\n");
        std::thread::sleep(Duration::from_millis(2500));
        assert!(!marker.exists(), "background child survived the process-group kill");
    }

    #[test]
    fn output_flood_is_capped() {
        let t = TempDir::new("probe-flood");
        let p = t.path().join("flood");
        write_script(&p, "yes help-line-padding-padding");
        let start = Instant::now();
        let r = run(&spec(&p, &[], t.path(), 5000, 64 * 1024)).unwrap();
        assert!(r.truncated);
        assert!(r.stdout.len() <= 64 * 1024);
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn stdin_is_not_interactive() {
        let t = TempDir::new("probe-stdin");
        let p = t.path().join("reader");
        write_script(&p, "read line; echo \"got:[$line]\"");
        let r = run(&spec(&p, &[], t.path(), 2000, 1000)).unwrap();
        assert_eq!(r.stdout, b"got:[]\n");
        assert!(!r.timed_out);
    }

    #[test]
    fn etxtbsy_retries_stay_within_the_deadline() {
        let t = TempDir::new("probe-busy");
        let p = t.path().join("busy");
        write_script(&p, "echo hi");
        // Holding the executable open for writing makes every exec fail
        // with ETXTBSY for as long as the handle lives.
        let _writer = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        let start = Instant::now();
        let r = run(&spec(&p, &[], t.path(), 60, 100));
        let elapsed = start.elapsed();
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::ExecutableFileBusy);
        assert!(elapsed >= Duration::from_millis(60), "gave up early: {elapsed:?}");
        assert!(elapsed < Duration::from_millis(60 + 40), "exceeded budget: {elapsed:?}");
    }

    #[test]
    fn missing_program_is_an_error_not_a_panic() {
        let t = TempDir::new("probe-missing");
        let p = t.path().join("nope");
        assert!(run(&spec(&p, &[], t.path(), 100, 100)).is_err());
    }
}
