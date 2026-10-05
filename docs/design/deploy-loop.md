# Deploy loop (`unvrs deploy | rollback | versions`)

Status: design for the DEPLOY LOOP slice (PID 76, branch `feat/deploy-loop`). Deploy is
separate from merge: it takes a branch or commit that already exists in the repo and never
writes to that repo, never merges and never pushes.

## Why (what exists today)

- `versions/<v>/unvrs` is keyed by `CARGO_PKG_VERSION` alone. `stage_version` overwrites a
  slot when the bytes differ (`upgrade`: "restaged with new bytes"), so a slot is not
  immutable, has no commit identity, and `rollback` after a same-version rebuild returns to
  the new bytes. `versions/current|previous` keep one step of history.
- The swap is `switch_bin` (atomic symlink) then `launchctl kickstart -k`. There is no drain,
  no health gate and no automatic rollback. The kernel has no SIGTERM handler, so the kill
  skips `serve`'s shutdown path; a busy worker's harness process keeps running as an orphan.
- The next kernel's sweep restarts every `working` headless worker that is not `running`
  (requeue already exists), but it loads `busy: true` and a stale `os_pid` from `state.json`.
- Every earlier deploy was hand-rolled per task: a version string bumped only for the
  build, `upgrade --from`, a shell gate polling `kernel status --json` until no worker is
  busy, curl plus a Python smoke, and a hand-written ROLLBACK.md with sha256.
  The deploy loop automates that recipe.

## Commands

```
unvrs deploy <branch|commit> [--repo <dir>] [--no-test] [--drain-secs n] [--force-requeue] [--allow-drop] [--dry-run]
unvrs rollback                     # to the previous deployed slot, same drain and gate
unvrs versions [--json]            # slots, the live one, its branch and commit, deploy history
```

`--repo` defaults to `UNVRS_REPO`, then `~/github/unvrs-rs`. `--dry-run` resolves, builds,
tests and stages, then stops before the drain (a staged slot can be deployed later with no
rebuild).

Deploy requires the candidate to contain the currently installed live commit. Before
checkout, build or slot reuse, Git lists commits reachable from live but absent from
the candidate; any missing commit refuses deployment and names its full hash and subject.
The installed symlink's slot manifest identifies live; legacy or external installs use
the installed binary's `--version` output. Stale `current` or `live.json` records cannot
override the installed link. Unknown identity or missing Git history also refuses the
deploy. A first install with no existing binary has no live ancestry to preserve.

`--allow-drop` explicitly bypasses this check and is recorded in deploy status/history.
Neither `--no-test` nor `--dry-run` bypasses it. `unvrs rollback` and automatic recovery
after a failed health gate return to an earlier slot through their existing drain/gate
paths, without requiring that earlier commit to contain the rejected release.

## Immutable versions with commit identity

Slot id: `<CARGO_PKG_VERSION>+g<sha12>`, for example `0.8.0+g233f9cc79a1e`. The binary is
built from the commit, never from a working tree, so the id always names exactly one
source tree. A slot is `versions/<id>/` with:

- `unvrs`, mode 0555
- `manifest.json`: `{id, version, commit, ref, branch, built_at, sha256, tested, builder}`

A slot is written once: staged in `versions/.<id>.tmp-<pid>`, then renamed into place. If a
slot with that id already exists, deploy reuses it and does not rebuild. When the fresh
bytes differ from the existing ones (a non-reproducible build), the existing slot wins and
the difference is reported. The binary knows its own identity:
`UNVRS_BUILD_COMMIT` and `UNVRS_BUILD_REF` are baked in at build time through
`option_env!`, which cargo tracks, so a change of commit rebuilds. `unvrs --version` then
prints `commit:` and `ref:` lines, and the kernel reports them in `kernel.json` and the
snapshot. `versions/current` and `versions/previous` hold slot ids, so the legacy
`rollback`, `doctor` and `upgrade` keep working.

## Pipeline and states

The deploy record lives in `deploy/status.json`, a file rather than kernel state because the
kernel restarts mid-deploy. The Observatory reads it. Each finished deploy appends one line
to `deploy/history.jsonl`, and `deploy/live.json` holds `{slot, commit, branch, since, previous}`.

```
resolve -> ancestry -> checkout -> build -> test -> stage -> drain -> swap -> gate -> live
                                                               |        |       \-> rollback -> rolled_back | broken
                                                               \-> aborted (drain timeout; nothing swapped, undrain)
failures before the drain -> failed (live untouched)
```

1. **Lock.** `flock(deploy/deploy.lock)` is non-blocking: one deploy or rollback at a time. A
   second one fails at once and names the holder.
2. **Resolve.** `git -C <repo> rev-parse --verify <ref>^{commit}`. The ref is recorded as a
   branch when `refs/heads/<ref>` or `refs/remotes/*/<ref>` exists.
