---
id: msn_coding_tdd
status: draft
investment: standard
owner_pid: null
updated: 2026-09-21
---

# Outcome
`unvrs ctl handoff` accepts a `deadline` field on the handoff summary: a TOON/JSON
summary carrying `deadline: 2026-10-01T12:00:00Z` validates, round-trips through the
kernel and shows up in the receiving seat's briefing. Built test-first: the failing
test for the new field is committed before the implementation that makes it pass.

# Done check
`cargo test --workspace` is green and includes a new test in
`crates/drv_hdff/src/lib.rs` named around `deadline` that fails when the field is
dropped from the parser. `cargo clippy --workspace --all-targets -- -D warnings` and
`cargo fmt --all --check` are clean. `git log -p` shows the test commit landing before
or in the same commit as the parser change.

# Scope
For the captain and any seat receiving a handoff: a deadline makes an inherited task
schedulable instead of open-ended.
In: the summary struct, its TOON and JSON parsers, validation (reject a deadline that
is not an RFC3339 timestamp), the briefing text.
Out: any scheduling or alerting behaviour, cockpit widgets, persistence changes.
Must not: break the existing 12-edge handoff matrix or change existing field names.

# Inputs
- prompt: Add an optional `deadline` field to the handoff summary, test-first. Write the failing parser/validation tests first, then the smallest implementation that makes them pass, then extend the briefing renderer.
- pointers: crates/drv_hdff/src/lib.rs, bins/unvrs/src/main.rs, behaviour-eval/scripts/handoff-matrix-smoke.py, docs/design/uke-design.md

# Log
…
