---
id: msn_no_skill_needed
status: draft
investment: cheap
owner_pid: null
updated: 2026-09-21
---

# Outcome
The second heading in `README.md` reads `## Quick start` instead of `## Getting
started`, and every in-repo link or anchor that pointed at the old heading points at
the new one.

# Done check
`grep -c "## Quick start" README.md` prints 1, `grep -r "getting-started" docs README.md`
prints nothing, and `git diff --stat` touches only Markdown files.

# Scope
For a first-time reader of the repo: the section is a five-minute setup, so the
heading should say so.
In: the heading text in README.md and any anchor links to it.
Out: the body copy under the heading, any other section, docs restructuring.
Must not: reflow or rewrite paragraphs.

# Inputs
- prompt: Rename the README's "Getting started" heading to "Quick start" and update any anchors that referenced it. Nothing else.
- pointers: README.md, docs/

# Log
…
