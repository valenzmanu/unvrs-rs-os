#!/usr/bin/env python3
"""SETUP · the captain's one command (`./install.sh` → `unvrs setup`) end to end.

Temp UNVRS_HOMEs and faithful copies of the captain's ~/.claude and ~/.codex (homes.py:
the captain's own UNVRS entries left out, a dev.unvrs.test.<random> LaunchAgent label, a
free Observatory port, a temp link dir and shell rc), with the captain's real seed
(~/context/_seed, read only; UNVRS_SEED overrides). Every captain run happens from
"Terminal": a double fork to launchd (no harness ancestor), and for the prompts a real
pty (python `pty`) that answers them.

Home A (link dir on PATH with an older unvrs file in it, another old build later on PATH,
as ~/.local/bin and ~/.cargo/bin on the captain's Mac; the old file is backed up and
replaced by the link, the other is moved after a yes, doctor checks `unvrs` on PATH):
  1. `./install.sh` (UNVRS_SETUP_BIN = the dev build) on a pty; the captain answers yes to
     "Trust these 5 UNVRS hooks in Codex? [Y/n]": install, PATH symlink, kernel, seed,
     trust written through Codex's app server (only our 5 keys added to hooks.state,
     config.toml backed up), doctor passes, the ready block;
  2. the seed landed: sources, projects, captain facts, an L2 finds its source; a model's
     seed import is refused;
  3. a second run is a no-op: no prompt, everything "already", seed unchanged, the harness
     files byte-identical, doctor passes.
Home B (link dir not on PATH):
  4. `scripts/setup-captain.sh --no-seed` (the compat wrapper) without a terminal: trust
     skipped with the manual step, the rc line printed and not written, exit 0;
  5. again on a pty, piped as from GitHub (`… | sh -s -- --no-seed`): yes to the rc line (appended once), no to the trust (not written).
Refusals: `unvrs setup` from this (harness) process and with a harness thread env, and
`install.sh` from a model env, are refused. Finally the captain's live ~/.claude, ~/.codex
config files, ~/.local/bin/unvrs and ~/.zshrc are byte-identical (sha256). Exit 0 on pass.
"""
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import tomllib
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import homes  # noqa: E402
from crewlib import BIN, Thread, ctx, wait  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
SEED = Path(os.environ.get("UNVRS_SEED", str(Path.home() / "context/_seed")))
LIVE = ([homes.CAPTAIN_CLAUDE / f for f in homes.CLAUDE_FILES] + [homes.CAPTAIN_CODEX / f for f in homes.CODEX_FILES]
        + [Path.home() / ".local/bin/unvrs", Path.home() / ".zshrc", Path.home() / ".bash_profile"])
MODEL_ENV = ("CODEX_THREAD_ID", "CLAUDE_CODE_SESSION_ID", "CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT",
             "CODEX_SANDBOX", "CODEX_SANDBOX_NETWORK_DISABLED", "UNVRS_DRIVEN_PID")
TRUST_Q = "Trust these 5 UNVRS hooks in Codex? [Y/n]"


def sums():
    return {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in LIVE if p.is_file()}


class Home:
    """What crewlib.Thread needs: an env and a thread list."""

    def __init__(self, env):
        self.env = env
        self.threads = []


def as_captain(env, argv, tmp, answers=None, timeout=900):
    """Runs argv from the captain's Terminal (no harness among its ancestors). With
    `answers` [(prompt, reply)…] it runs on a pty and types each reply when its prompt
    appears; without, stdin is /dev/null (no terminal)."""
    out = Path(tmp) / f"captain-{time.time_ns()}.json"
    code = f'''
import os, subprocess, json, pty, select, time
if os.fork(): os._exit(0)
os.setsid()
if os.fork(): os._exit(0)
argv, answers, out = {argv!r}, {answers!r}, {str(out)!r}
if answers is None:
    r = subprocess.run(argv, capture_output=True, text=True, stdin=subprocess.DEVNULL)
    res = {{"code": r.returncode, "out": r.stdout + r.stderr, "answered": []}}
else:
    pid, fd = pty.fork()
    if pid == 0:
        os.execvp(argv[0], argv)
    buf, pos, answered, code = b"", 0, [], None
    while True:
        r, _, _ = select.select([fd], [], [], 0.5)
        if r:
            try:
                d = os.read(fd, 65536)
            except OSError:
                d = b""
            if not d:
                break
            buf += d
            while answers and answers[0][0].encode() in buf[pos:]:
                q, a = answers.pop(0)
                pos = buf.index(q.encode(), pos) + len(q)
                time.sleep(0.2)
                os.write(fd, (a + "\\n").encode())
                answered.append(q)
    _, st = os.waitpid(pid, 0)
    res = {{"code": os.waitstatus_to_exitcode(st), "out": buf.decode(errors="replace").replace("\\r\\n", "\\n"),
           "answered": answered}}
open(out + ".tmp", "w").write(json.dumps(res))
os.rename(out + ".tmp", out)
'''
    subprocess.run([sys.executable, "-c", code], env=env, check=True)
    wait(lambda: out.exists(), timeout=timeout, what=" ".join(argv))
    return json.loads(out.read_text())


