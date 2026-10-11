#!/usr/bin/env python3
"""Tab completion over a real SSH session, inside a disposable container.

MUST run as root inside a throwaway container (see tests/docker/Dockerfile):
it starts a private sshd on 127.0.0.1:2222 for the container-local user
`tester`, installs Tabtrail into that user's (container) dotfiles, then
connects with `ssh -tt` through a pseudo-terminal and presses Tab in the
remote interactive Bash, Zsh and Fish. Nothing on the host is touched and no
port is published.

  docker run --rm -v "$PWD:/src:ro" tabtrail-test:ubuntu-24.04 \
      python3 /src/tests/ssh/ssh_test.py --tabtrail /src/target/x86_64-unknown-linux-musl/release/tabtrail
"""

from __future__ import annotations

import argparse
import os
import pwd
import re
import shutil
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.join(REPO, "tests", "shell"))
from pty_harness import PtySession  # noqa: E402

USER = "tester"
PORT = "2222"
PROMPT = "SSHP> "


def sh(cmd, user=None, check=True):
    argv = ["su", "-", user, "-c", cmd] if user else ["sh", "-c", cmd]
    p = subprocess.run(argv, capture_output=True, text=True, timeout=120)
    if check and p.returncode != 0:
        raise SystemExit(f"failed: {cmd}\n{p.stdout}\n{p.stderr}")
    return p


def setup(tabtrail):
    if os.getuid() != 0 or not os.path.exists("/.dockerenv"):
        raise SystemExit("refusing to run: this test must run as root inside a disposable container")
    home = pwd.getpwnam(USER).pw_dir
    bindir = os.path.join(home, "bin")
    os.makedirs(bindir, exist_ok=True)
    fixtures = os.path.join(REPO, "tests", "fixtures", "cli")
    for name in os.listdir(fixtures):
        shutil.copy2(os.path.join(fixtures, name), bindir)
    shutil.copy2(tabtrail, os.path.join(bindir, "tabtrail"))
    sh(f"chown -R {USER}: {bindir}")
    # Private sshd bound to loopback only.
    sh("mkdir -p /run/sshd && ssh-keygen -A >/dev/null")
    sh(f"/usr/sbin/sshd -p {PORT} -o ListenAddress=127.0.0.1 -o PasswordAuthentication=no "
       "-o KbdInteractiveAuthentication=no -o PermitRootLogin=no")
    sh("rm -rf ~/.ssh && mkdir -p ~/.ssh && ssh-keygen -q -t ed25519 -N '' -f ~/.ssh/id_ed25519 && "
       "cp ~/.ssh/id_ed25519.pub ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys", user=USER)
    # Remote-side shell configuration (container user only).
    path_line = f'export PATH="{bindir}:$PATH"'
    with open(os.path.join(home, ".bashrc"), "w") as f:
        f.write(f"{path_line}\nPS1='{PROMPT}'\n"
                "bind -x '\"\\C-x\\C-p\": printf \"\\nBUF<%s>\\n\" \"$READLINE_LINE\"'\n")
    with open(os.path.join(home, ".zshrc"), "w") as f:
        f.write(f"{path_line}\nPROMPT='{PROMPT}'\nautoload -Uz compinit && compinit -u\n"
                "__tt_dump() { print -r -- $'\\n'\"BUF<$BUFFER>\" > /dev/tty }\nzle -N __tt_dump\nbindkey '^X^P' __tt_dump\n")
    os.makedirs(os.path.join(home, ".config", "fish"), exist_ok=True)
    with open(os.path.join(home, ".config", "fish", "config.fish"), "w") as f:
        f.write(f"fish_add_path -g {bindir}\nfunction fish_prompt; printf '{PROMPT}'; end\nset -g fish_greeting ''\n"
                "bind \\cx\\cp 'echo; echo \"BUF<\"(commandline)\">\"; commandline -f repaint'\n")
    sh(f"chown -R {USER}: {home}")
    for shell in ["bash", "zsh", "fish"]:
        sh(f"{bindir}/tabtrail install --shell {shell}", user=USER)
    sh(f"cd {home} && {bindir}/tabtrail trust ttdemo && {bindir}/tabtrail discover ttdemo && "
       f"{bindir}/tabtrail discover ttdemo deploy", user=USER)
    return home


def over_ssh(shell, line):
    cmd = (f"ssh -tt -p {PORT} -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null "
           f"-o LogLevel=ERROR {USER}@127.0.0.1 {shell} -i")
    env = {"PATH": "/usr/bin:/bin", "TERM": "xterm", "LANG": "C.UTF-8", "HOME": "/root"}
    with PtySession(["su", "-", USER, "-c", cmd], env, "/") as s:
        s.expect(re.escape(PROMPT), timeout=20)
        s.send(line)
        time.sleep(0.3)
        s.drain(quiet=0.3)
        s.send("\t")
        time.sleep(0.5)
        s.drain(quiet=0.8)
        s.send("\x18\x10")
        return s.expect(r"BUF<(.*?)>", timeout=10).group(1)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tabtrail", required=True)
    args = ap.parse_args()
    home = setup(args.tabtrail)
    cases = [
        ("ttdemo dep", "ttdemo deploy "),
        ("ttdemo deploy --env=st", "ttdemo deploy --env=staging "),
        ("ttdemo deploy --dr", "ttdemo deploy --dry-run "),
    ]
    failures = 0
    for shell in ["bash", "zsh", "fish"]:
        version = subprocess.run([shell, "--version"], capture_output=True, text=True).stdout.splitlines()[0]
        for line, want in cases:
            got = over_ssh(shell, line)
            ok = got == want
            failures += not ok
            print(f"{'PASS' if ok else 'FAIL'} ssh {shell} ({version}): {line!r} -> {got!r}")
    log = os.path.join(home, "ttdemo-invocations.log")
    entries = open(log).read().split() if os.path.exists(log) else []
    executed = [e for e in open(log).read().splitlines() if not e.endswith("--help")] if entries else []
    if executed:
        failures += 1
        print(f"FAIL composed command executed: {executed}")
    else:
        print("PASS no composed command executed (only help probes in the fixture log)")
    print(f"ssh tests: {'OK' if not failures else f'{failures} FAILED'}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
