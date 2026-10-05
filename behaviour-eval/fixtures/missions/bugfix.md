---
id: msn_bugfix
status: draft
investment: standard
owner_pid: null
updated: 2026-09-21
---

# Outcome
Restoring a docked flight no longer loses the COMMS scrollback: after `unvrs` is
quit and relaunched in the same directory, the conversation pane shows the same
messages it showed before the quit, in the same order, with the seat that sent each
one still attributed. The root cause is identified in the fix commit message, not
just patched at the symptom.

# Done check
`python3 behaviour-eval/scripts/restore-smoke.py` passes, and a new assertion in it
fails against the pre-fix binary (verify by stashing the fix). `cargo test --workspace`
green. Manually: run `unvrs --demo`, send two messages, quit with F10, relaunch — both
messages are present with their original authors.

# Scope
For the captain, who loses context every time the cockpit is restarted mid-voyage.
In: session save/restore of the COMMS log, ordering and author attribution, the
restore smoke script.
Out: compaction or truncation policy for very long logs, OBS log persistence,
cross-directory session sharing.
Must not: change the on-disk session schema in a way that makes older
`.unvrs/session.json` files unloadable — degrade gracefully instead.

# Inputs
- prompt: Diagnose why COMMS scrollback disappears after quit and relaunch, then fix it. Reproduce first with the restore smoke, state the root cause, add a regression assertion, then fix.
- pointers: crates/drv_intf/src/bridge.rs, behaviour-eval/scripts/restore-smoke.py, .unvrs/session.json

# Log
…
