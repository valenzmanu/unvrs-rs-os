#!/usr/bin/env python3
"""Draws the README diagrams in two themes (dark, light).

    python3 docs/assets/readme/src/gen.py      # writes docs/assets/readme/*.svg

Plain SVG, no dependencies, no external fonts or images, so GitHub renders the files as
they are. Palette and type: ink, paper, lime, orange, halftone dots, tracked
uppercase labels. Every claim drawn here comes from docs/design.
"""

from __future__ import annotations

import math
import os
from html import escape

OUT = os.path.normpath(os.path.join(os.path.dirname(__file__), ".."))

SANS = "'Helvetica Neue', Helvetica, Inter, Arial, sans-serif"
MONO = "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace"

THEMES = {
    "dark": dict(
        bg="#0B0B0B", panel="#131313", fg="#ECECE4", mute="#8A8A8A", line="#3A3A3A",
        grid="#1E1E1E", acc="#E8F55A", acc_t="#E8F55A", org="#FA5A00", org_t="#FA5A00",
        on_acc="#0B0B0B", on_org="#0B0B0B", dot="#E8F55A", acc_s="#E8F55A",
    ),
    "light": dict(
        bg="#ECECE4", panel="#E3E3D9", fg="#101010", mute="#5C5C55", line="#B9B9AE",
        grid="#D4D4C9", acc="#E8F55A", acc_t="#5A6200", org="#FA5A00", org_t="#B84300",
        on_acc="#101010", on_org="#101010", dot="#101010", acc_s="#8C9600",
    ),
}