def show(r):
    print("\n".join("  | " + l for l in r["out"].splitlines()), flush=True)


def notes(root):
    return sorted(str(p.relative_to(root)) for p in root.glob("**/notes/*.md"))


def hook_state_keys(codex):
    t = tomllib.loads((Path(codex) / "config.toml").read_text())
    return set((t.get("hooks") or {}).get("state", {}))


def harness_bytes(env):
    return {str(p): p.read_bytes() for p in homes.touched_files(env) if p.is_file()}


def stale_unvrs(d):
    """An older unvrs build (a 0.5 `cargo install`, an old copy) in directory d."""
    d = Path(d)
    d.mkdir(parents=True, exist_ok=True)
    (d / "unvrs").write_text("#!/bin/sh\necho old unvrs\n")
    (d / "unvrs").chmod(0o755)
    return d / "unvrs"


def captain_env(env, path_first=None, path_last=None):
    full = {k: v for k, v in os.environ.items() if k not in MODEL_ENV and not k.startswith("CLAUDE_CODE_")}
    full.update(env)
    full.update({"UNVRS_SETUP_BIN": str(BIN), "UNVRS_SEED": str(SEED), "UNVRS_NOTIFY": "0", "UNVRS_QUOTA": "0",
                 "TERM": "xterm"})
    # PATH without any unvrs of the captain's (~/.local/bin holds one), then the test's link dir.
    dirs = [d for d in full["PATH"].split(":") if d and not (Path(d) / "unvrs").exists()]
    full["PATH"] = ":".join(([path_first] if path_first else []) + dirs + ([path_last] if path_last else []))
    return full


