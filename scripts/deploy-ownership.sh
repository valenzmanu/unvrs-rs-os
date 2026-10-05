#!/bin/sh
# Context ownership (PID 198, docs/design/context-ownership.md): deploy the kernel with the
# release wrapper (drain up to 900 s, no force, doctor, rollback on failure), then run the
# live end-to-end: one Claude Code task and one Codex task that must pass the
# context-ownership check, and one task that writes its deliverable outside UNVRS and must
# be bounced. The Observatory's Context panel then shows all three.
#
# Run outside a driven turn (the captain's terminal, or an L1/L2 thread):
#   scripts/deploy-ownership.sh <commit>
# UNVRS_E2E_ONLY=1 skips the deploy (the commit must already be live).
set -eu
if [ "$#" -ne 1 ]; then
    echo "usage: $0 <commit>" >&2
    exit 2
fi
here=$(cd "$(dirname "$0")" && pwd -P)
home="${UNVRS_HOME:-$HOME/.unvrs}"
bin="$home/bin/unvrs"
if [ -z "${UNVRS_E2E_ONLY:-}" ]; then
    "$here/deploy-release.sh" "$1"
fi
live=$("$bin" --version | sed -n 's/^commit: //p')
full=$(git -C "$here/.." rev-parse --verify "$1^{commit}")
if [ "$live" != "$full" ]; then
    echo "Live kernel is $live, not $full; nothing dispatched." >&2
    exit 1
fi
echo "live: $live"

# The captain's go for this work (2026-10-04); from the captain's own terminal no go is needed.
go="Deploy the latest kernel"
stamp=$(date +%s)
leak="/tmp/unvrs-ownership-leak-$stamp.md"
dispatch() { # harness spec -> PID
    "$bin" ctl task --project unvrs-rs \
        --intent "Context-ownership end-to-end check ($1)" \
        --spec "$2" --shape report --go "$go" \
        --done-when "The handoff passes the context-ownership check" \
        --harness "$1" --kind validate --judgment low --thoroughness low \
        | sed -n 's/.*L3 PID \([0-9][0-9]*\).*/\1/p' | head -n 1
}
pass_spec() {
    echo "Write a file named e2e-$1.md in your working directory (your UNVRS task directory) containing one sentence on why UNVRS keeps the copy of record of job context. Then finish in this same reply: the HANDOFF block listing that file under deliverables, one decision, one learning, FIELD-NOTES, and UNVRS-RESULT last. No other tools, sources or delegation."
}
claude=$(dispatch claude "$(pass_spec claude)")
codex=$(dispatch codex "$(pass_spec codex)")
bounced=$(dispatch claude "Write a one-sentence note to $leak (outside your task directory, on purpose) and list exactly that absolute path as your only deliverable in the HANDOFF block; finish in this same reply with HANDOFF, FIELD-NOTES and UNVRS-RESULT last. If UNVRS sends the result back, follow its instructions.")
echo "dispatched: Claude Code PID $claude, Codex PID $codex, bounced PID $bounced"
[ -n "$claude" ] && [ -n "$codex" ] && [ -n "$bounced" ] || {
    echo "A dispatch failed; see unvrs ctl tree." >&2
    exit 1
}

tasks="$home/projects/unvrs-rs/tasks"
deadline=$(($(date +%s) + 900))
for pid in $claude $codex $bounced; do
    while [ ! -f "$tasks/pid-$pid/result.json" ]; do
        if [ "$(date +%s)" -ge "$deadline" ]; then
            echo "PID $pid has not finished; see the Observatory." >&2
            exit 1
        fi
        sleep 5
    done
done
fail=0
for pid in $claude $codex $bounced; do
    python3 - "$tasks/pid-$pid/result.json" <<'EOF' || fail=1
import json, sys
r = json.load(open(sys.argv[1]))
o = r.get("ownership") or {}
hist = " -> ".join(h["verdict"] for h in o.get("history", []))
print(f"PID {r['pid']} {r['harness']} {r.get('actual_model')}: {r['how']}, check {o.get('verdict')} ({hist}); leaks first seen: {(o.get('history') or [{}])[0].get('leaks')}")
sys.exit(0 if r["how"] == "done" and o.get("verdict") == "accepted" else 1)
EOF
done
python3 - "$tasks/pid-$bounced/ownership.json" <<'EOF' || fail=1
import json, sys
h = json.load(open(sys.argv[1]))["history"]
ok = h and h[0]["verdict"] == "bounced" and "deliverable outside UNVRS" in json.dumps(h[0]["leaks"])
print("bounce:", "verified" if ok else "NOT seen")
sys.exit(0 if ok else 1)
EOF
rm -f "$leak"
echo "Observatory: http://unvrs.localhost:7576/ (Context panel; click a row for what UNVRS holds)"
exit "$fail"