3. **Checkout.** `git archive <sha> | tar -x -C deploy/checkouts/<sha12>`: an isolated,
   read-only source snapshot. Nothing in the repo or its worktrees is touched.
4. **Build.** `cargo build --release --locked -p unvrs`, using the shared cache
   `CARGO_TARGET_DIR=deploy/cache/target` with `UNVRS_BUILD_COMMIT` and `UNVRS_BUILD_REF` set.
5. **Test.** `cargo test --workspace --locked` in the same checkout and cache. `--no-test`
   records `tested: false`.
6. **Stage** (above).
7. **Drain** (below). A timeout aborts the deploy unless `--force-requeue` is given.
8. **Swap.** Point `bin/unvrs` at the slot atomically and shift `current` and `previous`.
   Then `kernel stop` on the old kernel, which is its graceful path: it saves state and
   stops busy process groups. The new kernel is started with `launchctl kickstart -k`, or
   with `kernel start` when there is no LaunchAgent.
9. **Gate** (below). On failure, roll back automatically.
10. **Live.** Write `live.json` and history, and clean the checkout. The build cache is kept.

## Drain and requeue

The new kernel op `drain {on: bool}` holds new work. While draining, `worker_loop` does not
start another turn and leaves the worker's pid `working`, with its session and brief
intact. The sweeper starts no workers and no detached seat runs. The op answers
`{draining, busy: [pid…], folding: n}`. Deploy polls it until `busy` is empty and nothing
is folding, for up to `--drain-secs` (default 900). A turn that is in flight finishes its
turn, and the worker then parks between turns.

The drain flag lives only in memory, so the new kernel starts undrained. Its sweep
requeues every parked `working` worker (not busy), which resumes from its last finished
turn with the same harness session. That is the existing restart path. A worker still
`busy` at load is not requeued: worker-recovery.md's startup rule blocks it
(`restart-unverified`) and wakes its lead, who verifies the old process group is gone.

When the drain times out without `--force-requeue`, deploy sends `drain {on:false}` and
ends `aborted`, with nothing swapped. With `--force-requeue`, the graceful `kernel stop`
sends SIGTERM to the busy process groups, as `serve` does today; the new kernel blocks
each cut worker (`restart-unverified`) and wakes its lead, which dispatches a continuation.

Holds, mailboxes, memory and the journal are already persisted (`state.json`, `memory/`,
`journal.jsonl`). The swap never touches them, and both slots read the same home.

## Health gate (doctor plus smoke)

Deploy runs only the checks that do not depend on a harness login. The full `unvrs doctor`
also exercises Claude and Codex, which would make deploy flaky.

- **kernel:** answers `ping` within 15 s
- **identity:** the kernel's reported `commit` equals the slot's commit (proves the new
  binary is the one running, not an old kernel that survived)
- **mcp:** `bin/unvrs mcp` lists the tool `unvrs`
- **observatory:** answers 200 for `unvrs.localhost` and non-200 for a foreign Host,
  unless `UNVRS_OBSERVATORY_PORT=off`
- **smoke:** `kernel status --json` parses, with the same seats and at least as many pids
  as before the swap (state preserved)

On any failure: switch back to the previous slot, restart, and run the same gate against the
previous slot's commit. If that passes, the deploy ends `rolled_back` with the reason; if not,
it ends `broken`, and the output gives the exact manual commands (symlink, `current`, and
kickstart), which are also saved in the history.

## Shared build cache without corruption

Every build uses one `CARGO_TARGET_DIR` under `deploy/cache/target`. Cargo already
serialises builds on its own target-dir lock. On top of that, deploy holds an exclusive
`flock(deploy/cache/target.lock)` from `cargo build` through copying and hashing
`release/unvrs` into the slot, so a concurrent build cannot replace the artifact between
build and copy. Slots are copies, never links into the cache, so pruning or corrupting
the cache can never change a live binary. Task worktrees keep their own `target/` for
development; only deploys share the cache.

## Observatory

The snapshot gains `deploy`, which holds `live.json` plus `status.json` when a deploy is
running, and `kernel.commit`. The header shows `v0.8.0 · g233f9cc (branch) · pid …`, and
while a deploy runs it shows a line with `deploying <ref>@<sha7> · <phase>`. The
Observatory itself restarts with the kernel, so the phase is always read from the file.

## Deferred

- Auto-deploy from a green, reviewed integration branch. It needs a notion of "reviewed"
  that UNVRS does not hold yet; it can later be a thin loop that calls `deploy` on a new head.
- Slot and cache pruning (`unvrs versions prune --keep n`).
- Staging a second kernel on another home for a blue/green swap.

## Risks

- A drain waits for the longest in-flight turn. The default timeout aborts rather than cutting
  a turn, so deploys can be refused on a busy system (safe, but slower).
- Detached children that workers spawn in their own process group are neither tracked nor
  stopped; that is unchanged.
- A release build plus tests in the shared cache needs about 2–3 GB of disk.
