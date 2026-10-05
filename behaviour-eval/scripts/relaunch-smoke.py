#!/usr/bin/env python3
"""Verify abrupt-flight socket recovery through the normal startup path."""
import glob
import os
from pathlib import Path
import pty
import signal
import subprocess
import sys
import tempfile
import time


def start(binary, env):
    master, slave = pty.openpty()
    process = subprocess.Popen([binary, "--demo", "--fresh"], stdin=slave, stdout=slave, stderr=slave, env=env)
    os.close(slave)
    return process, master


def flight_dir(pid):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        matches = glob.glob(f"{Path(tempfile.gettempdir())}/unvrs-{pid}-*/bus.sock")
        if matches:
            return Path(matches[0]).parent
        time.sleep(0.05)
    raise AssertionError(f"flight socket for PID {pid} was not created")


def stop(process, master):
    if process.poll() is None:
        os.write(master, b"\x1b[21~")  # F10
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
    os.close(master)


binary = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "target/debug/unvrs")
first = second = None
first_master = second_master = None
try:
    first, first_master = start(binary, os.environ.copy())
    old_dir = flight_dir(first.pid)
    old_socket = old_dir / "bus.sock"
    recorder = old_dir / "seat-1.log"
    recorder.write_text("preserve flight evidence\n")
    os.kill(first.pid, signal.SIGKILL)
    assert first.wait(timeout=5) == -signal.SIGKILL
    assert old_socket.exists(), "abrupt exit unexpectedly removed the socket"

    inherited = os.environ.copy()
    inherited["UNVRS_SOCKET"] = str(old_socket)
    inherited["UNVRS_TOKEN"] = "stale-relaunch-smoke-token"
    second, second_master = start(binary, inherited)
    new_dir = flight_dir(second.pid)
    assert new_dir != old_dir, "relaunch reused the old flight directory"
    assert not old_socket.exists(), "relaunch retained a dead flight socket"
    assert recorder.read_text() == "preserve flight evidence\n", "relaunch removed recorder evidence"
    print(f"stale socket: removed\nrecorder: preserved\nrelaunch: PASS ({new_dir})", flush=True)
finally:
    if first_master is not None:
        stop(first, first_master)
    if second_master is not None:
        stop(second, second_master)
