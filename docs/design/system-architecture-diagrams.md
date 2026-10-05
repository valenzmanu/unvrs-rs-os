# UNVRS system architecture — diagrams (v1)

**Status:** explanatory, not normative. The rules are in
[system-architecture.md](./system-architecture.md) and
[uke-design.md](./uke-design.md) v1; if a picture disagrees, the text wins.

Legend: plain text exists at 0.6. `(proposed)` is locked design not yet
built. `──►` is a call or data flow.

| # | Picture | Shows |
| --- | --- | --- |
| 1 | The whole system | the map |
| 2 | Inside uKe | who is responsible for what |
| 3 | Process tree | ranks, attached and driven seats, a focus |
| 4 | Thread binding | how a new thread joins the tree |
| 5 | The bridge | how UNVRS lives in a harness |
| 6 | Context flow | owned context: memory and behaviours |
| 7 | Focused work | human, L2, writer, readers |
| 8 | Swap and handoff | mobile work |
| 9 | Workspace lifecycle | malloc / free |

---

## 1. The whole system

```
                              CAPTAIN
                                 │ flies from any harness
   ┌──────────────┬──────────────┼──────────────┬──────────────┐
   │    Codex     │ Claude Code  │   Cursor     │  Pi / other  │  HARNESSES
   │  surface  +  │  surface  +  │  surface  +  │     CPU      │  • surface (human sits here)
   │    CPU       │    CPU       │    CPU       │              │  • CPU (turns run here)
   └──────▲───────┴──────▲───────┴──────▲───────┴──────▲───────┘
          │ bridge       │ bridge       │ bridge       │ ACP / headless
          │ (plugin)     │ (plugin)     │ (plugin)     │
          ▼              ▼              ▼              ▼
   ┌────────────────────────────────────────────────────────────┐     ┌───────────────┐
   │                 uKe + drivers  (headless kernel)           │────►│ Observability │
   │   PIDs · ranks · missions · mailboxes · power · leases     │     │ one journal   │
   │   meta apps run here as userspace PIDs                     │     │ across CPUs   │
   └───────┬─────────────────────┬──────────────────────┬───────┘     └───────────────┘
           ▼                     ▼                      ▼
   ┌───────────────┐    ┌─────────────────┐    ┌──────────────────┐
   │   CONTEXT     │    │   WORKSPACES    │    │    SERVICES      │
   │ memory        │    │ (proposed)      │    │ accounts/quota   │
   │ behaviours    │    │ worktrees,      │    │ credentials      │
   │ app packages  │    │ folders         │    │ MCP / tools      │
   └───────────────┘    └─────────────────┘    └──────────────────┘
                     all under <universe>/.unvrs/
```

Harnesses supply compute and screens. uKe supplies authority and continuity.
The bottom row is what the user owns.

## 2. Inside uKe: kernel core and driver plane

```
 ┌──────────────────────────────── uKe ────────────────────────────────┐
 │  KERNEL CORE                                                         │
 │  ┌────────────┐ ┌─────────────────┐ ┌───────────┐ ┌───────────────┐  │
 │  │ PID table  │ │ MissionRegistry │ │ Mailboxes │ │ MemoryIndex   │  │
 │  │ ranks,     │ │ index over      │ │ FIFO per  │ │ index over    │  │
 │  │ leases     │ │ mission files   │ │ PID       │ │ notes/sessions│  │
 │  │ (proposed: │ └─────────────────┘ └───────────┘ └───────────────┘  │
 │  │ L1 lease,  │ ┌──────────────────────────────┐                     │
 │  │ ws leases) │ │ Workspace table (proposed)   │                     │
 │  └────────────┘ │ alloc / free / reclaim       │                     │
 │                 └──────────────────────────────┘                     │
 │ ════════════════════════ KERNEL BUS ════════════════════════════════ │
 │        │          │          │          │          │          │      │
 │  ┌─────┴───┐ ┌────┴────┐ ┌───┴────┐ ┌───┴────┐ ┌───┴────┐ ┌───┴────┐ │
 │  │DrvIntf  │ │DrvAgent │ │DrvHdff │ │DrvBoot │ │DrvEcon │ │DrvObs  │ │
 │  │captain  │ │bind PID │ │handoff,│ │ready a │ │tokens, │ │journal,│ │
 │  │channel; │ │to CPU;  │ │swap,   │ │PID:    │ │quota,  │ │traces  │ │
 │  │renders  │ │project  │ │summary,│ │nesting │ │disk    │ │        │ │
 │  │via      │ │context; │ │compact │ │        │ │(power) │ │        │ │
 │  │bridge + │ │build    │ │        │ │        │ │(prop.) │ │        │ │
 │  │TUI      │ │bridge   │ │        │ │        │ │        │ │        │ │
 │  │console  │ │         │ │        │ │        │ │        │ │        │ │
 │  └─────────┘ └─────────┘ └────────┘ └────────┘ └────────┘ └────────┘ │
 │   uOS: loads and supervises the driver plane                         │
 └──────────────────────────────────────────────────────────────────────┘
```

