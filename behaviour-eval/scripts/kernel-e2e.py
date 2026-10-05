#!/usr/bin/env python3
"""The 0.7 kernel gate checks, adapted to 0.8 (UNVRS_HOME, bind by command, seats that
move instead of cwd binding and auto-L2): A3 detach / notice / take back, A4 rehydrate
payload, A5+A11 L3 handoff package with fixture CPUs and hot set per rank, A12 detach
fold with compare-and-swap. The real `unvrs` binary and kernel; fixture CPUs only.

usage: kernel-e2e.py [A3 A4 A5 A11 A12]   (default: all)
"""
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from crewlib import TMP, Home, Thread, ctx, log, run_checks, wait  # noqa: E402


def captain(home, *args):
    """Captain's terminal (no harness ancestor): double fork, then `unvrs ctl`."""
    out = TMP / f"cap-{time.time_ns()}.json"
    code = f'''
import os, subprocess, json
if os.fork(): os._exit(0)
os.setsid()
if os.fork(): os._exit(0)
r = subprocess.run({[os.environ.get("UNVRS_BIN", str(Path(__file__).resolve().parents[2] / "target/debug/unvrs")), "ctl", *args]!r}, capture_output=True, text=True)
open({str(out)!r} + ".t", "w").write(json.dumps({{"code": r.returncode, "out": r.stdout, "err": r.stderr}}))
os.rename({str(out)!r} + ".t", {str(out)!r})
'''
    subprocess.run([sys.executable, "-c", code], env=home.env, check=True)
    wait(lambda: out.exists(), timeout=30, what="captain call")
    return json.loads(out.read_text())


def quiet(h, secs=3):
    last, since = -1, time.time()
    while time.time() - since < secs:
        n = len(h.journal("fold"))
        if n != last:
            last, since = n, time.time()
        time.sleep(0.3)


def a3():
    h = Home("r3")
    t1 = Thread(h)
    t1.prompt("$unvrs:l1")
    t1.stop()
    t1.turn("plan the release; OPEN: waiting on the changelog", "ok, waiting on the changelog")
    before = h.session(1)
    t1.kill()
    wait(lambda: not h.thread(t1.key)["bound"], what="detach after the harness process exits")
    assert h.thread(t1.key)["detached"] == "harness exited"
    fold = wait(lambda: [e for e in h.journal("fold") if e["pid"] == 1 and e["reason"] == "detach"], what="detach fold")
    after = h.session(1)
    for k in ("tail", "active_scope", "cold_transcript", "imports"):
        assert before.get(k) == after.get(k), k
    log(f"A3 detach on harness exit keeps the seat's session (tail, pointers unchanged); fold {fold[0]['result']}")
    t2 = Thread(h, harness="codex")
    r = t2.prompt("$unvrs:l1")
    assert "moved L1 into this thread" in ctx(r) and h.pid(1)["thread"] == t2.key, ctx(r)[:300]
    t2.stop()
    t3 = Thread(h)
    t3.prompt("$unvrs:l1")
    t3.stop()
    moves = h.journal("move")
    assert len(moves) == 2 and moves[-1]["from"] == t2.key and moves[-1]["to"] == t3.key, moves
    # A move while the old thread is mid-turn takes the seat at once; the old thread's
    # in-flight reply still lands in the seat's tail, labelled.
    t3.prompt("MIDTURN work in progress")
    t4 = Thread(h, harness="codex")
    t4.prompt("$unvrs:l1")
    t4.stop()
    t3.stop("MIDTURN-REPLY finished after the move")
    assert h.pid(1)["thread"] == t4.key
    tail = h.session(1)["tail"]
    assert any("MIDTURN-REPLY" in x and "previous thread" in x for x in tail), tail[-3:]
    assert [e for e in h.journal("move")][-1]["mid_turn"] is True
    log("A3 mid-turn move: the seat moves at once (journal mid_turn: true); the old thread's reply lands in the seat tail, labelled")
    t2.prompt("$unvrs:l1")
    t2.stop()
    t4.prompt("hi")  # notice consumed
    t3.prompt("hi")
    t5 = Thread(h, harness="codex")
    t5.prompt("$unvrs:l1")
    t5.stop()
    r = t2.prompt("what next?")
    assert "no longer an UNVRS seat" in ctx(r) and "no longer" in (r.get("systemMessage") or ""), r
    assert t2.prompt("and now?") is None, "notice must be delivered once, then the thread is untouched"
    log("A3 move: the old thread's next prompt gets the notice once (context + systemMessage), then nothing")
    r = t2.prompt("$unvrs:l1")
    assert h.pid(1)["thread"] == t2.key and "plan the release" in ctx(r), ctx(r)[:400]
    log("A3 $unvrs:l1 in the old thread takes L1 back with its tail")
    h.close()


