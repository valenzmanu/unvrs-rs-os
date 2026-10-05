#!/usr/bin/env python3
"""A11 · install / uninstall on faithful copies: idempotent, only our entries, byte-identical back.

On a faithful copy of the captain's harness homes (homes.py; never the live ones) with a
test LaunchAgent label (dev.unvrs.test.<random>):
  1. snapshot every harness file the install may touch (gate start);
  2. `unvrs install`; every file differs from the snapshot only by our entries, and the
     captain's own hooks (drain.sh, gh-axi, chrome-devtools-axi, …) are all still there;
  3. `unvrs install` again: reports "already", changes no byte;
  4. `unvrs doctor`: its lines are reported (not required to pass in a tree without the
     kernel's `mcp`/`ping`);
  5. `unvrs uninstall`: every file `cmp`-identical to the gate-start snapshot and to the
     install's own backup; the LaunchAgent is gone (`launchctl print` fails, plist deleted);
  6. `unvrs uninstall` again: no-op.
The captain's live files are hashed before and after and reported (info). Exit 0 on pass.

    a11-install.py [--keep] [--no-launchagent]
"""
import hashlib
import json
import os
import re
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import homes  # noqa: E402

PLUGIN_ID = "unvrs@unvrs-local"
MARKET = "unvrs-local"
CAPTAIN_HOOK_MARKERS = ["drain.sh", "gh-axi", "chrome-devtools-axi"]


def result_line(out):
    lines = [l for l in out.splitlines() if l.startswith("result:")]
    return lines[-1] if lines else out.strip()[-200:]


def strip_ts(v):
    if isinstance(v, dict):
        return {k: strip_ts(x) for k, x in v.items() if k != "lastUpdated"}
    if isinstance(v, list):
        return [strip_ts(x) for x in v]
    return v


def json_without_ours(text, old):
    v = json.loads(text)
    for container, key in [("enabledPlugins", PLUGIN_ID), ("extraKnownMarketplaces", MARKET), ("plugins", PLUGIN_ID)]:
        m = v.get(container)
        if isinstance(m, dict):
            m.pop(key, None)
            if not m and container not in (old or {}):
                v.pop(container)
    v.pop(MARKET, None)
    return strip_ts(v)


def toml_without_ours(text, home):
    out, skip = [], False
    for line in text.splitlines(keepends=True):
        t = line.strip()
        if t.startswith("["):
            skip = (t in (f"[marketplaces.{MARKET}]", f'[marketplaces."{MARKET}"]')
                    or t.startswith(f'[plugins."{PLUGIN_ID}"')
                    or t.startswith(f'[hooks.state."{PLUGIN_ID}:')
                    or (t.startswith('[projects."') and t[len('[projects."'):].startswith(home)))
        if not skip:
            out.append(line)
    text = "".join(out)
    if skip:
        while text.endswith("\n\n"):
            text = text[:-1]
    return text


def only_ours(path, old, new, home):
    """True when `new` equals `old` once our entries are taken out."""
    if old == new:
        return True
    if old is None:
        return new is not None and (path.suffix != ".toml" and not any(json_without_ours(new.decode(), None).values()))
    if path.suffix == ".toml":
        return toml_without_ours(new.decode(), home) == toml_without_ours(old.decode(), home)
    o = json.loads(old)
    return json_without_ours(new.decode(), o) == strip_ts(o)


def sha(p):
    try:
        return hashlib.sha256(Path(p).read_bytes()).hexdigest()
    except OSError:
        return None


