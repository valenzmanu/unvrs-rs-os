---
id: msn_bad_no_done_check
status: draft
investment: standard
owner_pid: null
updated: 2026-09-21
---

# Outcome
The cockpit feels faster when a lot of log lines arrive at once: scrolling stays
smooth while OBS is streaming.

# Done check
TODO

# Scope
For the captain watching a busy flight.
In: the OBS log view render path.
Out: the COMMS pane, recorder.

# Inputs
- prompt: Make the log view feel faster under load.
- pointers: crates/drv_obs/src/lib.rs

# Log
…
