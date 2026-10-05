# Skill loader

**Status:** Locked direction 2026-09-25 (operator). Build: loader v0 in the
real-work readiness phase, v1 after v0 has data
([voyages/PROTOTYPE-PLAN.md](./voyages/PROTOTYPE-PLAN.md)).
**Why:** [decisions-2026-09-24-v1.md](./decisions-2026-09-24-v1.md) D25, D26.
**Superseded in part 2026-09-26** (D31, [mapp-unvrs.md](./mapp-unvrs.md) §3):
L3s are built from **profiles** with fixed skills; on-demand loading, per-load
Jev decisions and forms (§2–§7) are not built. Still valid: pools and pinning
(§1), changing skills safely (§8), source of truth (§9).
**Builds on:** uke-design §1 goals and §10 nesting; memory-layer §13 R3 (notes
vs skills) and R5 (`supersedes`); D5 (behaviours owned like memory); D23 (APM
installs); D25 (clean-room harness homes); the 0.5 DrvBoot picker
(`uke/src/boot.rs`: rules + Jev).

## Purpose

Load the skills a task needs, so the work reaches the highest quality at the
lowest cost. Uke's two goals applied to procedural memory: quality at least the
minimum, fewest resources.

## Shape

```
 TRUSTED POOLS ──► INDEX ──► LOADER (per task) ──────────────► SEAT
 pinned sources    purpose,    WHAT:  rank candidates             as-is
 the captain       triggers,   WHEN:  before start · on demand ·  distilled (cache)
 approved          size,              trigger moments             adapted (owned)
                   harness     HOW:   as-is / distilled / adapted
                   fit, record        ▲
                                      └── outcome feedback (done check, reverts, rework, cost)
```

## 1. Trusted pools

- A registry of sources the captain approves (e.g. `mattpocock/skills`,
  Anthropic's public skills, the captain's own library in the universe). Each
  pool is **pinned to a commit and hashed**. Nothing unpinned loads.
- Skills that ship executables are flagged.
- The **index** holds per skill: purpose and triggers (from `SKILL.md`
  frontmatter), size in tokens, harness fit, and its track record (§6).
- The universe library (`<universe>/.unvrs/behaviours/skills/`) is a pool too:
  the captain's own and adapted skills (§5).

## 2. What to load

- Candidates are ranked against the task: mission outcome and done check,
  seat rank, scope and harness, budget, and what helped on similar tasks.
- **Budget per rank.** L3 workers get only what their task needs. L1 sees
  descriptions. Nothing exceeds `NEST_CAP` without a reason in the journal.
- Rankers: Jev (§4) with the rules ranker as fallback (§7).

## 3. When to load

| Moment | Mechanism |
| --- | --- |
| **Before start** (bind, spawn, focus) | install into the clean-room harness home (D25) via APM (D23); the hot set names what is active |
| **On demand** | the seat runs `unvrs ctl skill find "…"` and `unvrs ctl skill load <name>`; the kernel returns the skill content as a tool result |
| **Trigger moments** (v1) | hooks see the moment (tests failing, about to commit, handoff) and suggest or load a skill |

Harnesses discover skills at session start, so "before start" is install and
"mid-session" is content returned by the kernel. For attached seats sharing a
clean-room home, per-seat activation is through the hot set and on-demand
loads, unless a spike shows per-seat homes are cheap.

## 4. Jev: the judge for every loader decision

Jev (TypeSafe System One) answers fast, calibrated judgments: `noul` (a
calibrated yes/no score) and `choice` (a closed set with probabilities and
confidence). Jev decides; a generating model (Luna) only writes distilled or
adapted text.

| Decision | Jev question | Type | Version |
| --- | --- | --- | --- |
| Need | "Would a skill materially help this task?" | noul | v0 (exists) |
| What | "Which of these skills fit this task?" multi-select from probabilities, capped by budget | choice | v0 (extend the single pick) |
| On demand | the seat's query: lexical shortlist, Jev picks | choice | v0 |
| How | "As-is, distilled, adapted, or skip?" given size, harness, repo | choice | v1 |
| When | at a trigger moment: "does this moment call for skill X?" | noul | v1 |
| Derived gate | "Does this derived version keep the source's method for this kind of task?" | noul | v1 |
| Did it help | after the work: "given outcome and brief, did skill X help?" | noul | v1 |

