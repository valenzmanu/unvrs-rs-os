#!/usr/bin/env python3
"""A6 live: tell L1 something once in a Claude thread (the captain's `$unvrs:remember`,
and a fact the L1 model files itself through the MCP tool); a new L1 thread in Codex
answers both without being told. Real Claude and real `codex app-server`."""
import random
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from livelib import ClaudeThread, CodexThread, LiveHome, log  # noqa: E402


def main():
    boat = f"OSPREY-{random.randint(100, 999)}"
    editor = f"HELIX-{random.randint(100, 999)}"
    h = LiveHome("a6")
    ok = True
    try:
        a = ClaudeThread(h)
        a.ask("/unvrs:l1")
        r = a.ask(f"/unvrs:remember My boat is called {boat}.")
        log(f"claude A after /unvrs:remember: {r[:200]!r}")
        r = a.ask(f"Please remember for me, durably: my favourite editor is {editor}. Use your unvrs tool to file it, then confirm in one line.")
        tools = a.tools_used()
        log(f"claude A filed the second fact itself: {r[:160]!r} tools={[t for t in tools if 'unvrs' in t[0]]}")
        notes = [e for e in h.journal("remember")]
        log(f"journal remember: {[(e['note'], e['tier'], e['by']) for e in notes]}")
        assert any(e["by"] == "captain" for e in notes), notes
        assert any(str(e["by"]).startswith("pid:") for e in notes), "the L1 model did not file the fact through the MCP tool"
        a.close()
        b = CodexThread(h)
        r = b.ask("$unvrs:l1")
        log(f"codex B after $unvrs:l1: {r[:200]!r}")
        r = b.ask("What is my boat called, and what is my favourite editor? One line, no tools.")
        log(f"codex B answers: {r[:200]!r}")
        assert boat in r and editor in r, r
    except AssertionError as e:
        ok = False
        log(f"A6 FAIL: {e}")
    finally:
        h.close()
    log("A6 PASS" if ok else "A6 FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
