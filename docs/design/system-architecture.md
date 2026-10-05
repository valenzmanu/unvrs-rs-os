# UNVRS system architecture (v1)

**Status:** Locked 2026-09-24 by the operator.
**Amended 2026-09-26:** [mapp-unvrs.md](./mapp-unvrs.md) wins
where they differ: bind by command only (no auto-L2, no cwd binding, §2);
L2 is per project and workspaces belong to L3 writers (§3); swap is a handoff
into the same seat (§4); the universe home is `~/.unvrs`. See its §11.
**What this is:** the shape of the whole system after the v1 pivot: the
pieces, how seats and threads bind, focused work, swap and handoff, and
workspaces. Normative together with [uke-design.md](./uke-design.md) v1.
**Why:** [decisions-2026-09-24-v1.md](./decisions-2026-09-24-v1.md) records
each decision and the reason for it.
**Pictures:** [system-architecture-diagrams.md](./system-architecture-diagrams.md)
(explanatory, not normative).
**Code layout:** [codebase-architecture.md](./codebase-architecture.md).

If you are implementing: read §0, then the section your voyage card names. A
behaviour in this file that is not in your voyage card is not yours to build.

---

## 0. The system in one paragraph

**UNVRS owns the user's state. Harnesses own the compute and the interface.**
The captain works in the harness they already use (Claude Code, Codex, …).
A UNVRS plugin in that harness binds the session to a PID in one process tree
(L1 → L2 → L3). uKe owns the tree, missions, memory and behaviours, workspaces
and power. Work moves between harnesses by **swap** (same PID, new harness) or
**handoff** (new PID, summary) without losing context, because no harness owns
the context. The core is those two halves: **owned context** and **mobile
work**. Meta apps sit above as userspace; observability sits beside as
plumbing.

```
                    captain
                       │ flies from
   ┌───────────┬───────┴─────┬──────────────┐
   │  Codex    │ Claude Code │ Other harness│   surfaces (attached seats)
   └─────▲─────┴──────▲──────┴──────▲───────┘   + CPUs (driven seats)
   bridge│ plugin     │             │ spawn / handoff / swap
         ▼            ▼             ▼
   ┌─────────────────────────────────────────┐        ┌──────────────┐
   │ uKe + drivers                           │───────►│ Observability│
   │ PIDs · ranks · missions · mail · power  │        │ one journal  │
   │ meta apps run here as userspace PIDs    │        └──────────────┘
   └───────┬──────────────┬──────────────┬───┘
           ▼              ▼              ▼
   ┌──────────────┐ ┌────────────┐ ┌──────────────────┐
   │ Context      │ │ Workspaces │ │ Services         │
   │ memory +     │ │ worktrees, │ │ accounts/quota,  │
   │ behaviours + │ │ folders    │ │ credentials,     │
   │ app packages │ │            │ │ MCP / tools      │
   └──────────────┘ └────────────┘ └──────────────────┘
          all under <universe>/.unvrs/
```

## 1. Pieces

