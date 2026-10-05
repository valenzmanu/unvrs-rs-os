# uKe design

**What this is:** behaviour and responsibility design for **uKe** (the UNVRS kernel) and the ship around it.  
**What this is not:** an implementation guide. No stack, crate layout, APIs, or build steps.

**Status:** v1, Locked 2026-09-24. A purpose revision: kernel behaviour from v0 is unchanged; the purpose, the captain's seat and the core focus change. System shape: [system-architecture.md](./system-architecture.md). Why: [decisions-2026-09-24-v1.md](./decisions-2026-09-24-v1.md). Changes are listed in the [revision log](#revision-log). Prior code lives under `legacy/` as reference only.

**How we construct:** [construction-method.md](./construction-method.md) — **BSLA v0** (Build → Ship → Learn → Adapt; methodology refined as we build UNVRS). Voyages are SemVer eval ids (`X.Y.Z-eval.N`); cards in [voyages/](./voyages/).

**Crate / layout (Locked):** [codebase-architecture.md](./codebase-architecture.md) — product at repo root (`uke`, `drv_*`); eval pins tags. Not behaviour law.

Scratch notes from the design session: [scratch/](./scratch/).

---

## 1. Purpose

UNVRS runs **work** through agents. uKe is the **kernel** of that system: it owns who may work, how work moves, and what is allowed. External harnesses (first: **Pi**) execute turns. They are not the seat of authority.

**Goals (priority order):**

1. Deliver an outcome at least at the desired **minimum quality**.
2. Use as few resources as needed (tokens, quota, time) to do so.

The system is **work-agnostic**: not limited to software engineering. Interfaces adapt to the kind of work.

**UNVRS owns the user's state. Harnesses own the compute and the interface.** Harnesses play two roles: a **CPU** that runs turns and a **surface** where the captain sits. UNVRS competes with neither. A large improvement in a model or harness must be a large improvement to UNVRS.

**Core (v1):**

| | Focus | Role |
| --- | --- | --- |
| **1** | **Owned context**: memory and behaviours | The user keeps memory, skills and instructions once, in the universe. Every CPU receives a projection. Harness copies are a cache and an inbox. |
| **2** | **Mobile work**: harness interoperability and management | PIDs, ranks, missions, handoff, swap and power let work use every CPU the user has, and move between them without losing context. |

The two are one product. Owned context makes a handoff cheap; handoff makes owned context pay off.

Around the core:

| Layer | Role |
| --- | --- |
| **Meta apps** | Userspace. Behaviours that take intent from A to B. Direction: an app is **packaged context**, i.e. behaviours, mission templates with done checks, and a memory scope, runnable on any CPU. Format design follows the core (§17). |
| **Observability** | Plumbing. One journal across CPUs, built for what the core consumes: cold memory, power usage, done-check evidence, where work is. No dashboards; surfaces render it. |

**Feature test:** if models and harnesses get 10× better, does this feature become more valuable, or obsolete? Build the first kind.

---

## 2. Metaphor

The system is a **spaceship**.

| Role | Meaning |
| --- | --- |
| **Captain** | The human operator |
| **Cockpit** | Whichever harness the captain already flies (Claude Code, Codex, Cursor, T3, …), with UNVRS attached |
| **Ship** | uKe + agents + drivers |
| **Move** | Do work |
| **Power** | Tokens / quota (economic constraints) |
| **Mode** | Who/how the ship is steered |

**v0 modes:**

| Mode | Behaviour |
| --- | --- |
| **human-driven** | Captain is directing; work may be in flight |
| **idle** | Work is done; ship holds with low burn |

Further modes (e.g. autopilot, off) may be added later. **Power** and **mode** are orthogonal.

---

## 3. Layer picture

```
  Host OS
     |
    uKe          ← kernel (not a process in the agent tree)
     |
  agent PID tree ← userspace (work)
```

- **uKe** sits under the agent tree. It creates PID 1, mediates communication and lifecycle, and hosts the driver plane.
- **Userspace** is only agent processes (PIDs). Drivers are not agents in that tree.

