"""Shared harness for 0.8 scripted checks: a temp UNVRS home and fake harness
threads. A "thread" is a real process (the parent of its hooks, its MCP server and its
model's shell calls) that runs the real `unvrs` binary, exactly as Claude Code and
Codex do. No kernel or harness is mocked; model CPUs are fixture scripts where a
check says so."""
import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("UNVRS_BIN", str(ROOT / "target/debug/unvrs")))
FIX = Path(__file__).resolve().parent / "fixtures"
TMP = Path(tempfile.mkdtemp(prefix="unvrs-crew-")).resolve()

HARNESS = r'''
import json, subprocess, sys
for line in sys.stdin:
    c = json.loads(line)
    r = subprocess.run(c["argv"], input=c.get("stdin", ""), capture_output=True, text=True, cwd=c.get("cwd"), env=c.get("env"))
    print(json.dumps({"code": r.returncode, "out": r.stdout, "err": r.stderr}), flush=True)
'''


def log(*a):
    print(*a, flush=True)


def free_port():
    import socket
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


class Home:
    """A temp UNVRS_HOME with its own kernel (fixture CPUs unless live=True)."""

    def __init__(self, name, live=False, extra_env=None):
        self.root = TMP / name
        self.root.mkdir(parents=True)
        self.port = free_port()
        self.env = dict(os.environ)
        self.env.update({
            "UNVRS_HOME": str(self.root),
            "UNVRS_OBSERVATORY_PORT": str(self.port),
            "UNVRS_NOTIFY": "0",
            "UNVRS_NO_OPEN": "1",
            "UNVRS_QUOTA": "0",
            "UNVRS_KERNEL_IDLE_SECS": "600",
        })
        for k in ("CODEX_THREAD_ID", "CLAUDE_CODE_SESSION_ID", "UNVRS_DRIVEN_PID"):
            self.env.pop(k, None)
        if not live:
            self.env.update({
                "UNVRS_COMPACTION_CPU": str(FIX / "fold-worker"),
                "UNVRS_CLAUDE_BIN": str(FIX / "fake-claude"),
                "UNVRS_CODEX_BIN": str(FIX / "fake-codex"),
                "FAKE_CPU_DIR": str(self.root.parent / f"{name}-cpu"),
                "FOLD_CTL": str(self.root.parent / f"{name}-fold-mode"),
            })
        self.env.update(extra_env or {})
        self.threads = []

    def run(self, *args, check=False, **kw):
        r = subprocess.run([str(BIN), *args], capture_output=True, text=True, env=self.env, **kw)
        if check:
            assert r.returncode == 0, (args, r.stdout, r.stderr)
        return r

    def table(self):
        r = self.run("kernel", "status", "--json")
        return json.loads(r.stdout)

    def snapshot(self):
        return json.loads(self.run("kernel", "snapshot", check=True).stdout)

    def pid(self, n):
        return next((p for p in self.table()["pids"] if p["pid"] == n), None)

    def thread(self, key):
        return next((t for t in self.table()["threads"] if t["key"] == key), None)

    def journal(self, kind=None):
        p = self.root / "kernel/journal.jsonl"
        ev = [json.loads(l) for l in p.read_text().splitlines()] if p.exists() else []
        return [e for e in ev if kind is None or e["kind"] == kind]

    def session(self, pid):
        return json.loads((self.root / f"sessions/pid-{pid}/session.json").read_text())

    def hot(self, pid):
        return self.run("kernel", "hot", "--pid", str(pid)).stdout

    def close(self):
        for t in self.threads:
            t.kill()
        self.run("kernel", "stop")


