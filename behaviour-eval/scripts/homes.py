#!/usr/bin/env python3
"""Test homes for every 0.8 script: a temp UNVRS_HOME plus copies of the harness homes.

Never touches the captain's live ~/.claude, ~/.codex, ~/.unvrs or launchd domain: every
path the `unvrs` binary computes comes from the env returned by `make()`, and the
LaunchAgent label is `dev.unvrs.test.<random>` (booted out by `cleanup()`).

    env = homes.make(tmp, faithful=True, live=False)   # env additions (str -> str)
    ...                                                 # run with {**os.environ, **env}
    homes.cleanup(env)

faithful=True copies the captain's harness config files the install touches (Claude
settings.json, settings.local.json, plugins/installed_plugins.json,
plugins/known_marketplaces.json; Codex config.toml, hooks.json) and their skills dirs,
never caches of Codex, sessions or credentials. Two safety edits make the copy
self-contained; both happen before any gate snapshot, so byte-identical checks compare
against the copy as made:
  * every occurrence of the captain's ~/.claude and ~/.codex path is rewritten to the
    copy (Claude's registered marketplaces and plugin cache, about 15 MB, are copied so
    those entries still resolve; a harness never writes into the captain's dirs);
  * the captain's own UNVRS install (plugin, marketplace, trust state, projects under
    ~/.unvrs, the unvrs-local plugin cache) is left out, so the copy is the captain's config
    as it was before UNVRS and a test home never points at the live ~/.unvrs;
  * the captain's hook commands are neutered as `true # <original command>`: the entries
    (drain.sh, gh-axi, chrome-devtools-axi, …) stay, byte for byte after the prefix, but a
    test session never runs the captain's scripts against their live state.
live=True adds sanitized credentials (Claude: keychain JSON without
claudeAiOauth.refreshToken; Codex: auth.json with tokens.refresh_token replaced) and, when
not faithful, a minimal clean config for live runs.
"""
import json
import os
import re
import secrets
import shutil
import socket
import subprocess
import time
from pathlib import Path

HOME = Path.home()
REPO = Path(__file__).resolve().parents[2]
CAPTAIN_CLAUDE = HOME / ".claude"
CAPTAIN_CODEX = HOME / ".codex"
CLAUDE_FILES = ["settings.json", "settings.local.json", "plugins/installed_plugins.json",
                "plugins/known_marketplaces.json"]
