# Memory layer

**Status:** Locked 2026-09-22; **v1.1 revision Locked 2026-09-24** (§13). §1–§11 and the working answers in §12 are the lock; §13 adds to it and wins where it differs. The four items under "Still open" stay open; they are implementation questions, not a second design.
**Date:** 2026-09-22.
**Vocabulary:** [uke-design.md](./uke-design.md). Captain, PID, seat, mission, session, summary, transcript, CPU (harness), driver, bus, catalogue and compaction mean what that file says. Nothing here changes a lock in uke-design §4 (missions) or §9 (PID session).
**uke-design v1 (2026-09-24):** "cockpit" here means the captain's surface, which is now their own harness through the bridge (uke-design §7). The memory lock is unchanged; memory is half of the v1 core.
**Does not contradict:** the non-goals in [iii-improvement-plan.md](./iii-improvement-plan.md). No SQLite as the only copy of anything, no harness-announced capabilities, no iii as runtime.

---

## 1. How the captain uses UNVRS

UNVRS is the one gateway for all of the captain's work and life. The captain opens `unvrs` in one directory, `$HOME`, and works from there: coding, building software, research, knowledge work for their companies, WhatsApp, sales, entrepreneurship, personal matters. One home, so they can switch context without switching products. The name is literal: UNVRS is the universe from which work, entrepreneurship and personal life are overseen.

Two words from this usage carry through the rest of the file:

| Word | Meaning here |
| --- | --- |
| **Universe** | The directory `unvrs` is launched in. For the captain that is `$HOME`. Everything durable lives under `<universe>/.unvrs/`. The ship (uKe, agents, drivers) flies in the universe; it is not the universe. |
| **Area** | A region of the captain's life inside the universe: work, one company, sales, personal. Areas are names the captain chooses, not a fixed list, and not separate stores. |

The memory layer therefore has one root, one store, and scopes that tell notes apart. Switching from a coding seat to a sales seat changes which memories are loaded. It never changes where memory lives.

---

## 2. Goal

The captain owns one memory of what was worked on and what they care about. A later seat, on any harness, can continue from it or find it. The harness does not own that memory.

That is the whole goal. The layer exists so the captain can change tool, area, or seat without re-explaining, copy-pasting, or remembering which product still has the fact.

Work continuity and "things we care about" are the same goal. A coding seat that finished yesterday and a preference about how a company should be written to are both things a future turn must be able to find. A mission admits work that has an outcome and a done check. A memory keeps what should still be findable after that work, and what was never work at all.

### Why a harness memory is not enough

Hermes, Claude Code, and Codex each remember something. Each memory is the harness's. Using Hermes memory as the universe memory fails the goal in every way that matters here:

| What Hermes memory is | Why that is not this layer |
| --- | --- |
| Two short files, `MEMORY.md` and `USER.md`, that the Hermes agent edits. `MEMORY.md` is capped at about 2,200 characters. | A cap that small cannot hold an open task, a cold transcript, or a life of areas. The file only grows until something is dropped by hand. |
| Pasted into Hermes at session start, then frozen for that session. | A pin made mid-session is invisible until Hermes starts again. Codex and Cursor never see the file. |
| The agent decides what is worth keeping. | The captain does not own the choice. A fact the agent skips is gone from every future tool. |
| Stored for Hermes, under Hermes's home. | Switching to Claude Code or Codex means starting from that product's own store, or from nothing. |
| One file for the whole Hermes process. | Sales rules and coding conventions share one note. There is no active scope, so areas bleed. |

Claude Code auto-memory and Codex memories fail the same way: one product can read them, the captain cannot search them from another seat, and they are not scoped to an area of the universe. Section 10 says what UNVRS does with those files instead of adopting them.

---

## 3. Intended use

Daily use is one launch, at the universe root (`$HOME` for the captain).

| The captain wants | What they do |
| --- | --- |
| Start the day | Open `unvrs` at the universe root. The cockpit shows open notes for the seat in view. |
| Switch from coding to a company, or to personal | Switch the seat's active scope. The hot set reloads. Files do not move. |
| Keep something that is not a mission | `/remember`. Pin it only if every turn in that scope must see it. Leave it unpinned if it only needs to be findable. |
| Ask any seat what happened somewhere else | The seat runs `recall`. It searches the whole universe, not the harness's files. |
| Turn a TODO into real work | `admit` from that note. The note settles. The mission carries the outcome and the done check. |
| Stop seeing something | `forget` retires it. It leaves the hot set and stays findable. Delete is a separate, explicit act. |

