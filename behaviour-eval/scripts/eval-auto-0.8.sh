#!/usr/bin/env bash
# Voyage 0.8.0-eval.1 gate: A1–A12, plus SETUP (./install.sh → `unvrs setup` on a pty, with the captain's
# ~/context/_seed, read only). Real binaries, temp UNVRS_HOME, copies of the harness
# homes (never the captain's live ~/.claude, ~/.codex, ~/.unvrs or launchd domain), no
# mocks of kernel or harness. A3/A4, A6 and A8 run live against real Claude
# (stream-json, as T3 Code runs it) and real `codex app-server`.
set -uo pipefail
cd "$(dirname "$0")/../.."
S=behaviour-eval/scripts
LOG=behaviour-eval/journals/eval-auto-0.8.log
mkdir -p "$(dirname "$LOG")"
: > "$LOG"
FAILS=0
check() {
  local id=$1
  shift
  echo "==> $id" | tee -a "$LOG"
  "$@" 2>&1 | tee -a "$LOG"
  local code=${PIPESTATUS[0]}
  echo "$id exit=$code" | tee -a "$LOG"
  if [ "$code" -ne 0 ]; then FAILS=$((FAILS+1)); fi
}
echo "eval-auto-0.8 · $(date -u +%FT%TZ) · $(git rev-parse --short HEAD) · claude $(claude --version 2>/dev/null) · $(codex --version 2>/dev/null)" | tee -a "$LOG"
cargo build -q -p unvrs || exit 1
LIVE_FILES=("$HOME/.claude/settings.json" "$HOME/.claude/settings.local.json" "$HOME/.claude/plugins/installed_plugins.json" \
  "$HOME/.claude/plugins/known_marketplaces.json" "$HOME/.codex/config.toml" "$HOME/.codex/hooks.json")
sums() { for f in "${LIVE_FILES[@]}"; do [ -f "$f" ] && shasum -a 256 "$f"; done; }
BEFORE=$(sums)

check A1 python3 "$S/a1-skills.py"
check A2 python3 "$S/crew-e2e.py" A2
check A3-A4 python3 "$S/a4-live-move.py"
check A5 python3 "$S/crew-e2e.py" A5
check A6 python3 "$S/a6-live-memory.py"
check A7 python3 "$S/crew-e2e.py" A7
a8() { python3 "$S/crew-e2e.py" A8f && python3 "$S/a8-live-task.py"; }
check A8 a8
check A9 python3 "$S/crew-e2e.py" A9
check A10 python3 "$S/crew-e2e.py" A10
check A11 python3 "$S/a11-install.py"
a12() {
  "$S/workspace-smoke-0.8.sh" && python3 "$S/kernel-e2e.py" && \
    cargo test -q -p uke --test memory && cargo test -q -p uke --test brief && \
    cargo test -q -p drv_hdff --test compaction a6_ && cargo test -q -p drv_agent --test memory && \
    "$S/mission-admit-smoke" && "$S/boot-e2e-smoke"
}
check A12 a12
check SETUP python3 "$S/setup-captain-e2e.py"
live_homes() {
  local after
  after=$(sums)
  if [ "$BEFORE" = "$after" ]; then echo "captain's live harness files unchanged by the gate (sha256)"; else
    echo "captain's live harness files CHANGED during the gate"; diff <(echo "$BEFORE") <(echo "$after"); return 1; fi
}
check LIVE-HOMES live_homes
if [ "$FAILS" -eq 0 ]; then echo 'eval-auto-0.8: PASS' | tee -a "$LOG"; else echo "eval-auto-0.8: FAIL ($FAILS); see $LOG" | tee -a "$LOG"; exit 1; fi