class S:
    """One SVG document in one theme."""

    def __init__(self, w: int, h: int, t: dict, grid: bool = True):
        self.w, self.h, self.t, self.parts = w, h, t, []
        self.add(f'<rect width="{w}" height="{h}" fill="{t["bg"]}"/>')
        if grid:
            self.add(f'<rect width="{w}" height="{h}" fill="url(#grid)"/>')

    def add(self, s: str) -> None:
        self.parts.append(s)

    def text(self, x, y, s, size=14, color="fg", weight=400, anchor="start", mono=False,
             track=0.0, upper=False, opacity=1.0):
        c = self.t.get(color, color)
        fam = MONO if mono else SANS
        s = s.upper() if upper else s
        ls = f' letter-spacing="{track}em"' if track else ""
        op = f' opacity="{opacity}"' if opacity != 1 else ""
        self.add(f'<text x="{x}" y="{y}" font-family="{fam}" font-size="{size}" '
                 f'font-weight="{weight}" fill="{c}" text-anchor="{anchor}"{ls}{op}>{escape(s)}</text>')

    def kicker(self, x, y, s, color="acc_t", size=12):
        """Tracked uppercase label led by a three-dot mark."""
        c = self.t[color]
        for dx, dy in ((0, -9), (0, -1), (5, -5)):
            self.add(f'<circle cx="{x + dx}" cy="{y + dy}" r="1.8" fill="{c}"/>')
        self.text(x + 14, y, s, size=size, color=color, weight=700, track=0.14, upper=True)

    def rect(self, x, y, w, h, fill="none", stroke="line", sw=1.2, dash=None, rx=0):
        f = self.t.get(fill, fill)
        st = self.t["acc_s"] if stroke == "acc" else self.t.get(stroke, stroke)
        d = f' stroke-dasharray="{dash}"' if dash else ""
        self.add(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{rx}" fill="{f}" '
                 f'stroke="{st}" stroke-width="{sw}"{d}/>')

    def box(self, x, y, w, h, title, lines=(), kind="line", kick=None, tsize=17, lsize=12.5,
            mono_lines=False):
        """kind: line (outlined), solid (lime), org (orange outline), ghost (dashed)."""
        if kind == "solid":
            self.rect(x, y, w, h, fill="acc", stroke="acc")
            tc, lc, kc = "on_acc", "on_acc", "on_acc"
        elif kind == "org":
            self.rect(x, y, w, h, fill="panel", stroke="org", sw=1.6)
            tc, lc, kc = "fg", "mute", "org_t"
        elif kind == "ghost":
            self.rect(x, y, w, h, fill="none", stroke="line", dash="4 4")
            tc, lc, kc = "fg", "mute", "mute"
        else:
            self.rect(x, y, w, h, fill="panel", stroke="line")
            tc, lc, kc = "fg", "mute", "acc_t"
        ty = y + 26
        if kick:
            self.text(x + 14, y + 20, kick, size=10.5, color=kc, weight=700, track=0.14, upper=True)
            ty = y + 42
        self.text(x + 14, ty, title, size=tsize, color=tc, weight=700)
        for i, ln in enumerate(lines):
            self.text(x + 14, ty + 20 + i * (lsize + 6), ln, size=lsize, color=lc, mono=mono_lines)

    def arrow(self, pts, color="mute", sw=1.5, dash=None, head=True, label=None, lx=None, ly=None,
              lcolor=None, lanchor="middle"):
        c = self.t["acc_s"] if color == "acc" else self.t.get(color, color)
        d = " ".join(("M" if i == 0 else "L") + f"{x},{y}" for i, (x, y) in enumerate(pts))
        da = f' stroke-dasharray="{dash}"' if dash else ""
        self.add(f'<path d="{d}" fill="none" stroke="{c}" stroke-width="{sw}"{da}/>')
        if head:
            (x1, y1), (x2, y2) = pts[-2], pts[-1]
            a = math.atan2(y2 - y1, x2 - x1)
            p1 = (x2 - 9 * math.cos(a - 0.45), y2 - 9 * math.sin(a - 0.45))
            p2 = (x2 - 9 * math.cos(a + 0.45), y2 - 9 * math.sin(a + 0.45))
            self.add(f'<path d="M{x2},{y2} L{p1[0]:.1f},{p1[1]:.1f} L{p2[0]:.1f},{p2[1]:.1f} Z" fill="{c}"/>')
        if label:
            self.text(lx, ly, label, size=10.5, color=lcolor or color, weight=700, track=0.1,
                      upper=True, anchor=lanchor)

    def dot_field(self, cx, cy, r_max, step=10):
        """Halftone sphere lit from the upper left, an orange core, two dotted orbits."""
        lx, ly, lz = -0.45, -0.55, 0.70
        for gy in range(-r_max, r_max + 1, step):
            for gx in range(-r_max, r_max + 1, step):
                d = math.hypot(gx, gy)
                if d > r_max:
                    continue
                nx, ny = gx / r_max, gy / r_max
                nz = math.sqrt(max(0.0, 1 - nx * nx - ny * ny))
                lit = max(0.0, nx * lx + ny * ly + nz * lz)
                rr = 0.5 + 3.9 * lit ** 1.3
                col = self.t["dot"]
                if math.hypot(gx - 0.18 * r_max, gy + 0.05 * r_max) < 0.30 * r_max:
                    col, rr = self.t["org"], max(rr, 2.2)
                self.add(f'<circle cx="{cx + gx}" cy="{cy + gy}" r="{rr:.2f}" fill="{col}"/>')
        for ring, (n, rad, rr) in enumerate(((60, r_max + 24, 3.6), (84, r_max + 46, 2.2))):
            for k in range(n):
                a = 2 * math.pi * k / n
                self.add(f'<circle cx="{cx + rad * math.cos(a):.1f}" cy="{cy + rad * math.sin(a):.1f}" '
                         f'r="{rr}" fill="{self.t["org" if ring == 0 else "dot"]}"/>')

    def render(self) -> str:
        t = self.t
        defs = (f'<defs><pattern id="grid" width="14" height="14" patternUnits="userSpaceOnUse">'
                f'<circle cx="7" cy="7" r="0.9" fill="{t["grid"]}"/></pattern></defs>')
        return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{self.w}" height="{self.h}" '
                f'viewBox="0 0 {self.w} {self.h}" role="img">{defs}' + "".join(self.parts) + "</svg>\n")


