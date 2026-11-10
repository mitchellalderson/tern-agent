#!/usr/bin/env python3
"""Drive the real `tern` TUI in a pseudo-terminal with the mock provider.

Usage: scripts/tui-smoke.py path/to/tern

Unit tests cover the TUI's state and rendering; this checks the parts they
can't: terminal setup and teardown, key decoding, the async event loop,
approvals, cancellation, resume, and attaching to a running daemon.
Unix only. Standard library only.
"""

import fcntl
import json
import os
import pty
import re
import select
import struct
import sys
import tempfile
import termios
import time

ROWS, COLS = 30, 100


def tern_env(home, script):
    script_path = os.path.join(home, "script.json")
    with open(script_path, "w") as f:
        json.dump(script, f)
    # The smoke test exercises approval prompts, so pin supervised rather than
    # depend on the new-install default (autonomous).
    os.makedirs(os.path.join(home, ".tern"), exist_ok=True)
    cfg = os.path.join(home, ".tern", "config.toml")
    if not os.path.exists(cfg):
        with open(cfg, "w") as f:
            f.write('autonomy = "supervised"\n')
    return {
        "PATH": os.environ["PATH"],
        "HOME": home,
        "TERN_HOME": os.path.join(home, ".tern"),
        "TERN_PROVIDER": "mock:" + script_path,
        "TERM": "xterm-256color",
    }


class Tui:
    def __init__(self, binary, home, script):
        env = tern_env(home, script)
        self.home = home
        self.buf = b""
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.execve(binary, [binary], env)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))

    def pump(self, seconds):
        end = time.time() + seconds
        while time.time() < end:
            ready, _, _ = select.select([self.fd], [], [], 0.05)
            if not ready:
                continue
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                return
            if not data:
                return
            self.buf += data
            # Answer the queries crossterm makes, as a real terminal would.
            if b"\x1b[6n" in data:
                os.write(self.fd, b"\x1b[1;1R")
            if b"\x1b[?u" in data:
                os.write(self.fd, b"\x1b[?0u")
            if b"\x1b[c" in data:
                os.write(self.fd, b"\x1b[?1;2c")

    def screen(self):
        """Replay output onto a grid: enough VT100 for ratatui (CUP, ED, EL)."""
        grid = [[" "] * COLS for _ in range(ROWS)]
        r = c = 0
        data = self.buf.decode("utf-8", "replace")
        i = 0
        while i < len(data):
            ch = data[i]
            if ch == "\x1b":
                m = re.match(r"\x1b\[([0-9;?>=]*)([A-Za-z~])", data[i:])
                if m:
                    params, cmd = m.group(1), m.group(2)
                    nums = [int(x) for x in params.replace("?", "").split(";") if x.isdigit()]
                    if cmd in "Hf":
                        r = nums[0] - 1 if nums else 0
                        c = nums[1] - 1 if len(nums) > 1 else 0
                    elif (cmd == "J" and not params.startswith("?")) or (cmd == "h" and "1049" in params):
                        grid = [[" "] * COLS for _ in range(ROWS)]
                    elif cmd == "K":
                        for x in range(c, COLS):
                            grid[r][x] = " "
                    i += len(m.group(0))
                    continue
                m = re.match(r"\x1b[\]P][^\x07\x1b]*(\x07|\x1b\\)?", data[i:])
                i += len(m.group(0)) if m else 2
                continue
            if ch == "\r":
                c = 0
            elif ch == "\n":
                r = min(r + 1, ROWS - 1)
            elif ch >= " ":
                if 0 <= r < ROWS and 0 <= c < COLS:
                    grid[r][c] = ch
                c += 1
            i += 1
        return "\n".join("".join(row).rstrip() for row in grid)

    def wait_for(self, needle, seconds=5):
        end = time.time() + seconds
        while time.time() < end:
            if needle in self.screen():
                return True
            self.pump(0.1)
        return False

    def type(self, text, enter=True):
        for ch in text:
            os.write(self.fd, ch.encode())
            time.sleep(0.005)
        if enter:
            os.write(self.fd, b"\r")

    def ctrl_c(self):
        os.write(self.fd, b"\x03")

    def wait_exit(self, seconds=5):
        end = time.time() + seconds
        while time.time() < end:
            self.pump(0.1)
            done, status = os.waitpid(self.pid, os.WNOHANG)
            if done:
                return os.WIFEXITED(status) and os.WEXITSTATUS(status) == 0
        os.kill(self.pid, 9)
        os.waitpid(self.pid, 0)
        return False


