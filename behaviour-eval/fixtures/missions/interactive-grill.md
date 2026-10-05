---
id: msn_interactive_grill
status: draft
investment: frontier
owner_pid: null
updated: 2026-09-21
---

# Outcome
A written plan for the Econ driver that the captain has been interrogated about
before any code is written: the agent grills the captain question by question —
who spends, what a budget overrun does to a running mission, what is out of scope —
and only once the captain has answered does `docs/design/econ-plan.md` exist,
capturing the decisions in the captain's own words plus the open questions that
remain unresolved.

# Done check
`docs/design/econ-plan.md` exists and contains an "Answers from the captain" section
with at least six question/answer pairs the captain actually responded to in the
session, and an "Open questions" section. A stranger reading it can say which
decisions are settled and which are not, without reading the transcript. No source
file outside `docs/` is modified.

# Scope
For the captain, who wants their own thinking stress-tested before committing power
to an Econ driver.
In: an interactive interview with the captain (one question at a time, follow-ups on
vague answers), then the written plan.
Out: any implementation, crate scaffolding, or cost model code.
Must not: guess or invent the captain's answers, or write the plan from assumptions
when the captain has not replied — this mission is worthless without live captain
turns, so a seat with no captain channel must refuse it rather than fake it.

# Inputs
- prompt: Grill me about the Econ driver before you plan it. Interview me one question at a time, push back on hand-waving, and only then write up the plan and the open questions.
- pointers: docs/design/uke-design.md, docs/design/voyages/0.5.0-eval.1.md

# Log
…