- Thresholds (`noul_min`, `confidence_min`, `probability_min`) are the
  quality / cost dial. Every answer is journaled with the outcome it led to, so
  thresholds are tuned from data.
- Closed sets only: Jev can choose only trusted pool entries.
- Jev runs at boundaries (spawn, bind, handoff, failures, commits), never on
  every turn. Task briefs sent to Jev are redacted like every other payload.

## 5. How to load: forms

| Form | What | Lifetime | When |
| --- | --- | --- | --- |
| **as-is** | the pinned skill unchanged | pool | default; harnesses load the body lazily, so an unused skill costs about its description |
| **distilled** | a model condenses a long skill to the slice relevant to a kind of task | **cache**: keyed by source hash and task kind; regenerated when the source changes; never supersedes | large skills where a slice matters |
| **adapted** | rewritten for a repo, area, harness or model | **owned skill** in the universe library; may supersede the original in its scope | a generic skill that keeps misfiring somewhere |

Rules for every derived form:

- Provenance in frontmatter: `derived_from: <pool>/<skill>@<hash>`, the form,
  the scope, the evidence.
- A derived skill may not add commands or steps the source does not have
  (checked before use).
- A pool's original is never edited.

### Adapted skills supersede per scope

- An adapted skill starts as a **candidate**: used only in trials.
- It **supersedes** the original (`supersedes: <pool>/<skill>`, `scope: …`)
  when it did at least as well on the same kind of task **and the captain
  approves**. The loader proposes; the captain decides, like pins. The Jev
  derived gate is part of the evidence.
- In its scope the loader uses the adapted skill; elsewhere, the original.
- **Upstream drift:** when the pool's pinned hash changes, the adapted skill is
  flagged "source changed". The loader keeps using it, asks Jev whether the
  change matters, and suggests re-adapting if so.

## 6. Feedback

- **Every load is journaled:** PID, skill, form, bytes, decision tier (§7),
  Jev scores, reason. The Observatory shows loads.
- **Outcome signals:** done check pass, captain revert, rework and handoffs,
  tokens spent.
- Signals rank future choices, tune the Jev thresholds, and decide promotion of
  adapted skills. Nothing is promoted on a guess.

## 7. Fallback when Jev is unavailable

Tiered, always degrading to cheaper and safer, never to riskier:

```
Jev ──(down / slow / no key)──► decision cache ──(miss)──► rules ranker
```

- **Decision cache:** every Jev answer is stored by (task signature,
  candidate-set hash) and reused for a repeat decision, which also saves cost
  when Jev is up.
- **Rules ranker:** the existing lexical ranker; always available,
  deterministic.
- **Circuit breaker:** after repeated Jev failures, skip Jev for a cooling
  period so a spawn never waits on a timeout.

| Decision | Without Jev |
| --- | --- |
| Need / what | cache, then rules with a smaller cap |
| On demand | lexical search only |
| How | **as-is**; never transform without a judge |
| When (trigger moments) | skip |
| Derived gate | **do not promote** |
| Did it help | queue; label when Jev returns |

The journal records which tier answered each decision.

## 8. Changing skills safely (and what we do not automate)

Studied 2026-09-25: the Hermes agent's **self-improvement loop** (NousResearch,
open source): a background review every ~10 tool calls or user turns that
creates and patches skills, and a weekly curator that marks stale, archives and
optionally consolidates. It never measures whether a skill helped, edits
upstream skills in place (they silently stop receiving updates), and its
"be active" review prompt produced skill bloat that the curator must clean up.

Adopted in **v1** (cheap; each prevents a failure Hermes hit):

- **Ownership separate from origin.** Origin: pool / captain / adapted. Only
  owned skills are ever changed; pool skills never.
- **Ledger with rollback.** Every adapt, supersede and archive is an
  append-only journal entry with before / after content by hash; a single entry
  can be rolled back.
- **Read-before-write.** A job that adapts a skill must load the exact current
  source version first, and applies compare-and-swap on it (D22).
