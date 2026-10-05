#!/usr/bin/env bash
# Voyage 0.6: A1–A11. A10 makes live calls to all four authenticated ACP harnesses.
set -uo pipefail
cd "$(dirname "$0")/../.."
S=behaviour-eval/scripts
LOG=behaviour-eval/journals/eval-auto-0.6.log
mkdir -p "$(dirname "$LOG")"
: > "$LOG"
FAILS=0
check() {
  local id=$1
  shift
  echo "==> $id"
  "$@" >> "$LOG" 2>&1
  local code=$?
  echo "$id exit=$code" | tee -a "$LOG"
  if [ "$code" -ne 0 ]; then FAILS=$((FAILS+1)); fi
}
check A1 "$S/workspace-smoke.sh"
check A2 cargo test -p uke --test memory a2_
check A3 cargo test -p uke --test memory a3_
check A4 cargo test -p uke --test memory a4_
check A5 cargo test -p uke --test memory a5_
a6() {
  cargo test -p uke --test memory a6_ && cargo test -p drv_hdff --test compaction a6_
}
check A6 a6
a7() { cargo test -p drv_agent --test memory a7_ && "$S/memory-bus-smoke.py"; }
check A7 a7
check A8 cargo test -p uke --test memory a8_
check A9 cargo test -p drv_agent --test memory a9_
a10() {
  "$S/mission-admit-smoke" && "$S/boot-e2e-smoke" && "$S/cockpit-boot-smoke.py" && \
    UNVRS_CODEX_MODEL="${UNVRS_CODEX_MODEL:-gpt-5.6-sol[low]}" "$S/handoff-matrix-smoke"
}
check A10 a10
check A11 "$S/cockpit-memory-smoke.py"
if [ "$FAILS" -eq 0 ]; then echo 'eval-auto-0.6: PASS' | tee -a "$LOG"; else echo "eval-auto-0.6: FAIL ($FAILS); see $LOG"; exit 1; fi
