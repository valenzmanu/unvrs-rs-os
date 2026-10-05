#!/usr/bin/env python3
"""A8 live: an L2 (Codex app-server thread) dispatches a report task through its MCP
tool; the L3 runs on the driven path (real `claude -p`) and returns a result package to
the L2's wake queue; the L2 thread is closed, so the kernel runs the L2 on its own
(real `codex exec`) and the outcome reaches L1's digest. No relay by the captain."""
import json
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from livelib import CodexThread, LiveHome, codex_app_mention, log, wait  # noqa: E402


def fixture(root):
    d = root / "docs-src"
    (d / "brand").mkdir(parents=True)
    (d / "CONTEXT.md").write_text("# Docs\nBrand rules live in brand/.\n")
    (d / "brand/voice.md").write_text("# Voice\n\nOSPREY-VOICE rule: write warm, short sentences; never use jargon.\n")
    for c in (["git", "init", "-q"], ["git", "add", "-A"], ["git", "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "docs"]):
        subprocess.run(c, cwd=d, check=True)
    return d


def main():
    h = LiveHome("a8")
    ok = True
    try:
        src = fixture(h.tmp)
        assert h.captain("ctl", "sources", "add", "docs", "path", str(src), "--purpose", "brand docs")["code"] == 0
        r = h.captain("ctl", "project", "new", "kappa", "Kappa brand work", "--source", "docs")
        assert r["code"] == 0, r
        l2 = CodexThread(h)
        # The Codex app's picked-skill shape with an argument: [$unvrs:l2](…/SKILL.md) kappa
        r = l2.ask(codex_app_mention(h, "l2", "kappa"))
        log(f"codex L2 after [$unvrs:l2](…SKILL.md) kappa: {r[:200]!r}")
        r = l2.ask('Ok, go. Dispatch exactly one report task with your unvrs tool, command: task --intent "which file in docs states the OSPREY-VOICE rule, and what does it say?" '
                   '--spec "use ctx search and ctx get on the docs source; answer with the file, the rule and its ctx ref" --shape report --source docs --on claude --go "Dispatch exactly one report task" --done-when "Report names the voice file and rule with a ctx ref". '
                   'Then reply only: dispatched.')
        log(f"codex L2 dispatch reply: {r[:160]!r} tools={l2.tools_used()}")
        task = wait(lambda: h.journal("task"), timeout=30, what="task event")[0]
        worker = task["pid"]
        log(f"journal task: {({k: task.get(k) for k in ('pid', 'parent', 'project', 'shape', 'sources', 'harness')})}")
        l2.close()  # the L2 thread closes: its seat is detached
        wait(lambda: any(e["kind"] == "detach" for e in h.journal()), timeout=30, what="L2 detach")
        res = wait(lambda: [e for e in h.journal("result") if e["pid"] == worker], timeout=900, step=2, what="L3 result")[0]
        pkg = json.loads(Path(res["package"]).read_text())
        assert pkg["contract"]["go_quote"] == "Dispatch exactly one report task", pkg["contract"]
        assert pkg["contract"]["go_source"]["kind"] == "captain_prompt", pkg["contract"]
        assert pkg["contract"]["done_when"] == "Report names the voice file and rule with a ctx ref", pkg["contract"]
        log(f"L3 PID {worker} result ({res['how']}): {pkg['result'][:300]!r}")
        reads = [e for e in h.journal("ctx.read") if e.get("pid") == worker]
        log(f"L3 context reads: {[(e['op'], e['ref']) for e in reads][:4]}")
        wk = [e for e in h.journal("wake") if e["wake_kind"] in ("done", "failed") and e.get("from") == worker]
        assert wk, "no wake on the L2"
        log(f"wake on L2 PID {wk[0]['pid']}: {wk[0]['text'][:160]!r}")
        out = wait(lambda: [e for e in h.journal("outcome") if e["pid"] == wk[0]["pid"]], timeout=600, step=2, what="detached L2 run")[0]
        log(f"detached L2 run outcome ({out['harness']}): {out['text'][:300]!r}")
        assert res["how"] == "done" and ("voice.md" in pkg["result"] or "OSPREY" in pkg["result"]), pkg["result"]
        dg = h.captain("ctl", "digest")
        text = dg["out"]
        log("L1 digest:\n" + "\n".join("  " + l for l in text.splitlines()[:16]))
        assert "kappa" in text and "lead handled" in text, text
    except AssertionError as e:
        ok = False
        log(f"A8 FAIL: {e}")
    finally:
        h.close()
    log("A8 PASS" if ok else "A8 FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