- **Archive, never delete.** Superseded and stale skills stay recoverable.
- **"Lessons, not logs"** for adapted and distilled text: an imperative rule
  plus one clause of why; no incident ids or transcripts; each lesson once. A
  distilled skill uses a lean `SKILL.md` index plus reference files.

**Adaptation on request, suggestions on repeated signals (v1).** The captain
asks ("make this a skill", "adapt this skill for this repo"), or the loader
suggests one only when a clear signal repeats (the same gotcha in several
briefs, a skill the captain keeps overriding). Every change still passes §5's
candidate → evidence → approval path.

**Not built: an automatic background self-improvement loop.** At one
captain's scale there is too little data to tell whether an automatic change
helped, an always-on loop creates churn and review load, and Hermes shows it
then needs a curator. Revisit only if loader data shows repeated patterns worth
automating.

## 9. Source of truth for skills (reference, parked)

**Status:** reference, not locked. Written 2026-09-26, then parked while the
captain explores the "universe" meta app direction. Revisit before loader v0.

Inventory on 2026-09-26: about 70 skills in four places with duplicates
(`~/.agents/skills` 30, `~/.codex/skills` 36 mostly `mattpocock`,
`~/.claude/skills` 6 with symlinks into `coke-interop` and `acme`,
`~/.hermes/skills` ~25 categories). `coke` alone appears in four places.

Rule, same as memory: **one record; every harness copy is a cache.**

```
<universe>/.unvrs/behaviours/
├── pools.toml   the registry: every trusted source, pinned
│                (the 0.5 format in uke/src/pool.rs: git | path, ref, root, overlays)
└── skills/      the captain's own and adapted skills (a git repo, for history)
generated, never edited: apm.yml + lock → clean-room homes (D25)
```

| Kind of skill | Source of truth |
| --- | --- |
| Upstream (`mattpocock`, Anthropic) | the upstream repo at the commit pinned in `pools.toml` |
| Lives in a project repo (acme, coke-interop) | that repo, registered as a `path` pool; no copies |
| The captain's own | `.unvrs/behaviours/skills/`, in git |
| Adapted | the same library, with `derived_from` and `supersedes` (§5) |
| `~/.claude/skills`, `~/.codex/skills`, `~/.agents/skills`, clean-room homes | cache; a new skill appearing there is an inbox item, imported once |

Draft starter set (from names only; confirm with a one-time inventory the
captain approves):

| Group | Skills | Where |
| --- | --- | --- |
| UNVRS core | `unvrs` (plugin) | every seat |
| Engineering method (`mattpocock`, pinned) | tdd, diagnose, zoom-out, improve-codebase-architecture, review, to-prd / to-issues / triage | workers and focus seats |
| Needs the captain in the loop | grill-me, grill-with-docs | L1/L2 only |
| Captain's workflow | no-mistakes, split-to-prs, new-repo, frontier-efficient-work | engineering scopes |
| Project-specific | hermes-vps-oneshot, vorc-landing-page-framework, … | scoped to that repo |
| Writing | edit-article, writing-*, m-effective-comm | writing area |

Leave out: skills that duplicate or fight UNVRS (`handoff`, `coke`, `loop`,
`autopilot`, `unvrs-t3-control`), harness admin skills (`create-hook`,
`create-rule`, `statusline`, `update-cursor-settings`), and Hermes's own
library unless a specific skill is needed.

## Phasing

| Version | Builds |
| --- | --- |
| **v0** (readiness phase) | pool registry and index; loader wired to v1 seats (spawn, focus, bind); Jev need + multi-select what; on-demand `skill find` / `skill load`; as-is only, installed into clean-room homes and on demand; fallback tiers and circuit breaker; every load journaled and shown in the Observatory |
| **v1** (after v0 data) | forms: distilled cache and adapted candidates; Jev how / when / derived gate / did-it-help; supersede with captain approval; upstream drift; outcome signals feeding ranking and thresholds |

## Open

- Initial trusted pools beyond `mattpocock/skills` (pinned in 0.5).
- Per-seat clean-room homes versus one shared home per harness (spike).
- Task signature for the decision cache (mission id, normalized brief, scope).
