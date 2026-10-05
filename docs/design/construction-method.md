# BSLA v0 — Build, Ship, Learn, Adapt

**Status:** v0 — living methodology. Refine as we build UNVRS; do not treat this as finished law.

**BSLA** is a simple loop for building almost any *thing* with AI (software, products, processes, subsystems). AI does most of the making. Humans decide what matters, how hard to check, and whether we still understand the dangerous parts. The loop stays the same; ceremony scales with how bad being wrong would be.

```
  Intent
     │
     v
  BUILD  →  Design?  →  Eval plan  →  Implement  →  “the thing”
     │
     v
  SHIP   →  put it where Learn can happen
     │
     v
  LEARN  →  Product Learn  +  Human Learn?
     │
     v
  ADAPT  →  next Intent / Build
```

This is process, not product implementation.

---

## Stance

| Principle | Meaning |
| --- | --- |
| AI as labor | Default: AI implements; humans steer |
| Human as quality | Humans own intent, criticality, ship depth, Learn judgment, Adapt |
| Adaptive ceremony | Skip or lighten steps when risk is low; harden when failure is expensive |
| Evidence over vibes | Adapt from written Learn, not silent chat opinion |
| No forced theater | Empty templates and unused gates are a methodology bug |

---

## Prerequisite — Intent

You need a **fuzzy idea** of what you want. No idea → no BSLA.

Turn it into a short **Intent**: what the thing is, who it’s for, what “good enough” roughly means. One sentence can be enough.

---

## 1. Build

Three parts. Only Implement is always required in full; Design and Eval plan scale.

### 1a. Design (optional)

Shape the thing: architecture, interfaces, constraints, safety rules.

| Do it when | Skip when |
| --- | --- |
| Failure is costly (safety, money, security, long-lived core) | Landing page, vibe prototype, throwaway demo, one-off script |

Engineering when it matters — not vibing by default. Vibing is fine for low-stakes work.

### 1b. Eval plan (always; light → hard)

**Before** implement, decide how you’ll know the thing worked. Not “only unit tests.” Ask:

- Does it behave as expected?
- Does it fail safely / meet the minimum bar?
- Who checks it — us, automation, real users?
- How deep is Ship for this trip (local / staging / users)?

| Mode | What “eval” means |
| --- | --- |
| **Light** | Manual smoke: “does it do the job?” |
| **Standard** | Behavior checklist + automated checks that matter |
| **Hard** | Stronger suite; **independent checker ≠ implementer**; clear bar before wider Ship |

### 1c. Implement

Produce **the thing**. AI usually owns this. End of Build = a concrete artifact, not a plan.

---

## 2. Ship

Put the thing somewhere Learn can happen: local run, staging, or real users.

Ship is not always public launch. Depth matches risk and the eval plan. Learn evidence can decide whether the *next* Ship is wider (promotion gate, kept thin).

---

## 3. Learn

Two lanes. Product Learn is always on (scaled). Human Learn is optional.

### 3a. Product Learn (always)

Run the eval designed in Build. Add useful extras: bugs, surprises, logs, benchmarks, user comments.

Output: written notes — what worked, what failed, what to change.

### 3b. Human Learn (optional)

Ask: **does a responsible human understand the parts that matter?**

It does not matter whether AI or a human wrote the code. If something goes wrong, an engineer must be able to **reason** about the system — not depend entirely on a rented model that may be gone, wrong, or unavailable.

Example: a simple brake signal that actuates brakes. Code may be small; failure is catastrophic. Someone in charge must own the critical path (signal → actuate → fail-safe).

| Do Human Learn when | Skip when |
| --- | --- |
| Safety, irreversible harm, money, security boundary, core you’ll operate without the agent | Throwaway prototype, marketing page, disposable script |

Not “understand the whole system.” Yes “own the critical path”: invariants, failure modes, how to verify, how to stop or roll back. Proof can be light (walkthrough, teach-back, explain without the chat log).

---

## 4. Adapt

Use Product Learn (and Human Learn if done) to decide what changes next: the thing, the design, the eval, or the intent. Then start the next Build with that evidence. Do not silently redefine success after the fact.

