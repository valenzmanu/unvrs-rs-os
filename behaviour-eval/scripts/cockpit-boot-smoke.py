#!/usr/bin/env python3
"""A9 · cockpit Boot over a real PTY: the Ratatui UI is driven by `/` slash commands only.

Proves the cockpit (never the `unvrs boot|mission` CLI) refuses a bad mission out loud, admits a
good one, nests seats with the rules and jev pickers, installs skills on disk, applies the
interactive policy per seat rank (L1 keeps grill skills, an unfocused L2 and an L3 get them
stripped), reaches ready on an empty nest before any mail is dispatched, and blocks a seat when
Boot refuses. Screen text proves the captain sees it; the flight journal, the Obs boot journal and
the installed files are the hard assertions.

Run from the repo root: ./behaviour-eval/scripts/cockpit-boot-smoke.py
"""
import codecs
import fcntl
import json
import os
import pty
import re
import select
import struct
import subprocess
import termios
import time
from pathlib import Path

from smokelib import MISSIONS, POOLS, ROOT, Smoke, has_key

ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b[()][B0]|\x1b[=>]|\x1b\][^\x07]*\x07")
F10 = b"\x1b[21~"
TAB = "\t"


def api_key():
    """The repo-root .env key, tolerant of `export ` and of a space after `=`. Never printed."""
    env = ROOT / ".env"
    if os.environ.get("TYPESAFE_API_KEY", "").strip():
        return os.environ["TYPESAFE_API_KEY"].strip()
    for line in env.read_text().splitlines() if env.exists() else []:
        name, _, value = line.partition("=")
        if name.replace("export", "").strip() == "TYPESAFE_API_KEY":
            return value.strip().strip("\"'")
    return ""


class Cockpit:
    """One cockpit process on a 44x140 pseudo-terminal, typed at like a captain would."""

    def __init__(self, smoke, cwd, env=None):
        self.cwd = Path(cwd)
        (self.cwd / ".unvrs").mkdir(parents=True, exist_ok=True)
        merged = {
            **os.environ,
            "NO_COLOR": "1",
            "UNVRS_MISSIONS": str(MISSIONS),
            "UNVRS_POOLS": str(POOLS),
        }
        for name, value in (env or {}).items():
            merged.pop(name, None) if value is None else merged.__setitem__(name, value)
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 44, 140, 0, 0))
        self.proc = subprocess.Popen(
            [smoke.binary, "--demo", "--fresh"],
            stdin=slave, stdout=slave, stderr=slave, cwd=self.cwd, env=merged,
        )
        os.close(slave)
        self.raw = ""
        self.decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")
        self.pump(2)

    @property
    def screen(self):
        """Everything the cockpit has painted, ANSI stripped over the whole tape: escapes and
        multi-byte glyphs straddle read boundaries, so nothing is decoded chunk by chunk."""
        return ANSI.sub("", self.raw)

    def pump(self, seconds=0.4):
        """Drains the terminal; a full pipe would stall the cockpit."""
        until = time.monotonic() + seconds
        while time.monotonic() < until:
            if select.select([self.master], [], [], 0.05)[0]:
                try:
                    chunk = os.read(self.master, 65536)
                except OSError:
                    break
                if not chunk:
                    break
                self.raw += self.decoder.decode(chunk)
        return self.screen

    def type(self, text, settle=0.4):
        os.write(self.master, text.encode())
        self.pump(settle)

    def cmd(self, line):
        """A trailing space closes the slash popup, so Enter submits exactly what was typed."""
        self.type(line if " " in line else line + " ")
        self.type("\r")

    def saw(self, *tokens):
        """Ratatui repaints only changed cells: match short, freshly painted tokens."""
        return any(token in self.screen for token in tokens)

    def flight(self):
        lines = []
        for path in sorted(self.cwd.glob(".unvrs/flight-*.jsonl")):
            for line in path.read_text().splitlines():
                try:
                    lines.append(json.loads(line)["event"])
                except (json.JSONDecodeError, KeyError):
                    pass
        return lines

    def obs(self):
        path = self.cwd / ".unvrs/boot-journal.jsonl"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def mark(self):
        return len(self.flight())

    def wait(self, pred, seconds=10, since=0):
        """Polls the flight journal (Boot runs on a worker thread) and returns the match or None."""
        until = time.monotonic() + seconds
        while True:
            for line in self.flight()[since:]:
                if pred(line):
                    return line
            if time.monotonic() > until:
                return None
            self.pump(0.25)

    def quit(self):
        os.write(self.master, F10)
        try:
            return self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            return None

    def close(self):
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        os.close(self.master)


def ready(pid, backend=None):
    def match(line):
        return (
            line.startswith(f"NEST / PID {pid} / ")
            and line.endswith("/ ready")
            and (backend is None or f" / {backend} / nest [" in line)
        )

    return match