A seat does not curate the universe. It may `remember` an unpinned note and it may `recall`. Only the captain pins, retires, deletes, and switches scope.

What the captain should stop doing: pasting context into a new Codex or Claude thread, maintaining a Hermes `MEMORY.md` by hand as the source of truth, and keeping a second notes file inside a project repo.

---

## 4. The problem

The captain wants the ship to remember what we work on and the things we care about, so that the captain owns their context and not the tool of the day. Today each harness (Codex, Claude Code, Hermes, Cursor) keeps its own memory in its own files. Switching tools means re-explaining, copy-pasting, or guessing which tool still remembers.

The current session design (uke-design §9) has three gaps the captain named. The usage in §1 adds a fourth. Each gap is one instance of a general problem, and each general problem has more instances than the one named.

| General problem | Instance named | Further instances |
| --- | --- | --- |
| **Growth.** A record grows until its old part is large and no longer worth loading. | The transcript tail. | The mission `# Log` section. The list of handoff summaries in the cockpit session snapshot. Flight journals in `.unvrs/` (22 files today, none ever read back). Any "sticky" file such as Hermes `MEMORY.md`, which only grows. The rolling summary itself: each rewrite can silently drop a fact that an older version held. |
| **No recall.** A record exists but nothing can search it from where the question is asked. | Per-PID summaries. "Remember XYZ?" asked at a Cursor seat fails when a Codex seat produced XYZ. | Handoff summaries (one per transfer, then forgotten). Done missions and their logs. Decisions taken in one mission are unknown inside another. A harness's own memory (Claude Code auto-memory, Codex memory) is invisible to every other CPU and to the kernel. |
| **Care that is not work.** Something matters but has no outcome, no done check and no seat. | An idea, a TODO, "remember this for later". | A preference ("always use X here"). A decision and its reason, including things we chose not to do. A rule for agents ("never do Y again"). A pointer (URL, ticket, dashboard). A fact about this machine, this repo, this customer. A question to ask later. A thing to watch. |
| **Bleed.** With work and life in one universe, a record that is true in one area is loaded in another, where it is noise. | A coding seat that carries a company's sales rules in its context. | A personal note in a research seat. A repo convention in a WhatsApp seat. Every pinned note, if pins had no scope. |

The three named items (idea, TODO, remember) are not a taxonomy. §6 shows they are one kind with two flags. Bleed is solved by scope, not by a second store (§7).

---

## 5. One definition

> **A memory is a kernel-owned note that a later turn, on any CPU, can find without the turn that produced it.**

Every word carries weight:

- **Kernel-owned:** uKe holds it, like a PID or a mission. No harness owns it.
- **Note:** a short record with a scope, a source and a body. Pointers, not payloads.
- **A later turn:** memory exists for the future, not for the current context window.
- **On any CPU:** if only one harness can read it, it is a cache, not a memory.
- **Can find:** it is reachable by search, not only by being loaded.
- **Without the turn that produced it:** the note must stand alone. A note that only makes sense next to its transcript is a transcript line, not a memory.

This one definition covers both halves of the captain's ask. **Work continuity** is the note a compaction worker writes about a PID's session. **Things we care about** is the note the captain writes on purpose. Same record, different source. The areas of the captain's life are scopes on the note, not new kinds of memory.

---

## 6. One kind: the note

The layer has exactly one record kind. Everything the captain listed, and everything in the "further instances" column, is a note.

The PID session **summary** (uke-design §9) is not a second kind. It stays the `summary` field of the session record. The index (§8) points at it, so recall finds it. Nothing copies it into the notes directory, and this file does not give it a second name.

A note has these fields. Two of them are flags:

