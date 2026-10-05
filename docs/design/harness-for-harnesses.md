# UNVRS as a harness for harnesses (pointer)

**Status:** operator-accepted framing, 2026-09-16. Not an ADR; not a bet to
implement. The full living draft is
[unvrs-platform-architecture.md](unvrs-platform-architecture.md); this file is
a summary and will not be expanded further.

## One sentence

UNVRS is a system for **verified meta-software**: users compose apps (including
nested apps) that pursue intent and produce artifacts with reviewed evidence.
**HFH** (harness for harnesses) is one layer of that platform, L4: the embedded
system loop that owns authority and dispatches selectable userspace runtimes
(Codex, Hermes, Claude Code, Cursor, and others). HFH alone is infrastructure;
value lives in apps + store + surfaces + HFH together.

## Layers

```
L5 Apps + Surfaces     apps: sales-bot | factory | …
                       surfaces: Dioxus (primary) | T3 | BB | Herdr (thin hosts)
L4 System harness      embedded agent loop + Kernel · Store · Memory · Drivers
L3 Userspace runtimes  Codex | Hermes | Claude Code | Cursor | … (per attempt)
L2 Environment         process / container / account isolation
L1 Hardware            own PC / VPS / VM  (macOS + Linux hosts)
```

## Decisions

1. **Embedded loop (accepted 2026-09-16):** [fx](https://fx.sh/) is the golden
   reference; UNVRS maintains a Rust-side equivalent that tracks fx's roadmap
   and design. jcode is not the system harness; a direct Zig embed is only a
   learning spike.
2. **Surfaces (accepted 2026-09-16):** Dioxus (desktop, web, mobile
   co-developed) is the primary product surface; BB, T3 and Herdr are host
   surfaces with thin adapters only. No Mission Control in BB.
   **Superseded 2026-09-24:** main surfaces are the harnesses users already
   fly, through one uKe bridge —
   [decisions-2026-09-24-v1.md](decisions-2026-09-24-v1.md).
3. **Open:** Store: keep a persistence trait open versus accept CoKe now.
4. **Open:** L1 seat transition: T3/BB over L4 during transition versus a
   cutover date to Dioxus.

The full decision ledger, bets and open items are in
[unvrs-platform-architecture.md](unvrs-platform-architecture.md) (v0.14+):
session decision ledger at the top, then sections 1–13.

Visual ELI5 pass:
[unvrs-platform-architecture-eli5.html](unvrs-platform-architecture-eli5.html).

## Related

- Full draft with vocabulary, composition, enforced guarantees, non-goals,
  slices and change log: [unvrs-platform-architecture.md](unvrs-platform-architecture.md)
- [ADR-0012](../adr/0012-supported-interfaces.md) still stands until an
  amending ADR lands (Dioxus primary narrows "custom UI in BB").
- [Research note 07](../research/07-harness-and-llm-as-runtime.md).
  (surface *after* bet 1 proves owned attempts; bet 2 is the Dioxus shell).
