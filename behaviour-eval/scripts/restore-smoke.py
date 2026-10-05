#!/usr/bin/env python3
"""Quit/reopen restore in a real PTY (training mode: no model calls)."""
import fcntl
import os
import pty
import re
import select
import struct
import subprocess
import sys
import termios
import time

binary = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "target/debug/unvrs")


def fly(args, keys, want, timeout=15):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 44, 160, 0, 0))
    process = subprocess.Popen([binary, *args], stdin=slave, stdout=slave, stderr=slave)
    os.close(slave)
    output = ""
    try:
        time.sleep(0.5)
        for data in keys:
            os.write(master, data)
            time.sleep(0.3)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if select.select([master], [], [], 0.1)[0]:
                output += os.read(master, 65536).decode("utf-8", "replace")
            # Ratatui emits diffs, so streamed text interleaves with other cells: match by word.
            plain = re.sub(r"\x1b\[[0-9;?<]*[A-Za-z]", "", output)
            if all(word in plain for text in want for word in text.split()):
                break
        else:
            raise AssertionError(f"missing {want} in cockpit output")
        os.write(master, b"\x1b[21~")  # F10 docks the flight
        deadline = time.monotonic() + 10
        while process.poll() is None and time.monotonic() < deadline:
            if select.select([master], [], [], 0.1)[0]:  # keep draining or the PTY fills
                try:
                    os.read(master, 65536)
                except OSError:
                    break
        assert process.wait(timeout=2) == 0, "F10 did not exit cleanly"
    finally:
        if process.poll() is None:
            process.kill()
        os.close(master)


from pathlib import Path

session = Path(".unvrs/session-demo.json")
fly(["--demo", "--fresh"], [b"restore marker alpha\r", b"unsent draft beta"], ["Training transmission received"])
assert "restore marker alpha" in session.read_text(), "F10 did not save the flight"
fly(["--demo"], [], ["restore marker alpha", "unsent draft beta"])
# Screen diffs against the restored frame are partial, so /new is verified through the saved session.
fly(["--demo"], [b"\x0c", b"/new\r"], [])
assert "restore marker alpha" not in session.read_text(), "/new did not start a fresh flight"
assert any("restore marker alpha" in p.read_text() for p in Path(".unvrs").glob("session-demo-archived-*.json")), "/new lost the archived flight"
print("restore: PASS (transcript and draft survived F10; /new archived the flight and started fresh)")
