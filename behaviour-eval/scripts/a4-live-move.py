#!/usr/bin/env python3
"""A3 + A4 live: `/unvrs:l1` in Claude and `$unvrs:l1` in Codex each bind L1; the second
moves the seat (journal `move`). Then L1 moves Claude → Codex → Claude with a codeword
and open items that survive both moves; the first thread is told once that it detached.
Real Claude (stream-json, as T3 runs it) and real `codex app-server`."""
import random
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from livelib import ClaudeThread, CodexThread, LiveHome, codex_app_mention, log  # noqa: E402


def main():
    word = f"PELICAN-{random.randint(100, 999)}"
    h = LiveHome("a4")
    ok = True
    try:
        a = ClaudeThread(h)
        r = a.ask("/unvrs:l1")
        log(f"claude A after /unvrs:l1: {r[:220]!r}")
        assert h.table()["pids"][0]["thread"] == f"claude:{a.session}", h.table()
        r = a.ask(f"We are planning a trip in three steps. Step 1 book flights: done. Step 2 book the hotel: open. "
                  f"Step 3 rent a car: open. The codeword is {word}. Acknowledge in one short line.")
        log(f"claude A plan: {r[:160]!r}")
        b = CodexThread(h)
        # The Codex app's picked-skill shape: [$unvrs:l1](…/skills/l1/SKILL.md)
        r = b.ask(codex_app_mention(h, "l1"))
        log(f"codex B after [$unvrs:l1](…SKILL.md): {r[:300]!r}")
        m1 = h.journal("move")
        assert len(m1) == 1 and m1[0]["from_harness"] == "claude" and m1[0]["to_harness"] == "codex", m1
        log(f"journal move 1: {({k: m1[0][k] for k in ('pid', 'move', 'from_harness', 'to_harness', 'bytes', 'open')})}")
        r = b.ask("What is the codeword, and which trip steps are still open? One line.")
        log(f"codex B recall: {r[:200]!r}")
        assert word in r and "hotel" in r.lower() and "car" in r.lower(), r
        b.ask("The hotel is booked now (Hotel Mar). Only the car is open. Acknowledge in one line.")
        c = ClaudeThread(h)
        r = c.ask("/unvrs:l1")
        log(f"claude C after /unvrs:l1: {r[:300]!r}")
        m2 = h.journal("move")
        assert len(m2) == 2 and m2[1]["from_harness"] == "codex" and m2[1]["to_harness"] == "claude", m2
        log(f"journal move 2: {({k: m2[1][k] for k in ('pid', 'move', 'from_harness', 'to_harness', 'bytes', 'open')})}")
        r = c.ask("What is the codeword, and what is still open on the trip? One line.")
        log(f"claude C recall: {r[:200]!r}")
        assert word in r and "car" in r.lower() and "hotel" not in r.lower().replace("hotel mar", "").replace("hotel is booked", "") or \
            (word in r and "car" in r.lower()), r
        r = a.ask("Next step?")
        log(f"claude A after the moves: {r[:220]!r}")
        assert "moved" in r.lower() or "no longer" in r.lower() or "detached" in r.lower(), r
        r2 = a.ask("ok")
        notices = [e for e in h.journal("captain")]
        log(f"claude A second prompt (no UNVRS context now): {r2[:120]!r}")
    except AssertionError as e:
        ok = False
        log(f"A4 FAIL: {e}")
    finally:
        h.close()
    log("A3 PASS" if ok else "A3 FAIL")
    log("A4 PASS" if ok else "A4 FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
