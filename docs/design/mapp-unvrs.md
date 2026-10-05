# The `unvrs` mapp: the captain's crew

**Status:** Locked direction 2026-09-26 (operator). Normative for the product
shape. Where it disagrees with [system-architecture.md](./system-architecture.md)
or [skill-loader.md](./skill-loader.md), **this file wins** (see §11).
**Why:** [decisions-2026-09-26-mapps.md](./decisions-2026-09-26-mapps.md)
D27–D38.
**Builds on:** the kernel (uKe), drivers, bridge plugin, hooks, handoff
package, memory and Observatory built through 0.7. They stay; new or changed
drivers are allowed when this design needs them.

If you are implementing: read §0 and §1, then the section your voyage card
names.

---

## 0. In one paragraph

The `unvrs` mapp is mapp #0 ([mapps.md](./mapps.md)): the mapp the captain uses
to talk to UNVRS and drive it; other mapps run alongside and are operated
through it. The human is the
**captain** and has a **crew**. **L1** is the captain's chief of staff and
personal assistant: it remembers everything the captain has said and sees every
project. Each **project** has an **L2** that leads it: it understands the
project end to end and runs its missions, but does no hands-on work. **L3**
workers are specialists built from **profiles**; they do the work, file what
they learned, and die. **Consultants** are frontier models on call for hard
questions. The captain drives all of it from any harness thread with `/`
commands. Inspired by Kun Chen's Firstmate (captain → first mate → crewmates)
with one extra layer, the project lead, like a boss, capos and their crews.

```
                     CAPTAIN (any harness thread)
          /l1-unvrs │          │ /l2-unvrs        │ /consult-unvrs
                    ▼          ▼                  ▼
 SEATS      ┌────────────┐  ┌────────────┐   CONSULTANTS
 persistent │     L1     │─►│ L2 per     │   frontier models,
 read-only  │  chief of  │  │ project    │   read-only, report back
 any harness│   staff    │  │ lead       │
            └────────────┘  └─────┬──────┘
                                  │ missions → tasks (each names a profile)
 WORKERS                    ┌─────▼──────┐
 ephemeral, the only        │ L3 from a  │──► field notes flow back up
 writers                    │ profile    │
                            └────────────┘
```

## 1. The crew

| Role | Job | Lifetime | Writes project files |
| --- | --- | --- | --- |
| **Captain** | decides, approves, talks mostly to L1 | — | yes (the human is never sandboxed) |
| **L1** | chief of staff; knows the captain and every project; routes work to L2s; may create an L2 or an L3; owns cross-project missions; writes the captain's digest | one permanent seat | **no** |
| **L2** | leads one project: its context, missions, plans, the L3s it creates, curation of its knowledge | one seat per project, lives as long as the project | **no** |
| **L3** | a specialist from a profile; does one task | born for the task, dies after | **yes**, in a workspace |
| **Consultant** | frontier model for planning, architecture, hard calls | per question or short session | **no** |

Rules:

- **Only L3s write project files.** L1, L2 and consultants read everything in
  their scope and change only UNVRS state (memory, projects, missions) through
  `unvrs ctl`. Enforced best effort by the role sandbox (hooks deny edit tools
  on project paths).
- **Exactly three levels** ([uke-design.md](./uke-design.md) §5). L1 may
  create an L2 or an L3; an L2 may create L3s only; an L3 creates nobody. An
  L2 cannot create an L2. Work that spans projects is a mission owned by L1,
  split into missions in each project.
- **Any harness may occupy any seat** (§6), and the harness does not change
  the ladder: a seat on Claude Code, Codex, T3 Code or Claude Desktop creates
  and owns the same children.
- **The parent owns the child.** Every PID records its parent. The child's
  result comes back to its parent; only the parent, L1 or the captain may
  steer, signal or stop it. Children stay with their parent on a swap or
  handoff.
