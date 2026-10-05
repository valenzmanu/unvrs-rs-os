# Kernel gaps

Tracked failures of uKe against the design ([uke-design.md](./uke-design.md),
[mapp-unvrs.md](./mapp-unvrs.md), [worker-recovery.md](./worker-recovery.md)). Every
run that finds a failure adds or updates a row here; a row closes only when its check
passes on a commit.

Source: PID 39 audit of `fix/2026-09-29-functional-kernel` @ 6bb7bcf
(`~/.unvrs/projects/unvrs-rs/tasks/pid-39/REPORT.md`), with the slice plan S1–S8.
Repros: `uke/src/kernel/audit_repro.rs`. A repro whose slice has not landed is
`#[ignore]`d with that slice's name. Run them all with
`cargo test -p uke --lib audit_repro -- --include-ignored`.

Status, only as verified:
- **fixed**: the check passes on the named commit.
- **reproduced**: a test fails today.
- **characterized**: a test pins today's behaviour.
- **verified**: checked against live data, not a test.
- **code-read**: seen in code, not run.
- **reported**: observed by someone, not yet confirmed here.

| id | gap | evidence | fix (slice) | check | status |
|---|---|---|---|---|---|
| C1 | `quota-low` ran before the kernel resolved the caller, so any local process could stop and hand off another PID's worker. It also promised a harness move to pinned workers. | ops.rs:97-106, driven.rs:736-771 @6bb7bcf | S1: resolve the caller first. Only the captain, L1 or the target's parent may signal; anyone else is refused and a `refused` event is journaled. Pinned workers are told the task ends. | `audit_repro::repro_c1`, `s1_quota_low_owner_rule`, `s1_quota_low_pinned_wording` | **fixed** in 810c174 |
| C2 | A lead cannot steer its running L3: `send` refuses L3 targets and nothing writes the mailbox the worker reads. | ops.rs:449-452, state.rs:349, driven.rs:290 @c566430 | S4: `send <pid>` to an L3 is allowed from its lead or L1 (else refused, journaled `refused`), follows `handed_to` to the live successor, refuses an ended worker, writes `State::mail` and journals `mail`. The worker reads unread mail at the start of its next turn, turn 0 included (it was marked read unseen), and journals `mail.delivered`. Unread mail moves to a handoff or continuation successor. `send` to seats is unchanged (scope: S8, C12). | `audit_repro::repro_c2`, `s4_send_*`; `recover::tests::lead_mail_*` (scripted prompts) | **fixed** in 0c9aeac |
| C3 | A detached seat run whose driver fails leaves the handback wake delivered but never acknowledged, and nothing redelivers it. | crew.rs:543-549, 588, 486; hot.rs:452 @6bb7bcf | S2: a failed run acks nothing; the wakes it took go back to the queue (delivered cleared). The seat then backs off (30 s, doubling, max 15 min, in memory; a success clears it), so a failing driver cannot start a model turn on every 500 ms sweep (I9). `seat.run` failed events carry `requeued`, `fails`, `retry_in_ms`. The kernel-restart-mid-run case stays with S3. | `audit_repro::repro_c3`, `s2_failed_seat_run_backs_off` | **fixed** in de3a74a |
| C4 | A detached seat run acknowledges its wakes even when the turn reported an error, so the handback is consumed silently. | crew.rs:620-633 @6bb7bcf | S2: a reported error is a failed run (as C3): no ack, no `outcome` event, no "handled N wake(s)" wake to L1; requeue and back off. | `audit_repro::repro_c4` | **fixed** in de3a74a |
| C5 | A wake delivered to a thread that detaches before Stop is never re-queued. | hot.rs:420-456, bind.rs:471 | S3: detach and a thread leaving its seat for another put that seat's unacked wakes delivered to the thread back in the queue (`requeue_delivered`, journaled `wake.requeue`). Kernel start requeues wakes held as "driven" (or by a thread no longer the seat's) on seats not running, and clears a seat run cut off by the restart. | `audit_repro::repro_c5_detach_requeues_in_flight_wakes`, `s3_claim_leaving_a_seat_requeues_its_wakes`, `s3_restart_requeues_driven_wakes` | **fixed** in 6ab5bdd |
| C6 | L1 could not create an L3 despite uke-design §5 and mapp-unvrs §1. | `task` required rank 2 and a project on the caller | L1 supplies `--project <id>`; `task` validates it and creates the L3 under L1. | `audit_repro::c6_l1_dispatches_l3_report_through_ctl` checks the public path and direct handback | **fixed** in ed86d71 |
| C7 | A detached seat run ignores seat model and effort ownership, and silently defaults to claude when the seat never bound. | crew.rs:567-587, 98-106 | S6: a seat with no known harness is refused (journaled `seat.run` refused, backoff; claim() clears it); `seat.run` started carries the requested model; the seat and its `outcome` carry requested and accepted model and effort. | `audit_repro::s6_seat_run_refuses_an_unknown_harness`, `s6_l1_outside_away_is_not_refused`, `s6_seat_run_outcome_carries_the_model` | **fixed** in ffedc1e |
| C8 | A seat run leaves a stale `Driven{cpu:"seat-run"}` on an attached seat. | crew.rs:520-526, state.rs:469-476 | S5 | S5 check | code-read |
| C9 | The snapshot lacks each seat's model and effort and each worker's parent, so CREW cannot show harness · model · effort or nest workers under their owners. | snapshot.rs:108-109, 119-137 | S5 | `audit_repro::repro_c9` | reproduced |
| C10 | Field notes are never filed: result.json always has `"field_notes": []`, and nothing refuses an exit without them. | driven.rs:684. The task results of PIDs 8–41 on this machine (34 files) all have empty `field_notes`. | S7: the worker prompt asks for a FIELD-NOTES block; the done path files it in result.json and the lead's wake; a missing block or non-done end records `none: <reason>`. No extra turn is spent to ask for notes. | `recover::tests::s7_done_with_notes_files_field_notes`, `s7_missing_or_trailing_notes_are_never_empty` | **fixed** in 05e03ab |
| C11 | An env default model (`UNVRS_L3_<H>_MODEL`) is enforced as an exact pin. | driven.rs:241, 444-458 | S8: the driver enforces any requested model as an exact id, so the env default stays a pin and the task reply and the mismatch stop now say so (`model_terms`), naming the variable and the fix. | `audit_repro::s8_env_default_model_is_reported_as_a_pin` | **fixed** in cf642c4 |
| C12 | `send` has no scope check: any L3 can wake any project's L2 or L1. | ops.rs:434-467 | S8: `send` reaches L1, the caller's lead or its project's seat; only L1 writes anywhere; anything else is refused with a journaled `refused` event. | `audit_repro::s8_send_is_scoped_to_lead_l1_and_project` | **fixed** in cf642c4 |
| C13 | A hint without a harness defaults to `codex`. | ops.rs:86-94 | S8: a hint without a harness matches the session on any harness among bound threads in the caller's process tree. | `audit_repro::s8_hint_without_harness_is_resolved_from_the_process_tree` | **fixed** in cf642c4 |
| G1 | Requested-harness fallback: `task` without `--on` silently runs on claude, and a never-bound seat's run defaults to claude. The `spawn` and `task` journal events record only the chosen harness, not the requested one, so a reported fallback cannot be audited. | ops.rs:406-410, crew.rs:567-568, journal `spawn` events | Enforce the requested harness or refuse; journal requested vs chosen. | unit test: a `task --on codex` whose harness is unavailable is refused, not moved | reported (captain); code-read |
| G2 | No stop or cancel verb: a parent cannot end its child's run. Only `quota-low` (a hand-off) and the watchdog stop a worker. | ops.rs `op` match has no `stop` or `cancel` arm | New `stop <pid>` under the S1 owner rule: kill the process group, finish as `cancelled`, wake the parent. | unit test: the parent stops its L3; a stranger is refused | verified (verb absent) |
| G3 | Lost wakes: the handback to a lead is dropped or stranded (C3, C4, C5). | see C3–C5 | S2 (C3, C4 fixed in de3a74a), S3 (C5 and restart fixed in 6ab5bdd) | C3–C5 repros, `s3_restart_requeues_driven_wakes` | **fixed** (C3, C4 in de3a74a; C5 in 6ab5bdd) |
| G5 | A seat whose detached runs keep failing is never escalated: the wakes wait in the queue and the backoff grows to 15 min, but L1 and the captain hear only through `seat.run` failed journal events. The backoff is in memory, so a kernel restart retries at once. | crew.rs `seat_run` failure arm (de3a74a) | Wake L1 (or notify the captain) after N consecutive failures; consider persisting the backoff. Not in S1–S8. | unit test: the Nth failed run wakes L1 once | code-read |
| G4 | L1 dispatch (same as C6). | see C6; mapp-unvrs §1 | as C6 | as C6 | **fixed** in ed86d71 |

Not verified live: caller resolution for seats bound in Claude Desktop, T3 Code and the
Codex app (ops.rs:29-76). Check it with `unvrs ctl whoami` from a bound seat in each
app.