| Field | Meaning |
| --- | --- |
| `scope` | Where the note is true. Exactly one of `universe` (always true, every area), `area:<name>`, `path:<path relative to the universe root>`, `mission:<id>`, `pid:<n>`. Area names are the captain's own. |
| `source` | Who wrote it: `captain`, `pid:<n>` (a seat, or a learning or decision from that PID's handoff), `compaction:pid:<n>`. `extractor:<name>` is reserved for a later system job and is not in v0. |
| `pinned` | **Hot.** Loaded into the CPU on every bind whose active scope (§7) includes this note's scope. Off by default. Only the captain sets it. |
| `open` | **Asks for something later.** Listed in the cockpit until settled. Off by default. |
| `status` | `live` or `retired`. Retired notes stay on disk and stay searchable; they leave the hot set and the open list. |
| `settled` | Present once an open note was answered. Holds the mission id when `admit` settled it (§9). |

Now the three named items:

| Captain says | It is |
| --- | --- |
| "Remember this" | A note. Pinned if the captain wants it in every turn of its scope, unpinned if it is enough to be findable. |
| "TODO" | A note with `open` on. It is not a mission because it has no outcome and no done check. When the captain admits it, the mission file points back at the note and the note is settled. |
| "Idea" | A note. Usually unpinned, usually not open. It may become a mission later, by the captain, through the admit path. |

The "further instances" fit the same way. A preference or a rule is a pinned note on `universe` or on one area. A decision is an unpinned note whose body holds the reason. A pointer is a note whose body is a URI. A question to ask later is an open note. A fact about a customer is a note on `area:sales`; a convention of one repo is a note on `path:github/that-repo`, stored in the universe, not in the repo.

**Facts extracted by a model** (the Mem0 pattern) would be notes with an extractor source; not in v0. **Sticky notes** (the Hermes `MEMORY.md` pattern) are the pinned notes, delivered to the harness on bind (§10). **Searchable observations** (the agentmemory pattern) are the index in §8. None of them is a second system. They are three ways of writing into, or reading out of, this one layer.

---

## 7. Hot versus recalled

**Hot** means it is placed into the CPU's context on bind, swap and rebind, without anyone asking. **Recalled** means it stays on disk until a turn asks for it by search.

### Active scope

Every PID has an **active scope**: the set of note scopes that are hot for it. It is `universe`, plus the PID's area, plus its path if it has one, plus its mission if it has one, plus the PID itself. The PID session records it as a structured fact, which uke-design §9 already allows. It is set when the captain spawns the seat or assigns a mission, and it changes when the captain switches context. A context switch reloads the hot set; it does not move any file.

### What is hot

| Hot on bind (the rehydrate package) | Recalled on ask |
| --- | --- |
| The PID's session **summary** | Every other PID's summary |
| The PID's **recent transcript tail**, bounded as uke-design §9 already says | The PID's **cold transcript**: turns that compaction folded out of the tail, kept as a file, never loaded whole |
| The PID's session `open` / `done` / `artifacts` when non-empty | Unpinned notes, any scope |
| **Pinned notes** whose scope is in the PID's active scope | Pinned notes of other areas, paths, missions and PIDs. Retired notes |
| Nothing else | Mission files, including done ones. Handoff summaries |

Flight journals and bus events are in neither column. Recall does not search them (§11).

### Two rules keep hot small

1. **Pins have a byte budget.** One budget covers the hot set of the active scope. A second, smaller budget covers `universe` pins, because those load into every seat in every area. Over either budget, the kernel refuses the new pin with a readable reason and names the pins the captain could retire to make room. Pins never silently fall out. Numbers are an implementation question.
2. **Compaction has two outputs, not one.** When the worker folds old tail into the summary, it also writes **notes** for facts that should survive the summary being rewritten later (a decision, a pointer, a rule the captain stated in chat). The summary may lose detail on the next rewrite; the note does not. The folded turns go to the cold transcript file. Nothing is deleted.

This turns one growing record into three tiers with different reach:

```
  transcript tail   (hot, bounded, rehydrated)
        │ compaction, on threshold, not every turn
        ▼
  summary           (hot, bounded, rewritten)  +  notes  (durable, appended)
        │
        ▼
  cold transcript   (file, unbounded, recalled only)
```

Rehydrate reads the first tier and the summary. Recall reaches all three.

---

## 8. Where it lives, who writes, who searches

### One root

There is one memory root: `<universe>/.unvrs/`. For the captain that is `$HOME/.unvrs/`.

- The kernel does not hard-code `$HOME`. The universe is the directory `unvrs` was launched in, and memory lives only under it. There is no second root such as a `~/.unvrs/` reached from another launch directory, and no "captain scope" that crosses roots. With one universe, `universe` scope already means "true everywhere the captain works".
- Notes about a project live in the universe with a `path:` scope. They do not live inside the project repo. The repo stays free of UNVRS memory files.
- A launch inside some other directory creates a separate universe with its own `.unvrs/` and its own notes, which never reach the captain's. For daily use that is the wrong door. It is the right door only for eval flights of UNVRS itself, which today fly in this repo through `make run`.

### Records are files the captain can open

```
<universe>/.unvrs/
  memory/notes/<id>.md              one Markdown file per note
  memory/index/                     disposable search index, rebuilt from files
  sessions/pid-<n>/                 PID session record and cold transcript
  missions/<id>.md                  mission files, as already locked
```

| Record | Document of truth | Form |
| --- | --- | --- |
| Note | `.unvrs/memory/notes/<id>.md` | YAML frontmatter with the §6 fields, then a one-line title and a short body. Same shape as a mission file. Editable by hand; the kernel picks up edits by mtime and hash, as the MissionRegistry does. |
| Summary | The PID session record (uke-design §9) | The `summary` field. Not copied into the notes directory. |
| Cold transcript | `.unvrs/sessions/pid-<n>/transcript.jsonl` | Append-only. The session record holds a pointer to it, per the "pointers, not payloads" rule. |
| Mission | `.unvrs/missions/<id>.md` | Unchanged in content. Indexed, never written by this layer, except that `admit` from a note adds the note id to the mission's inputs (§9). |

Exact file names and id shapes are an implementation question.

### The index is thin and disposable

uKe core owns a **MemoryIndex** beside the MissionRegistry and the PID table. Like the registry it holds pointers and a few columns, not bodies:

```
  id → path or record ref, scope, source, pinned, open, status, updated, hash
  plus a term index over title, body and frontmatter for search
```

The term index may be backed by any engine (tantivy, SQLite, a flat file). It lives under `.unvrs/memory/index/`, is rebuilt from the files at boot when stale, and can be deleted at any time without losing a memory. It is never the only copy of anything. That satisfies the non-goal against SQLite as the only copy.

### Who does what

| Actor | May do | May not do |
| --- | --- | --- |
| **Captain** (through DrvIntf) | `remember` (write, pin, open, settle), `forget` (retire), delete a note file, `recall`, switch the active scope of a seat, hand-edit any note file | |
| **Agent at a seat** (through `unvrs ctl`) | `remember` an unpinned note with `source: pid:<n>`, `recall` | Pin. Retire. Delete. Write any session's summary. Change its own active scope. |
| **Compaction worker** (system job on the bus, DrvHdff's) | Rewrite one summary; append unpinned notes with `source: compaction:pid:<n>`; append the cold transcript | Pin. Retire. Touch any other PID. Keep secrets: redaction is already a uke-design §9 lock. |
| **DrvAgent** | Project the hot set into the harness; install the code of conduct (context-ownership.md); install `remember` / `recall` skills from the catalogue; report turns back | Own any memory. Write the index. Decide what is hot. |
| **uKe core** | Own the MemoryIndex; serve the memory ops; assemble the hot set on bind from the active scope | Rewrite mission bodies. Admit anything. |

Drivers reach memory only through the kernel bus, as every driver does.

---

## 9. Remember, recall, forget, and the captain surface

One kernel operation per verb, two surfaces, one catalogue id, exactly as swap already works (uke-design §13).

| Surface | Form |
| --- | --- |
| Captain | `/recall XYZ`, `/remember …`, `/forget <id>` in the cockpit |
| Agent, any harness | `unvrs ctl recall "XYZ"`, `unvrs ctl remember …` |
| Behaviour catalogue | One row per op. DrvAgent installs `remember` and `recall` into Pi, Codex, Claude Code, Hermes and Cursor as harness-native skills, so every CPU learns the same ops under the same names. The installed `recall` skill says: before answering "I don't know", run `recall`. |

### Recall

1. The kernel receives `recall` with the query and the calling PID.
2. The MemoryIndex searches the **whole universe**: every note in every scope and status, every PID's summary, cold transcripts, mission files and handoff summaries. It does not search flight journals or bus events. v0 search is lexical (words, scope, source, time). Embeddings can be added later as more disposable index state; that is a search-quality question, not a design change.
3. The kernel returns a bounded list of hits. Each hit is: what it is, scope, source, when, a one-line title, a short snippet, and a pointer to the file or record.
4. The agent reads the pointer if it needs more. It has full access, so this is a file read, not a new permission.
5. The hits enter the harness context as turn input. They are a cache there, like everything else in the window.

The Cursor seat that never saw XYZ now finds the Codex seat's summary, the note the compaction worker wrote from it, or the mission log where it happened. No copy was made when the Codex seat worked. Recall found it because the kernel indexed the record where it already lived. Recall ignores the active scope on purpose: hot follows the scope, search does not.

### Remember, forget, delete

- `remember` writes one note through the kernel. From a seat it is always unpinned with `source: pid:<n>`. From the captain it may pin, open, or settle.
- `forget <id>` **retires**: the note leaves the hot set and the open list, the file stays, recall still finds it. Captain-only.
- A separate, explicit **delete** removes the file. It is a different op with a different name, captain-only, and not offered to seats. Retire is the default because a retired note can still answer "why did we stop doing that".

### Open notes are always in view

Open notes are a list in the cockpit, always visible, not something the captain has to ask for. The list follows the active scope of the seat the cockpit is looking at, and shows one line with the count of open notes outside that scope. `/recall` is for everything else.

### A TODO becomes a mission in one step

`admit` accepts a note id. The kernel creates the mission from the note's body as the draft, the captain supplies the outcome and done check (the admit floor of uke-design §4 still applies, unchanged), the mission's inputs point at the note, and the note is settled with the mission id. One captain action, not two.

How that hot set reaches Hermes, Claude Code, and Codex, and what UNVRS does with the files those products already write, is §10.

---

## 10. Harness memory

Amended 2026-10-04 (PID 198, [context-ownership.md](context-ownership.md)). A harness memory file is the harness's own cache. The universe note is the copy of record. Hermes, Claude Code, and Codex keep their own stores because the product needs something to read at the start of a turn. A harness may use its own memory and tools as it likes. UNVRS does not read, import, adopt or police those stores, and it does not try to keep them identical to the universe. Job context never lives only there: the handoff and the context-ownership check put what matters into the universe.

### What each product already has

| Product | Instructions, not memory | Memory the product writes |
| --- | --- | --- |
| **Hermes** | — | `MEMORY.md` and `USER.md`. The agent edits them. `MEMORY.md` is capped near 2,200 characters. Hermes freezes the file at session start, so a change made during the session appears on the next start. |
| **Claude Code** | `CLAUDE.md` | Auto-memory under `~/.claude/projects/<git-slug>/memory/`: a `MEMORY.md` index plus topic files. Claude loads the first 200 lines or 25KB. |
| **Codex** | `AGENTS.md` | Memories under `~/.codex/memories/`, off unless `[features] memories = true`. |
| **Cursor** | Cursor rules | Whatever memory the product writes for that seat. Same treatment as the rows above. |

Instruction files stay instruction files. UNVRS does not import them, does not overwrite them, and does not treat a project convention in `CLAUDE.md` or `AGENTS.md` as a note. A convention the captain wants every seat to know is a pinned note in the universe, projected on bind.

### How the hot set is delivered

On bind, swap, and rebind, DrvAgent delivers the hot set (§7) as turn input over ACP, in the same package as the rehydrate summary and tail. That is the path. The hot set follows the seat's active scope, so a coding seat and a sales seat of the same harness do not share one pasted file.

A harness that cannot take the hot set as turn input gets a marked, generated region in a file that harness reads at start. UNVRS overwrites that region on every bind. The region is never written into a project repo. Edits inside it are lost on the next bind. To keep a change, the seat or the captain calls `remember`.

UNVRS does not write the hot set into `~/.claude/projects/…/memory/` or `~/.codex/memories/`. Those directories are shared by every seat of that harness. Writing one seat's hot set there would bleed areas and would fight the product's own writer.

### Nothing is imported

DrvAgent does not read harness memory. The earlier import (DrvAgent reported new entries from `~/.claude/projects/<slug>/memory/`, `~/.codex/memories/` and Hermes files, uKe filed each as an unpinned note) was retired on 2026-10-04: since commit 892a875 nothing called it, and the principle forbids it. The code (`memory_entries`, `inbox_paths`, `import_entry`, `baseline_inbox`) and the `UNVRS_<HARNESS>_MEMORY` settings were removed.

What a seat or worker learns reaches the universe through UNVRS tools: `remember`, `decide`, and the end-of-task handoff, whose decisions and learnings the kernel files as notes with `source: pid:<n>` ([context-ownership.md](context-ownership.md) §4). The code of conduct skill says to work this way; it says nothing about what to keep in the harness's own memory.

### Conflicts and drift

The universe wins. There is no merge, because UNVRS never reads the harness store.

| What happened | What UNVRS does |
| --- | --- |
| The captain pins or edits a note. | The next bind delivers the new hot set. Hermes shows it on the next session start, because Hermes freezes memory at start. |
| Someone edits the generated region. | The next bind overwrites it. |
| The harness writes its own memory. | Nothing. It is the harness's cache. |
| The harness and a note disagree about the same fact. | The note is the record. A seat that wants the fact changed calls `remember`; the captain pins or retires. |
| Two seats of the same harness are active. | Each receives its own hot set as turn input. Neither writes into the shared `~/.claude` or `~/.codex` memory directory. |

Swap and rebind cannot lose job context, because the harness never held the only copy.

---

## 11. Explicitly not memory

| Thing | Why not | What it is instead |
| --- | --- | --- |
| **Mission admit floor** (outcome + done check) | A mission is admitted intent with a pass/fail check. A memory has neither. A note may become a mission only through the captain's admit; the note then points at the mission and is settled. | Mission (uke-design §4). Mission files are **indexed** by recall; this layer never rewrites their bodies. |
| **Harness context window** | Scarce, harness-local, lost on swap. | Cache (uke-design §4). |
| **Harness memory files** (Hermes `MEMORY.md` / `USER.md`, Claude auto-memory, Codex memories, Cursor memory) | Only one CPU can read them; the kernel cannot search them. | Cache and inbox (§10). |
| **Instruction files** (`CLAUDE.md`, `AGENTS.md`, Cursor rules) | They tell a harness how to behave in a repo. They are not a record a later turn on any CPU can find. | Left untouched. A convention every seat must see is a pinned note (§10). |
| **Skill catalogue / behaviour catalogue** | Static tables of what the ship can do, not what we learned or care about. | Catalogue (uke-design §13). Nesting content (uke-design §10). |
| **Mailboxes** | Messages in flight between PIDs. | IPC (uke-design §6). A PID may `remember` what it received; the mail itself is not a memory. |
| **Flight journals and bus events** | Observability truth about what happened, at event granularity. Large and event-shaped; indexing them would bury the useful hits. | DrvObs. Not indexed by recall. |
| **Secrets** | Compaction already must redact them. A note holding a secret is a bug. | Credentials belong to DrvAuth readiness, not to memory. |
| **Artifacts, code, large blobs** | Payloads. | Pointed at from a note or a session, never embedded. |
| **A project repo** | The captain's memory is about the repo, not inside it. | `path:` scope in the universe (§8). |

---

## 12. Working answers, adjustments, still open

The review of 2026-09-22 answered the twelve questions this file used to carry. Those answers, and §1–§11, are the captain lock.

### Accepted as working answers

1. **One definition is enough.** A memory is a kernel-owned note a later turn on any CPU can find. Areas of life are scopes on the note (`universe`, `area:<name>`, `path:…`, `mission:…`, `pid:…`), not new kinds. Area names are the captain's own, not a fixed list. (§5, §6)
2. **One kind: the note.** The session summary stays the `summary` field of the PID session; the index points at it; there is no second copy and no second name for it. (§6)
3. **Only the captain may pin.** A seat may write an unpinned note. It may not pin. (§6, §8)
4. **One byte budget** for the hot set of the active scope, plus a small `universe` budget for notes that are always true. Over budget, refuse the new pin and name what to retire. (§7)
5. **No second memory root.** The universe is where `unvrs` is launched; for the captain that is `$HOME`; notes never live inside a project repo; launching elsewhere is the wrong door for daily use. Adjusted, see below. (§8)
6. **Compaction-written notes** live in the same directory, marked by `source`, unpinned, retirable by the captain. (§7, §8)
7. **`forget` retires**; a separate explicit delete removes the file. (§9)
8. **Harness-written memory** is the harness's own cache and is not imported (amended 2026-10-04, §10). Instruction files are not imported. The hot set is turn input over ACP, not a write into `~/.claude` or `~/.codex`. The universe wins on conflict. (§10)
9. **Recall does not search flight journals.** It covers notes, summaries, cold transcripts, missions and handoffs. (§9, §11)
10. **Open notes are a cockpit list**, always visible; `/recall` is for everything else. Adjusted, see below. (§9)
11. **`admit` accepts a note id.** One step creates the mission and settles the note. (§9)
12. **Directories** under the universe root: `.unvrs/memory/notes/`, `.unvrs/sessions/`, `.unvrs/missions/`. (§8)

### Adjustments and why

- **Answer 5, launch directory.** Accepted as "one root, the launch directory, no `~/.unvrs/` reached from elsewhere", but the kernel does not hard-code `$HOME`. Why: UNVRS's own eval flights launch in this repo through `make run` and need a throwaway universe of their own; hard-coding `$HOME` would make those flights write into the captain's real memory. A launch outside the captain's universe is still the wrong door for daily use.
- **Answer 10, open list.** Accepted, with the list filtered by the active scope and a one-line count for the rest. Why: one universe holds work, companies and personal life, so an unfiltered list would be long, and the rule that hot follows the active scope should apply to the open list as well. Recall still finds every open note.

### Consequences outside this layer

- **Boot refuses `$HOME` today.** DrvIntf refuses a flight whose workspace is `$HOME` because harness skill dirs there are user-global. The captain's usage launches exactly there, so that refusal must be replaced by a Boot design for installing the catalogue into a `$HOME` universe. That belongs to Boot, not to this file.
- **Mission directory.** Answer 12 picks `.unvrs/missions/`, which is the question uke-design §16 leaves open. This file does not edit uke-design.
- **Shared harness files.** With one universe, a harness's global files (`~/.claude/…`, `~/.codex/…`) are read by every seat of that harness kind. That is why §10 delivers the hot set as turn input over ACP and does not write it into those directories.

### Still open

Only what the usage in §1 does not decide:

- **Where a seat's area comes from.** A field on the mission file, the captain at spawn or assign, or the cockpit's current area at the moment of spawn. The PID session records it either way; the source is undecided.
- **The shape of the context-switch op** in the cockpit that changes a seat's active scope. A DrvIntf question.
- **Budget numbers**, search engine, note id and file naming. Implementation.
- **An extractor job** that writes notes from a model's reading of transcripts. Reserved by `source`, not in v0.

---

## 13. v1.1 revision (2026-09-24)

Why this revision: in UNVRS v1 ([uke-design.md](./uke-design.md) v1, [system-architecture.md](./system-architecture.md)) memory carries swap and handoff, the core of the product. The 0.6 lock above stays. This section adds purpose, a model of memory kinds, and five rules. Items marked **0.7** are built in [voyages/0.7.0-eval.1.md](./voyages/0.7.0-eval.1.md); items marked **0.9** wait for the behaviours voyage.

### Purpose

**Memory makes the harness replaceable.** Any seat, on any harness, at any rank, starts where the work actually is, and the captain's judgement compounds across harnesses. Three jobs: continuity for swap and handoff; no re-explaining; judgement that compounds.

### Three kinds, one store

| Kind | Holds | UNVRS record | Serves |
| --- | --- | --- | --- |
| **Episodic** (what happened) | sessions, turns, handoffs, mission logs | PID session (brief, tail, open / done / artifacts), cold transcripts, handoff packages | swap, handoff, "what were we doing" |
| **Semantic** (what is true) | facts, decisions and why, preferences, pointers, one-line rules | notes (§6) | recall, no re-explaining |
| **Procedural** (how we do things) | procedures | skills, as behaviours (uke-design §10) | every seat works the captain's way |

The harness context window is none of these: it is a cache.

**Small hot set, big recall.** Stuffing the context window competes with harnesses. Recall as a tool the model calls improves as models improve. The hot set stays small; everything else is fetched on demand.

### R1. Rank-aware hot set (0.7)

`hot_set(pid, rank)` is built from layers:

| Layer | L1 | L2 | L3 |
| --- | :-: | :-: | :-: |
| Identity: PID, rank, harness, scope, mission | ✓ | ✓ | ✓ |
| Pins in scope | ✓ | ✓ | ✓ |
| Unread mailbox items | ✓ | ✓ | ✓ |
| Tree digest: one line per L2 / L3 (pid, harness, scope, state, next step) | ✓ | own children | — |
| Missions: digest of active and admitted | ✓ | own mission in full (outcome, done check, scope) | — |
| Open notes in scope, plus a count elsewhere | ✓ | ✓ | — |
| Own brief (R2) and tail | ✓ | ✓ | — |
| Task package | — | — | ✓ |
| Kernel suggestions (swap on low quota, idle L2s, stale pins) | ✓ | — | — |

- Digest lines are pointers, each short, with a cap and a "more: …" line.
- **The swap rehydrate payload is `hot_set(pid, rank)`. The handoff package is the L3 hot set.** One function serves both.
- After attach, the prompt hook injects only a **delta since the last turn** (new mailbox items, child state changes, new L2s), never a full reload.

### R2. One brief (0.7)

The session summary, the swap rehydrate and the handoff package use one schema, the **brief**. It extends the existing `HandoffSummary` (`next_focus` becomes `next`):

| Field | Holds |
| --- | --- |
| `goal` | what and why, plus mission id |
| `now` | the step in progress, concretely |
| `next` | one to three next steps |
| `decisions` | `decision — why`, recent, bounded |
| `open` | open items, questions, waiting-on (who) |
| `done` | recent, bounded |
| `gotchas` | learned constraints ("don't run X; it breaks Y") |
| `artifacts` | pointers |
| `narrative` | short free text for nuance, bounded |

Rules against drift, checked mechanically:

- **Open items cannot vanish.** Every open item of the previous brief appears in the new one as open, done or cancelled. Otherwise the fold is rejected and the previous brief stays.
- **Decisions graduate, never drop.** When the bounded list overflows, the oldest become unpinned notes.
- **Previous briefs go to the cold file**, so a bad fold can be compared and recovered.
- **Compare-and-swap on write.** The brief carries a version. A fold job records the version it read and applies only if the brief is still that version; otherwise it is discarded and re-run on the current brief. This stops a fold from overwriting a newer brief written by a swap, a detach fold or another job. (Idea from CoKe, D22.)
- **Fresh at swap time.** Fold when a thread detaches, so the brief is usually ready. At attach, if the tail since the last fold is small, rehydrate with brief plus tail; if it is large, fold first.
- The compaction worker writes the brief. The seat's own CPU still does not (uke-design §9).

### R3. Notes versus behaviours (line now, build 0.9)

No new record kind. The line is delivery, not grammar:

| Always in context and short | Loaded when relevant, procedural |
| --- | --- |
| **pinned note**: "never push to main in repo X", "company A writes in Spanish" | **skill**: "how we cut a release", "how we qualify a lead" |

One-line rules and preferences stay pinned notes (scope, budget, captain-only authority). Procedures are skills. A long procedural `/remember` may suggest "make this a skill?"; the captain decides. `CLAUDE.md` and `AGENTS.md` stay untouched; whether UNVRS owns them is a 0.9 question.

### R4. Consolidation (measure 0.7, build 0.9)

Unpinned notes are never hot, so pile-up hurts recall precision and disk, not context.

1. **0.7 measures**: notes written per day per source, recorded in LEARN.
2. **0.9, write-time dedupe**: before writing a compaction or handoff note, look for a near-identical note in the same scope; if found, update its `confirmed` (R5) instead of writing.
3. **0.9, recall ranking**: pinned > live > retired; captain > seat > compaction / handoff; recent `confirmed` first; superseded demoted.
4. **Only if data demands it**: a periodic consolidation job that proposes merges (retire with `superseded_by`), never deletes, never touches pins, and reports a one-line digest.

### R5. Trust over time (0.9)

Two optional frontmatter fields; hand-edited notes without them stay valid.

- **`confirmed: <date>`**: set on write, on re-remember, on dedupe. Ranking uses age since confirmed.
- **`supersedes: <id>`**: the new note names what it replaces. The old one is retired automatically only if the captain wrote the new one or the same source wrote both. A seat may propose superseding a captain note, never retire it.
- **Stale pins are surfaced, never auto-retired**: pins unconfirmed past N days appear as one line in L1's hot set.
- **Conflicts stay visible**: recall shows both, with dates and sources.

---

## Related

| Doc | Role |
| --- | --- |
| [uke-design.md](./uke-design.md) §4, §9, §13 | Missions, PID session and compaction, catalogue: the locks this proposal builds on |
| [iii-improvement-plan.md](./iii-improvement-plan.md) | Catalogue ids (improvement 2) carry `remember` / `recall`; bus binding table (improvement 3) carries the compaction job; non-goals honoured |
| [codebase-architecture.md](./codebase-architecture.md) | MemoryIndex sits in `uke` beside MissionRegistry; projection and the code of conduct in `drv_agent`; compaction job in `drv_hdff`; captain surface in `drv_intf` |
| [voyages/0.6.0-eval.1.md](./voyages/0.6.0-eval.1.md) | What to build and what good looks like |