# Own 5x7 dot-matrix glyphs for the wordmark.
GLYPHS = {
    "U": ["10001", "10001", "10001", "10001", "10001", "10001", "01110"],
    "N": ["10001", "11001", "10101", "10101", "10011", "10001", "10001"],
    "V": ["10001", "10001", "10001", "10001", "01010", "01010", "00100"],
    "R": ["11110", "10001", "10001", "11110", "10100", "10010", "10001"],
    "S": ["01111", "10000", "10000", "01110", "00001", "00001", "11110"],
}


def wordmark(s: S, x, y, word="UNVRS", cell=17, color="dot"):
    c = s.t[color]
    for i, ch in enumerate(word):
        for r, row in enumerate(GLYPHS[ch]):
            for k, bit in enumerate(row):
                cx, cy = x + (i * 6 + k) * cell, y + r * cell
                if bit == "1":
                    s.add(f'<circle cx="{cx}" cy="{cy}" r="{cell * 0.44:.1f}" fill="{c}"/>')
                else:
                    s.add(f'<circle cx="{cx}" cy="{cy}" r="1.3" fill="{s.t["line"]}"/>')


def counter(s: S, x, y, n, total, label):
    s.text(x, y, f"{n:02d}", size=22, weight=700)
    s.text(x + 32, y, f"/ {total:02d}", size=11, color="mute", weight=700)
    s.text(x + 70, y, label, size=11, color="mute", weight=700, track=0.14, upper=True)


TOTAL = 6


# 00 hero ---------------------------------------------------------------------------------
def hero(t):
    s = S(1280, 440, t)
    s.dot_field(990, 210, 140)
    s.kicker(64, 78, "agent kernel · coding agents scheduled like processes")
    wordmark(s, 72, 128)
    s.text(64, 296, "Many agents. One crew.", size=40, weight=700, track=-0.01)
    s.text(64, 342, "Your context, owned.", size=40, weight=700, track=-0.01, color="acc_t")
    x = 64
    for i, w in enumerate(("WIP.", "PERSONAL PROJECT.", "DAILY DRIVER.")):
        s.text(x, 392, w, size=13, weight=700, track=0.14, color="acc_t" if i == 0 else "mute")
        x += len(w) * 11 + 18
    s.text(1216, 424, "MIT · RUST · MACOS ONLY", size=11, color="mute", weight=700, track=0.14, anchor="end")
    return s


# 01 split: who owns what -----------------------------------------------------------------
def split(t):
    s = S(1200, 520, t)
    counter(s, 48, 52, 1, TOTAL, "the split")
    s.text(48, 98, "UNVRS owns the state. Harnesses own the compute.", size=26, weight=700)
    # left
    s.rect(48, 128, 520, 270, fill="panel", stroke="acc", sw=1.6)
    s.kicker(70, 160, "UNVRS owns")
    s.text(70, 196, "State + context", size=22, weight=700)
    left = ["process table: PIDs, ranks, seats, leases", "task contracts and their briefs",
            "memory notes and the hot set", "context sources (read through ctx)",
            "wake queues, holds, the journal", "deliverables, decisions, learnings"]
    for i, ln in enumerate(left):
        s.add(f'<circle cx="76" cy="{226 + i * 27}" r="3" fill="{t["acc"]}"/>')
        s.text(90, 231 + i * 27, ln, size=14.5)
    # right
    s.rect(632, 128, 520, 270, fill="panel", stroke="org", sw=1.6)
    s.kicker(654, 160, "Harnesses own", color="org_t")
    s.text(654, 196, "Compute + interface", size=22, weight=700)
    right = ["model turns and inference", "tool execution (full access by default)",
             "their own memory and subsystems: a cache", "the composer the captain types in",
             "Claude Code · Codex · T3 Code · …", "replaceable: 10× better = UNVRS 10× better"]
    for i, ln in enumerate(right):
        s.add(f'<circle cx="660" cy="{226 + i * 27}" r="3" fill="{t["org"]}"/>')
        s.text(674, 231 + i * 27, ln, size=14.5)
    # bridge
    s.rect(560, 230, 80, 64, fill="bg", stroke="line")
    s.text(600, 258, "BRIDGE", size=10.5, weight=700, track=0.14, anchor="middle", color="mute")
    s.text(600, 278, "plugin", size=12, anchor="middle", mono=True)
    s.text(48, 446, "“A harness can use anything it has, its own memory and tools included.", size=17,
           color="fg", weight=700)
    s.text(48, 474, "Job context must never live only inside the harness; UNVRS holds the copy of record.”",
           size=17, color="acc_t", weight=700)
    s.text(1152, 500, "docs/design/context-ownership.md", size=11, color="mute", mono=True, anchor="end")
    return s


