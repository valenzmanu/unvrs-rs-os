#!/usr/bin/env python3
"""0.8 scripted end-to-end checks against the real `unvrs` binary and kernel daemon,
without model calls: A2 unbound threads untouched, A5 proposal → hold → captain
approval (a model's call refused), A7 context sources (bounded, versioned, scoped,
journaled), A8f the report-task path with fixture CPUs (task → wake → detached L2 run →
L1 digest), A9 decisions in every digest until answered in the captain's exact words,
A10 the Observatory snapshot and page (Host guard, GET only, six sections).

usage: crew-e2e.py [A2 A5 A7 A8f A9 A10]   (default: all)
"""
import json
import os
import re
import statistics
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from crewlib import BIN, TMP, Home, Thread, ctx, log, reason, run_checks, wait  # noqa: E402


def captain_ctl(home, *args):
    """The captain's own terminal: `unvrs ctl …` in a process with no harness among its
    ancestors (double fork to launchd), exactly as from Terminal."""
    out = TMP / f"captain-{time.time_ns()}.json"
    code = f'''
import os, subprocess, json, sys
if os.fork(): os._exit(0)
os.setsid()
if os.fork(): os._exit(0)
r = subprocess.run({[str(BIN), "ctl", *args]!r}, capture_output=True, text=True)
open({str(out)!r} + ".tmp", "w").write(json.dumps({{"code": r.returncode, "out": r.stdout, "err": r.stderr}}))
os.rename({str(out)!r} + ".tmp", {str(out)!r})
'''
    subprocess.run([sys.executable, "-c", code], env=home.env, check=True)
    wait(lambda: out.exists(), timeout=30, what="captain terminal call")
    return json.loads(out.read_text())