def main():
    c = homes.Checks()
    seed_sources = [s["id"] for s in tomllib.loads((SEED / "sources.toml").read_text())["source"]]
    seed_projects = [p["id"] for p in tomllib.loads((SEED / "projects.toml").read_text())["project"]]
    before = sums()
    tmp = tempfile.mkdtemp(prefix="unvrs-setup-", dir="/tmp")
    env = homes.make(Path(tmp) / "a", faithful=True)
    envb = homes.make(Path(tmp) / "b", faithful=True)
    # A stale build later on PATH in both homes, as ~/.cargo/bin/unvrs on the captain's Mac.
    cargo_a, cargo_b = stale_unvrs(Path(tmp) / "a/cargobin"), stale_unvrs(Path(tmp) / "b/cargobin")
    full = captain_env(env, path_first=env["UNVRS_LINK_DIR"], path_last=str(cargo_a.parent))
    fullb = captain_env(envb, path_last=str(cargo_b.parent))
    root = Path(env["UNVRS_HOME"])
    install = str(REPO / "install.sh")
    try:
        # ── refusals (a model never runs the captain's setup) ──
        r = subprocess.run([str(BIN), "setup", "--no-seed"], env=full, capture_output=True, text=True,
                           stdin=subprocess.DEVNULL)
        c.check("setup refused under a harness process", r.returncode != 0 and "Refused" in r.stderr, r.stderr[:160])
        r = as_captain({**full, "CODEX_THREAD_ID": "t-1"}, [str(BIN), "setup", "--no-seed"], tmp)
        c.check("setup refused with a harness thread env", r["code"] != 0 and "Refused" in r["out"], r["out"][-160:])
        r = as_captain({**full, "CLAUDECODE": "1"}, [install, "--no-seed"], tmp)
        c.check("install.sh refused from a model env", r["code"] != 0 and "not inside a Claude or Codex" in r["out"])
        c.check("nothing installed by the refused runs", not (root / "bin/unvrs").exists())

        # ── 1. first run on a pty, the captain says yes to the trust ──
        keys0 = hook_state_keys(env["CODEX_HOME"])
        # As on the captain's Mac: an older unvrs binary already sits in ~/.local/bin.
        stale_unvrs(env["UNVRS_LINK_DIR"])
        t0 = time.time()
        r = as_captain(full, [install], tmp, answers=[("Move it to", "y"), (TRUST_Q, "y")])
        first = time.time() - t0
        show(r)
        text = r["out"]
        c.check("first run exit 0", r["code"] == 0, f"exit {r['code']}, {first:.0f}s")
        c.check("installed: binary, plugin, LaunchAgent", "result: installed" in text and "launchagent: written" in text
                and homes.launchd_loaded(env), env["UNVRS_LAUNCHD_LABEL"])
        link = Path(env["UNVRS_LINK_DIR"]) / "unvrs"
        c.check("unvrs on PATH: link dir symlink to the stable binary",
                link.is_symlink() and os.readlink(link) == str(root / "bin/unvrs") and f"path: {link} ->" in text)
        kept = [p for p in root.glob("backup/*-path/*unvrs") if "old unvrs" in p.read_text()]
        c.check("the older unvrs in the link dir kept in the backup", f"path: moved the old {link}" in text
                and len(kept) == 2 and len(list(root.glob("backup/*-path/moved.txt"))) == 1, str(kept))
        c.check("a stale unvrs later on PATH listed, moved after yes",
                f"  {cargo_a}" in text and f"path: moved {cargo_a} to" in text and not cargo_a.exists())
        c.check("doctor: unvrs on PATH is ours", "ok unvrs on PATH: `unvrs` is" in text)
        c.check("kernel started", "kernel: running" in text)
        c.check("seed imported", f"- sources: {len(seed_sources)} added" in text
                and f"- projects: {len(seed_projects)} created" in text and "warning:" not in text)
        listed = re.findall(r"^\s+(preCompact|sessionStart|sessionEnd|userPromptSubmit|stop)\s+(.+)$", text, re.M)
        c.check("the 5 hooks listed with event and command, then the question",
                len(listed) == 5 and all(f"'{root}/bin/unvrs' hook " in cmd for _, cmd in listed)
                and r["answered"] == ["Move it to", TRUST_Q], f"{[e for e, _ in listed]}")
        c.check("trust written through Codex's app server", "codex hooks: trusted 5 (written by Codex's app server" in text)
        added = hook_state_keys(env["CODEX_HOME"]) - keys0
        c.check("only our 5 hook keys added to hooks.state",
                len(added) == 5 and all(k.startswith("unvrs@unvrs-local:") for k in added), str(sorted(added)))
        backups = list(root.glob("backup/*-codex-trust/config.toml"))
        c.check("config.toml backed up before the trust", len(backups) == 1)
        c.check("doctor: every check passed", "doctor: every check passed" in text and "FAIL" not in text)
        port = env["UNVRS_OBSERVATORY_PORT"]
        c.check("ready block: start, Observatory, restart reminder",
                "==> Ready" in text and "$unvrs:l1" in text and "/unvrs:l1" in text
                and f"http://unvrs.localhost:{port}" in text and "restart the Codex app and T3 Code" in text)

        # ── 2. the seed is live ──
        h = Home(full)
        l1 = Thread(h, harness="codex", cwd=str(REPO))
        out = l1.prompt("$unvrs:l1")
        pid = int(re.search(r"PID (\d+)", ctx(out) or "PID 0").group(1)) or 1
        hot = subprocess.run([str(BIN), "kernel", "hot", "--pid", str(pid)], env=full, capture_output=True,
                             text=True).stdout
        facts = ["Co-founder of Acme", "Runs a small studio", "Works remotely", "Personal brand handle @captain"]
        # The hot set is budget-bound (2,200 bytes of memory): some seed facts, not all.
        c.check("L1 hot set holds captain facts", sum(f in hot for f in facts) >= 2 and "(pinned)" in hot,
                f"{len(hot)} bytes; {[f for f in facts if f in hot]}")
        res, _ = l1.mcp("ctx sources")
        listed_src = res["content"][0]["text"]
        c.check("ctx sources lists every seed source", all(f"- {s} (" in listed_src for s in seed_sources))
        l2 = Thread(h, harness="codex", cwd="/tmp")
        c.check("L2 acme seat bound", "acme" in ctx(l2.prompt("$unvrs:l2 acme")))
        res, _ = l2.mcp('ctx search "Acme" --source acme')
        hits = res["content"][0]["text"]
        c.check("L2 acme ctx search finds its source",
                not res.get("isError") and "ctx://acme/" in hits and "/media/" not in hits and ".env" not in hits)
        req = urllib.request.Request(f"http://127.0.0.1:{port}/api/snapshot", headers={"Host": "unvrs.localhost"})
        snap = wait(lambda: json.loads(urllib.request.urlopen(req, timeout=5).read()), what="observatory")
        c.check("Observatory snapshot lists every seed project",
                sorted(p.get("id") for p in snap.get("projects", [])) == sorted(seed_projects))
        res, _ = l1.mcp(f"seed import {SEED}")
        c.check("a model's seed import is refused", res.get("isError") and "only the captain" in res["content"][0]["text"])

        # ── 3. second run: a no-op ──
        n1, s1, b1 = notes(root), (root / "sources.toml").read_text(), harness_bytes(env)
        t0 = time.time()
        r = as_captain(full, [install], tmp, answers=[])
        second = time.time() - t0
        text = r["out"]
        c.check("second run exit 0", r["code"] == 0, f"exit {r['code']}, {second:.0f}s (first {first:.0f}s)")
        c.check("second run: nothing to install, link and trust already there, no question",
                "result: already installed; nothing changed" in text and "path: already" in text
                and "all 5 UNVRS hooks already trusted" in text and "[Y/n]" not in text,
                " / ".join(l for l in text.splitlines() if l.startswith(("result:", "path:", "codex hooks:"))))
        c.check("second run: seed unchanged, no duplicates",
                f"0 added, 0 updated, {len(seed_sources)} unchanged" in text and "captain memory: 0 new" in text
                and notes(root) == n1 and (root / "sources.toml").read_text() == s1)
        c.check("second run: harness files byte-identical, one trust backup",
                harness_bytes(env) == b1 and len(list(root.glob("backup/*-codex-trust"))) == 1)
        c.check("second run: doctor passes", "doctor: every check passed" in text)
        for t in h.threads:
            t.kill()

        # ── 4. home B, no terminal: trust skipped and said so ──
        keysb = hook_state_keys(envb["CODEX_HOME"])
        r = as_captain(fullb, [str(REPO / "scripts/setup-captain.sh"), "--no-seed"], tmp)
        show(r)
        text = r["out"]
        rc = Path(envb["UNVRS_SHELL_RC"])
        c.check("non-TTY run exit 0", r["code"] == 0, f"exit {r['code']}")
        c.check("non-TTY: trust skipped, says so, prints the manual step",
                "codex hooks: not trusted (no terminal to ask you in" in text and "Settings > Hooks" in text
                and "/hooks" in text and "[Y/n]" not in text and hook_state_keys(envb["CODEX_HOME"]) == keysb)
        c.check("non-TTY: doctor ok except the trust that is yours and PATH until a new terminal",
                "doctor: ok except " in text and "Codex hook trust (yours to give" in text
                and "unvrs on PATH (open a new terminal)" in text and "==> Ready" in text and r["code"] == 0)
        c.check("non-TTY: stale unvrs listed, left in place with the rm line",
                f"  {cargo_b}" in text and "path: left as they are; remove them with: rm" in text and cargo_b.exists())
        c.check("non-TTY: rc line printed, rc untouched",
                'export PATH="' in text and "path: not changed" in text and not rc.exists())
        c.check("--no-seed honoured", "seed: skipped (--no-seed)" in text)

        # ── 5. home B on a pty: yes to the rc line, no to the trust ──
        # Piped like the GitHub one-liner: stdin is the script, the prompts read /dev/tty.
        piped = f"cat '{install}' | sh -s -- --no-seed"
        r = as_captain({**fullb, "UNVRS_REPO_DIR": str(REPO)}, ["sh", "-c", piped], tmp, answers=[("zshrc? [Y/n]", "y"), ("Move it to", "n"), (TRUST_Q, "n")])
        text = r["out"]
        line = f'export PATH="{Path(envb["UNVRS_HOME"]) / "bin"}:$PATH"'
        c.check("piped run uses the clone and reads answers from the terminal",
                f"Using {REPO} as it is" in text, text.splitlines()[0] if text else "")
        c.check("pty: rc line appended once after yes",
                r["code"] == 0 and rc.is_file() and rc.read_text().count(line) == 1, rc.read_text() if rc.exists() else "")
        c.check("pty: no to the trust leaves hooks.state alone",
                "codex hooks: not trusted (your answer)" in text and hook_state_keys(envb["CODEX_HOME"]) == keysb
                and r["answered"] == ["zshrc? [Y/n]", "Move it to", TRUST_Q] and cargo_b.exists())
    finally:
        for e, f in ((env, full), (envb, fullb)):
            homes.cleanup(e, remove=False)  # boots out the test LaunchAgent first
            subprocess.run([str(BIN), "kernel", "stop"], env=f, capture_output=True)
        if os.environ.get("UNVRS_KEEP_TMP") != "1":
            import shutil
            shutil.rmtree(tmp, ignore_errors=True)
    c.check("captain's live harness files, ~/.local/bin/unvrs and shell rc unchanged (sha256)", sums() == before)
    c.exit()


if __name__ == "__main__":
    main()