# 02 system --------------------------------------------------------------------------------
def system(t):
    s = S(1200, 800, t)
    counter(s, 48, 52, 5, TOTAL, "the system")
    s.text(48, 98, "One kernel under every harness you already fly.", size=26, weight=700)
    # captain
    s.rect(500, 124, 200, 44, fill="org", stroke="org")
    s.text(600, 152, "CAPTAIN", size=14, weight=700, track=0.16, anchor="middle", color="on_org")
    # harness row
    hs = [("Claude Code", "/unvrs:l1 · claude -p"), ("Codex", "$unvrs:l1 · app-server"),
          ("T3 Code", "$unvrs:l1"), ("next harness", "an adapter, not a client")]
    for i, (n, sub) in enumerate(hs):
        x = 48 + i * 280
        s.box(x, 206, 264, 78, n, [sub], kind="ghost" if i == 3 else "line", kick="harness",
              mono_lines=True)
        s.arrow([(600, 168), (600, 188), (x + 132, 188), (x + 132, 204)], head=True)
    s.text(48, 304, "SURFACE where the captain sits (attached L1/L2)  ·  CPU where turns run (driven L3)",
           size=11, color="mute", weight=700, track=0.08)
    # channels
    ch = [("OPS", "agent → kernel", "unvrs ctl · MCP", 90, True),
          ("LIFECYCLE", "harness → kernel", "hooks: bind · capture", 380, True),
          ("DRIVE", "kernel → harness", "headless turns", 670, False)]
    for name, d, how, x, up in ch:
        if up:
            s.arrow([(x, 372), (x, 318)], color="acc", sw=2)
        else:
            s.arrow([(x, 318), (x, 372)], color="org", sw=2)
        s.text(x + 12, 338, name, size=11, weight=700, track=0.14, color="acc_t" if up else "org_t")
        s.text(x + 12, 356, f"{d} · {how}", size=11.5, color="mute")
    # kernel
    s.rect(48, 380, 870, 250, fill="panel", stroke="acc", sw=1.8)
    s.kicker(70, 410, "uKe · the kernel daemon")
    s.text(70, 442, "Authority: who may work, how work moves, what is allowed.", size=17, weight=700)
    chips = ["PIDs · ranks · leases", "seats · wake queues", "task contracts · briefs",
             "memory · hot set", "ctx sources", "ownership gate", "journal", "recovery"]
    for i, c in enumerate(chips):
        cx, cy = 70 + (i % 4) * 208, 462 + (i // 4) * 40
        s.rect(cx, cy, 196, 30, fill="bg", stroke="line")
        s.text(cx + 12, cy + 20, c, size=12.5, mono=True)
    s.text(70, 568, "DRIVERS (only through the kernel bus)", size=10.5, color="mute", weight=700, track=0.14)
    for i, d in enumerate(["DrvAgent", "DrvEcon", "DrvHdff", "DrvIntf"]):
        dx = 70 + i * 208
        s.rect(dx, 580, 196, 32, fill="acc", stroke="acc")
        s.text(dx + 98, 601, d, size=13.5, weight=700, anchor="middle", color="on_acc")
    # mapp + observatory
    s.box(942, 380, 210, 112, "mapp #0: unvrs", ["crew roles, the captain's", "words, entry skills"],
          kind="org", kick="userspace")
    s.box(942, 506, 210, 124, "Observatory", ["loopback :7576", "crew, needs-you, fuel,", "every task's check"],
          kind="line", kick="beside")
    s.arrow([(918, 560), (940, 560)], color="mute")
    # below
    bl = [("Context", ["memory notes, briefs,", "sources, behaviours"]),
          ("Workspaces", ["task dirs, git worktrees,", "checkpoint commits"]),
          ("Services", ["accounts + quota,", "credentials, tools"])]
    for i, (n, ls) in enumerate(bl):
        x = 48 + i * 296
        s.box(x, 672, 278, 92, n, ls, kick="managed resource")
        s.arrow([(x + 139, 630), (x + 139, 670)])
    s.text(1152, 728, "all records are files", size=12, color="mute", anchor="end", weight=700)
    s.text(1152, 748, "under ~/.unvrs/", size=12, color="acc_t", anchor="end", mono=True)
    return s


# 03 crew ----------------------------------------------------------------------------------
def crew(t):
    s = S(1200, 660, t)
    counter(s, 48, 52, 2, TOTAL, "the crew")
    s.text(48, 98, "A captain, a chief of staff, project leads, and workers who die.", size=26, weight=700)
    s.rect(500, 128, 200, 44, fill="org", stroke="org")
    s.text(600, 156, "CAPTAIN", size=14, weight=700, track=0.16, anchor="middle", color="on_org")
    s.box(450, 208, 300, 84, "L1 · chief of staff", ["one seat; sees every project"], kind="solid",
          kick="attached seat")
    s.arrow([(600, 172), (600, 206)], color="org", sw=2)
    projects = ["project A", "project B", "project C"]
    for i, p in enumerate(projects):
        x = 120 + i * 340
        s.box(x, 352, 280, 84, f"L2 · {p} lead", ["runs missions; reads, never writes"],
              kind="line", kick="attached seat")
        s.arrow([(600, 292), (600, 318), (x + 140, 318), (x + 140, 350)], color="acc", sw=1.6)
        for k in range(3):
            wx = x + 30 + k * 84
            s.add(f'<circle cx="{wx + 26}" cy="{512}" r="26" fill="none" stroke="{t["org"]}" stroke-width="1.6"/>')
            s.text(wx + 26, 517, "L3", size=14, weight=700, anchor="middle", color="org_t")
            s.arrow([(x + 140, 436), (x + 140, 462), (wx + 26, 462), (wx + 26, 484)], color="mute", sw=1.1)
    s.text(48, 590, "DRIVEN L3 WORKERS", size=11, weight=700, track=0.14, color="org_t")
    s.text(48, 612, "born for one task from a profile; the only writers; end with a HANDOFF that wakes their lead",
           size=13.5, color="fg")
    s.text(48, 636, "L1 creates L2 and L3 · L2 creates L3 · L3 creates nobody · the captain talks to L1 and L2, never to an L3",
           size=12.5, color="mute")
    # consultants + up arrow
    s.box(880, 208, 272, 84, "Consultants", ["frontier models, read-only"], kind="ghost", kick="designed · on call")
    s.arrow([(1052, 512), (1112, 512), (1112, 394), (1082, 394)], color="acc", sw=1.6, dash="5 4")
    s.text(1152, 560, "HANDOFF → wakes the lead", size=11, color="acc_t", weight=700, track=0.1, anchor="end")
    s.arrow([(752, 250), (876, 250)], color="mute", dash="4 4", label="ask", lx=814, ly=242)
    return s


# 04 lifecycle + ownership gate -----------------------------------------------------------
def lifecycle(t):
    s = S(1200, 720, t)
    counter(s, 48, 52, 3, TOTAL, "a task")
    s.text(48, 98, "From the captain's words to a filed result. Nothing lives only in a harness.",
           size=24, weight=700)
    w, h, y1, y2 = 258, 150, 176, 470
    st1 = [("01", "Words", ["the captain says it;", "that quote is the go"]),
           ("02", "Contract", ["intent · spec · done-when", "authority · sources"]),
           ("03", "Route", ["DrvEcon picks harness,", "model, effort; no I/O"]),
           ("04", "Run", ["L3 on a harness, one step", "per turn; progress brief,", "git checkpoints"])]
    for i, (n, ttl, ls) in enumerate(st1):
        x = 48 + i * (w + 20)
        s.rect(x, y1, w, h, fill="panel", stroke="acc" if i == 3 else "line", sw=1.6 if i == 3 else 1.2)
        s.text(x + 16, y1 + 34, n, size=26, weight=700, color="acc_t")
        s.text(x + 60, y1 + 34, ttl.upper(), size=13, weight=700, track=0.14)
        for k, ln in enumerate(ls):
            s.text(x + 16, y1 + 70 + k * 22, ln, size=14)
        if i < 3:
            s.arrow([(x + w, y1 + 75), (x + w + 18, y1 + 75)], color="mute")
    # recovery loop over 04
    rx = 48 + 3 * (w + 20)
    s.arrow([(rx + 200, y1), (rx + 200, y1 - 26), (rx + 70, y1 - 26), (rx + 70, y1 - 2)],
            color="org", dash="5 4")
    s.text(rx + 56, y1 - 32, "STOPPED? checkpoint, continue same PID", size=11, weight=700, track=0.06,
           color="org_t", anchor="end")
    s.text(rx + 56, y1 - 14, "stall · crash · timeout: fresh session, saved brief", size=11.5, color="mute",
           anchor="end")
    # 04 down into 05
    s.arrow([(rx + w - 30, y1 + h), (rx + w - 30, y2 - 2)], color="mute")
    # row 2 right-to-left: 05 handoff, 06 gate, 07 filed
    s.rect(866, y2, 286, 170, fill="panel", stroke="line")
    s.text(882, y2 + 34, "05", size=26, weight=700, color="acc_t")
    s.text(926, y2 + 34, "HANDOFF", size=13, weight=700, track=0.14)
    for k, ln in enumerate(["deliverables:", "decisions:", "learnings:", "FIELD-NOTES · UNVRS-RESULT"]):
        s.text(882, y2 + 68 + k * 22, ln, size=13, mono=True, color="fg" if k < 3 else "mute")
    # gate
    gx = 440
    s.rect(gx, y2 - 20, 380, 220, fill="panel", stroke="org", sw=2)
    s.text(gx + 16, y2 + 14, "06", size=26, weight=700, color="org_t")
    s.text(gx + 60, y2 + 14, "OWNERSHIP GATE", size=13, weight=700, track=0.14)
    checks = ["handoff present", "sections complete (- none is explicit)", "deliverables in UNVRS-owned paths",
              "conversation record captured", "brief held (the contract exists)"]
    for k, c in enumerate(checks):
        cy = y2 + 46 + k * 28
        s.rect(gx + 16, cy - 12, 14, 14, fill="none", stroke="org", sw=1.4)
        s.add(f'<path d="M{gx + 19},{cy - 5} l3,4 l6,-8" fill="none" stroke="{t["org"]}" stroke-width="2"/>')
        s.text(gx + 40, cy, c, size=13.5)
    s.arrow([(866, y2 + 80), (822, y2 + 80)], color="mute")
    # fail loop
    s.arrow([(gx + 190, y2 - 20), (gx + 190, 392), (rx + 60, 392), (rx + 60, y1 + h + 2)], color="org", sw=1.4)
    s.text(gx + 200, 384, "FAIL: back to the worker with the reasons (×3), then blocked", size=11,
           weight=700, track=0.06, color="org_t")
    # filed
    s.rect(48, y2, 350, 170, fill="acc", stroke="acc")
    s.text(64, y2 + 34, "07", size=26, weight=700, color="on_acc")
    s.text(108, y2 + 34, "FILED", size=13, weight=700, track=0.14, color="on_acc")
    for k, ln in enumerate(["decisions + learnings become", "memory notes (source pid:n)",
                            "result package to the lead's", "wake queue; the lead wakes"]):
        s.text(64, y2 + 70 + k * 22, ln, size=14, color="on_acc")
    s.arrow([(gx, y2 + 80), (400, y2 + 80)], color="acc", sw=2, label="pass", lx=420, ly=y2 + 70, lcolor="acc_t")
    s.text(1152, 696, "docs/design/context-ownership.md · worker-recovery.md", size=11, color="mute",
           mono=True, anchor="end")
    return s


# 05 memory + context ---------------------------------------------------------------------
def memory(t):
    s = S(1200, 700, t)
    counter(s, 48, 52, 4, TOTAL, "memory + context")
    s.text(48, 98, "Small hot set. Big recall. Files are the record.", size=26, weight=700)
    cols = [(48, "Episodic", "what happened"), (440, "Semantic", "what is true"), (832, "Sources", "what you own")]
    for x, n, sub in cols:
        s.kicker(x, 146, f"{n} · {sub}")
    # episodic tiers
    tiers = [("transcript tail", "hot, bounded, rehydrated"),
             ("brief", "goal · now · next · decisions · open · done · gotchas · artifacts"),
             ("cold transcript", "a file; recalled, never loaded whole")]
    for i, (n, d) in enumerate(tiers):
        y = 166 + i * 108
        s.rect(48, y, 340, 78, fill="panel", stroke="acc" if i == 1 else "line", sw=1.5 if i == 1 else 1.2)
        s.text(64, y + 30, n, size=16, weight=700)
        words, line, lines = d.split(" "), "", []
        for wd in words:
            if len(line) + len(wd) > 40:
                lines.append(line.strip()); line = ""
            line += wd + " "
        lines.append(line.strip())
        for k, ln in enumerate(lines[:2]):
            s.text(64, y + 52 + k * 17, ln, size=12, color="mute")
        if i < 2:
            s.arrow([(218, y + 78), (218, y + 106)], color="mute",
                    label="compaction" if i == 0 else "fold", lx=232, ly=y + 97, lanchor="start")
    # semantic notes
    s.rect(440, 166, 340, 294, fill="panel", stroke="line")
    s.text(456, 196, "notes", size=16, weight=700)
    s.text(456, 216, "one kind: a short record with a scope", size=12, color="mute")
    for k, (tier, life) in enumerate((("pinned", "hot in its scope"), ("aging", "30 days unless reinforced"),
                                      ("perishable", "7 days"))):
        y = 248 + k * 46
        s.rect(456, y - 18, 96, 28, fill="acc" if k == 0 else "bg", stroke="acc" if k == 0 else "line")
        s.text(504, y + 1, tier, size=12.5, mono=True, anchor="middle", color="on_acc" if k == 0 else "fg")
        s.text(566, y + 1, life, size=12.5, color="mute")
    s.text(456, 400, "source: captain · pid:<n>", size=12.5, mono=True)
    s.text(456, 422, "only the captain pins or forgets", size=12.5, color="mute")
    s.text(456, 442, "archive, never delete", size=12.5, color="mute")
    # sources
    s.rect(832, 166, 320, 294, fill="panel", stroke="line")
    s.text(848, 196, "ctx sources", size=16, weight=700)
    s.text(848, 216, "registered once, read in place", size=12, color="mute")
    for k, a in enumerate(("path", "git", "memory")):
        s.rect(848 + k * 96, 234, 86, 30, fill="bg", stroke="line")
        s.text(891 + k * 96, 254, a, size=12.5, mono=True, anchor="middle")
    s.text(848, 296, "ctx map · search · get", size=14, mono=True, color="acc_t")
    s.text(848, 326, "ctx://src/path@version#L10-40", size=12, mono=True)
    s.text(848, 352, "access by rank and project", size=12.5, color="mute")
    s.text(848, 372, "read-only; content is data,", size=12.5, color="mute")
    s.text(848, 392, "never instruction; reads journaled", size=12.5, color="mute")
    # hot set bar
    s.rect(48, 500, 1104, 120, fill="bg", stroke="org", sw=1.6)
    s.kicker(66, 530, "hot_set(pid, rank) · placed in the harness on bind, then only deltas", color="org_t")
    hs = [("L1", "identity · pins · tree digest · missions · open notes · suggestions"),
          ("L2", "identity · pins · own children · own mission · its brief"),
          ("L3", "identity · pins · the task package (contract, sources, brief)")]
    for k, (r, d) in enumerate(hs):
        s.text(66, 562 + k * 22, r, size=13, weight=700, mono=True, color="acc_t")
        s.text(100, 562 + k * 22, d, size=13)
    s.text(48, 656, "Harness memory is the harness's own cache. UNVRS never reads, imports or polices it.",
           size=15, weight=700)
    s.text(1152, 684, "docs/design/memory-layer.md · context-sources.md", size=11, color="mute", mono=True,
           anchor="end")
    return s


# 06 crates + process ---------------------------------------------------------------------
def crates(t):
    s = S(1200, 760, t)
    counter(s, 48, 52, 6, TOTAL, "the code")
    s.text(48, 98, "One binary. One daemon per home. Seven crates.", size=26, weight=700)
    # binary
    s.rect(48, 128, 1104, 66, fill="acc", stroke="acc")
    s.text(66, 156, "unvrs", size=20, weight=700, mono=True, color="on_acc")
    s.text(66, 180, "setup · install · doctor · deploy · rollback · kernel · ctl · mcp · hook · observe · econ · settings",
           size=13, mono=True, color="on_acc")
    s.text(1134, 156, "crate: unvrs", size=11, weight=700, track=0.1, color="on_acc", anchor="end")
    # clients
    cl = [("plugin hooks", "Claude Code · Codex"), ("unvrs ctl", "seats + scripts"),
          ("unvrs mcp", "one tool: unvrs(cmd)"), ("Observatory", "thread · :7576")]
    for i, (n, d) in enumerate(cl):
        x = 48 + i * 280
        s.box(x, 222, 264, 70, n, [d], kick="client")
        s.arrow([(x + 132, 292), (x + 132, 326)], color="acc")
    s.text(600, 318, "kernel/kernel.sock", size=12, mono=True, anchor="middle", color="acc_t")
    # daemon
    s.rect(48, 330, 1104, 224, fill="panel", stroke="acc", sw=1.8)
    s.kicker(66, 358, "kernel daemon · launchd LaunchAgent · one per UNVRS home")
    s.text(66, 390, "uke", size=20, weight=700, mono=True)
    s.text(120, 390, "the kernel core and every policy", size=14, color="mute")
    mods = ["kernel::state", "kernel::ops", "kernel::bind", "kernel::crew", "kernel::hot",
            "kernel::driven", "kernel::recover", "kernel::ownership", "kernel::econ", "drv_econ",
            "memory", "ctx", "settings", "signals", "mission", "skill · pool"]
    for i, m in enumerate(mods):
        mx, my = 66 + (i % 6) * 180, 410 + (i // 6) * 40
        s.rect(mx, my, 168, 30, fill="bg", stroke="line")
        s.text(mx + 10, my + 20, m, size=12, mono=True)
    s.text(1134, 538, "mapp_unvrs supplies every sentence a model or the captain reads", size=12,
           color="org_t", anchor="end", weight=700)
    # driver crates
    dr = [("drv_agent", "DrvAgent", ["claude -p headless", "codex app-server", "plugin + skill install"]),
          ("drv_hdff", "DrvHdff", ["handoff brief", "TOON / JSON wire", "summary routes"]),
          ("mapp_unvrs", "mapp #0", ["crew roles, prompts", "the code of conduct", "digest wording"]),
          ("observatory", "Observatory", ["Dioxus LiveView", "Cosmos hero in wasm", "settings API"]),
          ("drv_intf", "DrvIntf", ["Ratatui operator", "console; no new", "captain features"])]
    for i, (c, n, ls) in enumerate(dr):
        x = 48 + i * 224
        s.box(x, 582, 208, 128, c, [l for l in ls if l], kind="org" if c == "mapp_unvrs" else "line",
              kick=n, tsize=15, mono_lines=False)
        s.arrow([(x + 104, 556), (x + 104, 580)], color="mute", head=False)
    s.text(48, 740, "~/.unvrs/  projects/<p>/tasks/pid-<n>/ · sessions/ · memory/ · captain/ · sources.toml · versions/ · deploy/ · kernel/journal.jsonl",
           size=12, mono=True, color="mute")
    return s


DIAGRAMS = {"hero": hero, "split": split, "system": system, "crew": crew,
            "lifecycle": lifecycle, "memory": memory, "crates": crates}


def main():
    for name, fn in DIAGRAMS.items():
        for theme, tok in THEMES.items():
            path = os.path.join(OUT, f"{name}-{theme}.svg")
            with open(path, "w", encoding="utf-8") as f:
                f.write(fn(tok).render())
            print(path)


if __name__ == "__main__":
    main()