**v1 system picture:** harnesses above uKe as surfaces and CPUs; context, workspaces and services below as managed resources; observability beside it. See [system-architecture.md](./system-architecture.md) §0.

---

## 4. Core concepts

| Term | Behaviour |
| --- | --- |
| **Harness** | Execution environment for an agent loop — treated as a **CPU** |
| **Agent** | One agent-loop *instance* inside a harness |
| **PID** | Process that **manages** one agent: identity, mailbox, durable state, rank. Wrapper for an agent/harness — **not** the unit of work |
| **Mission** | Bounded unit of **intent** the ship admits and tracks (see below). Independent of PID |
| **Slice** | A subset / small part of one mission. No separate kinds |
| **Context window** | Harness-local **cache** (scarce). Not the same as durable PID state |
| **Mailbox** | Per-PID inbox for work and messages |
| **Surface** | A harness or host where a human sees and steers. Renders kernel state; never authority |
| **Seat** | A PID bound to a harness session. **Driven** seat: uKe runs the CPU (ACP, headless). **Attached** seat: the captain's own session in their harness; uKe binds it to a PID through the bridge but does not send its turns |
| **Bridge** | The per-harness projection of UNVRS: plugin, skills, hooks, `unvrs ctl` and tool calls. One source, rendered natively per harness |
| **Context** | What the user owns: **memory** (notes, PID sessions, missions, handoffs) and **behaviours** (skills, instructions, nesting) |
| **Meta app** | Packaged context that pursues intent: behaviours, mission templates, done checks, memory scope |
| **Focus** | An L2 with a managed workspace, for work that needs the human close to it |
| **Workspace** | A kernel-owned working directory (git worktree or folder) under `.unvrs/workspaces/`, allocated and freed like memory, owned by a PID and its mission |
| **Services** | Resources seats use: accounts and quota, credentials, MCP servers and tools |

### Mission (Locked)

A **mission** is a bounded unit of intent. Harness turns pursue a mission; they are not the mission.

**Missions and PIDs are independent.** A PID may be assigned to work a mission; assignment can change. The mission document does not live inside `PID.session` (session may **point** at a `mission_id`).

**Who creates:** only the **captain** admits/creates missions. L1/L2 may **suggest** a draft; captain must approve.

**Admit floor (mandatory):**

| Field | Rule |
| --- | --- |
| **Outcome** | What concrete thing exists when done |
| **Done check** | How a stranger says pass/fail without re-reading chat |

Without both, the text stays a **draft**, not a mission.

**Expected / supporting fields:**

| Field | Rule |
| --- | --- |
| **Scope** | Who it’s for, purpose, problem solved, in / out / must-not — specificity that drives outcome quality |
| **Inputs** | Minimum = the prompt. Pointers (paths, URIs, ids) when known. An **exploratory** phase may enrich inputs; that does not invent a new mission kind |
| **Investment** | Optional: how much power to spend (e.g. cheap / standard / frontier). Hint for Econ/profile — not a “quality” dial |

**Not a mission field:** quality floor. Quality is a **byproduct** of how sharp outcome, done check, scope, and inputs are.

**Slice:** a subset of the same mission (narrower outcome/scope). No taxonomy of slice kinds.

### Mission files + registry (Locked)

**Document of truth:** one Markdown file per mission, YAML frontmatter + body sections — inspectable, git-diffable, updated by the system as work proceeds.

Template shape (exact path layout at impl; fields locked):

```markdown
---
id: msn_…
status: draft | admitted | active | done | refused | cancelled
investment: cheap | standard | frontier   # optional
owner_pid: null                           # set when assigned; independent of mission id
updated: …
---

# Outcome
…

# Done check
…

# Scope
…

# Inputs
- prompt: …
- pointers: …

# Log
…
```

**Kernel ownership:** **uKe core** owns a thin **MissionRegistry** alongside the PID table and mailboxes — not a driver.

| Layer | Holds |
| --- | --- |
| Mission **file** | Full packet (outcome, done check, scope, inputs, log) |
| **MissionRegistry** | Index only: id → path, status, owner_pid?, investment?, mtime/hash |
| Drivers | Clients: Intf (captain create/admit UX), Econ (investment pressure), Boot (ready before assign), Hdff (reassign paths), Obs (status events), Agent (runs turns; does not own missions) |

