#!/usr/bin/env python3
"""Exercise actual terminal input/render/exit with only Python's standard library."""
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
master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 44, 140, 0, 0))
process = subprocess.Popen([binary, "--demo", "--fresh"], stdin=slave, stdout=slave, stderr=slave, env={**os.environ, "NO_COLOR": "1"})
os.close(slave)


def read_for(seconds=0.5):
    chunks = []
    until = time.monotonic() + seconds
    while time.monotonic() < until:
        if select.select([master], [], [], 0.05)[0]:
            try:
                chunks.append(os.read(master, 65536))
            except OSError:
                break
    return b"".join(chunks).decode("utf-8", errors="replace")


def key(data):
    os.write(master, data)
    return read_for()


def plain(output):
    return re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", output)


try:
    initial = read_for(1)
    assert "U N V R S" in initial, "cockpit did not render"
    assert "38;2;99;235;233" in initial, "CRT cyan lost under inherited NO_COLOR=1"
    assert "GALAGA" in key(b"\x1b[19~"), "idle Galaga did not open"
    key(b"ad ")
    key(b"\x1b")
    assert "COMMAND" in key(b"\x0b"), "Ctrl+K did not open commands"
    assert "No matching actions" in key(b"zzzzzz"), "search is not filtering"
    key(b"\x15")  # Ctrl+U clears only the palette query.
    # Observation is root row 4 in the 74x24 centered menu, two lines per row.
    assert "TELEMETRY" in key(b"\x1b[<0;50;21M\x1b[<0;50;21m"), "palette mouse failed"
    key(b"\x1b")
    key(b"\x0bflight logs\r")
    assert "FLIGHT LOGS / ERROR" in key(b"errors\r"), "flight logs are not findable or filterable"
    key(b"\x1b")
    key(b"\x0bcrew\r")
    assert "ACTIONS" in key(b"scout\r"), "crew submenu did not disclose seat actions"
    assert "MESSAGE" in key(b"message\r"), "message form did not open"
    key(b"\x1b[200~menu transmission\x1b[201~")
    assert "menu transmission" in key(b"\r"), "message form did not submit to selected seat"
    key(b"\x0b")
    key(b"display\r")
    # The terminal diff reuses the unchanged "e motion" suffix.
    assert "Enabl" in key(b"\r"), "display action did not toggle motion"
    assert "COMMAND" in key(b"\x1b"), "Escape did not return to parent menu"
    key(b"\x1b")
    # Clicking a roster row still changes selected COMMS outside the menu.
    assert "COPILOT" in key(b"\x1b[<0;10;11M\x1b[<0;10;11m"), "roster mouse failed"
    key(b"first line")
    assert "first line" in key(b"\x0a"), "Ctrl+J did not create a COMMS line"
    assert "secondline" in plain(key(b"second line")), "multiline COMMS did not render its second line"
    assert "secondline" in plain(key(b"\r")), "multiline COMMS did not submit"
    key(b"\x1b[5~\x1b[6~")
    key(b"\x1b[21~")
    assert process.wait(timeout=5) == 0, "F10 did not exit cleanly"
    print("UX_SMOKE: PASS (NO_COLOR, Galaga, logs, menus, multiline COMMS, mouse, motion, F10)")

finally:
    if process.poll() is None:
        process.terminate()
        process.wait(timeout=5)
    os.close(master)