| Piece | Role | State at 0.6 |
| --- | --- | --- |
| **Captain** | Flies from the harness they already use. That session is an attached seat. | Flies the UNVRS TUI; attached seats not built |
| **Harnesses** | Two roles. **Surface:** where a human sees and steers. **CPU:** where turns run. | Pi, Codex, Claude, Cursor as CPUs over ACP |
| **Bridge** (up arrows) | The per-harness projection of UNVRS, packaged as that harness's plugin: session hooks, tool calls or `unvrs ctl`, skills, native commands. One source, rendered per harness by DrvAgent. | `remember` / `recall` skills and `unvrs ctl` for driven seats; no plugin, no hooks |
| **Dispatch** (down arrows) | uKe starts and moves work on CPUs: spawn a driven seat, handoff, swap. | spawn and handoff over ACP; **swap not built** |
| **uKe + drivers** | Authority: PID table, ranks, leases, missions, mailboxes, power. Drivers talk only through the kernel bus. | Built, but the PID table, bus socket and handoff live in the TUI's flight loop (`drv_intf/src/bridge.rs`). **uKe cannot run without the TUI yet.** |
| **Mapps** (meta apps) | Userspace: packaged agent programs ([mapps.md](./mapps.md)); mapp #0 is the `unvrs` mapp. | `unvrs` mapp designed ([mapp-unvrs.md](./mapp-unvrs.md)); others later |
| **Observability** | One journal across CPUs: who ran what, where, at what cost. Feeds cold memory, Econ and done-check evidence. Rendered by the **Observatory** (`unvrs observe`, localhost, Dioxus, read-only; D24) and by host surfaces. | Bus events and flight journals per cockpit process |
| **Context** | What the user owns. **Memory:** notes, PID sessions, missions, handoffs. **Behaviours:** skills, instructions, nesting. **App packages** later. Harness copies are a cache and an inbox. | Memory built (0.6). Behaviours partly: skill pools and catalogue install |
| **Workspaces** | Where work happens, owned by a PID and its mission, never by a harness, so swap and handoff land in the same place. | Not built |
| **Services** | Resources seats use. **Accounts and quota:** every subscription is CPU capacity. **Credentials:** `.env`, redaction. **MCP servers and tools.** | `.env` and redaction; no account model; no Econ driver |
| **Universe** | `<universe>/.unvrs/` holds every record. Files are the record; indexes are disposable. | Built |

**Consequence for implementation:** uKe must run as its own process (a
headless kernel that owns the bus socket and the PID table). The TUI and every
bridge are clients of it. Nothing in §2–§5 works while the kernel lives inside
the TUI.

### Channels between a harness and the kernel

| Channel | Direction | Started by | Carries | Mechanism |
| --- | --- | --- | --- | --- |
| **Ops** | agent → kernel | the model | remember, recall, spawn, send, handoff | `unvrs ctl` + skills; MCP only as an adapter for hosts without a shell |
| **Lifecycle** | harness → kernel | harness events | attach, heartbeat, capture, hot-set injection | hooks |
| **Drive** | kernel → harness | the kernel | start a driven seat, send turns, read events | ACP or headless mode |

Anything that must happen is a hook, never a tool the model may skip. Captain-only ops (pin, retire, delete, scope) arrive through a path the model cannot take. Swap is a kernel operation carried by the lifecycle channel, not a transport. Why: [decisions-2026-09-24-v1.md](./decisions-2026-09-24-v1.md) D21.

## 2. Seats and threads

**Threads are views; PIDs are processes.** A host lets the captain open any
number of threads. uKe keeps one process tree. Rank limits apply to the tree,
not to the number of threads. UNVRS never blocks a thread in a host.

| Kind of seat | What it is | Ranks |
| --- | --- | --- |
| **Attached** | The captain's own harness session, bound to a PID by the bridge. uKe does not send its turns. | L1, L2 |
| **Driven** | uKe starts the CPU (ACP or headless) and sends its turns. The captain never talks to it. | L3 |

**L2 is where the human is. L3 is where the human never is.**

### Binding at session start

Only threads opened **inside the universe** bind. The bridge's session-start
hook asks uKe where the thread goes:

| Thread opened in… | Result |
| --- | --- |
| outside the universe | **unbound**; UNVRS does nothing |
| the universe, no live L1 | **L1**: PID 1 attaches here and rehydrates from its session |
| the universe, L1 live | **auto-L2**, child of PID 1; scope from cwd, refined by the first prompt; L1 gets a mailbox note |
| a managed workspace whose L2 is not live | **rebind** that L2 (session, mission, scope, hot set) |
| a managed workspace whose L2 is live elsewhere | **takeover**: same PID moves to this thread, possibly on another harness (this is swap, §4); the old thread is told it detached on its next prompt |