CODEX_FILES = ["config.toml", "hooks.json"]
NEUTER = "true # "


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _write_private(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as f:
        f.write(text)


def _neuter_json_hooks(obj):
    """Prefix every hook `command` with `true # ` (idempotent)."""
    hooks = obj.get("hooks") if isinstance(obj, dict) else None
    if not isinstance(hooks, dict):
        return obj
    for groups in hooks.values():
        for g in groups if isinstance(groups, list) else []:
            for h in g.get("hooks", []) if isinstance(g, dict) else []:
                c = h.get("command")
                if isinstance(c, str) and not c.startswith(NEUTER):
                    h["command"] = NEUTER + c
    return obj


UNVRS_PLUGIN, UNVRS_MARKET = "unvrs@unvrs-local", "unvrs-local"


def _without_unvrs(obj):
    """Drops the captain's live UNVRS plugin and marketplace entries from a Claude JSON file."""
    if not isinstance(obj, dict):
        return obj
    for key in ("enabledPlugins", "plugins"):
        if isinstance(obj.get(key), dict):
            obj[key].pop(UNVRS_PLUGIN, None)
    if isinstance(obj.get("extraKnownMarketplaces"), dict):
        obj["extraKnownMarketplaces"].pop(UNVRS_MARKET, None)
    obj.pop(UNVRS_MARKET, None)
    return obj


def codex_without_unvrs(text):
    """The Rust `codex_config_without_ours` for the captain's live home: drops our tables."""
    live = str(HOME / ".unvrs")

    def ours(h):
        h = h.strip()
        return (h in (f"[marketplaces.{UNVRS_MARKET}]", f'[marketplaces."{UNVRS_MARKET}"]')
                or h.startswith(f'[plugins."{UNVRS_PLUGIN}"]') or h.startswith(f'[plugins."{UNVRS_PLUGIN}".')
                or h.startswith(f'[hooks.state."{UNVRS_PLUGIN}:')
                or (h.startswith('[projects."') and h[len('[projects."'):].startswith(live)))
    out, skip = [], False
    for line in text.splitlines(keepends=True):
        if line.lstrip().startswith("["):
            skip = ours(line.strip())
        if not skip:
            out.append(line)
    return "".join(out)


def _copy_json(src, dst, subs):
    text = src.read_text()
    for a, b in subs:
        text = text.replace(a, b)
    try:
        obj = _without_unvrs(json.loads(text))
    except ValueError:
        dst.parent.mkdir(parents=True, exist_ok=True)
        dst.write_text(text)
        return
    # Keep the captain's formatting style (Claude writes 2-space JSON).
    dst.parent.mkdir(parents=True, exist_ok=True)
    dst.write_text(json.dumps(_neuter_json_hooks(obj), indent=2, ensure_ascii=False) + ("\n" if text.endswith("\n") else ""))


def _faithful(claude, codex):
    subs_claude = [(str(CAPTAIN_CLAUDE) + "/", str(claude) + "/")]
    subs_codex = [(str(CAPTAIN_CODEX) + "/", str(codex) + "/")]
    for rel in CLAUDE_FILES:
        src = CAPTAIN_CLAUDE / rel
        if src.is_file():
            _copy_json(src, claude / rel, subs_claude)
    for d in ["plugins/marketplaces", "plugins/cache", "skills"]:
        src = CAPTAIN_CLAUDE / d
        if src.is_dir():
            shutil.copytree(src, claude / d, symlinks=True, dirs_exist_ok=True,
                            ignore=shutil.ignore_patterns(UNVRS_MARKET))
    src = CAPTAIN_CODEX / "config.toml"
    if src.is_file():
        text = codex_without_unvrs(src.read_text())
        for a, b in subs_codex:
            text = text.replace(a, b)
        _write_private(codex / "config.toml", text)
    src = CAPTAIN_CODEX / "hooks.json"
    if src.is_file():
        _copy_json(src, codex / "hooks.json", subs_codex)
    if (CAPTAIN_CODEX / "skills").is_dir():
        shutil.copytree(CAPTAIN_CODEX / "skills", codex / "skills", symlinks=True, dirs_exist_ok=True,
                        ignore=shutil.ignore_patterns(".system"))


def _live(claude, codex, faithful):
    user = os.environ.get("USER", "")
    r = subprocess.run(["security", "find-generic-password", "-a", user, "-w", "-s", "Claude Code-credentials"],
                       capture_output=True, text=True)
    if r.returncode == 0 and r.stdout.strip():
        cred = json.loads(r.stdout)
        if isinstance(cred.get("claudeAiOauth"), dict):
            cred["claudeAiOauth"].pop("refreshToken", None)
        _write_private(claude / ".credentials.json", json.dumps(cred))
    auth = CAPTAIN_CODEX / "auth.json"
    if auth.is_file():
        a = json.loads(auth.read_text())
        if isinstance(a.get("tokens"), dict) and "refresh_token" in a["tokens"]:
            a["tokens"]["refresh_token"] = "unvrs-test-copy-no-refresh"
        _write_private(codex / "auth.json", json.dumps(a, indent=2))
    if not faithful:
        model = os.environ.get("UNVRS_TEST_CODEX_MODEL", "gpt-5.6-sol")
        _write_private(codex / "config.toml",
                       f'model = "{model}"\nmodel_reasoning_effort = "low"\napproval_policy = "never"\n'
                       f'sandbox_mode = "danger-full-access"\n')
        (claude / "settings.json").write_text("{}\n")


def make(tmp, faithful=False, live=False):
    """Creates the homes under `tmp` and returns the env additions."""
    root = Path(tmp).resolve()
    claude, codex, unvrs, agents = root / "claude", root / "codex", root / "unvrs", root / "LaunchAgents"
    for d in (claude, codex, unvrs, agents):
        d.mkdir(parents=True, exist_ok=True)
    if faithful:
        _faithful(claude, codex)
    if live:
        _live(claude, codex, faithful)
    return {
        "UNVRS_HOME": str(unvrs),
        "CLAUDE_CONFIG_DIR": str(claude),
        "CODEX_HOME": str(codex),
        "UNVRS_LAUNCHAGENTS_DIR": str(agents),
        "UNVRS_LAUNCHD_LABEL": "dev.unvrs.test." + secrets.token_hex(4),
        "UNVRS_OBSERVATORY_PORT": str(free_port()),
        "UNVRS_TEST_ROOT": str(root),
        # `unvrs setup` links unvrs here and edits this rc, never ~/.local/bin or ~/.zshrc.
        "UNVRS_LINK_DIR": str(root / "localbin"),
        "UNVRS_SHELL_RC": str(root / "zshrc"),
    }


def launchd_loaded(env):
    label = env["UNVRS_LAUNCHD_LABEL"]
    return subprocess.run(["launchctl", "print", f"gui/{os.getuid()}/{label}"],
                          capture_output=True).returncode == 0


def cleanup(env, remove=True):
    """Boots out the test LaunchAgent (only a dev.unvrs.test.* label) and removes the tree."""
    label = env.get("UNVRS_LAUNCHD_LABEL", "")
    if label.startswith("dev.unvrs.test.") and launchd_loaded(env):
        subprocess.run(["launchctl", "bootout", f"gui/{os.getuid()}/{label}"], capture_output=True)
    root = env.get("UNVRS_TEST_ROOT")
    if remove and root and Path(root).resolve() != HOME and str(Path(root).resolve()).startswith(("/tmp", "/private", tempdir_prefix())):
        shutil.rmtree(root, ignore_errors=True)


def tempdir_prefix():
    import tempfile
    return str(Path(tempfile.gettempdir()).resolve())


# ───────────────────────────── shared helpers ─────────────────────────────

def unvrs_bin():
    """The dev build under test (`UNVRS_BIN`, else target/debug/unvrs, built if missing)."""
    b = os.environ.get("UNVRS_BIN")
    if b:
        return b
    p = REPO / "target/debug/unvrs"
    if not p.exists():
        subprocess.run(["cargo", "build", "-q", "-p", "unvrs"], cwd=REPO, check=True)
    return str(p)


def full_env(env):
    e = {**os.environ, **env}
    return e


def run(env, *args, timeout=300, check=False):
    r = subprocess.run([unvrs_bin(), *args], env=full_env(env), capture_output=True, text=True,
                       timeout=timeout, stdin=subprocess.DEVNULL, cwd=env["UNVRS_HOME"])
    if check and r.returncode != 0:
        raise RuntimeError(f"unvrs {' '.join(args)} exited {r.returncode}: {r.stdout}{r.stderr}")
    return r


def touched_files(env):
    c, x = Path(env["CLAUDE_CONFIG_DIR"]), Path(env["CODEX_HOME"])
    return [c / f for f in CLAUDE_FILES] + [x / f for f in CODEX_FILES]


def snapshot(env):
    """{path: bytes | None} of every harness file the install may touch."""
    return {p: (p.read_bytes() if p.is_file() else None) for p in touched_files(env)}


def app_server(env, calls, timeout=60):
    """One `codex app-server` session: initialize, then each (method, params); returns responses."""
    import select
    p = subprocess.Popen(["codex", "app-server"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         stderr=subprocess.DEVNULL, text=True, env=full_env(env), cwd=env["UNVRS_HOME"])
    out = []
    try:
        def send(m):
            p.stdin.write(json.dumps(m) + "\n")
            p.stdin.flush()

        def wait(i):
            end = time.time() + timeout
            while time.time() < end:
                r, _, _ = select.select([p.stdout], [], [], 0.5)
                if r:
                    line = p.stdout.readline()
                    if not line:
                        return None
                    try:
                        m = json.loads(line)
                    except ValueError:
                        continue
                    if m.get("id") == i and "method" not in m:
                        return m
            return None

        send({"jsonrpc": "2.0", "id": 0, "method": "initialize",
              "params": {"clientInfo": {"name": "unvrs-eval", "version": "0"}}})
        wait(0)
        send({"jsonrpc": "2.0", "method": "initialized"})
        for i, (method, params) in enumerate(calls, 1):
            send({"jsonrpc": "2.0", "id": i, "method": method, "params": params})
            out.append(wait(i))
    finally:
        p.kill()
        p.wait()
    return out


def claude_init(env, prompt="/unvrs:tree", timeout=60):
    """The first system/init line of `claude -p`; the process is killed right after (no model turn)."""
    import select
    p = subprocess.Popen(["claude", "-p", "--output-format", "stream-json", "--verbose", prompt],
                         stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
                         env=full_env(env), cwd=env["UNVRS_HOME"])
    try:
        end = time.time() + timeout
        while time.time() < end:
            r, _, _ = select.select([p.stdout], [], [], 0.5)
            if not r:
                continue
            line = p.stdout.readline()
            if not line:
                return None
            try:
                m = json.loads(line)
            except ValueError:
                continue
            if m.get("type") == "system" and m.get("subtype") == "init":
                return m
        return None
    finally:
        p.kill()
        p.wait()


def entry_names():
    """Every ENTRY skill name from mapp_unvrs (the source of truth), as `unvrs:<name>`."""
    src = (REPO / "mapp_unvrs/src/lib.rs").read_text()
    body = src.split("pub const ENTRY", 1)[1].split("];", 1)[0]
    return ["unvrs:" + n for n in re.findall(r'name:\s*"([^"]+)"', body)]


class Checks:
    """One line per assertion; exit code from the tally."""

    def __init__(self):
        self.failed = 0

    def check(self, name, ok, detail=""):
        print(f"{'ok' if ok else 'FAIL'} {name}{': ' + detail if detail else ''}", flush=True)
        if not ok:
            self.failed += 1
        return ok

    def exit(self):
        print(f"result: {'pass' if not self.failed else f'{self.failed} failed'}")
        raise SystemExit(1 if self.failed else 0)
