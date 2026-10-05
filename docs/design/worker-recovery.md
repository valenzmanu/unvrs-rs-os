# Driven worker recovery

Amends uke-design §9 and its recovery open question. No artificial budget ends an attempt or the task. The kernel checkpoints stopped L3 work, then continues it in place, under the same PID, with a fresh harness session and the saved state.

## Stop classification

| Signal | Action |
| --- | --- |
| No progress (`UNVRS_STALL_TURNS`, default 3, turns in a row leave brief and work-tree HEAD unchanged), hung or runaway turn, harness crash or reported non-quota error | Checkpoint and continue on the same harness and PID |
| Model or effort mismatch, including driver errors before a turn returns | Checkpoint and end without retry; preserve the pin |
| Harness unavailable | Checkpoint and block with the reason |
| Quota/rate limit | Existing quota handoff policy; a pinned task cannot silently substitute its model or effort |
| `UNVRS-RESULT` without a driver error | Finish and wake the lead |

There is no turn count limit. A turn times out only when the harness is silent for `UNVRS_TURN_IDLE_SECS` (default 1200) or runs past the runaway ceiling `UNVRS_TURN_MAX_SECS` (default 4 h). The driver must stop and reap its process group before returning a timeout or pin error. The bounded headless watchdog supplies that guarantee; installing recovery without it would allow overlapping writers.

If a Codex `turn/start` times out before the server supplies a turn id, the driver may return that timeout for same-PID continuation only after a post-timeout owned-terminal re-list is empty, command accounting is empty, and the app-server exits on its own after stdin EOF. It records `cleanup.verified` with that basis. If any check fails or the server does not exit within cleanup grace, cleanup is unconfirmed: the kernel blocks without checkpoint or continuation. Process-group kill and reap still run on every path. A process that detached from the group is outside this verification and needs separate observation.

## Checkpoint and missing brief

The kernel keeps the latest self-folded brief and locates `<cwd>/wt`, otherwise `<cwd>`. Only an actual repository root is eligible; the universe root and its ancestors are excluded. Dirty changes are committed on the current branch with `unvrs checkpoint: PID <n> stopped (<reason>)`; `target/` directories are excluded by pathspec, so build output is never staged or scanned. Git hooks, signing and fsmonitor are disabled for this internal checkpoint. Git runs outside the kernel mutex; each git command has its own budget (`UNVRS_CHECKPOINT_SECS`, default 600) and the job watchdog allows the whole sequence. A timeout kills and reaps its process group. The recovery flag prevents another handoff during checkpointing or summarization. A failed checkpoint blocks continuation and retains the files; it is never reported as success. Existing `REPORT.md` and only the latest checkpoint are recorded as artifact pointers. Nothing is pushed or merged.

When the brief never advanced and a transcript or report exists, the kernel calls `Drivers::recovery_summary(prompt)`. The prompt contains the current brief with item ids, bounded transcript and report, redacted before dispatch. The driver owns ordered, bounded subscription routes configured by `UNVRS_SUMMARY_ROUTES`; workers' model and effort pins remain unchanged. No API-key fallback belongs to this interface.

The reply must contain `UNVRS-PROGRESS`. The kernel validates that open items survive and applies it only if the saved brief version still matches. On failure, the stale brief and inherited transcript remain available to the continuation. Recovery summary routing and process restrictions are supplied by the subscription-summary driver implementation; the default trait implementation fails explicitly.

## Continuation and limits

A recovery continuation keeps the PID the captain sees, with its contract, model, effort, harness, working directory, scope, brief and transcript. Only the harness session, turn count and per-turn stall counter reset; `attempt` counts continuations and each one writes an audit package to `handoffs/pid<n>-attempt<k>-<ms>.json` and a `continue` journal event. The first prompt of the new session says “Resume; do not restart,” names the checkpoint, report and next step, and requires checkpoint commits plus a current report after meaningful steps.

Continuations are unbounded by default. `UNVRS_MAX_CONTINUATIONS` (or a lineage's `max_continuations`) sets an optional ceiling; `none`/`unlimited` or unset means none. Quota and manual handoffs change harness, so they still start a new PID linked by `handed_from`/`handed_to`, with `lineage` naming the original worker. Manual handoff refuses while a driver is busy; its child must finish or time out and be reaped before a successor can start. The low-quota signal retains its deferred handoff path. These handoffs carry the same lineage, counters, ceiling and history and consume a continuation; an effort pin retains its original harness. Two successive continuation attempts with unchanged brief content and commit also block the lineage. Progress compares the content of `now`, `next`, `open` and `done`, not list lengths. Checkpoint/report artifact bookkeeping does not manufacture progress.

Only a completed task or a blocked/non-retryable lineage wakes the lead. The blocked result names the reason, every attempted PID/stop/turn count, last checkpoint and next step. Journal events `stop`, `checkpoint`, `continue`, `fold` and `blocked`, plus `result.json` and driven snapshot fields, expose the lineage to the Observatory.

## Validation and deployment boundary

`cargo test -p uke kernel::recover -- --nocapture` runs the kernel with a scripted driver: a progressing worker running past the old turn budget; no-progress stop, dirty-tree commit (without `target/`) and in-place continuation to a result; timeout and crash followed by success; missing-brief summary; failed-summary transcript fallback; two stalls; unbounded continuations unless a ceiling is set; model/effort refusal; reported errors; failed git commit; and universe-root exclusion. `cargo test -p drv_agent` covers the idle and ceiling turn timeouts.

Workspace checks and exact results belong in the task report. The route driver and headless watchdog must be integrated and tested together before installation. On startup, a persisted headless L3 marked busy or recovering is blocked and wakes its lead. Its files, brief, OS PID and recovery history are retained. The kernel does not kill an unverifiable process or automatically create a successor; the lead must verify that the prior process group is gone before dispatching a continuation. Ordinary stopped `working` records remain eligible for the scheduler.