If it decides who may work or whether work is allowed, uKe owns it. If it runs
a turn, a harness does. At 0.6 the PID table, bus socket and handoff still live
in the TUI flight loop; 0.7 moves them into the headless kernel.

## 3. Process tree: ranks, seats and a focus

```
                         CAPTAIN
               ┌────────────┼─────────────────────┐
               │ talks      │ talks               │ talks
               ▼            ▼                     ▼
        ┌────────────┐  ┌──────────────────┐  ┌──────────────────┐
        │ L1 · PID 1 │  │ L2 focus A       │  │ L2 (auto, no     │
        │ attached   │  │ attached         │  │ workspace)       │
        │ universe   │  │ workspace ws-A   │  │ attached         │
        │ root       │  │ READ only        │  │ scope from cwd   │
        └─────┬──────┘  └───────┬──────────┘  └──────────────────┘
              │ spawns          │ drives via mailbox
              │ L2/L3           ├──────────────┬──────────────┐
              ▼                 ▼              ▼              ▼
        ┌───────────┐    ┌────────────┐ ┌────────────┐ ┌────────────┐
        │ L3 driven │    │ L3 WRITER  │ │ L3 reader  │ │ L3 reader  │
        │ (autonom. │    │ 1 lease    │ │ explore    │ │ review     │
        │  mission) │    │ RW ws-A    │ │ R  ws-A    │ │ R  ws-A    │
        └───────────┘    └────────────┘ └────────────┘ └────────────┘

   ATTACHED seat = the captain's own harness thread, bound to a PID (L1, L2)
   DRIVEN seat   = uKe runs the CPU; the captain never talks to it (L3)
   Spawn rule    : L1 → L2, L3   ·   L2 → L3   ·   L3 → nobody
```

## 4. Thread binding: what happens when you open a thread

```
   Captain opens a new thread in some harness
                    │
                    ▼
        bridge session-start hook ──► uKe: "where does this go?"
                    │
          ┌─────────┴──────────┐
          │ inside universe?   │── no ──► UNBOUND (UNVRS does nothing)
          └─────────┬──────────┘
                    │ yes
          ┌─────────┴──────────┐
          │ cwd in a managed   │── yes ─┐
          │ workspace?         │        │
          └─────────┬──────────┘        ▼
                    │ no        ┌──────────────────┐
                    │           │ its L2 live      │── no ──► REBIND that L2
                    │           │ elsewhere?       │          (session, mission,
                    │           └────────┬─────────┘           scope, hot set)
                    │                    │ yes
                    │                    ▼
                    │           TAKEOVER: same PID moves here
                    │           (another harness = swap);
                    │           old thread told "detached"
          ┌─────────┴──────────┐
          │ L1 lease held by   │── no ──► become L1: PID 1 attaches,
          │ a live thread?     │          rehydrates from its session
          └─────────┬──────────┘
                    │ yes
                    ▼
            AUTO-L2 child of PID 1; scope from cwd;
            L1 gets a mailbox note

   Later: thread quiet or closed ──► DETACH (PID idles, session compacted, nothing lost)
          /l1 ──► take PID 1 here      /bind <pid> ──► resume an L2 here
```

## 5. The bridge: one plugin per harness

```
 ┌───────────────── Claude Code / Codex thread ─────────────────┐
 │   model ◄──── context in ────┐        ┌──── ops out ────►    │
 │  ┌───────────────────── UNVRS plugin (proposed) ──────────┐  │
 │  │ session-start hook  bind PID and rank; inject hot set  │  │
 │  │                     and behaviours for this scope      │  │
 │  │ prompt hook         heartbeat; "you are detached"      │  │
 │  │ stop / pre-compact  push tail and summary to the PID   │  │
 │  │ tools / ctl         remember · recall · admit · spawn  │  │
 │  │                     send · handoff · ws alloc          │  │
 │  │ skills              catalogue + the user's behaviours  │  │
 │  │ commands            /handoff /scope /missions /l1 /bind│  │
 │  └─────────────────────────┬──────────────────────────────┘  │
 └────────────────────────────┼─────────────────────────────────┘
                              │ authenticated call to the kernel
                              ▼
                     DrvIntf / DrvAgent ──► uKe bus

   One source in UNVRS; DrvAgent renders each harness's native plugin format.
   Exists at 0.6: remember / recall skills and unvrs ctl for driven seats.
```

