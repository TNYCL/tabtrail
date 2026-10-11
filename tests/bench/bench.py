#!/usr/bin/env python3
"""Reproducible latency/memory/footprint measurements for Tabtrail.

Measures, in an isolated temporary HOME:
  helper_warm      `tabtrail complete` with a warm cache (process start included)
  helper_no_metadata_denied
                   `tabtrail complete` for an untrusted command with man pages
                   off: nothing may be discovered, nothing is cached
  helper_first_tab_cold
                   first `tabtrail complete` for a TRUSTED command with a fresh,
                   empty cache: answers without metadata and starts background
                   discovery (timed); the bench then waits, untimed, until that
                   discovery has produced completions and fails otherwise
  helper_provider  `tabtrail complete` for `git switch ` (trusted git, temp repo)
  spawn_baseline   spawning /bin/true the same way (measurement floor)
  pty_<shell>      real Tab key press in an interactive shell until the
                   completed text appears on the terminal (end to end)
plus peak RSS of the helper (wait4) and the cache footprint on disk.

Usage:
  python3 tests/bench/bench.py --tabtrail target/release/tabtrail \
      [--iterations 500] [--pty-iterations 40] [--shells bash,zsh,fish] [--json out.json]

Timings are wall-clock on the machine running the script; they include the
Python driver's fork/exec cost, which `spawn_baseline` shows separately.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, os.path.join(REPO, "tests", "shell"))
from pty_harness import PtySession  # noqa: E402

FIXTURES = os.path.join(REPO, "tests", "fixtures", "cli")


def percentiles(samples_ns):
    s = sorted(samples_ns)
    def pct(p):
        k = max(0, min(len(s) - 1, int(round(p / 100 * (len(s) - 1)))))
        return s[k] / 1e6
    return {
        "n": len(s),
        "p50_ms": round(pct(50), 3),
        "p90_ms": round(pct(90), 3),
        "p99_ms": round(pct(99), 3),
        "max_ms": round(s[-1] / 1e6, 3),
        "mean_ms": round(statistics.fmean(s) / 1e6, 3),
    }


def timed_runs(argv, env, cwd, n):
    samples = []
    for _ in range(n):
        t0 = time.perf_counter_ns()
        p = subprocess.run(argv, env=env, cwd=cwd, stdin=subprocess.DEVNULL,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        samples.append(time.perf_counter_ns() - t0)
        if p.returncode != 0:
            raise SystemExit(f"{argv} exited {p.returncode}")
    return samples, peak_rss_kb(argv, env, cwd)


def peak_rss_kb(argv, env, cwd, n=20):
    """Peak RSS of the measured program via GNU time (`%M`). ru_maxrss
    survives fork+exec, so a child forked directly from this Python process
    would report Python's footprint; GNU time is a small process that forks
    the program itself. Returns None when GNU time is not installed."""
    gnu_time = shutil.which("time") if os.path.exists("/usr/bin/time") else None
    if not gnu_time:
        return None
    best = 0
    for _ in range(n):
        p = subprocess.run(["/usr/bin/time", "-f", "%M", *argv], env=env, cwd=cwd,
                           stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
        best = max(best, int(p.stderr.strip().splitlines()[-1]))
    return best


def frame(stdout):
    fields = stdout.split(b"\0")
    assert fields[0] == b"tabtrail1" and fields[-1] == b"", fields
    return [f.decode() for f in fields[3:-1]]


def first_tab_cold(tt, env, home, root, n):
    """Cold first Tab with permitted discovery. Each sample uses a fresh
    XDG_CACHE_HOME; only the foreground call is timed."""
    samples = []
    for i in range(n):
        cache = tempfile.mkdtemp(prefix=f"cold-{i}-", dir=root)
        cenv = env | {"XDG_CACHE_HOME": cache}
        argv = [tt, "complete", "--shell", "bash", "--before", "ttdemo "]
        t0 = time.perf_counter_ns()
        p = subprocess.run(argv, env=cenv, cwd=home, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        samples.append(time.perf_counter_ns() - t0)
        if frame(p.stdout):
            raise SystemExit("cold sample unexpectedly had cached metadata")
        deadline = time.monotonic() + 10
        while True:  # untimed: discovery must finish in the background
            got = frame(subprocess.run(argv, env=cenv, cwd=home, stdout=subprocess.PIPE).stdout)
            if "deploy" in got:
                break
            if time.monotonic() > deadline:
                raise SystemExit(f"background discovery did not complete (sample {i})")
            time.sleep(0.02)
        shutil.rmtree(cache, ignore_errors=True)
    return samples


def hardware():
    info = {"machine": platform.machine(), "kernel": platform.release(), "cpus": os.cpu_count()}
    try:
        with open("/proc/cpuinfo") as f:
            for line in f:
                if line.startswith("model name"):
                    info["cpu"] = line.split(":", 1)[1].strip()
                    break
        with open("/proc/meminfo") as f:
            info["mem_total"] = f.readline().split(":", 1)[1].strip()
    except OSError:
        pass
    return info


def du(path):
    total = 0
    for root, _, files in os.walk(path):
        for f in files:
            total += os.path.getsize(os.path.join(root, f))
    return total


def pty_latency(shell, env, cwd, n):
    if shell == "bash":
        argv = ["bash", "--noprofile", "-i"]
    elif shell == "zsh":
        argv = ["zsh", "-i"]
    else:
        argv = ["fish", "-i"]
    samples = []
    with PtySession(argv, env, cwd) as s:
        s.expect("TTP> ", timeout=15)
        for i in range(n):
            s.send("ttdemo deploy --dr")
            pre = s.drain(quiet=0.1)
            # A sample is only valid if the completed text is not already on
            # screen (e.g. an autosuggestion or a line that was not cleared).
            if "y-run" in pre:
                raise SystemExit(f"{shell}: invalid sample {i}: completion visible before Tab")
            t0 = time.perf_counter_ns()
            s.send("\t")
            s.expect("y-run", timeout=10)
            samples.append(time.perf_counter_ns() - t0)
            s.send("\x05\x15")  # end-of-line, kill to start: an empty line again
            s.drain(quiet=0.1)
    return samples


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tabtrail", required=True)
    ap.add_argument("--iterations", type=int, default=500)
    ap.add_argument("--pty-iterations", type=int, default=40)
    ap.add_argument("--shells", default="bash,zsh,fish")
    ap.add_argument("--json")
    args = ap.parse_args()

    root = tempfile.mkdtemp(prefix="tabtrail-bench-")
    try:
        home = os.path.join(root, "home")
        bindir = os.path.join(root, "bin")
        os.makedirs(home)
        os.makedirs(bindir)
        for name in os.listdir(FIXTURES):
            shutil.copy2(os.path.join(FIXTURES, name), bindir)
        tt = os.path.join(bindir, "tabtrail")
        shutil.copy2(args.tabtrail, tt)
        with open(os.path.join(bindir, "coldcmd"), "w") as f:
            f.write("#!/bin/sh\nexit 0\n")
        os.chmod(os.path.join(bindir, "coldcmd"), 0o755)
        env = {"HOME": home, "PATH": f"{bindir}:/usr/bin:/bin", "LANG": "C.UTF-8", "TERM": "xterm"}

        def tt_run(*a):
            subprocess.run([tt, *a], env=env, cwd=home, check=True, capture_output=True)

        tt_run("trust", "ttdemo")
        tt_run("discover", "ttdemo")
        tt_run("discover", "ttdemo", "deploy")
        results = {"hardware": hardware(), "tabtrail_bytes": os.path.getsize(tt)}

        line_warm = ["complete", "--shell", "bash", "--before", "ttdemo deploy --env "]
        s, rss = timed_runs([tt, *line_warm], env, home, args.iterations)
        results["helper_warm"] = percentiles(s) | {"max_rss_kb": rss}
        # Cold: untrusted command, man pages disabled so nothing is found;
        # the first call may spawn a detached worker (measured as part of it).
        with open(os.path.join(home, ".config", "tabtrail", "config"), "w") as f:
            f.write("man_pages = off\n")
        s, rss = timed_runs([tt, "complete", "--shell", "bash", "--before", "coldcmd --x"], env, home, args.iterations)
        results["helper_no_metadata_denied"] = percentiles(s) | {"max_rss_kb": rss}
        os.remove(os.path.join(home, ".config", "tabtrail", "config"))
        results["helper_first_tab_cold"] = percentiles(first_tab_cold(tt, env, home, root, max(20, args.iterations // 10)))
        s, rss = timed_runs(["/bin/true"], env, home, args.iterations)
        results["spawn_baseline"] = percentiles(s) | {"max_rss_kb": rss}

        git = shutil.which("git")
        if git:
            repo = os.path.join(home, "repo")
            os.makedirs(repo)
            genv = env | {"GIT_CONFIG_NOSYSTEM": "1", "GIT_AUTHOR_NAME": "b", "GIT_AUTHOR_EMAIL": "b@x",
                          "GIT_COMMITTER_NAME": "b", "GIT_COMMITTER_EMAIL": "b@x"}
            for cmd in (["init", "-q", "-b", "main"], ["commit", "-q", "--allow-empty", "-m", "x"],
                        ["branch", "feature-a"], ["branch", "feature-b"]):
                subprocess.run([git, *cmd], cwd=repo, env=genv, check=True)
            subprocess.run([tt, "trust", git], env=env, cwd=home, check=True, capture_output=True)
            s, rss = timed_runs([tt, "complete", "--shell", "bash", "--before", "git switch "], env, repo, args.iterations)
            results["helper_provider_git"] = percentiles(s) | {"max_rss_kb": rss}

        cache = os.path.join(home, ".cache", "tabtrail")
        results["cache_bytes"] = du(cache)
        results["schema_files"] = len(os.listdir(os.path.join(cache, "v1", "schemas")))

        for shell in [x for x in args.shells.split(",") if x]:
            if not shutil.which(shell):
                results[f"pty_{shell}"] = "shell not installed"
                continue
            senv = dict(env)
            subprocess.run([tt, "install", "--shell", shell], env=senv, cwd=home, check=True, capture_output=True)
            if shell == "bash":
                with open(os.path.join(home, ".bashrc"), "a") as f:
                    f.write("PS1='TTP> '\n")
            elif shell == "zsh":
                rc = os.path.join(home, ".zshrc")
                body = open(rc).read()
                with open(rc, "w") as f:
                    f.write("PROMPT='TTP> '\nautoload -Uz compinit && compinit -u\n" + body)
            else:
                cfg = os.path.join(home, ".config", "fish", "config.fish")
                with open(cfg, "w") as f:
                    # Autosuggestions (from history and completions) would show
                    # the completed text before Tab is pressed.
                    f.write("function fish_prompt; printf 'TTP> '; end\nset -g fish_greeting ''\n"
                            "set -g fish_autosuggestion_enabled 0\nset -g fish_history ''\n")
            samples = pty_latency(shell, senv, home, args.pty_iterations)
            ver = subprocess.run([shell, "--version"], capture_output=True, text=True).stdout.splitlines()[0]
            results[f"pty_{shell}"] = percentiles(samples) | {"version": ver}
            subprocess.run([tt, "uninstall", "--shell", shell], env=senv, cwd=home, check=True, capture_output=True)

        text = json.dumps(results, indent=2)
        print(text)
        if args.json:
            with open(args.json, "w") as f:
                f.write(text + "\n")
    finally:
        shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    main()