def fixture_repo(name):
    d = TMP / name
    (d / "brand").mkdir(parents=True)
    (d / "media").mkdir()
    (d / "CONTEXT.md").write_text(f"# {name}\nStart here: brand/voice.md describes the voice.\n")
    (d / "brand/voice.md").write_text("# Voice\n\nOSPREY-VOICE: warm, short sentences, no jargon.\n")
    (d / "media/clip.md").write_text("OSPREY-VOICE appears in media but media is excluded.\n")
    (d / "big.md").write_text("".join(f"line {i} OSPREY-BIG filler text to make this file large\n" for i in range(3000)))
    for c in (["git", "init", "-q"], ["git", "add", "-A"], ["git", "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "init"]):
        subprocess.run(c, cwd=d, check=True)
    return d


def a2():
    h = Home("a2")
    t = Thread(h, cwd="/tmp")
    times = []
    for ev, extra in [("SessionStart", {"source": "startup"}), ("UserPromptSubmit", {"prompt": "hello there"}),
                      ("Stop", {"last_assistant_message": "hi", "stop_hook_active": False}),
                      ("PreCompact", {}), ("SessionEnd", {"reason": "other"})]:
        t0 = time.time()
        r = t.hook_raw(ev, **extra)
        times.append(time.time() - t0)
        assert r["code"] == 0 and r["out"] == "" and r["err"] == "", (ev, r)
    assert not (h.root / "kernel").exists(), "an unbound thread started or wrote the kernel"
    log(f"A2 unbound thread, no kernel: 5 hooks exit 0, no output, nothing written; median {statistics.median(times) * 1000:.0f} ms per hook (incl. harness IPC)")
    # With a live L1 elsewhere, an unbound thread still changes nothing.
    l1 = Thread(h, cwd="/tmp")
    assert "L1" in ctx(l1.prompt("$unvrs:l1"))
    before = (h.table(), len(h.journal()))
    for cwd in ["/tmp", str(Path.home()), str(Path(__file__).resolve().parents[2])]:
        u = Thread(h, harness="codex", cwd=cwd)
        for ev, extra in [("SessionStart", {"source": "startup"}), ("UserPromptSubmit", {"prompt": "do some work"}),
                          ("Stop", {"last_assistant_message": "done", "stop_hook_active": False}), ("SessionEnd", {"reason": "other"})]:
            r = u.hook_raw(ev, **extra)
            assert r["code"] == 0 and r["out"] == "", (cwd, ev, r)
    after = (h.table(), len(h.journal()))
    strip = lambda t: {k: v for k, v in t.items() if k != "kernel"}
    assert strip(before[0]) == strip(after[0]) and before[1] == after[1], "kernel state changed for unbound threads"
    res, tools = u.mcp("digest")
    assert res.get("isError") and "not an UNVRS seat" in res["content"][0]["text"], res
    assert [t["name"] for t in tools] == ["unvrs"] and len(tools[0]["description"]) < 300, tools
    r = u.prompt("$unvrs:tree")
    assert "UNVRS crew" in reason(r) and h.thread(u.key) is None, (r, h.thread(u.key))
    log("A2 with L1 live: unbound threads in /tmp, ~ and the repo leave state and journal unchanged; their MCP tool refuses; "
        "an explicit $unvrs:tree answers without binding the thread")
    h.close()


def a5():
    h = Home("a5")
    repo = fixture_repo("a5-src")
    r = captain_ctl(h, "sources", "add", "demo-src", "path", str(repo), "--purpose", "demo material", "--exclude", "media/**")
    assert r["code"] == 0, r
    t1 = Thread(h)
    t1.prompt("$unvrs:l1")
    res, _ = t1.mcp("project propose demo 'A demo project for the gate' --source demo-src")
    assert not res.get("isError"), res
    hold = re.search(r"\b(d\d+)\b", res["content"][0]["text"]).group(1)
    assert not (h.root / "projects/demo").exists()
    assert any(c["id"] == hold for c in h.snapshot()["calls"])
    res, _ = t1.mcp(f"approve {hold}")
    assert res.get("isError") and "only the captain" in res["content"][0]["text"], res
    r = t1.ctl("approve", hold)
    assert r["code"] != 0 and "only the captain" in r["err"], r
    other = Thread(h, harness="codex")
    res, _ = other.mcp(f"approve {hold}")
    assert res.get("isError"), res
    refused = [e for e in h.journal("refused") if e["verb"] == "approve"]
    assert len(refused) >= 2 and not (h.root / "projects/demo").exists(), refused
    log(f"A5 L1 proposed demo → held as {hold}; the same approval from the model's MCP tool and its shell refused ({len(refused)} journaled); project absent")
    r = t1.prompt(f"$unvrs:approve {hold}")
    assert "Approved" in ctx(r) and (h.root / "projects/demo/project.toml").exists(), r
    seat = [p for p in h.table()["pids"] if p.get("project") == "demo" and p["rank"] == 2]
    assert seat, h.table()["pids"]
    t1.stop()
    lst = reason(Thread(h).prompt("$unvrs:l2"))
    assert "demo" in lst, lst
    log(f"A5 captain typed $unvrs:approve {hold} → project demo and its L2 seat PID {seat[0]['pid']} exist; $unvrs:l2 lists it")
    h.close()


def a7():
    h = Home("a7")
    ra, rb = fixture_repo("a7-alpha"), fixture_repo("a7-beta")
    for sid, repo in [("srca", ra), ("srcb", rb)]:
        r = captain_ctl(h, "sources", "add", sid, "path", str(repo), "--purpose", f"{sid} material", "--exclude", "media/**")
        assert r["code"] == 0, r
    l1 = Thread(h)
    l1.prompt("$unvrs:l1")
    for pid_, src in [("alpha", "srca"), ("beta", "srcb")]:
        r = l1.prompt(f"$unvrs:project new {pid_} Project {pid_} --source {src}")
        assert "created" in ctx(r), r
    a = Thread(h, harness="codex")
    c = ctx(a.prompt("$unvrs:l2 alpha"))
    assert "L2 lead of the alpha project" in c and "srca" in c and "srcb" not in c, c
    res, _ = a.mcp('ctx search "OSPREY-VOICE"')
    text = res["content"][0]["text"]
    commit = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ra, capture_output=True, text=True).stdout.strip()[:12]
    assert not res.get("isError") and f"ctx://srca/brand/voice.md@{commit}" in text and "media/" not in text, text
    ref = re.search(r"(ctx://srca/brand/voice\.md@\S+?)#", text).group(1)
    res, _ = a.mcp(f"ctx get {ref}")
    assert "warm, short sentences" in res["content"][0]["text"], res
    res, _ = a.mcp("ctx get ctx://srca/big.md")
    big = res["content"][0]["text"]
    m = re.search(r"\((\d+) bytes\)", big)
    assert m and int(m.group(1)) <= 12000 and "next: ctx://srca/big.md" in big, big[:300]
    res, _ = a.mcp("ctx map srca")
    assert "Start here" in res["content"][0]["text"], res
    log(f"A7 L2 alpha: search → {ref} (commit-versioned, media/** excluded); get returns the text; big file bounded to {m.group(1)} bytes with a next ref; map = CONTEXT.md")
    b = Thread(h)
    b.prompt("$unvrs:l2 beta")
    for cmd in ['ctx search "OSPREY-VOICE" --source srca', f"ctx get {ref}", "ctx map srca"]:
        res, _ = b.mcp(cmd)
        assert res.get("isError") and "Refused" in res["content"][0]["text"], (cmd, res)
    res, _ = b.mcp('ctx search "OSPREY-VOICE"')
    assert "srcb" in res["content"][0]["text"] and "srca" not in res["content"][0]["text"], res
    res, _ = l1.mcp('ctx search "OSPREY-VOICE"')
    assert "srca" in res["content"][0]["text"] and "srcb" in res["content"][0]["text"], res
    (ra / "brand/voice.md").write_text("# Voice\n\nOSPREY-VOICE: changed.\n")
    subprocess.run(["git", "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qam", "change"], cwd=ra, check=True)
    res, _ = a.mcp(f"ctx get {ref}")
    assert "stale" in res["content"][0]["text"] and "changed" in res["content"][0]["text"], res
    reads = h.journal("ctx.read")
    refusals = h.journal("ctx.refused")
    assert len(reads) >= 6 and all({"pid", "source", "ref", "bytes"} <= set(e) for e in reads) and len(refusals) >= 3
    assert h.snapshot()["context_reads"], "snapshot has no context reads"
    log(f"A7 L2 beta refused on srca (search, get, map; {len(refusals)} refusals journaled) and sees only srcb; L1 sees both; "
        f"a new commit marks the old ref stale; {len(reads)} reads journaled with pid, source, ref, bytes")
    h.close()


def a8f():
    h = Home("a8f", extra_env={"FAKE_STEPS": "a,b", "FAKE_DELAY": "0.4"})
    repo = fixture_repo("a8f-src")
    assert captain_ctl(h, "sources", "add", "docs", "path", str(repo), "--purpose", "docs")["code"] == 0
    l1 = Thread(h)
    l1.prompt("$unvrs:l1")
    l1.prompt("$unvrs:project new gamma Gamma project --source docs")
    l1.stop()
    l2 = Thread(h, harness="codex")
    l2.prompt("$unvrs:l2 gamma")
    l2.prompt("Ok, go")
    res, _ = l2.mcp('task --go "Ok, go" --done-when "Voice rules reported with refs" --intent "find the voice rules" --spec "report the voice rules with refs" --shape act --source docs')
    assert res.get("isError") and "workspaces (0.9)" in res["content"][0]["text"], res
    res, _ = l1.mcp('task --project gamma --intent "x" --spec "y" --done-when "Report returned"')
    assert res.get("isError") and "No captain go found" in res["content"][0]["text"], res
    res, _ = l2.mcp('task --go "Ok, go" --done-when "Requested report returned" --intent "find the voice rules" --spec "report the voice rules with refs" --shape report --source docs')
    assert not res.get("isError"), res
    worker = int(re.search(r"L3 PID (\d+)", res["content"][0]["text"]).group(1))
    l2.stop()
    seat = h.table()
    gamma = next(p for p in seat["pids"] if p.get("project") == "gamma" and p["rank"] == 2)["pid"]
    # The seat is mid-turn when the result lands: the Stop hook keeps the turn going once.
    l2.prompt("keep planning while the research runs")
    wait(lambda: [e for e in h.journal("result") if e["pid"] == worker], timeout=40, what="L3 result")
    pkg = json.loads((h.root / f"projects/gamma/tasks/pid-{worker}/result.json").read_text())
    assert pkg["contract"]["intent"] == "find the voice rules" and pkg["how"] == "done", pkg
    w = [e for e in h.journal("wake") if e["pid"] == gamma and e["wake_kind"] == "done"]
    assert w, "no wake on the L2"
    out = l2.hook("Stop", last_assistant_message="planned", stop_hook_active=False)
    assert out and out.get("decision") == "block" and "find the voice rules" in out.get("reason", ""), out
    again = l2.hook("Stop", last_assistant_message="summarized the report", stop_hook_active=True)
    assert again is None, again
    acks = [e for e in h.journal("wake.ack") if e["pid"] == gamma]
    assert acks, "wake not acknowledged after the handling turn"
    assert ctx(l2.prompt("anything new?")) == "", "a handled wake came back"
    l2.stop()
    log(f"A8f L2 gamma dispatched L3 PID {worker} (report; act refused, L1 without go refused); result package {len(json.dumps(pkg))} bytes; "
        "the result arrived mid-turn: Stop re-woke the turn once with it (decision block), the next Stop acked it")
    # Delivery at the next prompt when the seat is idle.
    res, _ = l2.mcp('task --go "Ok, go" --done-when "Requested report returned" --intent "idle delivery" --spec "anything" --source docs')
    w1 = int(re.search(r"L3 PID (\d+)", res["content"][0]["text"]).group(1))
    wait(lambda: [e for e in h.journal("result") if e["pid"] == w1], timeout=40, what="idle result")
    d = ctx(l2.prompt("anything new?"))
    assert "idle delivery" in d, d
    l2.stop("noted")
    log("A8f idle seat: the next wake was delivered at its next prompt and acked at Stop")
    # Detached: the L2 thread closes; a second task finishes; the kernel runs the L2 on its own.
    res, _ = l2.mcp('task --go "Ok, go" --done-when "Requested report returned" --intent "second pass" --spec "count the files" --source docs')
    w2 = int(re.search(r"L3 PID (\d+)", res["content"][0]["text"]).group(1))
    l2.end()
    l2.kill()
    wait(lambda: [e for e in h.journal("result") if e["pid"] == w2], timeout=40, what="second result")
    out = wait(lambda: [e for e in h.journal("outcome") if e["pid"] == gamma], timeout=40, what="detached L2 run")
    runs = [e for e in h.journal("seat.run") if e["pid"] == gamma]
    dg = reason(Thread(h).prompt("$unvrs:digest"))
    assert "gamma" in dg and "lead handled" in dg, dg
    l1w = [e for e in h.journal("wake") if e["pid"] == 1 and e["wake_kind"] == "outcome"]
    assert l1w, "L1 not woken with the outcome"
    log(f"A8f with the L2 detached the kernel ran it ({len(runs)} seat.run events); outcome journaled and in L1's wake queue; digest shows: "
        + next(l for l in dg.splitlines() if "lead handled" in l)[:120])
    h.close()


def a9():
    h = Home("a9")
    l1 = Thread(h)
    l1.prompt("$unvrs:l1")
    l1.prompt("$unvrs:project new delta The delta project")
    l1.stop()
    l2 = Thread(h, harness="codex")
    l2.prompt("$unvrs:l2 delta")
    l2.stop()
    res, _ = l2.mcp('decide "Ship the delta brochure this week?" --option yes --option no')
    hold = re.search(r"\b(d\d+)\b", res["content"][0]["text"]).group(1)
    digests = [reason(Thread(h).prompt("$unvrs:digest")) for _ in range(2)] + [ctx(l1.prompt("$unvrs:digest"))]
    l1.stop()
    assert all(f"{hold} (decision, delta" in d and "Ship the delta brochure" in d for d in digests), digests
    words = "yes, but only the short version — no prices"
    r = Thread(h).prompt(f"$unvrs:answer {hold} {words}")
    assert words in reason(r), r
    closed = [e for e in h.journal("hold.close") if e["hold"] == hold]
    assert closed and closed[0]["words"] == words and closed[0]["status"] == "answered", closed
    d = ctx(l2.prompt("any news?"))
    assert words in d, d
    l2.stop()
    after = reason(Thread(h).prompt("$unvrs:digest"))
    assert "Ship the delta brochure" not in after.split("Delivered")[0] and "nothing needs you" in after, after
    assert f"decided {hold}" in after, after
    log(f"A9 {hold} shown in 3 digests (two unbound threads, L1); answered with the exact words \"{words}\"; the asking L2 got them as a wake; "
        "gone from Your calls, listed under Delivered")
    h.close()


def a10():
    h = Home("a10", extra_env={"FAKE_STEPS": "a,b,c,d,e,f", "FAKE_DELAY": "3"})
    repo = fixture_repo("a10-src")
    assert captain_ctl(h, "sources", "add", "src", "path", str(repo), "--purpose", "material")["code"] == 0
    l1 = Thread(h)
    l1.prompt("$unvrs:l1")
    l1.turn("remember the codeword; OPEN: pick a title", "ok")
    l1.prompt("$unvrs:project new eps Epsilon project --source src")
    l1.stop()
    l2 = Thread(h, harness="codex")
    l2.prompt("$unvrs:l2 eps")
    l2.stop()
    l2.mcp('decide "Which logo?" --option round --option square')
    l2.mcp('ctx search "OSPREY-VOICE"')
    l2.prompt("Ok, go")
    l2.mcp('task --go "Ok, go" --done-when "Requested report returned" --intent "long research" --spec "take your time" --source src')
    x = Thread(h, harness="codex")
    x.prompt("$unvrs:l1")
    wait(lambda: h.snapshot()["workers"], what="a working L3")
    base = f"http://127.0.0.1:{h.port}"

    def get(path, host="unvrs.localhost", method="GET"):
        req = urllib.request.Request(base + path, method=method, data=b"{}" if method != "GET" else None,
                                     headers={"Host": host})
        try:
            with urllib.request.urlopen(req, timeout=5) as r:
                return r.status, r.read().decode()
        except urllib.error.HTTPError as e:
            return e.code, ""

    st, page = wait(lambda: (lambda r: r if r[0] == 200 else None)(get("/")), what="observatory up")
    assert 'name="viewport"' in page and "width=device-width" in page
    st, static = get("/static")
    for section in ["Your calls", "Seats", "Working now", "Quota", "Moves", "Recent"]:
        assert section.upper() in static.upper(), section
    for needle in ["Which logo?", "long research", "eps"]:
        assert needle in static, needle
    for host in ["localhost", "127.0.0.1"]:
        assert get("/", host)[0] == 200, host
    bad = [get("/", "evil.example")[0], get("/api/snapshot", "unvrs.localhost.evil.example")[0]]
    assert all(c in (400, 403, 421) for c in bad), bad
    mut = [get(p, method=m)[0] for p in ["/", "/api/snapshot"] for m in ["POST", "PUT", "DELETE"]]
    assert all(c in (404, 405) for c in mut), mut
    st, body = get("/api/snapshot")
    s = json.loads(body)
    keys = {"at", "kernel", "calls", "seats", "workers", "quota", "moves", "recent", "context_reads", "projects"}
    assert keys <= set(s), set(s)
    assert {"version", "os_pid", "uptime_s", "home"} <= set(s["kernel"])
    assert s["calls"] and {"id", "kind", "question", "options", "age_s", "project"} <= set(s["calls"][0])
    seat1 = next(z for z in s["seats"] if z["rank"] == 1)
    ob = seat1["occupied_by"]
    assert ob and {"app", "harness", "thread_id", "title", "link"} <= set(ob) and ob["harness"] == "codex" and ob["link"] == f"codex://threads/{x.session}", seat1
    assert any(z["rank"] == 2 and z["project"] == "eps" for z in s["seats"])
    wk = s["workers"][0]
    assert {"pid", "project", "intent", "shape", "harness", "model", "elapsed_s", "state"} <= set(wk) and wk["intent"] == "long research", wk
    assert all(q["remaining_pct"] is None for q in s["quota"]) and {"claude", "codex"} <= {q["harness"] for q in s["quota"]}
    mv = s["moves"][0]
    assert mv["kind"] in ("swap", "move") and mv["pid"] == 1 and mv["bytes"] > 0, mv
    assert s["recent"] and s["context_reads"] and s["projects"][0]["id"] == "eps"
    shot = ""
    chrome = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
    if os.path.exists(chrome):
        png = TMP / "obs-420.png"
        subprocess.run([chrome, "--headless=new", "--disable-gpu", f"--screenshot={png}", "--window-size=420,900",
                        f"--host-resolver-rules=MAP unvrs.localhost 127.0.0.1", f"http://unvrs.localhost:{h.port}/"],
                       capture_output=True, timeout=60)
        if png.exists() and png.stat().st_size > 5000:
            shot = f"; headless Chrome rendered it at 420 px ({png.stat().st_size} bytes PNG)"
    log(f"A10 Observatory: six sections and the snapshot schema from a seeded home (call, L1 moved to codex with link, L2, working L3, "
        f"unknown quota as null, move {mv['kind']} {mv['bytes']} bytes, recent, context reads); Host guard {bad}; mutations {sorted(set(mut))}{shot}")
    h.close()


CHECKS = {"A2": a2, "A5": a5, "A7": a7, "A8f": a8f, "A9": a9, "A10": a10}

if __name__ == "__main__":
    sys.exit(run_checks(CHECKS, sys.argv[1:]))
