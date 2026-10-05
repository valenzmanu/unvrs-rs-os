"""Shared helpers for the 0.5 auto smokes. Run every smoke from the repo root."""
import datetime
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
FIXTURES = ROOT / "behaviour-eval/fixtures"
MISSIONS = FIXTURES / "missions"
POOLS = FIXTURES / "skill-pools.toml"
PROFILES = ["pi", "codex", "claude", "cursor"]


class Smoke:
    def __init__(self, name):
        self.name, self.fails = name, 0
        os.chdir(ROOT)
        # Same clone cache as pool-catalog-smoke: gitignored, never the user's ~/.cache.
        os.environ.setdefault("UNVRS_POOL_CACHE", str(ROOT / "behaviour-eval/.unvrs/pools"))
        self.journal = ROOT / f"behaviour-eval/journals/{name}.log"
        self.journal.parent.mkdir(parents=True, exist_ok=True)
        self.journal.write_text("")
        self.binary = os.environ.get("UNVRS_BIN")
        if not self.binary:
            subprocess.run(["cargo", "build", "-q", "-p", "unvrs"], check=True)
            self.binary = str(ROOT / "target/debug/unvrs")
        self.tmp = Path(tempfile.mkdtemp(prefix=f"unvrs-{name}-"))

    def check(self, label, ok, detail=""):
        verdict = "PASS" if ok else "FAIL"
        stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        with self.journal.open("a") as log:
            log.write(f"{stamp} {verdict} {label}: {detail}\n")
        print(f"{verdict} {label}" + (f" — {detail}" if detail and not ok else ""), flush=True)
        self.fails += not ok
        return ok

    def note(self, text):
        with self.journal.open("a") as log:
            log.write(f"INFO {text}\n")
        print(f"INFO {text}", flush=True)

    def unvrs(self, *args, env=None):
        """Returns (exit code, stdout, stderr). `env` overrides; a None value unsets."""
        merged = dict(os.environ)
        for key, value in (env or {}).items():
            merged.pop(key, None) if value is None else merged.__setitem__(key, value)
        done = subprocess.run([self.binary, *map(str, args)], capture_output=True, text=True, env=merged)
        return done.returncode, done.stdout, done.stderr

    def boot(self, mission, workspace, *flags, env=None):
        """`unvrs boot --json` in an isolated workspace. Returns (exit code, parsed stdout or {})."""
        code, out, err = self.unvrs(
            "boot", MISSIONS / mission, "--workspace", workspace, "--pools", POOLS, "--json", *flags, env=env
        )
        try:
            return code, json.loads(out)
        except json.JSONDecodeError:
            return code, {"_stdout": out, "_stderr": err}

    def events(self, workspace):
        path = Path(workspace) / ".unvrs/boot-journal.jsonl"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def catalog(self):
        code, out, err = self.unvrs("pool", "list", "--config", POOLS, "--json")
        assert code == 0, err
        return json.loads(out)

    def finish(self):
        print(f"fails: {self.fails}\njournal: {self.journal.relative_to(ROOT)}")
        print(f"{self.name}: {'PASS' if not self.fails else 'FAIL'}")
        sys.exit(1 if self.fails else 0)


def has_key():
    """True when TYPESAFE_API_KEY is in the environment or the repo-root .env (value never read out)."""
    if os.environ.get("TYPESAFE_API_KEY", "").strip():
        return True
    env = ROOT / ".env"
    return env.exists() and any(
        line.split("=", 1)[0].replace("export ", "").strip() == "TYPESAFE_API_KEY" and line.split("=", 1)[1].strip()
        for line in env.read_text().splitlines()
        if "=" in line
    )
