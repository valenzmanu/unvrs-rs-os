"""Live 0.8 surfaces, driven headless through the same cores the apps use:
Claude Code through a persistent `claude -p --input-format stream-json` process (the
Agent SDK's transport, as T3 Code runs it), Codex through `codex app-server` (as T3's
Codex threads and the Codex app run it). The UNVRS plugin is installed with
`unvrs install --no-launchagent` into copies of the harness homes (homes.py, live=True:
sanitized credentials that can never rotate the captain's tokens). Prompts stay tiny.
"""
import json
import os
import select
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import homes  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
FIX = Path(__file__).resolve().parent / "fixtures"
CLAUDE_MODEL = os.environ.get("UNVRS_TEST_CLAUDE_MODEL", "sonnet")
CODEX_MODEL = os.environ.get("UNVRS_TEST_CODEX_MODEL", "gpt-5.6-sol")


def log(*a):
    print(*a, flush=True)


def wait(pred, timeout=120, step=0.5, what="condition"):
    end = time.time() + timeout
    while time.time() < end:
        v = pred()
        if v:
            return v
        time.sleep(step)
    raise AssertionError(f"timed out waiting for {what}")


class LiveHome:
    def __init__(self, name):
        self.tmp = Path(tempfile.mkdtemp(prefix=f"unvrs-{name}-")).resolve()
        self.extra = homes.make(self.tmp, faithful=False, live=True)
        self.env = homes.full_env(self.extra)
        self.env.update({
            "UNVRS_COMPACTION_CPU": str(FIX / "fold-worker"),
            "UNVRS_NOTIFY": "0",
            "UNVRS_NO_OPEN": "1",
            "UNVRS_KERNEL_IDLE_SECS": "900",
            "UNVRS_L3_CLAUDE_MODEL": CLAUDE_MODEL,
            "UNVRS_L3_CODEX_MODEL": CODEX_MODEL,
            "UNVRS_SEAT_CLAUDE_MODEL": CLAUDE_MODEL,
            "UNVRS_SEAT_CODEX_MODEL": CODEX_MODEL,
        })
        for k in ("CLAUDE_CODE_SESSION_ID", "CODEX_THREAD_ID", "CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT"):
            self.env.pop(k, None)
        self.root = Path(self.extra["UNVRS_HOME"])
        r = subprocess.run([str(homes.unvrs_bin()), "install", "--no-launchagent"], capture_output=True, text=True,
                           env=self.env, timeout=300)
        assert r.returncode == 0, r.stdout + r.stderr
        self.trust_codex_hooks()
        self.threads = []

    def trust_codex_hooks(self):
        """The captain's one-time hook trust, done the way the Codex app does it: through
        Codex's own app server (hooks/list, then config/batchWrite of the trust state) in
        this test copy of ~/.codex. UNVRS itself never writes trust."""
        env = dict(self.extra)
        r = homes.app_server(env, [("hooks/list", {"cwds": [str(self.tmp)]})])
        hooks = [h for e in (r[0] or {}).get("result", {}).get("data", []) for h in e.get("hooks", [])
                 if h.get("pluginId") == "unvrs@unvrs-local"]
        assert hooks, "codex lists no UNVRS hooks"
        state = {h["key"]: {"trusted_hash": h["currentHash"]} for h in hooks}
        r = homes.app_server(env, [("config/batchWrite", {"edits": [{"keyPath": "hooks.state", "value": state, "mergeStrategy": "upsert"}]}),
                                   ("hooks/list", {"cwds": [str(self.tmp)]})])
        after = [h for e in (r[1] or {}).get("result", {}).get("data", []) for h in e.get("hooks", [])
                 if h.get("pluginId") == "unvrs@unvrs-local"]
        self.trust = sorted({str(h.get("trustStatus")) for h in after})
        log(f"live home: captain trusted {len(state)} UNVRS hooks through codex app-server config/batchWrite → {self.trust}")

    def unvrs(self, *args, timeout=120):
        return subprocess.run([str(self.root / "bin/unvrs"), *args], capture_output=True, text=True, env=self.env, timeout=timeout)

    def journal(self, kind=None):
        p = self.root / "kernel/journal.jsonl"
        ev = [json.loads(l) for l in p.read_text().splitlines() if l.strip()] if p.exists() else []
        return [e for e in ev if kind is None or e["kind"] == kind]

    def table(self):
        return json.loads(self.unvrs("kernel", "status", "--json").stdout)

    def snapshot(self):
        return json.loads(self.unvrs("kernel", "snapshot").stdout)

    def captain(self, *args):
        """The captain's terminal: no harness among the ancestors (double fork)."""
        out = self.tmp / f"cap-{time.time_ns()}.json"
        code = f'''
import os, subprocess, json
if os.fork(): os._exit(0)
os.setsid()
if os.fork(): os._exit(0)
r = subprocess.run({[str(self.root / "bin/unvrs"), *args]!r}, capture_output=True, text=True)
open({str(out)!r} + ".t", "w").write(json.dumps({{"code": r.returncode, "out": r.stdout, "err": r.stderr}}))
os.rename({str(out)!r} + ".t", {str(out)!r})
'''
        subprocess.run([sys.executable, "-c", code], env=self.env, check=True)
        wait(lambda: out.exists(), timeout=60, what="captain call")
        return json.loads(out.read_text())

    def close(self):
        for t in self.threads:
            t.close()
        self.unvrs("kernel", "stop")
        self.unvrs("uninstall", timeout=300)
        homes.cleanup(self.extra)