def a4():
    h = Home("r4")
    (h.root / ".env").write_text("OPENAI_API_KEY=sk-live-PLANTED-SECRET-123456\n")
    t1 = Thread(h)
    t1.prompt("$unvrs:l1")
    t1.stop()
    t1.prompt("$unvrs:remember never deploy on Fridays")
    t1.stop()
    t1.turn("Step one of the migration; OPEN: waiting on DBA approval. key sk-live-PLANTED-SECRET-123456",
            "Started migration; artifact /tmp/migration.sql")
    for i in range(40):
        t1.turn(f"filler turn {i} " + "lorem ipsum " * 40, "ack " * 30)
    t1.end()
    wait(lambda: [e for e in h.journal("fold") if e["pid"] == 1], what="fold")
    quiet(h)
    assert h.session(1).get("brief", {}).get("open"), h.session(1).get("brief")
    body = h.hot(1)
    assert len(body.encode()) <= 9000, len(body)
    for part in ["## Brief", "## Captain memory", "never deploy on Fridays", "## Recent turns", "open:", "done:"]:
        assert part in body, part
    assert "PLANTED-SECRET" not in body
    assert any(e["reason"] == "tail threshold" and e["result"] == "applied" for e in h.journal("fold"))
    tc = Thread(h, harness="codex")
    c_codex = ctx(tc.prompt("$unvrs:l1"))
    tc.end()
    tl = Thread(h)
    c_claude = ctx(tl.prompt("$unvrs:l1"))
    strip = lambda c: c.split("\n", 1)[1]
    a_, b_ = strip(c_codex).split("## Recent turns")[0], strip(c_claude).split("## Recent turns")[0]
    if a_ != b_:
        import difflib
        log("\n".join(difflib.unified_diff(a_.splitlines(), b_.splitlines(), lineterm="")))
    assert a_ == b_, "payload differs between harnesses"
    assert "PLANTED-SECRET" not in c_codex + c_claude
    log(f"A4 rehydrate: brief, captain memory, tail, open/done; {len(body.encode())} bytes ≤ 9000; secret absent; same payload for codex and claude")
    h.close()