- `/l1` moves PID 1 to the current thread; the previous L1 thread becomes an L2.
- `/bind <pid>` resumes an existing L2 in the current thread (swap if the
  harness differs).
- The captain typing into an L3's session promotes it to L2. Opening it to
  watch does not.
- **L1 is a lease.** One session holds PID 1 at a time, so two threads racing
  at start cannot both be L1.
- **Liveness** comes from bridge hook heartbeats. A closed or quiet thread
  **detaches**: its PID idles, its session is compacted, nothing is lost.
- **"A few L2s" is a soft limit.** L1 sees every L2 and may suggest merging,
  handoff or retiring idle ones. The kernel does not refuse a thread.
- Every bound thread's work reaches memory whatever its rank. Hot follows the
  seat's scope; recall searches everything.

## 3. Focused work

A **focus** is an L2 with a managed workspace, for work that needs the human
close to it: critical code, design, anything that needs human input.

| Seat | Per focus | Workspace access |
| --- | --- | --- |
| Focused L2 (human present) | 1 | **read**; drives L3s through the mailbox |
| Writer L3 | 1, holding the writer lease; tasks queue | **read + write**; runs builds and tests; ends each task in commits |
| Reader L3 | N | **read**; explore, research, plan, review; reports against a commit hash |
| Captain | — | always free to edit directly |

- **Writes are sequential inside a focus; reads are parallel.** Parallel writes
  exist across focuses, each with its own workspace.
- **Review is in place.** The L2 and the captain read the writer's commits. A
  bad change is reverted. There is no separate accept step.
- Readers do not build or test in the shared workspace. Verification runs on
  the writer.
- L2 read-only and reader read-only are a **role sandbox**, enforced with each
  harness's own sandbox (Codex read-only sandbox mode; Claude Code deny rules
  and a pre-tool hook). Best effort: a shell can still write. This is not a
  per-tool permit product; writer L3s keep full access (uke-design §7).
- An L3 that needs a human decision sends to its L2's mailbox.
- Knowledge work has the same shape: the workspace is a folder plus pointers to
  external files; the writer drafts; the L2 and the captain review in place.
- **Deferred until a flight shows the need:** L2 write access; parallel writers
  inside one focus (a child workspace merged back through the writer lease).

## 4. Swap and handoff

Core to UNVRS: this is where owned context pays off. The difference is
**identity**.

| | Swap | Handoff |
| --- | --- | --- |
| PID | same | new |
| Mailbox, pending messages | kept | new |
| Children | stay parented | not moved |
| Leases, workspace ownership | kept | transferred with the task |
| Context | rehydrated from the PID session | summary and pointers |

| Seat | Changes harness via | Triggered by |
| --- | --- | --- |
| **L1** | swap | captain; the kernel may only suggest |
| **L2** | swap (= takeover or `/bind`) | captain; the kernel may only suggest |
| **L3** | handoff; a writer lease moves with the task atomically | kernel (Econ / Hdff) or its L2 |

- An attached seat is the captain's thread, so the kernel cannot move it. On
  low quota or a better model it suggests: "L1 is low on Claude; open it in
  Codex to continue."
- **Swap of an attached seat** = the new thread's session-start hook rehydrates
  the PID (summary, tail, open / done / artifacts, hot set, behaviours); the old
  thread is detached.
- L3s are disposable. A fresh context with a good summary is a feature; the
  workspace (commits, commit-pinned reports) carries continuity.
- **Swap never changes rank.** Moving work between ranks is always a handoff.

### What good looks like (the eval bar)

Every voyage that touches swap or handoff proves these across at least two
harnesses (Claude Code ↔ Codex): a scripted check where possible, a captain
check for "continues, not restarts".

