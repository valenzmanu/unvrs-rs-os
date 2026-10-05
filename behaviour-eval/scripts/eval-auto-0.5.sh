#!/usr/bin/env bash
# EVAL 0.5.0-eval.1 Part A one-shot: A1–A7 + A9. A8 (live handoff matrix) only with --with-handoffs.
set -uo pipefail
cd "$(dirname "$0")/../.."
S=behaviour-eval/scripts
STEPS=("A1 $S/workspace-smoke.sh" "A2 $S/mission-admit-smoke" "A3 $S/pool-catalog-smoke" "A4 $S/picker-rules-smoke"
  "A5 $S/picker-jev-smoke" "A6 $S/agent-install-matrix-smoke" "A7 $S/boot-e2e-smoke"
  "A9 $S/cockpit-boot-smoke.py")
[ "${1:-}" = "--with-handoffs" ] && STEPS+=("A8 $S/handoff-matrix-smoke")
SUMMARY=behaviour-eval/journals/eval-auto-0.5.log
mkdir -p "$(dirname "$SUMMARY")"
: >"$SUMMARY"
FAILS=0
for step in "${STEPS[@]}"; do
  id=${step%% *}
  script=${step#* }
  echo "==> $id $script"
  "$script"
  code=$?
  echo "$id $script exit=$code" | tee -a "$SUMMARY"
  [ "$code" -eq 0 ] || FAILS=$((FAILS + 1))
done
echo "---"
cat "$SUMMARY"
[ "$FAILS" -eq 0 ] && echo "eval-auto-0.5: PASS" || { echo "eval-auto-0.5: FAIL ($FAILS)"; exit 1; }
