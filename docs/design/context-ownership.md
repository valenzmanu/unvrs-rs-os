# Context ownership

Status: audit done and design accepted for build by the captain's go (PID 198, 2026-10-04). The principle comes from captain memory c84, c85 and c86.

> A harness can use anything it has, its own memory and tools included. Job context must never live only inside the harness; UNVRS holds the copy of record.

## 1. What job context is

| Part | What it holds | Where UNVRS keeps it |
| --- | --- | --- |
| Brief | task spec, sources, memory given | `projects/<p>/tasks/pid-<n>/contract.json`, the session brief (`sessions/pid-<n>/session.json`, `briefs.jsonl`) |
| Working record | conversation, decisions, plans, intermediate files | session tail and cold transcript (`sessions/pid-<n>/`), the task directory, git commits in the task work tree |
| Deliverables | files, reports, code | the task directory, a work-tree branch, or a path inside a registered source of the task's project |
| Learnings | what the next worker should know | memory notes with `source = "pid:<n>"` |

A path is **UNVRS-owned** when it is under `UNVRS_HOME` or under the `uri` of a `path` source listed in the task's project (`project.toml` `sources`, resolved through `sources.toml`). Anything else is outside UNVRS, even when the captain can open it.

## 2. Audit: where job context escaped on 2026-10-04

Evidence was read from the live home (`~/.unvrs`, kernel b709d6af) and the code at that commit.

### 2.1 Deliverables saved outside UNVRS

- 36 of 164 `result.json` files name paths outside `~/.unvrs`. The largest group is the brand project: PIDs 168, 170, 186–195 and 197 delivered into a folder outside UNVRS that is not a registered source. Others pointed into the Downloads folder, or into a registered source that was not one of their project's sources.
- Nothing in the kernel checked this. `finish_with_notes` (`uke/src/kernel/driven.rs`) accepted any `UNVRS-RESULT` text.

### 2.2 Done reports without decisions or learnings

- 80 of 164 results carry no field notes (`none: the final reply had no FIELD-NOTES block` or an empty block). There was no decisions field at all and no deliverables list: deliverables were prose inside the result.
- Field notes went to the parent's wake and `result.json` only. They never became memory notes, so the next worker could not recall them.

### 2.3 Working record per harness

| Seat | What UNVRS captured | What stayed only in the harness |
| --- | --- | --- |
| L3 on Claude Code (`claude -p`, `drv_agent/src/headless.rs`) | every turn's final text plus tool names (`append_turn` in `driven.rs`), brief from the progress block, git checkpoints | tool inputs and outputs, intermediate assistant text; the full log is in `~/.claude/projects/<cwd-slug>/<session>.jsonl` |
| L3 on Codex (app server, `drv_agent/src/codex.rs`) | every turn's `agentMessage` texts plus tool names, brief, git checkpoints | command output and reasoning; the full rollout is in `~/.codex/sessions/` |
| L1/L2 seat in a Claude Code or Codex thread (plugin hooks, `uke/src/kernel/bind.rs` `capture`) | the captain's prompt and the last assistant message of each turn | intermediate messages and tool results of the turn; anything the seat decided but did not `remember` or `decide` |

The tool-level gap is accepted for now: the record of record is the conversation text, the brief, the commits and the files. The handoff (§4) makes the decisions explicit so they do not depend on a transcript.

### 2.4 Harness memory

- `drv_agent/src/memory.rs` still had `memory_entries` and `inbox_paths`, which read `~/.claude/projects/<slug>/memory/` and `~/.codex/memories/`, and `uke` still had `import_entry` and `baseline_inbox`. Since commit 892a875 (flight.rs deleted) no production path calls them; only tests did. Settings still offered `UNVRS_<HARNESS>_MEMORY` "memory inbox" overrides that nothing read.
- Claude Code auto-memory is on for driven workers. Of 119 Claude project directories for UNVRS paths, 14 memory files exist, all in the captain's own repo threads, none in task directories. Codex memories are off (`[features]` has no `memories`).
- `memory-layer.md` §10 still described an import. That contradicts the principle: UNVRS does not read a harness's private memory.

### 2.5 Brief

- The brief is held: `contract.json` and the session brief exist for every driven PID. One known defect: a long spec was cut at an apostrophe once (PID 180 kept `full-spec.md` beside the contract). Not reproduced here.

## 3. Decisions

1. **Harness memory is the harness's own cache.** UNVRS does not read, import, write or police it. The import code is removed (§2.4). The code of conduct does not tell a harness what to keep in its own memory.
2. **One code of conduct.** DrvAgent installs one model-invocable skill, `conduct`, in the UNVRS plugin that every Claude Code and Codex seat and worker loads, and the worker prompt names it. Text lives in `mapp_unvrs` (`CONDUCT`).
3. **A kernel gate on every result.** A driven task is accepted only when its final reply passes the context-ownership check. A failing reply goes back to the same worker with the reasons, up to three times; after that the task ends `blocked` with the check attached.
4. **Learnings and decisions become notes.** On pass, each handoff learning and decision becomes an unpinned note with `source = "pid:<n>"` in the task's project.
5. **Observatory shows it.** Each task's check, with what UNVRS holds of its context, is a view in the Observatory.

## 4. The handoff and the check

The worker ends with this block before `UNVRS-RESULT`:

```
HANDOFF
deliverables:
- <path, or commit <sha> on <branch>>
decisions:
- <decision and why> | none
learnings:
- <what the next worker should know> | none
END-HANDOFF
```

The check (`uke/src/kernel/ownership.rs`) passes when all of these hold:

| Check | Fails when |
| --- | --- |
| handoff present | no `HANDOFF` block |
| sections complete | a section is missing or empty; `- none` is accepted only as an explicit item |
| deliverables owned | a path item is outside UNVRS-owned paths, or does not exist |
| record captured | UNVRS holds no turn of this PID's conversation |
| brief held | the task has no contract |

Each failure is one leak line, for example `deliverable outside UNVRS: ~/bridge/files/brand/x.png`.