| Property | Swap | Handoff |
| --- | --- | --- |
| **No re-asking** | the new harness does not ask the captain anything already answered in the session | the receiver does not ask its parent for facts in the package |
| **Open work survives** | every non-empty open item, artifact pointer and pending message is present after | every open item of the task is in the package |
| **Continues, not restarts** | the first turn after swap continues the current step | the first turn starts the next step, not a rediscovery |
| **Same memory and behaviours** | hot set and projected behaviours are equal before and after, across harnesses | the receiver gets the hot set and behaviours for its scope |
| **Cheap** | rehydrate payload bounded (summary and tail budgets) | package bounded; pointers, not payloads |
| **Safe** | no secrets in the rehydrate payload; old thread detached | lease moved atomically; never two writers |
| **Observable** | one journal event linking old CPU → new CPU for the PID | one journal event linking old PID → new PID with the package pointer |

## 5. Workspaces (malloc / free)

Worktrees cost disk, mostly build artifacts (`target/`, `node_modules`). uKe
allocates and frees them like memory, and reclaims what owners forget.

- Location: `<universe>/.unvrs/workspaces/<ws-id>/`. **Only the kernel creates
  them.** Worktrees a harness creates on its own are unmanaged.
- Allocation points: opening a focus allocates one. Children only on demand
  (deferred, §3). Reader L3s never get one.

| Op | Behaviour |
| --- | --- |
| **alloc** | `<repo>@<ref>` and a purpose; owner = calling PID and its mission. Refused over budget, listing live workspaces and their sizes. |
| **use** | One writer lease, N readers. |
| **free** | The owner frees it when its task is done. |
| **reclaim** | uKe frees it when the owner PID is killed, its mission is done or cancelled, or it is idle past a TTL. Freeing a parent frees its children. |

- **Free never loses work.** Before removing a checkout, uKe commits anything
  uncommitted to the branch `unvrs/salvage/<ws-id>` and keeps it. Free drops
  disk, never commits. A salvage branch can be reopened with a new alloc.
- A freed handle is dead; using it is refused.
- **Budget:** disk is a power resource beside tokens and quota: max live
  workspaces and total bytes, tracked by Econ, visible to the captain.

## 6. Build order

Each row is a voyage-sized step. A later row assumes the earlier ones.

| Step | Builds | Why this order |
| --- | --- | --- |
| **0.7** | Headless kernel; bridge plugins for Claude Code and Codex; thread binding (L1 lease, auto-L2, detach, `/l1`, `/bind`); swap for attached seats; L3 handoff graded against §4; rank-aware hot set and one brief (memory-layer §13 R1, R2); the Observatory (D24) | Everything else needs the kernel outside the TUI and a seat in the captain's harness. Swap and handoff are the core. |
| **0.8** | Real-work readiness: clean-room harness homes (D25), the captain's real universe, skill loader v0 (D26). Focus: workspace alloc / free / reclaim / salvage; writer lease; reader L3s; role sandbox; takeover in a workspace | Needs attached L2s and handoff from 0.7 |
| **0.9** | Behaviours as owned context: behaviour record in the universe, nesting by scope and mission, projection and inbox; APM as the static installer behind a DrvAgent seam (D23); skill loader v1 (D26); note consolidation and trust (memory-layer §13 R3–R5) | Needs the bridge to carry them |
| later | Accounts and Econ routing by quota; one journal format across CPUs; meta-app packaging with BSLA as the first app | Build on all of the above |

## 7. Open (decided in the voyage that needs it)

- How each harness's session identity binds to a PID, and how an attached
  session authenticates to the kernel. (0.7)
- How the kernel process starts, is found and stops. (0.7)
- Workspace kinds beyond git worktrees: allocating, salvaging and freeing a
  plain folder. (0.8)
- Workspace budget numbers and idle TTL. (0.8)
- Whether reader L3s ever need an isolated build dir (e.g. `CARGO_TARGET_DIR`). (0.8)
- Account model: how Econ sees several subscriptions per harness. (later)
- The journal format every bridge and adapter reports. (later)