No dual-write of mission body into a second database. Optional on-disk index cache is subordinate to the files. Mutations rewrite the file, then refresh the registry row.

```
  missions/*.md  (SoT)
       ▲
       │ load / rewrite
       ▼
  uKe MissionRegistry ── assign owner_pid ──► PID (independent)
```

---

## 5. Process tree and ranks

Processes form a **parent → child** tree. **PID 1** is the root seat (L1 agent).

**Agent ranks** (privilege ladder; not the host OS layer names):

| Rank | May create |
| --- | --- |
| **L1** | L2 and L3 |
| **L2** | L3 only |
| **L3** | Nobody |

Agents must not have to reason about this ladder for everyday handoffs; the system presents a useful illusion (see §9).

**v1:** L2 is where the human is; L3 is where the human never is. L1 and L2 are attached seats (the captain's own harness sessions); L3 is a driven seat. A **focus** is an L2 with a managed workspace: the L2 reads, one writer L3 writes, N reader L3s read. Thread binding and the L1 lease: [system-architecture.md](./system-architecture.md) §2. Focus: §3.

---

## 6. Communication between PIDs

- Each PID has a mailbox.
- Discipline: **FIFO** queue.
- Operations (OS jargon): **`send`** (enqueue to another PID), **`recv`** (dequeue from own inbox).
- Topology and routing policy live above delivery; the kernel delivers.

---

## 7. Captain and human interface

- The **captain** is the entry point. The ship is stateful and can hold or finish work without a human tick every turn, but the captain **drives** the system.
- Default address: **L1**. The captain may speak **directly to an L2** when needed.
- All human **course / mode / visibility** paths go through the **human interface** driver (DrvIntf). Harness UIs are not authority.
- **Autonomy (Locked):** workers default to **full access** (whatever the harness/CPU already can do). No per-tool permission product — blocking on permits fights an autonomous ship. Captain steers; ship executes. Host isolation/sandbox is a later concern, not click-to-approve. *v1 clarification:* a **role sandbox** (focused L2 and reader L3s are read-only on their workspace) is not a per-tool permit; writer L3s keep full access.

```
  CAPTAIN → Intf → L1 (default)
                 → L2 (optional direct)
```

**Where the captain sits (v1).** The captain flies from the harness they already use. That session is an **attached seat**: the bridge binds it to PID 1 (L1) by default, or to an L2 the captain opens. Driven seats (usually L3, and any autonomous work) run on CPUs uKe launches; the captain can open one in a host when needed.

```
  captain's harness (Claude Code | Codex | Cursor | T3 | …)
        │  bridge: plugin · skills · hooks · ctl
        ▼
  DrvIntf ──► uKe ──► driven seats on any CPU
```

- Every uKe capability is reachable from each supported harness, in that harness's own form (native commands, skills, tool calls, notifications). A new host is an adapter, not a new client.
- Autonomous work reports back into the captain's surface: status and recall through the bridge, notifications through the host, and a handoff that opens a seat when a decision is needed.
- The UNVRS TUI remains as the **operator console** (developer trace, local debugging). It is not the product surface and receives no new captain features.
- UNVRS builds no primary UI of its own. A UNVRS-owned surface returns only for something no host can show.

---

## 8. What the kernel owns vs what the harness does

| Concern | System owns | Harness used for | System overrides |
| --- | --- | --- | --- |
| Who may work / ranks / spawn | ✓ | | |
| **Missions / MissionRegistry** | ✓ (core) | | Files are document SoT; registry is thin index |
| Mailboxes, handoff illusion, swap policy | ✓ | | |
| Human course / mode / see | ✓ (Intf) | **rendering**: the captain's harness is the surface | Harness UI ≠ authority |
| **Behaviours** (skills, instructions, nesting) | ✓ record in the universe | receives a native projection | harness copies are a cache and an inbox |
| **Workspaces** | ✓ alloc / free / reclaim, leases | works inside them | harness-created worktrees are unmanaged |
| **Accounts / quota** | ✓ (Econ) | usage signals in | routing across accounts |
| Budget / quota | ✓ (Econ) | usage signals in | caps and stops still apply; mission **investment** is a hint |
| Nesting / skill packing | later (Boot) | **v0: harness defaults** | |
| Model turns / inference | | ✓ | |
| Tool execution | | ✓ **full access default** | No per-tool permit gate |
| Native harness subagents | | | Prefer system spawn/handoff |
| Observability truth | ✓ (bus + Obs) | raw events in | normalize; don’t trust harness alone |
| Process identity | PID | ACP/session is only a CPU handle | session ≠ PID; mission ≠ PID |

**Rule:** if it decides *who may work* or *whether work is allowed*, the system owns it. If it *runs a turn*, use the harness.

---

## 9. Handoff and harness swap

| Operation | Same PID? | Behaviour |
| --- | --- | --- |
| **Handoff** | No | Pass a **compacted summary** to another PID |
| **Swap** | Yes | Change CPU; **invalidate** old cache; **rehydrate** from the PID **session** into the new CPU |

**Who swaps, who hands off (v1, 2026-09-24):** the difference is identity. Swap keeps it (PID, mailbox, children, leases, workspace ownership, rehydrated context). Handoff creates a new one (new PID, summary only).

| Seat | Changes harness via | Triggered by |
| --- | --- | --- |
| **L1** (attached) | swap | captain; the kernel may only suggest |
| **L2** (attached) | swap (= takeover in its workspace) | captain; the kernel may only suggest |
| **L3** (driven) | handoff; a writer lease moves with the task atomically | kernel (Econ / Hdff) or its L2 |

Swap never changes rank. Moving work between ranks is always a handoff. Swap and handoff quality is core to UNVRS and is graded against the bar in [system-architecture.md](./system-architecture.md) §4.

**Handoff illusion:** a caller may request handoff to a peer rank (e.g. L3 → another L3). The system routes through parents as required by spawn policy. Agents do not orchestrate the ladder themselves.

**Swap vs handoff:** swap keeps identity and transfers continuity into a fresh cache; handoff starts (or uses) another PID with a summary package. Both read the PID session. Harness context windows are **caches only**; the PID owns durable session state.

### PID session (Locked direction)

Session is a **property of the PID**:

| Field | Rule |
| --- | --- |
| **transcript** | Append-only history (bounded); recent tail stays hot |
| **summary** | Rolling semantic brief for swap / rich handoff / reopen |
| **open** / **done** | Required **when non-empty**; omit only if none exist |
| **artifacts** | Required **when non-empty**; store **pointers** (paths, URIs, ids) — never embed raw blobs |
| other structured facts | Same: present if they exist; pointers over copies |

**Pointers, not payloads:** artifacts and large objects live outside the session blob; the session holds links and short labels only.

**Compaction:** when the transcript exceeds a budget, fold older turns into **summary**, keep a recent tail. Do **not** compact every turn — that wastes tokens. Trigger on thresholds / milestones (e.g. size budget, before swap, after handoff in/out, explicit request) — exact policy at impl.

**Who writes summary (Locked direction):** system-invoked **compaction worker**, not the seat’s working CPU by default and not the captain by hand. Redact secrets; never drop non-empty open work silently.

| Concern | v0 lock | Extensibility |
| --- | --- | --- |
| **Model** | **Luna** (cheap, good enough) | Architecture must allow selecting other models later (config / profile) without redesign |
| **Harness (CPU)** | **Pi** by default | Same: pluggable harness for the compaction job; seat’s current harness is unrelated |

Compaction is a short-lived system job: read session slice → Luna on Pi → write updated `summary` (+ trimmed transcript policy). Seat ACP sessions are not reused for this unless a later design explicitly says so.

**Rehydrate (swap v0):** best-effort **semantic** continuity from `summary` + transcript tail + required structured fields — not a cross-harness KV-cache clone.

### Memory (Locked)

The PID session above stays the session. Notes, recall, hot scope, and harness-memory treatment are locked in [memory-layer.md](./memory-layer.md).

Load-bearing rules, so this file and that one do not diverge:

- One universe: the directory `unvrs` is launched in. For the captain that is `$HOME`. Durable notes live under `<universe>/.unvrs/memory/`. The kernel does not hard-code `$HOME`.
- One note kind. The session `summary` is not copied into a second record. Recall indexes it where it already lives.
- Only the captain pins, retires, or deletes. A seat may write an unpinned note and may recall.
- Hot follows the seat's active scope. Recall searches the whole universe.
- Harness memory files are a cache and an inbox. The universe note is the copy of record. `CLAUDE.md` and `AGENTS.md` are instructions and are left untouched.
- Compaction still rewrites `summary` and keeps a tail, as this section says. It also appends unpinned notes and a cold transcript file, as the memory lock says.

Still open there, and not locked here: where a seat's area comes from, the cockpit control that switches scope, and pin-budget numbers.


---

## 10. Nesting (readying an agent)

**Nesting** means taking a barebones agent and specializing it for a job: relevant skills, tools, and high-quality context only (context is scarce; irrelevant skills are noise).

- Nesting is a **system** responsibility, not agent improvisation.
- Exposed as a kernel service (agents may request it; they do not implement it).
- **v0:** nesting is a **stub**. Use the harness’s default skills/setup.
- **v1 direction:** behaviours are **owned context**, under the same rule as memory. The user keeps skills and instructions once in the universe; nesting selects which of them a PID receives for its scope and mission; DrvAgent projects them into the CPU. Real nesting is the next deep design after the bridge.

On new PID bring-up, the boot path must still ensure the agent is **ready or refused** (tools/access/auth policy), even when skill packing is stubbed.

---

## 11. Driver plane

Specialized behaviours sit in **drivers**. They talk to the kernel and to each other **only through the kernel bus**. No private back-channels.

| Driver | Handles |
| --- | --- |
| **uOS** | Managing UNVRS itself: loading/bootstrapping the driver plane |
| **DrvBoot** | PID boot / nesting readiness (stub nesting in v0) |
| **DrvAuth** | Credential/connectivity readiness (optional); **not** a per-tool click-gate — v0 defaults to full access |
| **DrvHdff** | Handoff and harness swap |
| **DrvIntf** | Captain / human channel, rendered into host surfaces through the bridge; the TUI is its operator console |
| **DrvEcon** | Quota, budget, cost-aware admit / route / priority |
| **DrvObs** | System observability and trace export (bus is source of truth); one journal across CPUs that feeds cold memory, Econ and done-check evidence |
| **DrvAgent** | Binding a PID to a harness session (driven or attached); **projecting** the UNVRS behaviour catalogue and the user's behaviours into harness-native form; producing the per-harness bridge |

**uOS** is a peer on the driver plane (meta), not a side bus. Drivers may call the kernel directly; driver-to-driver goes through the kernel.

Because everything crosses one bus, **system-level observability and traceability** are first-class.

---

## 12. Syscalls (behaviour surface)

The agent-facing surface should stay small. v0 intent:

| Call | Behaviour |
| --- | --- |
| **spawn** | Create a PID (rank-checked); implies boot path |
| **kill** | Stop a PID (including self-exit) |
| **send** / **recv** | Mailbox IPC |
| **swap** | Same PID, new harness CPU + full context rehydrate |
| **handoff** | Request work move with summary; system applies rank illusion |
| **accept** / **refuse** | Optional outcome gate (exact shape open) |
| **ws alloc** / **ws free** | Allocate / free a managed workspace ([system-architecture.md](./system-architecture.md) §5); kernel reclaims on owner exit, mission end or idle; free salvages uncommitted work to a branch |

Auth, econ admit, human prompt, and recovery checkpoints are **not** required as separate agent-facing calls in v0; they may be bus-side or message-shaped. Exact shapes are left to implementation discovery.

---

## 13. Agent profiles and first harness

**Agent profiles** are named, **minimal** configuration bundles chosen at spawn (and related ops). They express intent at the UNVRS level (which CPU, coarse policy). They are not large schemas up front.

**Harness-native configuration** is whatever is required so the chosen CPU does not silently use the captain’s global defaults when the profile says otherwise. That translation is DrvAgent’s responsibility. Details are implementation.

**First harness (locked):** **Pi Agent**, reached via **ACP**. Mold Pi as needed; keep profiles minimal; discover missing knobs while building. Bridge already exercises Pi / Codex / Claude / Cursor via ACP; profiles stay minimal.

**v1:** Pi stays the default CPU for driven system jobs (compaction). Claude Code and Codex are first-class for both roles: CPU for driven seats, and surface for attached seats.

### Behaviour catalogue (Locked direction)

UNVRS owns a **single catalogue** of ship behaviours (ops such as spawn, send, handoff, swap, attention, …). Harnesses do **not** invent parallel command sets.

| Surface | Role |
| --- | --- |
| **Captain** | Intf slash (`/swap`, …) → kernel |
| **Agent** | `unvrs ctl <op>` → same kernel ops |
| **Harness UI / native slash** | Not authority; must not bypass the kernel |

**DrvAgent install (Locked direction):** at project / seat bring-up, DrvAgent **installs** the catalogue into each harness in a **harness-native** way (skills, AGENTS.md, hooks, prompts, or equivalent — exact shapes at impl). Source of truth stays in UNVRS; the harness receives a projection so every CPU understands the same ops under minimal harness config.

**Bridge (v1 direction):** the projection is packaged as each harness's native **plugin** where one exists (Claude Code and Codex both have them), rendered from one source:

| Plugin part | UNVRS job |
| --- | --- |
| Session-start hook | bind the session to a PID and rank; deliver the hot set for its scope |
| Stop / pre-compaction hooks | save tail and summary into the PID session |
| `unvrs ctl` (CLI-first; MCP adapter only for hosts without a shell) | kernel ops: remember, recall, admit, spawn, send, handoff |
| Skills | the catalogue plus the user's behaviours |
| Commands | captain ops as native commands (`/handoff`, `/scope`, `/missions`, …) |

Exact shapes per harness are discovered in the bridge voyage.

**Swap (Locked direction):** same PID; Hdff replaces CPU + rehydrates from PID session. Captain `/swap` and `ctl swap` are two surfaces for one behaviour. File/layout details deferred to the swap voyage.

---

## 14. End-to-end behaviours (v0 sketch)

### Captain sets course

Captain, in their own harness (attached L1) → bridge → Intf → admit **mission** (outcome + done check) → assign owner PID → L1/workers pursue → mode **human-driven**.

### Work moves to another CPU

Quota low, a better model, or work that splits → handoff (new PID + summary) or swap (same PID, new CPU) → the receiving seat gets the same memory and behaviours through its bridge → captain sees it in their surface.

### Worker boots

spawn → (econ/auth as needed) → Boot (stub nesting) → Agent on Pi via ACP → events to Obs (and visibility to captain via Intf as needed).

### Peer handoff under rank limits

Caller invokes handoff → Hdff inserts parent orchestration → new worker + summary delivered.

### Work complete

Ship returns to **idle** until the captain sets a new course.

---

## 15. Explicit non-goals (this design pass)

- Implementation language, crates, or repo layout  
- Full nesting / skill library design  
- Meta app runtime (format direction only, §17)  
- A primary UNVRS UI (TUI or GUI) beyond the operator console  
- Features a harness already does well: chat, input, rendering, per-turn context tricks  
- Autopilot and other modes beyond human-driven / idle  
- Complete recovery protocol  
- Exact warn-vs-refuse matrices and syscall argument schemas  

---

## 16. Open questions (for later design or discovery)

- Recovery / checkpoint behaviour  
- Auth as readiness vs abandoned per-tool permits (full access locked)
- Whether accept/refuse is a syscall or a privileged message  
- Rank field vs tree depth  
- Mailbox bounds and backpressure  
- How durable full context for swap is stored — **locked:** PID session (§9) plus the memory layer ([memory-layer.md](./memory-layer.md)). Exact pin budgets and the area-switch control stay open there.
- Compaction triggers / budgets (exact numbers) — discovery at impl  
- Compaction **model/harness:** v0 = **Luna on Pi**; must remain configurable for other models/harnesses later  
- Mission **done-check** richness (one sentence vs structured gates) — discovery at impl  
- Mission file **directory** (`missions/` vs `.unvrs/missions/`) and admit/assign **syscall** shapes — discovery at impl  
- Exact **investment** enum ↔ Econ/profile mapping — discovery at impl  
- **Attached seats:** how each harness's session identity binds to a PID and authenticates — 0.7. (What swap means for them is decided: takeover, [system-architecture.md](./system-architecture.md) §2, §4.)  
- Behaviour record: layout under the universe, scoping, inbox import from harness skill dirs — nesting design  
- Meta app packaging format — after the core (§17)  
- Whether an embedded system loop is needed beyond short-lived driven jobs, given harness headless modes — revisit before building  

---

## 17. Meta apps (direction)

A meta app is **packaged context** that pursues intent: behaviours, mission templates with done checks, and a memory scope. It runs on any CPU, attached or driven, and inherits handoff and swap. Apps may nest; nesting follows rank and spawn policy. **Superseded 2026-09-26 by [mapps.md](./mapps.md)** (mapps: packaged agent programs the kernel runs). Mapp #0 is the **`unvrs` mapp**, the captain's crew: [mapp-unvrs.md](./mapp-unvrs.md). BSLA is a candidate mapp.

---

## Related

| Doc | Role |
| --- | --- |
| [construction-method.md](./construction-method.md) | BSLA v0 + UNVRS voyage SemVer (`X.Y.Z-eval.N`) |
| [codebase-architecture.md](./codebase-architecture.md) | Locked crate layout (`uke`, `drv_*`); product vs eval |
| [memory-layer.md](./memory-layer.md) | Locked memory: notes, recall, hot scope, harness inbox |
| [voyages/0.6.0-eval.1.md](./voyages/0.6.0-eval.1.md) | Implementation contract for that memory lock |
| [voyages/](./voyages/) | Voyage cards |
| [scratch/](./scratch/) | Session working notes (non-normative) |
| [decisions-2026-09-24-v1.md](./decisions-2026-09-24-v1.md) | Decision record for v1 |
| [system-architecture.md](./system-architecture.md) | v1 system shape (Locked): seats, focus, swap/handoff, workspaces, build order |
| [system-architecture-diagrams.md](./system-architecture-diagrams.md) | Pictures of the v1 system (explanatory) |
| [mapps.md](./mapps.md) | What a mapp is; what the kernel owes every mapp (Locked direction) |
| [mapp-unvrs.md](./mapp-unvrs.md) | The `unvrs` mapp: crew, projects, profiles, consultants, knowledge, surfaces (Locked direction) |
| [decisions-2026-09-26-mapps.md](./decisions-2026-09-26-mapps.md) | Why (D27–D44) |
| [unvrs-platform-architecture.md](./unvrs-platform-architecture.md) | Broader platform framing (pre-uKe); its Surfaces decision is superseded by v1 |
| `legacy/` | Previous scaffold — reference only |

---

## Revision log

| Rev | Date | Change |
| --- | --- | --- |
| v0 | 2026-09 | Kernel, ranks, missions, drivers, syscalls, handoff/swap, PID session, memory lock |
| v1 | 2026-09-24 | Purpose: UNVRS owns state, harnesses own compute and interface. Core = owned context + mobile work; meta apps above, observability below (§1). Captain flies from their own harness as an attached seat; TUI becomes operator console (§7). Behaviours owned like memory (§8, §10). Bridge as per-harness plugin (§13). Multi-harness moved from non-goal to core (§15). Meta app direction (§17). Kernel behaviour unchanged. Attached / driven seats, focus, workspaces (§4, §5, §8, §12). Swap for L1/L2, handoff for L3 (§9). Locked with [system-architecture.md](./system-architecture.md). |
| v1.1 | 2026-09-26 | First meta app, later named the `unvrs` mapp (§17, [mapps.md](./mapps.md)): crew L1/L2/L3 + consultants, projects, profiles, bind by command, swap as handoff into a seat, `~/.unvrs` home. Normative in [mapp-unvrs.md](./mapp-unvrs.md). Kernel behaviour unchanged. |