## 6. Context flow: owned memory and behaviours

```
                    ┌─────────────── <universe>/.unvrs ───────────────┐
                    │  notes · PID sessions · missions · handoffs     │
                    │  behaviours (proposed) · app packages (later)   │
                    └──▲──────────────┬───────────────────▲───────────┘
            capture    │              │ project           │ fold
  ┌────────────────────┴───┐   ┌──────▼─────────────┐  ┌──┴──────────────────┐
  │ IN (harness → UNVRS)   │   │ OUT (UNVRS →       │  │ COMPACTION          │
  │ • stop hook: tail,     │   │ harness)           │  │ tail over budget ─► │
  │   summary              │   │ • hot set for the  │  │ short-lived Pi +    │
  │ • remember tool        │   │   seat's scope     │  │ Luna job ──►        │
  │ • inbox: new lines in  │   │ • behaviours for   │  │ summary + unpinned  │
  │   harness memory files │   │   scope / mission  │  │ notes + cold        │
  │   (imported once)      │   │ • recall results   │  │ transcript          │
  └────────────▲───────────┘   └─────────┬──────────┘  │ (redacted)          │
               └───────── any harness ◄──┘             └─────────────────────┘
        Harness files = cache + inbox.  Universe files = the record.
        Hot follows the seat's scope; recall searches everything.
```

## 7. Focused work: human, L2, writer and readers

```
 Captain        L2 (focus, R)        uKe              Reader L3s (R)     Writer L3 (RW)    ws-A
    │  "fix X"      │                 │                     │                  │             │
    │──────────────►│  spawn readers  │                     │                  │             │
    │               │────────────────►│──── start ─────────►│  read ───────────┼────────────►│
    │               │◄────── findings @ commit abc ─────────│                  │             │
    │◄─ plan ───────│                 │                     │                  │             │
    │── "go" ──────►│  task ──────────┼─── writer lease ────┼─────────────────►│             │
    │               │                 │                     │    edit, build,  │────────────►│
    │               │                 │                     │    test, commit  │             │
    │               │◄────────── done: commits def..ghi + evidence ────────────│             │
    │◄─ review ─────│  (reads diff in place)                │                  │             │
    │  ok / revert  │                 │                     │                  │             │
    │── "next" ────►│  next task ─────┼─────────────────────┼─────────────────►│  (queue)    │

   Reads parallel · writes sequential · review in place · captain can always edit directly
```

## 8. Swap and handoff: mobile work

```
  HANDOFF (L3: new PID, summary)            SWAP (L1/L2: same PID, new harness)
  ─────────────────────────────             ───────────────────────────────────
  PID 7 on Claude ── quota low              PID 2 in a Claude Code thread
        │                                         │
        ▼                                         │ captain opens Codex there
  Econ: power low ─► Hdff                         │ (takeover or /bind)
        │                                         ▼
        │ package: summary, open work,      Hdff: detach the old thread,
        │ pointers, writer lease            invalidate its cache
        ▼                                         │
  spawn PID 9 on Codex (rank-checked,             ▼
  routed via parent if needed)              session-start hook rehydrates PID 2:
        │                                   summary + tail + open/done/artifacts
        ▼                                   + hot set + behaviours
  PID 9 continues in the same                     │
  workspace with the same memory                  ▼
                                            same PID, children, mailbox,
                                            workspace; new harness

  Triggers: out of quota · better model · captain choice · stuck worker
  Swap never changes rank. Rank change = handoff.
```

## 9. Workspace lifecycle (malloc / free)

```
                 ws alloc repo@ref --for purpose
                 (refused over disk budget, with live list)
                              │
                              ▼
   ┌──────────┐  lease  ┌──────────┐  task done /   ┌────────────┐
   │ALLOCATED │────────►│  IN USE  │───────────────►│  FREEING   │
   │ owner =  │         │ 1 writer │  owner frees   │ uncommitted│
   │ PID +    │         │ N readers│                │ → branch   │
   │ mission  │         └────┬─────┘                │ unvrs/     │
   └──────────┘              │ RECLAIM by uKe:      │ salvage/   │
                             │ owner killed ·       │ <ws-id>    │
                             │ mission done ·       └─────┬──────┘
                             │ idle TTL ·                 │ remove checkout
                             │ parent freed               │ + build artifacts
                             └───────────────────────────►▼
                                                    ┌────────────┐
                                                    │   FREED    │ handle dead
                                                    │ branch kept│ (use refused)
                                                    └────────────┘
   Budget (Econ): max live workspaces + total bytes under .unvrs/workspaces/
```