def nest_ids(line):
    """`NEST / … / nest [mattpocock/tdd] / …` → `['tdd']`."""
    found = re.search(r"nest \[(.*?)\]", line or "")
    ids = [part.strip().rsplit("/", 1)[-1] for part in (found.group(1) if found else "").split(",")]
    return [skill for skill in ids if skill]


def stripped_count(line):
    found = re.search(r"stripped (\d+)", line or "")
    return int(found.group(1)) if found else -1


def pick_rows(cockpit):
    return [row for row in cockpit.obs() if row.get("kind") == "boot.pick"]


start = time.monotonic()
s = Smoke("cockpit-boot-smoke")
catalog = {entry["id"] for entry in s.catalog()}
keyed = has_key()
s.note(f"catalog {len(catalog)} skills · TYPESAFE_API_KEY {'present' if keyed else 'absent'}")
# The cockpit runs in a scratch cwd, so it never finds the repo .env by itself.
key_env = {"TYPESAFE_API_KEY": api_key() if keyed else None}

flight = Cockpit(s, s.tmp / "flight", key_env)
try:
    s.check("cockpit: rendered on the PTY", "UNVRS" in flight.screen, flight.screen[-160:])

    # 1 · admit refuses a mission with no Done check, loudly.
    at = flight.mark()
    flight.cmd("/admit bad-no-done-check")
    line = flight.wait(lambda e: e.startswith("ADMIT REFUSED"), 5, at)
    s.check("1 admit-refuse: flight journal names the Done check", bool(line) and "Done check" in line, str(line))
    s.check("1 admit-refuse: screen shows it", flight.saw("ADMIT REFUSED"), flight.screen[-160:])
    registry = flight.cwd / ".unvrs/missions.json"
    s.check(
        "1 admit-refuse: nothing registered",
        not registry.exists() or "msn_bad_no_done_check" not in registry.read_text(),
        registry.read_text()[:200] if registry.exists() else "no registry",
    )

    # 2 · admit the coding mission.
    at = flight.mark()
    flight.cmd("/admit coding-tdd")
    line = flight.wait(lambda e: e.startswith("MISSION ADMITTED / msn_coding_tdd"), 5, at)
    s.check("2 admit: flight journal has MISSION ADMITTED", bool(line), str(line))
    s.check("2 admit: header strip names the mission", flight.saw("MISSION msn_coding_tdd"), flight.screen[-160:])

    # 3 · rules pick on the L1 captain seat, installed on disk.
    at = flight.mark()
    flight.cmd("/picker rules")
    flight.cmd("/boot")
    line = flight.wait(ready(1, "rules"), 10, at)
    s.check("3 rules: PID 1 nested ready with the rules picker", bool(line), str(flight.flight()[at:]))
    picked = nest_ids(line)
    s.check("3 rules: nest is 1..3 skills from the catalogue", 1 <= len(picked) <= 3 and set(picked) <= catalog, str(picked))
    entry = flight.cwd / ".pi/skills/tdd/SKILL.md"
    s.check("3 rules: tdd installed on disk", entry.is_file() and entry.stat().st_size > 0, str(entry))
    s.check("3 rules: screen shows the Boot chip", flight.saw("BOOT msn_coding_tdd"), flight.screen[-160:])

    # 4 · jev pick, or the recorded fallback to rules when there is no key.
    at, picks = flight.mark(), len(pick_rows(flight))
    flight.cmd("/picker jev")
    flight.cmd("/boot")
    line = flight.wait(ready(1), 30, at)
    row = pick_rows(flight)[picks:][-1] if len(pick_rows(flight)) > picks else {}
    s.check("4 jev: PID 1 nested ready again", bool(line), str(flight.flight()[at:]))
    s.check("4 jev: boot.pick requested jev", row.get("backend_requested") == "jev", str(row)[:200])
    if keyed:
        s.note("4 jev: key branch (live TypeSafe call)")
        s.check("4 jev: backend_used is jev", row.get("backend_used") == "jev", str(row.get("fallback"))[:200])
        s.check(
            "4 jev: numeric noul and confidence",
            isinstance(row.get("noul"), (int, float)) and isinstance(row.get("confidence"), (int, float)),
            f"noul={row.get('noul')} confidence={row.get('confidence')}",
        )
        s.check("4 jev: nest capped at 3", len(row.get("selected", [])) <= 3, str(row.get("selected")))
    else:
        s.note("4 jev: keyless branch (fallback to rules)")
        s.check("4 jev: fell back to rules with a reason", row.get("backend_used") == "rules" and bool(row.get("fallback")), str(row)[:200])
        s.check("4 jev: chip names the fallback", flight.saw("fallback from jev", "fallback from"), flight.screen[-160:])

    # 5 · interactive policy · an L1 captain seat keeps the grill skills.
    at = flight.mark()
    flight.cmd("/picker rules")
    flight.cmd("/admit interactive-grill")
    flight.wait(lambda e: e.startswith("MISSION ADMITTED / msn_interactive_grill"), 5, at)
    at = flight.mark()
    flight.cmd("/boot")
    line = flight.wait(ready(1, "rules"), 10, at)
    picked = nest_ids(line)
    s.check(
        "5 L1: keeps an interactive skill, strips nothing",
        bool(line) and any("grill" in skill for skill in picked) and stripped_count(line) == 0,
        str(line),
    )

    # 6 · the same mission on an unfocused L2: no captain channel, grill skills withheld.
    at, picks = flight.mark(), len(pick_rows(flight))
    flight.cmd("/boot 2")
    line = flight.wait(ready(2), 10, at)
    row = pick_rows(flight)[picks:][-1] if len(pick_rows(flight)) > picks else {}
    s.check(
        "6 L2 unfocused: no grill skill, stripped > 0",
        bool(line) and not any("grill" in skill for skill in nest_ids(line)) and stripped_count(line) > 0,
        str(line),
    )
    s.check(
        "6 L2 unfocused: boot.pick is rank 2 with no captain channel",
        row.get("seat", {}).get("rank") == 2 and row.get("captain_channel") is False,
        str(row)[:200],
    )

    # 7 · an L3 worker under SCOUT never gets a captain channel, focused or not.
    flight.type(TAB)
    at, picks = flight.mark(), len(pick_rows(flight))
    flight.cmd("/spawn probe the hull")
    line = flight.wait(ready(4), 15, at)
    row = pick_rows(flight)[picks:][-1] if len(pick_rows(flight)) > picks else {}
    s.check(
        "7 L3: spawned PID 4 nested with no grill skill, stripped > 0",
        bool(line) and not any("grill" in skill for skill in nest_ids(line)) and stripped_count(line) > 0,
        str(line),
    )
    s.check("7 L3: boot.pick is rank 3", row.get("seat", {}).get("rank") == 3, str(row)[:200])
    s.check("7 L3: screen says stripped", flight.saw("stripped"), flight.screen[-160:])

    # 8 · an empty nest still reaches ready, and gates the seat's mail until it does.
    at = flight.mark()
    flight.cmd("/admit no-skill-needed")
    flight.wait(lambda e: e.startswith("MISSION ADMITTED / msn_no_skill_needed"), 5, at)
    at = flight.mark()
    flight.cmd("/launch")
    line = flight.wait(lambda e: e.startswith("NEST / PID 5 /") and e.endswith("/ ready"), 15, at)
    s.check("8 empty: PID 5 nested ready with an empty nest", bool(line) and nest_ids(line) == [], str(line))
    working = flight.wait(lambda e: e == "PID 5 / working", 10, at)
    events = flight.flight()[at:]
    s.check(
        "8 empty: nest gates the mail (ready before working)",
        bool(working) and bool(line) and events.index(line) < events.index(working),
        f"{line} | {working}",
    )
    s.check("8 empty: screen says the nest is empty", flight.saw("nest empty", "empty (no skill"), flight.screen[-160:])

    # 9 · F10 docks the flight.
    s.check("9 quit: F10 exits 0", flight.quit() == 0, str(flight.proc.returncode))
