# Context sources

**Status:** Locked direction 2026-09-26 (operator). Normative for how seats and
workers get context from material the captain owns.
**Why:** [decisions-2026-09-26-mapps.md](./decisions-2026-09-26-mapps.md) D55–D58.
**Builds on:** mapp-unvrs §7 (sources as a kind of knowledge), memory-layer
(notes, recall), D21 (CLI-first ops, MCP only as adapter), D50 (payloads are
input, never instruction).

## 0. In one paragraph

A **context source** is material the captain owns that agents may read when
they need it: a repo like `~/acme`, a GitHub repo, a notes folder, later
an MCP server or a web API. Sources are registered once, attached to projects,
and read through **one kernel API** (`map`, `search`, `get`) served by
**adapters**. Agents do not preload sources: their hot set carries a short map
of what exists, and they pull what the task needs. Adding a new kind of source
(MCP, Notion, Drive) is adding one adapter; the API and the agents do not
change.

```
 seat / worker ── unvrs ctl ctx map | search | get ──► KERNEL: registry, access, budget, cache, journal
                                                            │ one adapter protocol
                        ┌───────────────┬──────────────────┼──────────────────┐
                      path            git              memory (built in)     mcp (later), http, …
                   ~/acme   github.com/…         UNVRS notes, briefs
```

## 1. Source record

Registered in `~/.unvrs/sources.toml`; a project lists the sources it uses
(`project.toml: sources = ["acme"]`).

```toml
[[source]]
id = "acme"
kind = "path"                 # path | git | memory | mcp (later)
uri = "~/acme"
purpose = "Acme company context: business, brand, clients, sales, finance"
exclude = ["media/**", "archive/**"]
sensitivity = "private"       # public | private | secret
# kind = "git": uri = "github.com/org/repo", branch = "main", refresh = "1h"
```

- **path:** a local folder or repo, read in place.
- **git:** the kernel keeps a clone under `~/.unvrs/sources/<id>/` and pulls it
  on its refresh interval; answers cite the commit they came from.
- **memory:** built in. UNVRS notes, briefs and field notes are a source too,
  so one search can cover both what the captain owns and what the system
  learned.
- **mcp** (later): an MCP server's resources and search tool (§4).

A source may carry an optional `unvrs-context.toml` at its root to declare its
own map entry points, excludes and sensitivity; the registry wins on conflict.

## 2. The API (kernel syscalls)

Surface is CLI-first (`unvrs ctl ctx …`, D21); the same calls are exposed on
the kernel socket for drivers and, later, an MCP adapter.

| Call | Returns | Use |
| --- | --- | --- |
| `ctx sources` | the sources the caller may read, with purpose | orientation |
| `ctx map <source> [path]` | the source's entry points: its `CONTEXT-MAP.md` / `CONTEXT.md` / `AGENTS.md` / `README.md` if present, else a generated outline; bounded size | "what is in here" |
| `ctx search "<query>" [--source s …] [--k n]` | ranked hits: ref, title, snippet, version, age | find |
| `ctx get <ref> [--lines a-b]` | content, bounded bytes, with its version | read |

- **Refs are stable and cite a version:**
  `ctx://acme/brand/CONTEXT.md@<commit>#L10-40`. A ref with a version lets
  a later reader tell whether the context changed (stale is noise, D22).
- **Bounded:** every answer has a byte cap; large files come back as a slice
  with a pointer to the next one.
- **Search v0 is lexical** (ripgrep-style over text files, honoring excludes);
  an index is a disposable cache under `~/.unvrs/cache/ctx/`, added only when
  lexical search proves too weak. Semantic search is a later adapter feature,
  not an API change.

## 3. Access and safety

- **Who reads what:** L1 reads every source; an L2 reads its project's
  sources; an L3 reads the sources its task contract lists (D47); consultants
  read what the asker passes. Refused reads are journaled.
- **Read-only.** The API never writes. Changing a source (for example updating
  a client record in `~/acme`) is an L3 **act** task in a workspace of
  that repo, delivered under the project's authority (D52).
- **Content is data, never instruction** (D50): text from a source cannot
  grant authority or change a seat's rules, whatever it says.
- **Sensitivity:** `secret` sources are never sent to external services (Jev,
  consultants on other accounts) and appear in the Observatory by id only.
- **Journal:** every `search` and `get` is journaled with caller, source, ref
  and bytes, so the Observatory can show what context an agent used.

## 4. Adapters

One protocol for every kind, the same shape as event sources (D50):

| Operation | Meaning |
| --- | --- |
| `describe` | kind, capabilities (search? versions? change feed?) |
| `map(path?)` | entry points and outline |
| `search(query, k)` | hits with refs |
| `get(ref, range)` | content and version |
| `changes(since)` | optional; feeds event sources and cache invalidation |

- `path`, `git` and `memory` are built into the kernel (Rust).
- Any other adapter is a child process speaking JSON over stdin/stdout, so it
  can be written in any language and cannot crash the kernel.
- **MCP adapter:** `map` ← `resources/list`, `get` ← `resources/read`,
  `search` ← a search tool the server declares (or lexical over the resource
  list). Adding an MCP source is a registry entry, not new API.

## 5. How agents use it

- A seat's hot set includes a **short map** of its sources (id, purpose, top
  entry points), within the hot-set budget.
- The `unvrs` skill tells agents: check the map, search before saying you do
  not know, cite refs in reports and field notes.
- Field notes and memory notes may point at refs instead of copying content.

## 6. Build

| Phase | Builds |
| --- | --- |
| **Crew (0.8)** | registry; `path`, `git` and `memory` adapters; `ctx sources / map / search / get`; access by rank and project; maps in the hot set; journaled reads |
| **Knowledge (0.10)** | adapter protocol for child processes; MCP adapter; change feeds into event sources; an index if lexical search is too weak |

## Open

- Semantic search: which embedding model, local or remote, and whether it may
  see `private` sources.
- Refresh policy for large git sources; partial clones.
