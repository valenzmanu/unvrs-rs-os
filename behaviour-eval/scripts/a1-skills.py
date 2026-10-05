#!/usr/bin/env python3
"""A1 · the entry skills are listed by both harness cores, explicit-only, and nothing else moves.

Installs the dev build into a faithful copy of the captain's harness homes (homes.py;
never the live ones), then asks the same cores the apps use:
  * `codex app-server` skills/list: every ENTRY as `unvrs:<name>` (pluginId
    unvrs@unvrs-local); explicit-only is checked where Codex exposes it (skills/list does
    not carry the policy, so the listed skill's agents/openai.yaml is read);
  * Claude's `system/init` line (`claude -p --output-format stream-json`, killed before any
    model turn): slash_commands has every `unvrs:<name>`; explicit-only is read from the
    loaded plugin's SKILL.md (`disable-model-invocation: true`);
  * every skill and command the captain had before is still listed.
Then uninstalls. One line per assertion; exit 0 on pass.

    a1-skills.py [--keep]     (--keep leaves the temp homes for inspection)
"""
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import homes  # noqa: E402

PLUGIN_ID = "unvrs@unvrs-local"


def codex_skills(env):
    r = homes.app_server(env, [("skills/list", {"cwds": [env["UNVRS_HOME"]], "forceReload": True})])[0]
    out = {}
    for d in (r or {}).get("result", {}).get("data", []):
        for s in d.get("skills", []):
            out[s["name"]] = s
    return out


def main():
    keep = "--keep" in sys.argv
    c = homes.Checks()
    tmp = tempfile.mkdtemp(prefix="unvrs-a1-")
    env = homes.make(tmp, faithful=True)
    print(f"homes: {tmp} (label {env['UNVRS_LAUNCHD_LABEL']})")
    names = homes.entry_names()
    try:
        before_codex = codex_skills(env)
        init0 = homes.claude_init(env) or {}
        before_claude = set(init0.get("slash_commands", []))
        c.check("baseline", bool(before_codex) and bool(init0),
                f"codex lists {len(before_codex)} skills, claude init has {len(before_claude)} commands")

        r = homes.run(env, "install", "--no-launchagent")
        c.check("install", r.returncode == 0, next((l for l in r.stdout.splitlines() if l.startswith("result:")), r.stderr.strip()))

        after = codex_skills(env)
        ours = {n: s for n, s in after.items() if s.get("pluginId") == PLUGIN_ID}
        missing = [n for n in names if n not in ours]
        c.check("codex skills/list has every entry", not missing,
                f"{len(ours)} listed ({', '.join(sorted(ours))})" if not missing else f"missing {missing}")
        for n in ["unvrs:l1", "unvrs:l2", "unvrs:digest"]:
            s = ours.get(n, {})
            c.check(f"codex lists ${n}", n in ours,
                    f"displayName {s.get('interface', {}).get('displayName')!r}" if s else "absent")
        exposes = any("policy" in s or "allowImplicitInvocation" in s for s in ours.values())
        implicit = []
        for n, s in ours.items():
            y = Path(s["path"]).parent / "agents/openai.yaml"
            if not (y.is_file() and "allow_implicit_invocation: false" in y.read_text()):
                implicit.append(n)
        c.check("codex entries explicit-only", not implicit,
                ("skills/list exposes the policy" if exposes else
                 "skills/list does not expose the policy; agents/openai.yaml of every listed skill has allow_implicit_invocation: false")
                if not implicit else f"implicit: {implicit}")
        lost = sorted(n for n, s in before_codex.items() if n not in after)
        c.check("codex keeps the captain's skills", not lost,
                f"all {len(before_codex)} still listed" if not lost else f"lost {lost}")

        init = homes.claude_init(env) or {}
        cmds = set(init.get("slash_commands", []))
        cmissing = [n for n in names if n not in cmds]
        c.check("claude init slash_commands has unvrs:l1", "unvrs:l1" in cmds)
        c.check("claude init has every entry", not cmissing,
                f"{len([x for x in cmds if x.startswith('unvrs:')])} unvrs: commands" if not cmissing else f"missing {cmissing}")
        plug = next((p for p in init.get("plugins", []) if p.get("source") == PLUGIN_ID), None)
        not_explicit = []
        if plug:
            for n in names:
                f = Path(plug["path"]) / "skills" / n.split(":", 1)[1] / "SKILL.md"
                if not (f.is_file() and "disable-model-invocation: true" in f.read_text()):
                    not_explicit.append(n)
        c.check("claude entries explicit-only", plug is not None and not not_explicit,
                "init lists names only; every loaded SKILL.md has disable-model-invocation: true"
                if plug and not not_explicit else f"plugin {plug} not explicit: {not_explicit}")
        mcp = [m for m in init.get("mcp_servers", []) if m.get("name") == "plugin:unvrs:unvrs"]
        print(f"info claude mcp: {mcp or 'absent'}")
        clost = sorted(before_claude - cmds)
        c.check("claude keeps the captain's commands", not clost,
                f"all {len(before_claude)} still listed" if not clost else f"lost {clost}")

        r = homes.run(env, "uninstall")
        c.check("uninstall", r.returncode == 0 and "CHANGED" not in r.stdout,
                r.stdout.strip().splitlines()[-1] if r.stdout.strip() else r.stderr.strip())
        gone = [n for n in codex_skills(env) if n.startswith("unvrs:")]
        c.check("codex lists no unvrs: skill after uninstall", not gone, str(gone) if gone else "")
    finally:
        homes.run(env, "uninstall") if not keep else None
        homes.cleanup(env, remove=not keep)
    c.exit()


if __name__ == "__main__":
    main()