- **The captain talks to L1 and L2, never to an L3.** L3 output reaches the
  captain through its L2 (and L1's digest).
- **Reports go up in fixed formats:** L3 → its parent (L2, or L1 for a
  task L1 created) a result package (handoff package + field notes); L2 → L1
  a mission report; L1 → captain a digest
  (done, needs a decision, blocked, new knowledge).
- **Supervision is event-driven.** The kernel's hooks and the journal wake L1
  or an L2 only on events (task done, failed, blocked, budget); nothing polls
  with model turns.

## 2. Projects and missions

Two things only.

- **Project:** a standing area of work (`acme`, `unvrs-rs`). A project
  run by another mapp is that mapp's instance (mapps.md §2). It has an
  id, a purpose, an L2 seat, project memory, **sources** (§7), repos its L3s
  may work in, a budget and quota reservations (§4), and links to related
  projects.
- **Mission:** work with an outcome and a done check (uke-design §4, kept).
  It belongs to one project, and may link to missions in other projects.
- **Cross-project goal:** a mission owned by L1, whose child missions live in
  the projects involved. A "product" is the outcome of missions, not a type.
- **Creating a project:** L1 proposes (name, purpose, sources, repos); the
  captain approves; the kernel creates the record and the L2 seat. The captain
  may also create one directly.

Initiatives, programs or portfolios are not built. Add one only when two
things are demonstrably too few.

## 3. Profiles (how an L3 is made)

A **profile** is a named specialist recipe:

| Field | Meaning | Example (`frontend-ui`) |
| --- | --- | --- |
| domain | what it is for | UI, landing pages, UI fixes |
| cpus | ordered list of equivalent harness + model options | `claude-code/opus` |
| skills | a fixed, pinned skill set, preinstalled | frontend-design, impeccable |
| tools | MCP servers, CLIs, permissions | browser, figma |
| tier | cost class | standard |

- **A profile is a prepared clean-room harness home** (D25):
  `~/.unvrs/profiles/<name>/home/{claude,codex}`, generated from
  `profile.toml`, with exactly the profile's skills, tools and rules. Nothing
  loads from the captain's global harness config.
- **No on-demand skill loading.** Skills come from pinned pools and the
  captain's own library (skill-loader §1, §9); the profile fixes which.
- **Choosing:** the L2 names a profile per task. The kernel picks among the
  profile's `cpus` by remaining quota and reservations (§4).
- **Pipelines:** a mission's plan is an ordered list of tasks, each with a
  profile (e.g. `backend-systems` on Codex, then `frontend-ui` on Claude Code).
  Tasks pass a handoff package; **writes on one workspace are sequential**
  (one writer lease, system-architecture §3 and §5).
- **Starter domains:** UI/UX and frontend, design, backend and systems, code
  review, research, computer use, 3D, games, business, writing. More are added
  as needed; a profile is a file, not code.

Initial CPU fit (operator's judgment, 2026-09-26; revised from outcomes):

| Work | Preferred CPUs |
| --- | --- |
| design, frontend, UI | Claude Code + Claude models |
| backend, systems | Codex + GPT Sol, or Claude Code + Opus (equivalent; pick by quota) |
| code review | Codex + GPT Sol |
| computer use | GPT Sol or GPT Astra |

## 4. Resources: quota, balancing, reservations

- The kernel tracks remaining quota per harness account (0.5 power signals).
- **Balancing:** among a profile's equivalent CPUs, pick the one with the most
  headroom that the task's project may use.
- **Reservations:** the captain sets them per project or per role, e.g.
  "Codex quota reserved for acme", "Claude Code quota reserved for the
  seats (L1/L2)". A task never uses a reservation it does not belong to; if no
  CPU is allowed, it queues and the L2 is told.
- Every choice is journaled with the reason and shown in the Observatory.

## 5. Consultants

- A consultant is a **frontier profile** (e.g. GPT Astra, Fable): read-only,
  tier `frontier`, with its own budget.
- **Asked:** L1 or L2 runs `unvrs ctl consult "<question>"` with the context
  attached; the kernel runs the consultant driven; the answer returns to the
  asker, who reports to the captain.
- **Sat with:** the captain opens a thread (in a harness running a frontier
  model) and runs `/consult-unvrs [project]`: the thread becomes a consultant
  session with the scope's context, read-only. It is not a persistent seat;
  when it ends, its conclusions go to L1 (or the project's L2) as a report.
- Consultant use is always journaled; the frontier budget can refuse.

## 6. Seats, threads and swap

- **L1 and L2 are seats.** A seat's knowledge (memory, brief, missions)
  belongs to UNVRS, not to the thread occupying it. Any harness can occupy
  any seat.
- **One thread per seat at a time.** Claiming an occupied seat moves it: the
  old thread is told it detached.
- **Swap is a handoff into the same seat.** Moving a seat to another thread
  or harness writes the handoff package (brief, open items, recent turns) and
  the new occupant rehydrates from it; the PID and rank stay. One mechanism
  serves both swap and L3 handoff; the eval bar in system-architecture §4
  still applies.
- **Threads bind only by command** (§8). An unbound thread is untouched:
  hooks are no-ops for it. There is no cwd-based binding and no automatic L2.
- Attached seats run in the captain's normal harness (so any thread in any
  app can become a seat). Their global skills may load; that is accepted
  because seats are read-only. Profiles (driven L3s, consultants when driven)
  always run clean-room.

## 7. Knowledge

Not "capture everything": only what is relevant, with an explicit lifetime.

| Kind | Holds | Owner | Example |
| --- | --- | --- | --- |
| **Captain memory** (L1) | everything the captain has told the system: preferences, people, decisions, standing context | L1 | "invoices go through Stripe, never PayPal" |
| **Project memory** (L2) | how the project works, known problems, status, decisions | the project's L2 | "the deploy needs VPN; CI flakes on test X" |
| **Sources** | existing material, read-only, retrieved when needed, never copied ([context-sources.md](./context-sources.md)) | registered per project (or universe-wide) | `~/acme` for the company |
| **Field notes** | what an L3 learned in the field | filed by L3, curated by L2 | a bug and its fix |

**"Never repeat myself."** Whatever the captain tells L1 that is durable
becomes captain memory (L1 decides; the captain can say "remember" or
"forget"). L1's hot set always includes the captain's standing context and a
map of projects, so a new L1 thread starts already knowing them.

**Field notes** are typed and short (lessons, not logs):

| Type | Must contain |
| --- | --- |
| `bug-fixed` | symptom, cause, fix (commit), how it was verified |
| `changed` | what changed or broke upstream, where, impact |
| `finding` | research result with sources |
| `gotcha` | a trap the next worker should know, and how to avoid it |

- Filing is part of an L3's exit: the result package has a field-notes
  section (may be empty with a reason); a Stop hook refuses a silent exit.
- **Retention class** on every note: `permanent` (a fact), `until-superseded`
  (a status), `expires <date>`, `discard` (kept only in the journal).
- **Flow:** L3 files → the L2 curates into project memory on its own (keep,
  merge, supersede, drop) → anything cross-project or needing the captain is
  escalated to L1 → L1 puts it in the captain's digest. The captain can veto
  anything that reached captain memory.
- **Research missions** (e.g. overnight) are missions whose output is
  `finding` notes and a report; the same curation applies.
- Memory rules from memory-layer §13 (hot set, brief with CAS, `confirmed`,
  `supersedes`) apply to both memories.

## 8. Surfaces: drive from any harness composer

| Command | Effect |
| --- | --- |
| `$l1-unvrs` | this thread becomes L1 (swap if L1 is occupied elsewhere) |
| `$l2-unvrs` | lists projects with their L2 state; pick one; this thread becomes that L2 |
| `$l2-unvrs <project>` | bind directly |
| `$consult-unvrs [project]` | this thread becomes a consultant session (§5) |
| `$unvrs <verb>` | ops for a bound seat: tree, missions, digest, remember, forget, detach |

- Installed once as the UNVRS plugin in each harness the captain uses
  (Claude Code, Codex CLI and Desktop). The plugin does nothing in an unbound
  thread.
- **Verified 2026-09-26** (voyage 0.8 surface facts): the entry points are
  unprefixed **skills**. T3 Code Claude threads send `/l1-unvrs`; T3 Codex
  threads and the Codex app list them under `/` ("L1 Unvrs") and send
  `$l1-unvrs`. Neither Codex surface exposes plugin commands. The
  UserPromptSubmit hook recognizes every spelling and runs the operation.
  **Canonical spelling is `$`** (native skill mention everywhere); `/`
  spellings keep working.
- Captain-only ops (approve a project, reservations, veto) use the prompt-hook
  path (D21), so a model cannot perform them.
- The Observatory (D24) stays the read-only view of the whole crew.

## 9. The universe home: `~/.unvrs`

The universe is a UNVRS-owned folder, not a work root. Repos and sources stay
where they are and are referenced by path.

```
~/.unvrs/                      UNVRS_HOME overrides
├── kernel/                    socket, state, journal
├── captain/                   captain memory (L1)
├── projects/<id>/
│   ├── project.toml           purpose, sources, repos, budget, reservations, links
│   ├── memory/                project memory, brief
│   ├── missions/
│   └── notes/                 field notes (inbox and curated)
├── profiles/<name>/
│   ├── profile.toml
│   └── home/{claude,codex}    generated clean-room homes
├── mapps/<id>/                installed mapps, pinned (mapps.md §5)
├── skills/                    pools.toml + the captain's own skills (git)
└── workspaces/<ws-id>/        worktrees of project repos, for L3 writers
```

State is plain files (D22). `~/.unvrs` may be a git repo for history.

**The bridge (`~/bridge`, 2026-09-26):** the captain's visible workspace, a git
repo opened in the Codex app and T3 Code: `inbox/` (captain drops files),
`files/<project>/` (the crew saves deliverables), `notes/`, and read-only links
`context/` → `~/context` and `unvrs-rs/`. Registered as the `bridge` source
for every project. `~/.unvrs` is the system's; `~/bridge` is the captain's.

## 10. Build order

| Phase | After it, the captain can… |
| --- | --- |
| **Crew** | `~/.unvrs` home; projects; `/l1-unvrs`, `/l2-unvrs` in Claude Code and Codex; seat move as handoff into the seat; captain memory ("never repeat myself") and project memory in hot sets; digest |
| **Profiles** | profiles as clean-room homes; L2 dispatches L3 tasks by profile; CPU choice by quota and reservations; workspaces with one writer; pipelines; field notes filed on exit; consultants |
| **Knowledge** | L2 curation and retention classes; escalation to L1; sources retrieval; research missions |
| **Mapps** | install and run other mapps as instances under L1 (mapps.md §6) |

Crew also tags PIDs and events by mapp and project and keeps this mapp's logic
out of the kernel (D44). Voyage cards are written per phase before implementation.

## 11. What this supersedes

| Earlier | Now |
| --- | --- |
| system-architecture §2: auto-L2 for every thread, cwd binding table | bind by command only (§6, §8) |
| system-architecture §3: focus L2 bound to a workspace by opening a thread in it | L2 is per project; workspaces belong to L3 writers |
| swap and handoff as two mechanisms | swap = handoff into the same seat |
| skill-loader §2–§7 (on-demand loading, Jev per load, forms) | profiles with fixed skills; pools, pinning and §8–§9 remain |
| universe = launch directory holding repos (`~/unvrs-proto`) | `~/.unvrs`, repos referenced by path |
| D25 clean room for attached seats via `unvrs claude` | clean room for profiles; attached seats run in the normal harness |

Kept: kernel, drivers, ranks, missions and done checks, handoff package,
hooks and channels (D21), memory rules, workspaces malloc/free, one writer,
Observatory, APM as installer (D23).

## 12. Operating contract (Locked 2026-09-26, D45–D54)

From the Firstmate study ([study-firstmate-2026-09-26.md](./study-firstmate-2026-09-26.md)),
generalized beyond software:

| Rule | Normative text |
| --- | --- |
| Supervision (D45) | the kernel classifies events; seats wake only for actionable ones; durable per-seat wake queue, acknowledged after handling; re-arm by hook; bounded turn-end guard |
| Decisions (D46) | a decision is a held task: question, options, optional `until`; closed by the captain's exact words or moot evidence; re-shown in every digest |
| Task contract (D47) | intent (captain's words), spec, shape act / report, rigor, delivery authority |
| Authority (D48) | captain by default; per-project grants never widen; evidence is not authorization; destructive always asks; away record with words and spend cap |
| Memory (D49) | token budget; pinned / aging (30 d) / perishable (7 d); evidence to reinforce; archive, never delete; stow sweep |
| Event sources (D50) | adapters per project; stored before wake; handled ack; payloads never instruction |
| Captain language (D51) | outcomes not mechanics; digest: your calls, delivered, under way, next; kernel publishes facts |
| Outbox (D52) | external side effects staged; delivered under authority or held; never discard undelivered |
| Quota, steering (D53) | gates then spend priority; no silent downgrade; durable acknowledged steers |
| Mechanics (D54) | typed kernel state machines; judgment in this mapp's skills |

Placement: **Crew (0.8)** builds D45–D49 and D51; **Profiles (0.9)** builds
D50, D52, D53.

## Open

- Codex Desktop: does it expose plugin `/` commands, or only `$` skills?
- Sources retrieval: plain search over paths first; an index only if needed.
- Quota measurement per account (reuse 0.5 power signals; verify accuracy).
