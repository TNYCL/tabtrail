#!/usr/bin/env python3
"""End-to-end Tab completion tests in real interactive shells.

Every test builds an isolated sandbox (temporary HOME, PATH limited to the
fixture directory plus system directories), installs Tabtrail with the real
`tabtrail install` command, starts an interactive shell on a pseudo-terminal
and presses the actual Tab key. Results are read back from the line editor
buffer through a test-only key binding (Ctrl-X Ctrl-P prints `BUF<...>`).

Usage:
  python3 tests/shell/test_shells.py --tabtrail target/debug/tabtrail \
      [--shells bash,zsh,fish] [--require-all] [-v]

Nothing here touches the invoking user's real dotfiles: HOME, XDG_* and
ZDOTDIR all point into a temporary directory that is deleted afterwards.
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from pty_harness import PtySession, strip_ansi  # noqa: E402

REPO = os.path.dirname(os.path.dirname(HERE))
FIXTURES = os.path.join(REPO, "tests", "fixtures", "cli")
PROMPT = "TTP> "
DUMP = "\x18\x10"  # Ctrl-X Ctrl-P
TABTRAIL = None
SHELLS: list[str] = []


def run(argv, env, cwd=None, check=True):
    p = subprocess.run(argv, env=env, cwd=cwd, capture_output=True, text=True, timeout=60)
    if check and p.returncode != 0:
        raise AssertionError(f"{argv} failed ({p.returncode}):\n{p.stdout}\n{p.stderr}")
    return p


class Sandbox:
    """Temporary HOME with fixture executables and a warm Tabtrail cache."""

    def __init__(self, shell: str, bash_completion: bool = True, zsh_block_first: bool = False):
        self.shell = shell
        self.root = tempfile.mkdtemp(prefix=f"tabtrail-{shell}-")
        self.home = os.path.join(self.root, "home")
        self.bin = os.path.join(self.root, "bin")
        self.work = os.path.join(self.home, "work")
        os.makedirs(self.work)
        os.makedirs(self.bin)
        for name in os.listdir(FIXTURES):
            shutil.copy2(os.path.join(FIXTURES, name), os.path.join(self.bin, name))
        shutil.copy2(TABTRAIL, os.path.join(self.bin, "tabtrail"))
        self.tabtrail = os.path.join(self.bin, "tabtrail")
        for f in ["file-one.txt", "file-two.txt", "fïle-ünï.txt"]:
            open(os.path.join(self.work, f), "w").close()
        self.env = {
            "HOME": self.home,
            "PATH": f"{self.bin}:/usr/local/bin:/usr/bin:/bin",
            "TERM": "xterm",
            "LANG": "C.UTF-8",
            "LC_ALL": "C.UTF-8",
            "SHELL": shutil.which(shell) or shell,
            "DOCKER_HOST": "unix:///nonexistent/docker.sock",
            "fish_greeting": "",
        }
        self.write_rc(bash_completion, zsh_block_first)

    # -- setup ---------------------------------------------------------
    def write_rc(self, bash_completion: bool, zsh_block_first: bool):
        if self.shell == "bash":
            lines = [f"PS1='{PROMPT}'", "unset PROMPT_COMMAND"]
            if bash_completion:
                lines.append("[ -r /usr/share/bash-completion/bash_completion ] && . /usr/share/bash-completion/bash_completion")
            lines += [
                'complete -W "alpha beta" nativecmd',
                'bind -x \'"\\C-x\\C-p": printf "\\nBUF<%s>\\n" "$READLINE_LINE"\'',
                "bind 'set bell-style none'",
            ]
            self.rc_original = "\n".join(lines) + "\n"
            self.rc_path = os.path.join(self.home, ".bashrc")
        elif self.shell == "zsh":
            pre = [f"PROMPT='{PROMPT}'", "unsetopt beep"]
            compinit = ["autoload -Uz compinit && compinit -u -d \"$HOME/.zcompdump\""]
            post = [
                "_nativecmd() { compadd alpha beta }",
                "compdef _nativecmd nativecmd",
                "(( $+_comps[docker] )) && compdef -d docker",
                "__tt_dump() { print -r -- $'\\n'\"BUF<$BUFFER>\" > /dev/tty }",
                "zle -N __tt_dump",
                "bindkey '^X^P' __tt_dump",
            ]
            # Optionally place Tabtrail's block *before* compinit by
            # pre-creating it: the installer appends to existing content.
            self.rc_original = "\n".join(pre + compinit + post) + "\n"
            self.rc_path = os.path.join(self.home, ".zshrc")
            self.zsh_block_first = zsh_block_first
        else:
            conf = os.path.join(self.home, ".config", "fish")
            os.makedirs(conf, exist_ok=True)
            self.rc_path = os.path.join(conf, "config.fish")
            self.rc_original = "\n".join(
                [
                    f"function fish_prompt; printf '{PROMPT}'; end",
                    "function fish_title; end",
                    "set -g fish_greeting ''",
                    # Only user completions: lets the docker provider test run
                    # without Fish's bundled docker completions.
                    "set -g fish_complete_path $HOME/.config/fish/completions",
                    "complete -c nativecmd -f -a 'alpha beta'",
                    "bind \\cx\\cp 'echo; echo \"BUF<\"(commandline)\">\"; commandline -f repaint'",
                ]
            ) + "\n"
        with open(self.rc_path, "w") as f:
            f.write(self.rc_original)

    def tt(self, *args, check=True):
        return run([self.tabtrail, *args], self.env, cwd=self.work, check=check)

    def install(self):
        if self.shell == "zsh" and getattr(self, "zsh_block_first", False):
            # Install into an empty .zshrc, then append the user config after it.
            os.remove(self.rc_path)
            self.tt("install", "--shell", "zsh")
            with open(self.rc_path, "a") as f:
                f.write(self.rc_original)
        else:
            self.tt("install", "--shell", self.shell)

    def warm(self):
        # Trust is explicit and narrow: only these fixture executables.
        self.tt("trust", "ttdemo", "nativecmd", "hostile", "docker")
        self.tt("discover", "ttdemo")
        self.tt("discover", "ttdemo", "deploy")
        self.tt("discover", "nativecmd")
        self.tt("discover", "hostile")

    def argv(self):
        if self.shell == "bash":
            return ["bash", "--noprofile", "-i"]
        if self.shell == "zsh":
            return ["zsh", "-i"]
        return ["fish", "-i"]

    def session(self) -> PtySession:
        s = PtySession(self.argv(), self.env, self.work)
        s.startup = s.expect(re.escape(PROMPT), timeout=15).string
        return s

    def log(self, name="ttdemo"):
        p = os.path.join(self.home, f"{name}-invocations.log")
        return open(p).read().splitlines() if os.path.exists(p) else []

    def cleanup(self):
        shutil.rmtree(self.root, ignore_errors=True)


def complete(s: PtySession, text: str, tabs: int = 1, settle: float = 0.6) -> str:
    """Types `text`, presses Tab, returns the resulting buffer."""
    s.send(text)
    time.sleep(0.15)
    s.drain(quiet=0.15)
    for _ in range(tabs):
        s.send("\t")
        time.sleep(0.25)
    s.drain(quiet=settle)
    s.send(DUMP)
    m = s.expect(r"BUF<(.*?)>\n", timeout=10)
    return m.group(1)


def listing(s: PtySession, text: str, tabs: int = 2) -> str:
    """Types `text`, presses Tab `tabs` times, returns the screen text."""
    s.send(text)
    time.sleep(0.15)
    s.drain(quiet=0.15)
    out = ""
    for _ in range(tabs):
        s.send("\t")
        out += s.drain(quiet=0.6)
    return out


class ShellTests:
    shell = ""

    def setUp(self):
        self.sb = Sandbox(self.shell)
        self.sb.install()
        self.sb.warm()

    def tearDown(self):
        self.sb.cleanup()

    def new(self, **kw) -> PtySession:
        return self.sb.session()

    def assertNoCommandExecuted(self):
        for name in ["ttdemo", "nativecmd"]:
            for entry in self.sb.log(name):
                self.assertTrue(entry.endswith("--help") or entry.endswith("-h"),
                                f"{name} was executed with {entry!r}: Tab must only run help probes")

    # -- startup -------------------------------------------------------
    def test_fresh_startup_is_quiet(self):
        with self.new() as s:
            noise = strip_ansi(s.buffer)
            for bad in ["error", "not found", "warning", "command not found", "No such file"]:
                self.assertNotIn(bad, noise.lower() if bad.islower() else noise)

    # -- discovery-based completion -------------------------------------
    def test_subcommand(self):
        with self.new() as s:
            self.assertEqual(complete(s, "ttdemo dep"), "ttdemo deploy ")
        self.assertNoCommandExecuted()

    def test_unicode_subcommand(self):
        with self.new() as s:
            self.assertEqual(complete(s, "ttdemo prü"), "ttdemo prüfen ")

    def test_nested_flag(self):
        with self.new() as s:
            self.assertEqual(complete(s, "ttdemo deploy --dr"), "ttdemo deploy --dry-run ")
        self.assertNoCommandExecuted()

    def test_separate_enum_value(self):
        with self.new() as s:
            self.assertEqual(complete(s, "ttdemo deploy --env st"), "ttdemo deploy --env staging ")

    def test_short_flag_value(self):
        with self.new() as s:
            self.assertEqual(complete(s, "ttdemo deploy -e pr"), "ttdemo deploy -e prod ")

    def test_attached_enum_value(self):
        with self.new() as s:
            self.assertEqual(complete(s, "ttdemo deploy --env=pr"), "ttdemo deploy --env=prod ")

    def test_value_after_unicode_argument(self):
        with self.new() as s:
            self.assertEqual(complete(s, "ttdemo deploy ünï --env st"), "ttdemo deploy ünï --env staging ")

    def test_lists_enum_values(self):
        with self.new() as s:
            out = listing(s, "ttdemo deploy --env ")
            for v in ["dev", "staging", "prod"]:
                self.assertIn(v, out)
            self.assertNotIn("file-one.txt", out, "enum position must not list files")

    def test_lists_subcommands(self):
        with self.new() as s:
            out = listing(s, "ttdemo ")
            for v in ["deploy", "status", "prüfen"]:
                self.assertIn(v, out)

    def test_end_of_options_marker(self):
        with self.new() as s:
            # after `--`, `--dr` is an operand: no flag completion
            self.assertEqual(complete(s, "ttdemo deploy -- --dr"), "ttdemo deploy -- --dr")

    # -- fallback and coexistence ---------------------------------------
    def test_file_fallback_for_unknown_command(self):
        with self.new() as s:
            self.assertEqual(complete(s, "unknowncmd file-o"), "unknowncmd file-one.txt ")

    def test_file_fallback_for_untyped_positional(self):
        with self.new() as s:
            self.assertEqual(complete(s, "ttdemo deploy fïl"), "ttdemo deploy fïle-ünï.txt ")

    def test_path_typed_flag_value_offers_files(self):
        with self.new() as s:
            self.assertEqual(complete(s, "ttdemo deploy --config file-t"), "ttdemo deploy --config file-two.txt ")

    def test_native_completion_takes_precedence(self):
        with self.new() as s:
            out = listing(s, "nativecmd ")
            self.assertIn("alpha", out)
            self.assertIn("beta", out)
            self.assertNotIn("gamma", out)
        with self.new() as s:
            self.assertEqual(complete(s, "nativecmd al"), "nativecmd alpha ")

    # -- safety -----------------------------------------------------------
    SUBST_LINES = {
        "bash": ["ttdemo deploy $(touch PWNED1) --dr", "ttdemo deploy `touch PWNED2` --e", "ttdemo deploy --env=$(touch PWNED3)"],
        "zsh": ["ttdemo deploy $(touch PWNED1) --dr", "ttdemo deploy `touch PWNED2` --e", "ttdemo deploy --env=$(touch PWNED3)"],
        "fish": ["ttdemo deploy (touch PWNED1) --dr", "ttdemo deploy $(touch PWNED2) --e", "ttdemo deploy --env=(touch PWNED3)"],
    }

    def _pwned_after_tabs(self):
        for f in os.listdir(self.sb.work):
            if f.startswith("PWNED"):
                os.remove(os.path.join(self.sb.work, f))
        for line in self.SUBST_LINES[self.shell]:
            with self.new() as s:
                complete(s, line)
        return sorted(f for f in os.listdir(self.sb.work) if f.startswith("PWNED"))

    def test_substitutions_in_line_are_never_run(self):
        """Tabtrail adds no execution. Shell-native behaviour is measured
        first: Zsh's default Tab widget (expand-or-complete) itself expands a
        command substitution in the word under the cursor."""
        with_tabtrail = self._pwned_after_tabs()
        self.assertNoCommandExecuted()
        self.sb.tt("uninstall", "--shell", self.shell)
        native = self._pwned_after_tabs()
        self.assertTrue(set(with_tabtrail) <= set(native),
                        f"Tabtrail added execution: with={with_tabtrail} native={native}")
        if self.shell != "zsh":
            self.assertEqual(with_tabtrail, [], "no shell other than zsh expands on Tab")
        # Substitutions in earlier words are never expanded by anyone.
        self.assertNotIn("PWNED1", with_tabtrail)
        self.assertNotIn("PWNED2", with_tabtrail)

    def test_hostile_help_description_is_sanitized(self):
        with self.new() as s:
            s.send("hostile --")
            time.sleep(0.15)
            s.drain(quiet=0.15)
            start = len(s.buffer)
            s.send("\t\t")
            s.drain(quiet=0.8)
            raw = s.buffer[start:]
            self.assertNotIn(b"PWNED-TITLE", raw)
            self.assertNotIn(b"\x1b]0;", raw)
            self.assertIn(b"--other", raw)

    def test_hostile_provider_values_are_quoted(self):
        with self.new() as s:
            out = listing(s, "docker exec ")
            self.assertIn("web-1", out)
            self.assertNotIn("PWNED-TITLE", out, "values with control characters must be dropped")
        with self.new() as s:
            buf = complete(s, "docker exec x")
            # Execute the completed line: the fixture prints its argv. The
            # hostile name must arrive as one literal argument.
            s.send("\r")
            m = s.expect(r"ARG<x;touch PWNED_SEMI>", timeout=10)
            self.assertTrue(m)
        with self.new() as s:
            complete(s, "docker exec '$(t")
            s.send("\r")
            s.expect(re.escape("ARG<$(touch PWNED_SUBST)>"), timeout=10)
        for f in ["PWNED_SEMI", "PWNED_SUBST", "PWNED_TICK"]:
            self.assertFalse(os.path.exists(os.path.join(self.sb.work, f)), f)
        self.assertIn("x", buf)

    def test_untrusted_provider_is_not_run(self):
        self.sb.tt("untrust", "docker")
        with self.new() as s:
            out = listing(s, "docker exec ")
            self.assertNotIn("web-1", out)
        provider_calls = [l for l in self.sb.log("docker") if " ps " in f" {l} "]
        self.assertEqual(provider_calls, [], "Tabtrail ran the untrusted docker provider")

    def test_repeated_tab_sees_fresh_metadata(self):
        """No result from an earlier Tab may be reused for a later one."""
        self.sb.tt("cache", "clear")
        self.sb.tt("untrust", "ttdemo")
        with self.new() as s:
            self.assertEqual(complete(s, "ttdemo deploy --dr"), "ttdemo deploy --dr")
            # Metadata appears out of band while the line stays unchanged.
            self.sb.tt("trust", "ttdemo")
            self.sb.tt("discover", "ttdemo")
            self.sb.tt("discover", "ttdemo", "deploy")
            self.assertEqual(complete(s, ""), "ttdemo deploy --dry-run ")

    # -- removal ----------------------------------------------------------
    def test_uninstall_restores_rc_and_disables(self):
        self.sb.tt("uninstall", "--shell", self.shell)
        if self.shell == "fish":
            self.assertFalse(os.path.exists(os.path.join(self.sb.home, ".config/fish/conf.d/tabtrail.fish")))
        else:
            with open(self.sb.rc_path) as f:
                self.assertEqual(f.read(), self.sb.rc_original)
        with self.new() as s:
            self.assertEqual(complete(s, "ttdemo dep"), "ttdemo dep")


class BashTests(ShellTests):
    shell = "bash"

    def test_without_bash_completion(self):
        self.sb.cleanup()
        self.sb = Sandbox("bash", bash_completion=False)
        self.sb.install()
        self.sb.warm()
        with self.new() as s:
            self.assertEqual(complete(s, "ttdemo deploy --env=st"), "ttdemo deploy --env=staging ")
        with self.new() as s:
            self.assertEqual(complete(s, "unknowncmd file-o"), "unknowncmd file-one.txt ")

    def _with_default_spec(self, spec_line):
        self.sb.cleanup()
        self.sb = Sandbox("bash", bash_completion=False)
        with open(self.sb.rc_path, "a") as f:
            f.write(spec_line + "\n")
        self.sb.rc_original += spec_line + "\n"
        self.sb.install()
        self.sb.warm()

    def test_existing_word_list_default_is_preserved(self):
        self._with_default_spec('complete -D -W "wordA wordB"')
        with self.new() as s:
            out = listing(s, "unknowncmd word")
            self.assertIn("wordA", out)
            self.assertIn("wordB", out)
        with self.new() as s:
            # Tabtrail stays out of the way instead of replacing the default.
            self.assertEqual(complete(s, "ttdemo dep"), "ttdemo dep")
            s.send("\x15complete -p -D\r")
            s.expect(r'complete -W .wordA wordB. -D', timeout=10)

    def test_existing_command_default_is_preserved(self):
        self._with_default_spec('complete -o nospace -F _my_default -D; _my_default() { COMPREPLY=(mine-1 mine-2); }')
        with self.new() as s:
            out = listing(s, "unknowncmd mine-")
            self.assertIn("mine-1", out)
            s.send("\x15complete -p -D\r")
            s.expect(r"complete -o nospace -F _my_default -D", timeout=10)

    def test_native_git_completion_via_bash_completion_loader(self):
        if not shutil.which("git") or not os.path.exists("/usr/share/bash-completion/completions/git"):
            self.skipTest("git bash completion not installed")
        with self.new() as s:
            self.assertEqual(complete(s, "git swi"), "git switch ")

    def test_cursor_in_middle_of_word(self):
        with self.new() as s:
            s.send("ttdemo de --verbose")
            s.send("\x1b[D" * len(" --verbose"))  # left arrow
            buf = complete(s, "")
            self.assertTrue(buf.startswith("ttdemo deploy"), buf)
            self.assertTrue(buf.endswith("--verbose"), buf)

    def test_double_quoted_value(self):
        with self.new() as s:
            self.assertEqual(complete(s, 'ttdemo deploy --env "sta'), 'ttdemo deploy --env "staging" ')


class ZshTests(ShellTests):
    shell = "zsh"

    def test_block_before_compinit_registers_on_first_prompt(self):
        self.sb.cleanup()
        self.sb = Sandbox("zsh", zsh_block_first=True)
        self.sb.install()
        self.sb.warm()
        with open(self.sb.rc_path) as f:
            text = f.read()
        self.assertLess(text.index("tabtrail initialize"), text.index("compinit"))
        with self.new() as s:
            self.assertEqual(complete(s, "ttdemo deploy --env=st"), "ttdemo deploy --env=staging ")

    def test_descriptions_shown(self):
        with self.new() as s:
            out = listing(s, "ttdemo ", tabs=1)
            self.assertIn("Deploy the app", out)


class FishTests(ShellTests):
    shell = "fish"

    def test_descriptions_shown(self):
        with self.new() as s:
            out = listing(s, "ttdemo ", tabs=1)
            self.assertIn("Deploy the app", out)


def main():
    global TABTRAIL, SHELLS
    ap = argparse.ArgumentParser()
    ap.add_argument("--tabtrail", required=True)
    ap.add_argument("--shells", default="bash,zsh,fish")
    ap.add_argument("--require-all", action="store_true", help="fail instead of skipping missing shells")
    ap.add_argument("-v", "--verbose", action="store_true")
    ap.add_argument("-k", dest="pattern", default=None, help="only run tests whose name contains this")
    args = ap.parse_args()
    TABTRAIL = os.path.abspath(args.tabtrail)
    SHELLS = [s for s in args.shells.split(",") if s]
    suite = unittest.TestSuite()
    loader = unittest.TestLoader()
    classes = {"bash": BashTests, "zsh": ZshTests, "fish": FishTests}
    missing = []
    for name in SHELLS:
        if not shutil.which(name):
            missing.append(name)
            continue
        cls = type(f"{name.capitalize()}Shell", (classes[name], unittest.TestCase), {})
        for t in loader.getTestCaseNames(cls):
            if args.pattern and args.pattern not in t:
                continue
            suite.addTest(cls(t))
    for name in missing:
        print(f"shell not installed: {name}", file=sys.stderr)
    if missing and args.require_all:
        return 2
    for name in SHELLS:
        if name not in missing:
            v = subprocess.run([name, "--version"], capture_output=True, text=True).stdout.splitlines()[0]
            print(f"{name}: {v}")
    result = unittest.TextTestRunner(verbosity=2 if args.verbose else 1).run(suite)
    return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    sys.exit(main())