finally:
    flight.close()

# 10 · a second flight where Boot must refuse: jev requested, no key, on_error = refuse.
refuse = Cockpit(s, s.tmp / "refuse", {"TYPESAFE_API_KEY": None})
try:
    (refuse.cwd / ".unvrs/boot.toml").write_text('[boot]\non_error = "refuse"\n')
    at = refuse.mark()
    refuse.cmd("/admit coding-tdd")
    refuse.wait(lambda e: e.startswith("MISSION ADMITTED / msn_coding_tdd"), 5, at)
    at = refuse.mark()
    refuse.cmd("/picker jev")
    refuse.cmd("/boot")
    line = refuse.wait(lambda e: e.startswith("NEST / PID 1 /") and "REFUSED at pick" in e, 15, at)
    s.check("10 refuse: flight journal records REFUSED at pick", bool(line), str(refuse.flight()[at:]))
    s.check("10 refuse: screen shows the refusal and the attention", refuse.saw("REFUSED at") and refuse.saw("ATTENTION"), refuse.screen[-160:])
    refused = [row for row in refuse.obs() if row.get("kind") == "boot.refused"]
    s.check(
        "10 refuse: boot.refused row at stage pick",
        any(row.get("stage") == "pick" for row in refused),
        str(refused)[:300],
    )
    s.check("10 refuse: nothing installed", not (refuse.cwd / ".pi").exists())
    s.check("10 refuse: F10 exits 0", refuse.quit() == 0, str(refuse.proc.returncode))
finally:
    refuse.close()

s.note(f"cockpit only · no boot|mission subcommand was invoked · {time.monotonic() - start:.0f}s")
s.finish()