---

## Adaptive dials (same loop, different weight)

| Situation | Dial down | Dial up |
| --- | --- | --- |
| Landing page / quick prototype | Design, Human Learn, hard gates | Light eval, fast Ship |
| Internal tool | Formal review theater | Smoke + learn from use |
| Safety-critical / high blast radius | Vibing | Design, Hard eval, independent check, Human Learn, staged Ship |
| First exploration of an unknown | Big public Ship | Local Ship + discovery Learn |
| Repeat trip on a known base | Rebuilding everything | Reuse last eval; Adapt the delta |

**Rule of thumb:** ceremony tracks how bad being wrong would be.

---

## Roles

| Who | Owns |
| --- | --- |
| **Human** | Intent, criticality, Design when needed, Eval plan, Ship depth, Learn judgment, Human Learn on critical paths, Adapt |
| **AI** | Most of Implement; may draft Design / Eval / Learn notes for humans to accept or reject |

---

## Done for one trip

1. Intent was clear enough to build.  
2. The thing exists.  
3. It was Shipped somewhere real enough to Learn.  
4. Product Learn ran (and Human Learn if stakes required it).  
5. Adapt is written — what changes next — or we stop because intent is satisfied.

---

## One sentence

**BSLA = Intent → Build (Design if needed → Eval plan → Implement) → Ship → Learn (product, and human understanding when it matters) → Adapt — sized to risk, so AI can move fast without leaving you blind.**

---

## UNVRS instance (how this repo uses BSLA v0)

UNVRS applies BSLA with **voyages** (named trips) and eval SemVer. That packaging is optional for other projects; it is how *this* yard tracks trips.

| Element | UNVRS mapping |
| --- | --- |
| Intent / trip contract | **VC** (voyage card) in [voyages/](./voyages/) — `X.Y.Z-eval.N.md` |
| Standing drawings | [uke-design.md](./uke-design.md) |
| Build yard | `behaviour-eval/` (and later kernel crates) |
| Product Learn | Tests, smoke, journals, EVALUATION, auto section of `LEARN-X.Y.Z-eval.N.md` |
| Human Learn | Captain / engineer notes in LEARN (competence on critical paths when required) |
| Adapt | Written change orders → next VC / design amend / promote |

### Versioning (Locked for UNVRS)

| Clock | Form | Example |
| --- | --- | --- |
| Eval / Learn voyage | SemVer pre-release `X.Y.Z-eval.N` | `0.1.0-eval.1` |
| VC file | `docs/design/voyages/X.Y.Z-eval.N.md` | |
| LEARN summary | `LEARN-X.Y.Z-eval.N.md` beside the artifact | |
| Promoted artifact | Drop `-eval.N` when ready | `0.1.0` |
| Cargo package | `X.Y.Z` unless crate pre-release required | Maps to voyage until promote |

No parallel `v1`/`v2` voyage counters. Folder names like `bridge-v0.1` are artifact labels, not voyage ids.

### Yard rules (UNVRS)

1. No Build without a VC when running a named voyage.  
2. One primary question per voyage.  
3. Name deliberate limits on the VC.  
4. No silent expansion of authority — Adapt design or hull; don’t redefine success after the fact.  
5. Cheap trials before expensive pours.  
6. Journals private by default.  
7. CLI is [AXI](https://axi.md/)-compliant.

### Current yard state

| Element | Status |
| --- | --- |
| Methodology | **BSLA v0** (this doc) — refine while building UNVRS |
| Design drawings | `uke-design.md` |
| Active eval | [0.3.0-eval.1](./voyages/0.3.0-eval.1.md) · Learn **partial** · [adapt-0.3.0-eval.1.md](./adapt-0.3.0-eval.1.md) |
| Adapt (from 0.3) | Feel + session restore; slash `/`; logs search; Galaga graphics; images deferred |

### Related

- [uke-design.md](./uke-design.md) — what the ship must *be*  
- [voyages/](./voyages/) — VC files + [TEMPLATE.md](./voyages/TEMPLATE.md)  
- `behaviour-eval/` — sea-trial bay  
- `legacy/` — retired scaffold (reference only)
