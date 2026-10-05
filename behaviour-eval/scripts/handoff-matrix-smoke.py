#!/usr/bin/env python3
"""Run /test-handoffs in a real PTY; live native harness authentication required."""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import struct
import subprocess
import sys
import termios
import time

binary = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "target/debug/unvrs")
master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 44, 160, 0, 0))
process = subprocess.Popen([binary, "--fresh"], stdin=slave, stdout=slave, stderr=slave)
os.close(slave)
started = time.monotonic()
journal = None
seen = set()
output = ""
matrix_visible = False
acp_groups = set()
try:
    # Submit through the same COMMS path used by the captain, once the cockpit draws
    # (0.7: it attaches to the kernel first); retype if the matrix has not started.
    until = time.monotonic() + 20
    while time.monotonic() < until and "UNVRS" not in output:
        if select.select([master], [], [], 0.1)[0]:
            output += os.read(master, 65536).decode("utf-8", "replace")
    time.sleep(0.5)
    os.write(master, b"/test-handoffs\r")
    typed_at = time.monotonic()
    started_matrix = False
    complete = None
    while time.monotonic() - started < 2100:
        if select.select([master], [], [], 0.1)[0]:
            try:
                output += os.read(master, 65536).decode("utf-8", "replace")
                matrix_visible |= "HANDOFF MATRIX" in output
                output = output[-200000:]
            except OSError:
                break
        if journal is None:
            journal = next(Path(".unvrs").glob(f"flight-{process.pid}-*.jsonl"), None)
        if journal:
            for line in journal.read_text().splitlines():
                try:
                    event = json.loads(line).get("event", "")
                except json.JSONDecodeError:
                    continue
                if " / cpu / " in event:
                    acp_groups.add(int(event.rsplit(" / ", 1)[1]))
                if event.startswith("HANDOFF_MATRIX / starting"):
                    started_matrix = True
                if event.startswith("HANDOFF_MATRIX /") or event.startswith("HANDOFF MATRIX COMPLETE"):
                    if event not in seen:
                        print(event, flush=True)
                        seen.add(event)
                    if event.startswith("HANDOFF MATRIX COMPLETE"):
                        complete = event
            if complete:
                break
        if not started_matrix and time.monotonic() - typed_at > 10:
            os.write(master, b"/test-handoffs\r")
            typed_at = time.monotonic()
        if process.poll() is not None:
            break
    assert complete, f"matrix did not complete; journal: {journal}"
    # Ratatui emits terminal diffs, so look for stable title, not a full row.
    assert matrix_visible, "matrix was not rendered in the terminal"
    os.write(master, b"\x1b[21~")  # F10
    deadline = time.monotonic() + 20
    while process.poll() is None and time.monotonic() < deadline:
        if select.select([master], [], [], 0.1)[0]:
            try:
                os.read(master, 65536)
            except OSError:
                break
    assert process.wait(timeout=2) == 0, "F10 did not exit cleanly"
    # 0.7: the kernel owns the seats and docks them when the last console leaves;
    # give it a few seconds (graceful shutdown, then SIGKILL after 3 s).
    for group in acp_groups:
        until = time.monotonic() + 8
        while time.monotonic() < until:
            try:
                os.kill(-group, 0)
            except ProcessLookupError:
                break
            time.sleep(0.2)
        else:
            raise AssertionError(f"ACP process group {group} survived F10")
    print(f"journal: {journal}\nUI matrix: visible\nF10: PASS (ACP groups exited)", flush=True)
    assert "12 pass / 0 fail" in complete, complete
finally:
    if process.poll() is None:
        os.write(master, b"\x1b[21~")
        try:
            process.wait(timeout=20)
        except subprocess.TimeoutExpired:
            process.terminate()
            process.wait(timeout=5)
    os.close(master)