def a5_a11():
    h = Home("r5", extra_env={"FAKE_STEPS": "a,b,c,d", "FAKE_DELAY": "1.2"})
    (h.root / ".env").write_text("GITHUB_TOKEN=ghp_PLANTEDsecret000111\n")
    src = TMP / "r5-src"
    src.mkdir()
    (src / "README.md").write_text("fixture\n")
    assert captain(h, "sources", "add", "fx", "path", str(src), "--purpose", "fixture")["code"] == 0
    t1 = Thread(h)
    t1.prompt("$unvrs:l1")
    t1.prompt("$unvrs:project new rho The rho project --source fx")
    t1.stop()
    t2 = Thread(h, harness="codex")
    t2.prompt("$unvrs:l2 rho")
    t2.stop()
    t2.prompt("Ok, go")
    res, _ = t2.mcp('task --go "Ok, go" --done-when "All four items are reported" --intent "process items a b c d; artifact /tmp/big-report.txt" --spec "one item per step" --source fx')
    old = int(re.search(r"L3 PID (\d+)", res["content"][0]["text"]).group(1))
    wait(lambda: any(e["pid"] == old for e in h.journal("progress")), what="first progress")
    t2.turn("PARENT-TAIL-SECRET-WORD planning", "ok")
    l3 = h.hot(old)
    assert "## Task contract" in l3 and "## Task package" in l3 and "process items" in l3
    assert "## Recent turns" not in l3 and "PARENT-TAIL-SECRET-WORD" not in l3, l3
    r = captain(h, "quota-low", "--pid", str(old))
    assert r["code"] == 0, r
    hnd = wait(lambda: [e for e in h.journal("handoff") if e["from_pid"] == old], what="handoff")
    assert len(hnd) == 1
    new = hnd[0]["to_pid"]
    pkg_text = Path(hnd[0]["package"]).read_text()
    pkg = json.loads(pkg_text)
    assert len(pkg_text) <= 32000 and pkg["brief"]["open"], pkg["brief"]
    assert "ghp_PLANTED" not in pkg_text and "/tmp/big-report.txt" in pkg_text
    res = wait(lambda: [e for e in h.journal("result") if e["pid"] == new], timeout=40, what="result")[0]
    cpu = Path(h.env["FAKE_CPU_DIR"])
    receiver = [json.loads(p.read_text())["prompts"][0] for p in cpu.glob("*.json") if "HANDED OFF" in json.loads(p.read_text())["prompts"][0]]
    assert receiver and "continue from `next`" in receiver[0]
    turns = [e for e in h.journal("turn") if e["pid"] == new]
    first_tools = " ".join(turns[0]["tools"])
    assert not any(f"process {x}" in first_tools for x in "abcd" if any(f"item {x} done" in d for d in pkg["brief"]["done"])), first_tools
    log(f"A5 handoff PID {old} → {new} (claude → codex): one event, package {len(pkg_text)} bytes with every open item, pointer kept, secret absent; "
        f"receiver started at the next step ({first_tools}); result {res['how']}, delivered to the lead's wake queue")
    l1 = h.hot(1)
    assert "## Projects" in l1 and "rho" in l1 and "## Recent turns" in l1, l1
    t2_seat = next(p["pid"] for p in h.table()["pids"] if p.get("project") == "rho" and p["rank"] == 2)
    l2 = h.hot(t2_seat)
    assert "## Project memory" in l2 and "## Captain memory" not in l2, l2
    t1.ctl("send", "rho", "DELTA-NOTE from L1")
    d = ctx(t2.prompt("anything new?"))
    assert "DELTA-NOTE" in d and "## Brief" not in d and len(d) < 4000, d
    t2.stop()
    assert t2.prompt("and now?") is None
    log(f"A11 L3 hot set = contract + task package, no parent tail; L1 lists projects; L2 PID {t2_seat} delta = new wakes only ({len(d)} bytes), then nothing")
    h.close()


def a12():
    h = Home("r12")
    mode = Path(h.env["FOLD_CTL"])
    mode.write_text("")
    t1 = Thread(h)
    t1.prompt("$unvrs:l1")
    t1.stop()
    t1.turn("OPEN: first open item", "noted")
    t1.end()
    ev = wait(lambda: [e for e in h.journal("fold") if e["pid"] == 1 and e["reason"] == "detach" and e["result"] == "applied"], what="detach fold")
    brief = h.session(1)["brief"]
    assert any("first open item" in o for o in brief["open"]), brief
    mode.write_text("drop-always")
    t2 = Thread(h)
    t2.prompt("$unvrs:l1")
    t2.stop()
    t2.turn("more work; OPEN: second item", "ok")
    t2.end()
    rej = wait(lambda: [e for e in h.journal("fold") if e["result"] == "rejected"], what="rejected fold")
    time.sleep(1)
    assert h.session(1)["brief"] == brief
    mode.write_text("")
    log(f"A12 detach triggers a fold ({ev[0]['result']}, v{ev[0]['version']}); a fold dropping an open item is rejected "
        f"({len(rej)} rejections) and the previous brief stays")
    h.close()


CHECKS = {"A3": a3, "A4": a4, "A5": a5_a11, "A11": a5_a11, "A12": a12}

if __name__ == "__main__":
    sys.exit(run_checks(CHECKS, sys.argv[1:]))
