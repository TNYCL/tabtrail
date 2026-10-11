"""Minimal pseudo-terminal driver for interactive shell tests.

Only the Python standard library is used so the harness runs on stock CI
runners and in disposable containers. It spawns a real interactive shell on
a PTY, sends real key presses (the Tab key is the byte 0x09), answers the
few terminal capability queries modern shells send at startup, and collects
output with ANSI control sequences removed for assertions.
"""

from __future__ import annotations

import fcntl
import os
import pty
import re
import select
import signal
import struct
import termios
import time

ANSI_RE = re.compile(
    rb"\x1b\[[0-?]*[ -/]*[@-~]"  # CSI
    rb"|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)"  # OSC
    rb"|\x1bP[^\x1b]*\x1b\\"  # DCS
    rb"|\x1b[()][0-9A-Za-z]"  # charset selection
    rb"|\x1b[=>78DEHMNOZc]"  # misc two-byte escapes
)

# Terminal queries some shells (fish 4 in particular) send at startup.
QUERY_REPLIES = (
    (b"\x1b[6n", b"\x1b[1;1R"),  # cursor position report
    (b"\x1b[c", b"\x1b[?62;22c"),  # primary device attributes
    (b"\x1b[0c", b"\x1b[?62;22c"),
    (b"\x1b[?u", b""),  # kitty keyboard protocol query: no reply means unsupported
)


def strip_ansi(data: bytes) -> str:
    text = ANSI_RE.sub(b"", data)
    text = text.replace(b"\r", b"")
    return text.decode("utf-8", errors="replace")


class Timeout(AssertionError):
    pass


class PtySession:
    def __init__(self, argv, env, cwd, rows=50, cols=220):
        self.argv = list(argv)
        self.buffer = b""
        self.consumed = 0
        pid, fd = pty.fork()
        if pid == 0:  # child
            try:
                os.chdir(cwd)
                os.execvpe(self.argv[0], self.argv, env)
            finally:
                os._exit(127)
        self.pid = pid
        self.fd = fd
        winsize = struct.pack("HHHH", rows, cols, 0, 0)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, winsize)

    # -- low level -----------------------------------------------------
    def _pump(self, timeout: float) -> bool:
        ready, _, _ = select.select([self.fd], [], [], timeout)
        if not ready:
            return False
        try:
            chunk = os.read(self.fd, 65536)
        except OSError:
            return False
        if not chunk:
            return False
        for query, reply in QUERY_REPLIES:
            if query in chunk and reply:
                os.write(self.fd, reply)
        self.buffer += chunk
        return True

    def send(self, data) -> None:
        if isinstance(data, str):
            data = data.encode("utf-8")
        os.write(self.fd, data)

    def expect(self, pattern, timeout: float = 10.0) -> re.Match:
        """Wait until `pattern` (regex over ANSI-stripped text) appears in
        output produced after the last successful expect()."""
        regex = re.compile(pattern) if isinstance(pattern, str) else pattern
        deadline = time.monotonic() + timeout
        while True:
            text = strip_ansi(self.buffer[self.consumed :])
            match = regex.search(text)
            if match:
                # Advance past the raw bytes that contained the match. The
                # mapping between stripped and raw offsets is not 1:1, so we
                # conservatively consume everything read so far.
                self.consumed = len(self.buffer)
                return match
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise Timeout(
                    f"timed out waiting for {regex.pattern!r}; recent output:\n{text[-3000:]}"
                )
            self._pump(min(remaining, 0.2))

    def drain(self, quiet: float = 0.3, limit: float = 5.0) -> str:
        """Read until no output arrives for `quiet` seconds."""
        start = len(self.buffer)
        deadline = time.monotonic() + limit
        while time.monotonic() < deadline:
            if not self._pump(quiet):
                break
        self.consumed = len(self.buffer)
        return strip_ansi(self.buffer[start:])

    def close(self) -> None:
        try:
            os.kill(self.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            os.waitpid(self.pid, 0)
        except ChildProcessError:
            pass
        try:
            os.close(self.fd)
        except OSError:
            pass

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
        return False