def main():
    keep = "--keep" in sys.argv
    agent = "--no-launchagent" not in sys.argv
    c = homes.Checks()
    live_files = [homes.CAPTAIN_CLAUDE / f for f in homes.CLAUDE_FILES] + [homes.CAPTAIN_CODEX / f for f in homes.CODEX_FILES]
    live_before = {p: sha(p) for p in live_files}
    tmp = tempfile.mkdtemp(prefix="unvrs-a11-")
    env = homes.make(tmp, faithful=True)
    home = env["UNVRS_HOME"]
    plist = Path(env["UNVRS_LAUNCHAGENTS_DIR"]) / f"{env['UNVRS_LAUNCHD_LABEL']}.plist"
    print(f"homes: {tmp} (label {env['UNVRS_LAUNCHD_LABEL']})")
    install_args = ["install"] + ([] if agent else ["--no-launchagent"])
    try:
        s0 = homes.snapshot(env)
        present = {m: any(b and m.encode() in b for b in s0.values()) for m in CAPTAIN_HOOK_MARKERS}
        print(f"info captain hooks in the copy: {present}")

        r = homes.run(env, *install_args)
        print("\n".join("  | " + l for l in r.stdout.splitlines()))
        c.check("install", r.returncode == 0, result_line(r.stdout + r.stderr))
        s1 = homes.snapshot(env)
        changed = [p for p in s0 if s0[p] != s1[p]]
        print(f"info files changed by install: {[str(p) for p in changed]}")
        for p in s0:
            c.check(f"only our entries in {p.name}", only_ours(p, s0[p], s1[p], home),
                    "unchanged" if s0[p] == s1[p] else "differs only by unvrs entries")
        for m, was in present.items():
            if was:
                c.check(f"captain hook {m} still present", any(b and m.encode() in b for b in s1.values()))
        if agent:
            c.check("launchagent loaded", homes.launchd_loaded(env) and plist.is_file(), str(plist))

        r = homes.run(env, *install_args)
        s2 = homes.snapshot(env)
        c.check("second install is a no-op", r.returncode == 0 and "already installed; nothing changed" in r.stdout and s2 == s1,
                result_line(r.stdout + r.stderr))

        r = homes.run(env, "doctor", timeout=300)
        print(f"info doctor exit {r.returncode}:")
        print("\n".join("  | " + l for l in (r.stdout + r.stderr).splitlines()))

        # What `codex exec` in a directory under UNVRS_HOME does by itself (LEARN 0.7 K-1):
        # a project trust table. It is ours to remove; uninstall must still restore bytes.
        cfg = Path(env["CODEX_HOME"], "config.toml")
        with cfg.open("a") as f:
            f.write(f'\n[projects."{home}/work"]\ntrust_level = "trusted"\n')
        print("info simulated Codex project trust for a path under UNVRS_HOME")

        record = json.loads(Path(home, "install.json").read_text())
        manifest = json.loads(Path(record["backup"], "manifest.json").read_text())
        r = homes.run(env, "uninstall")
        print("\n".join("  | " + l for l in r.stdout.splitlines()))
        c.check("uninstall", r.returncode == 0 and "CHANGED" not in r.stdout, result_line(r.stdout + r.stderr))
        s3 = homes.snapshot(env)
        for p in s0:
            c.check(f"cmp gate-start {p.name}", s3[p] == s0[p],
                    "byte-identical" if s0[p] is not None else "absent before and after")
        for f in manifest["files"]:
            p = Path(f["path"])
            same = (not f["existed"] and not p.exists()) or (f["existed"] and Path(f["copy"]).read_bytes() == p.read_bytes())
            c.check(f"cmp backup {p.name}", same)
        c.check("launchagent gone", not homes.launchd_loaded(env) and not plist.exists(),
                "launchctl print fails; plist deleted")
        c.check("plugin dir removed, home kept", not Path(home, "plugin").exists() and Path(home, "versions").is_dir())

        r = homes.run(env, "uninstall")
        c.check("second uninstall is a no-op", r.returncode == 0 and "already uninstalled" in r.stdout and homes.snapshot(env) == s0,
                result_line(r.stdout + r.stderr))
    finally:
        homes.cleanup(env, remove=not keep)
    live_after = {p: sha(p) for p in live_files}
    for p in live_files:
        print(f"info captain live {p}: {'unchanged' if live_before[p] == live_after[p] else 'changed during the run (not by this test: every path came from the test env)'}")
    c.exit()


if __name__ == "__main__":
    main()