class Thread:
    """A fake harness process: the parent of its hooks and of its model's calls."""

    def __init__(self, home, harness="claude", cwd="/tmp"):
        self.home, self.harness = home, harness
        self.cwd = Path(cwd)
        self.session = str(uuid.uuid4())
        self.key = f"{harness}:{self.session}"
        self.proc = subprocess.Popen([sys.executable, "-c", HARNESS], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     text=True, env=home.env)
        self.lock = threading.Lock()
        home.threads.append(self)

    def _run(self, argv, stdin="", cwd=None, env=None):
        with self.lock:
            e = dict(self.home.env, **(env or {}))
            self.proc.stdin.write(json.dumps({"argv": argv, "stdin": stdin, "cwd": str(cwd or self.cwd), "env": e}) + "\n")
            self.proc.stdin.flush()
            return json.loads(self.proc.stdout.readline())

    def hook_raw(self, event, **extra):
        payload = {"session_id": self.session, "cwd": str(self.cwd), "hook_event_name": event, "transcript_path": None, **extra}
        return self._run([str(BIN), "hook", event, "--harness", self.harness], json.dumps(payload))

    def hook(self, event, **extra):
        r = self.hook_raw(event, **extra)
        assert r["code"] == 0, r
        return json.loads(r["out"]) if r["out"].strip() else None

    def start(self):
        return self.hook("SessionStart", source="startup")

    def prompt(self, text):
        return self.hook("UserPromptSubmit", prompt=text)

    def stop(self, reply="ok"):
        return self.hook("Stop", last_assistant_message=reply, stop_hook_active=False)

    def turn(self, text, reply="ok"):
        out = self.prompt(text)
        self.stop(reply)
        return out

    def end(self):
        return self.hook("SessionEnd", reason="other")

    def ctl(self, *args, env=None):
        """The model's shell: `unvrs ctl …` with the harness's thread env."""
        e = {"CODEX_THREAD_ID": self.session} if self.harness == "codex" else {"CLAUDE_CODE_SESSION_ID": self.session}
        e.update(env or {})
        return self._run([str(BIN), "ctl", *args], env=e)

    def mcp(self, command, meta_thread=True):
        """The model's MCP tool call through a real `unvrs mcp` child of this harness."""
        msgs = [
            {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}},
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
        ]
        call = {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "unvrs", "arguments": {"command": command}}}
        if self.harness == "codex" and meta_thread:
            call["params"]["_meta"] = {"threadId": self.session, "x-codex-turn-metadata": {"thread_id": self.session}}
        msgs.append(call)
        e = {} if self.harness == "codex" else {"CLAUDE_CODE_SESSION_ID": self.session}
        r = self._run([str(BIN), "mcp"], "\n".join(json.dumps(m) for m in msgs) + "\n", env=e)
        out = [json.loads(l) for l in r["out"].splitlines() if l.strip()]
        res = next(m for m in out if m.get("id") == 3)["result"]
        tools = next(m for m in out if m.get("id") == 2)["result"]["tools"]
        return res, tools

    def kill(self):
        if self.proc.poll() is None:
            self.proc.kill()
            self.proc.wait()


def ctx(out):
    return ((out or {}).get("hookSpecificOutput") or {}).get("additionalContext") or ""


def reason(out):
    return (out or {}).get("reason") or ""


def wait(pred, timeout=20, what="condition"):
    end = time.time() + timeout
    while time.time() < end:
        v = pred()
        if v:
            return v
        time.sleep(0.2)
    raise AssertionError(f"timed out: {what}")


def run_checks(checks, argv):
    import shutil
    want = argv or list(checks)
    ok = True
    done = set()
    for name in want:
        fn = checks[name]
        if fn in done:
            continue
        done.add(fn)
        try:
            fn()
            log(f"{name} PASS")
        except AssertionError as e:
            ok = False
            log(f"{name} FAIL: {e}")
        except Exception as e:  # a crash is a failure, with its reason
            ok = False
            log(f"{name} FAIL: {type(e).__name__}: {e}")
    if os.environ.get("UNVRS_KEEP_TMP") != "1":
        shutil.rmtree(TMP, ignore_errors=True)
    return 0 if ok else 1