class ClaudeThread:
    """A live Claude Code thread: one persistent stream-json process (like T3 Code)."""

    def __init__(self, home, cwd=None, session=None, resume=False):
        self.home = home
        self.session = session or str(uuid.uuid4())
        self.cwd = Path(cwd or home.tmp)
        sid = ["--resume", self.session] if resume else ["--session-id", self.session]
        self.proc = subprocess.Popen(
            ["claude", "-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose",
             "--permission-mode", "bypassPermissions", *sid, "--model", CLAUDE_MODEL],
            cwd=self.cwd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, env=home.env)
        self.events = []
        self.cv = threading.Condition()
        threading.Thread(target=self._read, daemon=True).start()
        home.threads.append(self)

    def _read(self):
        for line in self.proc.stdout:
            try:
                v = json.loads(line)
            except ValueError:
                continue
            with self.cv:
                self.events.append(v)
                self.cv.notify_all()

    def ask(self, text, timeout=300):
        with self.cv:
            start = len(self.events)
        self.proc.stdin.write(json.dumps({"type": "user", "message": {"role": "user", "content": text}}) + "\n")
        self.proc.stdin.flush()
        end = time.time() + timeout
        with self.cv:
            while True:
                for v in self.events[start:]:
                    if v.get("type") == "result":
                        self.last = self.events[start:]
                        return v.get("result") or ""
                left = end - time.time()
                if left <= 0:
                    raise AssertionError(f"claude thread {self.session} timed out on {text[:60]!r}")
                self.cv.wait(left)

    def tools_used(self):
        out = []
        for v in getattr(self, "last", []):
            if v.get("type") == "assistant":
                for c in v["message"].get("content", []):
                    if c.get("type") == "tool_use":
                        out.append((c["name"], c.get("input")))
        return out

    def init(self):
        return next((v for v in self.events if v.get("type") == "system" and v.get("subtype") == "init"), None)

    def close(self):
        if self.proc.poll() is None:
            try:
                self.proc.stdin.close()
                self.proc.wait(timeout=15)
            except Exception:
                self.proc.kill()


def codex_app_mention(home, verb, args=""):
    """The prompt the Codex app sends when the captain picks an UNVRS skill: a Markdown
    link to the skill file in Codex's plugin cache, then the arguments (trailing space
    when there are none)."""
    cache = Path(home.extra["CODEX_HOME"]) / "plugins/cache/unvrs-local/unvrs"
    ver = sorted(p.name for p in cache.iterdir()) if cache.is_dir() else ["0.8.0"]
    return f"[$unvrs:{verb}]({cache / ver[-1] / 'skills' / verb / 'SKILL.md'}) {args}"


class CodexThread:
    """A live Codex thread through `codex app-server` (as T3 Code and the Codex app)."""

    def __init__(self, home, cwd=None):
        self.home = home
        self.cwd = Path(cwd or home.tmp)
        self.proc = subprocess.Popen(["codex", "app-server"], cwd=self.cwd,
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                     text=True, env=home.env)
        self.msgs = []
        self.cv = threading.Condition()
        threading.Thread(target=self._read, daemon=True).start()
        self.nid = 0
        self.call("initialize", {"clientInfo": {"name": "unvrs-gate", "version": "0.8"}})
        self.notify("initialized")
        r = self.call("thread/start", {"cwd": str(self.cwd), "model": CODEX_MODEL})
        self.thread = r["thread"]["id"]
        self.session = self.thread
        home.threads.append(self)

    def _read(self):
        for line in self.proc.stdout:
            try:
                v = json.loads(line)
            except ValueError:
                continue
            with self.cv:
                self.msgs.append(v)
                self.cv.notify_all()

    def notify(self, method, params=None):
        self.proc.stdin.write(json.dumps({"method": method, **({"params": params} if params else {})}) + "\n")
        self.proc.stdin.flush()

    def call(self, method, params, timeout=120):
        self.nid += 1
        i = self.nid
        self.proc.stdin.write(json.dumps({"id": i, "method": method, "params": params}) + "\n")
        self.proc.stdin.flush()
        end = time.time() + timeout
        with self.cv:
            while True:
                for m in self.msgs:
                    if m.get("id") == i and "method" not in m:
                        if "error" in m:
                            raise AssertionError(f"{method}: {m['error']}")
                        return m["result"]
                left = end - time.time()
                if left <= 0:
                    raise AssertionError(f"app-server {method} timed out")
                self.cv.wait(left)

    def ask(self, text, timeout=300):
        with self.cv:
            start = len(self.msgs)
        self.call("turn/start", {"threadId": self.thread, "input": [{"type": "text", "text": text}]})
        end = time.time() + timeout
        with self.cv:
            while True:
                done = [m for m in self.msgs[start:] if m.get("method") == "turn/completed"]
                if done:
                    self.last = self.msgs[start:]
                    items = [m["params"]["item"] for m in self.last if m.get("method") == "item/completed"]
                    return "\n".join(i.get("text", "") for i in items if i.get("type") == "agentMessage")
                left = end - time.time()
                if left <= 0:
                    raise AssertionError(f"codex thread {self.thread} timed out on {text[:60]!r}")
                self.cv.wait(left)

    def tools_used(self):
        items = [m["params"]["item"] for m in getattr(self, "last", []) if m.get("method") == "item/completed"]
        return [(i.get("server", "") + "/" + i.get("tool", ""), i.get("arguments")) for i in items if i.get("type") == "mcpToolCall"]

    def hook_events(self):
        return [m for m in getattr(self, "last", []) if str(m.get("method", "")).startswith("hook/")]

    def close(self):
        if self.proc.poll() is None:
            self.proc.kill()
            self.proc.wait()