failures = 0


def check(tui, name, ok):
    global failures
    print(("PASS " if ok else "FAIL ") + name, flush=True)
    if not ok:
        failures += 1
        print("---- screen ----\n" + tui.screen() + "\n----------------", flush=True)


def chat_with_approval(binary):
    home = tempfile.mkdtemp(prefix="tern-tui-")
    t = Tui(binary, home, {"turns": [
        {"text": "Hello! I'll list the workspace.",
         "tool_calls": [{"name": "shell", "input": {"command": "echo tui-smoke > proof.txt && ls"}}]},
        {"text": "Done: proof.txt created."},
    ]})
    check(t, "welcome screen", t.wait_for("type a message"))
    t.type("list files")
    check(t, "assistant text streams", t.wait_for("I'll list the workspace"))
    check(t, "approval prompt", t.wait_for("Allow destructive · shell echo tui-smoke"))
    t.type("y", enter=False)
    check(t, "final answer", t.wait_for("Done: proof.txt created."))
    check(t, "tool row", "✓ shell echo tui-smoke" in t.screen())
    check(t, "approved tool ran", os.path.exists(os.path.join(home, "tern-workspace", "proof.txt")))
    t.type("/usage")
    check(t, "/usage", t.wait_for("session usage:"))
    t.ctrl_c()
    check(t, "ctrl-c hint", t.wait_for("press Ctrl+C again to quit", 0.8))
    t.ctrl_c()
    check(t, "double ctrl-c exits cleanly", t.wait_exit())
    check(t, "alternate screen restored", b"\x1b[?1049l" in t.buf)


def cancel_and_resume(binary):
    home = tempfile.mkdtemp(prefix="tern-tui-")
    t = Tui(binary, home, {"turns": [{"text": "slow answer arrived", "delay_ms": 2500}]})
    check(t, "welcome screen", t.wait_for("type a message"))
    t.type("take your time")
    check(t, "run in progress", t.wait_for("running"))
    t.ctrl_c()
    check(t, "ctrl-c interrupts", t.wait_for("interrupted — /resume"))
    m = re.search(r"/resume ([0-9a-f]{32})", t.screen())
    check(t, "run id shown", bool(m))
    time.sleep(1.2)  # let the double-Ctrl+C window lapse
    t.type("/resume " + (m.group(1) if m else ""))
    check(t, "/resume finishes the run", t.wait_for("slow answer arrived", 8))
    t.ctrl_c()
    time.sleep(0.1)
    t.ctrl_c()
    check(t, "exits cleanly", t.wait_exit())


def attached_to_daemon(binary):
    import subprocess
    home = tempfile.mkdtemp(prefix="tern-tui-")
    script = {"turns": [
        {"tool_calls": [{"name": "shell", "input": {"command": "echo attached > attached.txt"}}]},
        {"text": "Done through the daemon."},
    ]}
    daemon = subprocess.Popen([binary, "daemon"], env=tern_env(home, script),
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    sock = os.path.join(home, ".tern", "tern.sock")
    end = time.time() + 10
    while not os.path.exists(sock) and time.time() < end:
        time.sleep(0.05)
    try:
        t = Tui(binary, home, script)
        check(t, "attached: welcome screen", t.wait_for("type a message"))
        t.type("use the daemon")
        check(t, "attached: approval prompt via the daemon", t.wait_for("Allow destructive · shell echo attached"))
        t.type("y", enter=False)
        check(t, "attached: final answer", t.wait_for("Done through the daemon."))
        check(t, "attached: daemon ran the tool",
              os.path.exists(os.path.join(home, "tern-workspace", "attached.txt")))
        sessions = subprocess.run([binary, "sessions"], env=tern_env(home, script),
                                  capture_output=True, text=True).stdout
        # Daemon API sessions have source "api"; an embedded TUI would say "tui".
        check(t, "attached: the run went through the daemon API", " api " in sessions and " tui " not in sessions)
        t.ctrl_c()
        time.sleep(0.1)
        t.ctrl_c()
        check(t, "attached: exits cleanly", t.wait_exit())
    finally:
        daemon.kill()
        daemon.wait()


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    binary = os.path.abspath(sys.argv[1])
    chat_with_approval(binary)
    cancel_and_resume(binary)
    attached_to_daemon(binary)
    sys.exit(1 if failures else 0)
