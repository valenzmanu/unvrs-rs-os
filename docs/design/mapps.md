# Mapps (meta apps)

**Status:** Locked direction 2026-09-26 (operator). Normative for what a mapp
is and what the kernel owes every mapp. Replaces the direction in
[uke-design.md](./uke-design.md) §17.
**Why:** [decisions-2026-09-26-mapps.md](./decisions-2026-09-26-mapps.md)
D39–D44.
**Now:** only the `unvrs` mapp is built ([mapp-unvrs.md](./mapp-unvrs.md)).
Everything here is the shape it must fit, so other mapps can be added without
redesign.

---

## 0. What a mapp is

A **mapp** is a program for UNVRS whose runtime is agents. It declares what it
is for, the roles it needs, the profiles its workers use, the missions it runs
and when, what memory it keeps, what it may touch, and what it may spend. The
kernel runs it on harness CPUs. A mapp is to UNVRS what an app is to an
operating system.

**UNVRS, as the captain meets it, is the `unvrs` mapp:** the mapp you use to
talk to the system and drive it. Other mapps do one specific thing (run social
media, do recurring work, BSLA voyages) and run alongside it, operated through
it.

```
 CAPTAIN ── any harness thread (/l1-unvrs, /l2-unvrs, /unvrs …)
     │
 ┌───▼──────────────────────── MAPPS (userspace) ──────────────────────────┐
 │  unvrs  (mapp #0, the shell)   social           recurring-ops      …   │
 │  L1, projects, commands,       one instance     one instance            │
 │  digest; operates the others   per brand        per routine             │
 └──────────────────────────────▲─── mapp interface (§4) ──────────────────┘
 ┌──────────────────────────────┴── uKe kernel ─────────────────────────────┐
 │ processes and seats · handoff · memory · workspaces · scheduler ·        │
 │ quota and budgets · capabilities · events · journal                      │
 └──── drivers: harnesses (Claude Code, Codex, …) · services ───────────────┘
```

## 1. What a mapp is made of

A mapp is a **package of files**, pinned by git ref or path like a skill pool,
with a `mapp.toml` manifest:

| Part | Declares | Example (`social`) |
| --- | --- | --- |
| identity | id, version, purpose | `social` 0.1, "run a brand's social accounts" |
| lead | the L2 role: prompt, hot-set rules, what it reports | "social lead: plans the week, approves posts" |
| profiles | the L3 specialists it needs (mapp-unvrs §3 format) | `copywriter`, `designer`, `computer-use` |
| missions | templates with outcome and done check | `weekly-plan`, `daily-post`, `reply-mentions` |
| schedules | when missions start: time, event | `daily-post` at 09:00; `reply-mentions` on webhook (later) |
| memory | what it keeps and for how long (retention classes) | brand voice (permanent), campaign status (until superseded) |
| sources | what it reads | brand folder, past posts |
| capabilities | what it may touch; granted by the captain | network `x.com`, secret `X_TOKEN`, no repos |
| budget | spend limits and CPU reservations | 2% of Claude quota per day |
| commands | verbs the captain can use through `/unvrs <mapp> …` | `social pause`, `social draft` |

No mapp code runs inside the kernel. The compute is harness CPUs running the
mapp's roles and profiles; the kernel only schedules, isolates and records.

## 2. Instances: how a mapp runs

- **Installing** a mapp adds the package; **starting** it creates an
  **instance** (one mapp can run several, e.g. one per brand).
- **An instance is a project** (mapp-unvrs §2) marked `mapp = <id>`. Its lead
  is an L2 seat defined by the mapp; its workers are L3s from the mapp's
  profiles. The crew stays three levels with **one L1**: the captain never
  gets a second chief of staff.
- **Instances run on their own.** The kernel scheduler starts missions from
  the mapp's schedules; the lead wakes on events; nothing waits for the
  captain to be attached.
- **Operated through the `unvrs` mapp.** L1 sees every instance and puts its
  items in the digest; the captain sits in an instance's lead with
  `/l2-unvrs <instance>`; lifecycle through `/unvrs mapp list | install |
  start | pause | stop | remove`.
- A **plain project** is a project with no mapp: its L2 is the generic
  project lead from the `unvrs` mapp.

## 3. The `unvrs` mapp is mapp #0

- Always installed, exactly one, cannot be removed (like an OS shell).
- **Only it** owns the captain's surface (binding threads, the L1 seat, the
  digest) and may operate other mapps. Installs, capability grants and
  upgrades that add capabilities need the captain's approval through it.
- Other mapps do not bind threads or talk to the captain directly. They reach
  the captain through L1 (escalations, digest items) and are reached through
  `/l2-unvrs` and `/unvrs <mapp> …`.
- It is built on the **same manifest and kernel interface** as any mapp, so
  building it proves the interface. Its one privilege is the captain-authority
  path (D21).

## 4. What the kernel owes every mapp

| Service | Kernel provides |
| --- | --- |
| **Lifecycle** | install (pinned), start, pause, stop, upgrade, remove; each journaled; remove archives state, never deletes |
| **Isolation** | each instance has its own memory scope, missions, notes and workspaces; reading another instance's state only through L1 or an explicit link |
| **Capabilities** | declared in the manifest, granted by the captain; enforced through the profile's clean-room home and hooks (best effort, as with the role sandbox) |
| **Resources** | budgets and CPU reservations per instance (mapp-unvrs §4); over budget, the mission queues and the lead is told |
| **Scheduling** | time schedules first; events and webhooks later |
| **Events** | instance → L1: escalations and digest items, in one format |
| **Attribution** | every PID, load, spend and write tagged with its mapp instance; the Observatory groups by instance |

## 5. Where mapps live

```
~/.unvrs/
├── mapps/<id>/            installed package, pinned, read-only
│   └── mapp.toml
└── projects/<instance>/   an instance is a project (project.toml: mapp = <id>)
```

## 6. Now and later

| When | Builds |
| --- | --- |
| **Crew (0.8)** | PIDs and journal events tagged with mapp and project; the `unvrs` mapp's logic in its own module, outside the kernel (D44) |
| **Profiles (0.9)** | profiles belong to a mapp; the `unvrs` mapp ships the starter profiles |
| **Mapps (0.11)** | `mapp.toml` and registry, designed from two real mapps; install and start other mapps, instances, time scheduler, capability grants, per-instance budgets; a first small mapp (recurring work) as proof |

## Open

- How strongly capabilities can be enforced per harness (network, secrets).
- Mapp-to-mapp communication: only through L1 for now.
- Distribution: pinned git refs first; whether APM packages mapps too.
- Code-running mapps (own binaries or services): not in scope.
