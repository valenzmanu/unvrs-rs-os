#!/usr/bin/env bash
# 0.8 workspace smoke (the 0.7 A1 adapted): layout, fmt, clippy, tests, version 0.8.0,
# kernel daemon without a TTY on a temp UNVRS_HOME, drv_intf frozen (no kernel state),
# flight.rs gone, drv_obs folded, and no role prose in uke (C11).
set -uo pipefail
cd "$(dirname "$0")/../.."
JOURNAL=behaviour-eval/journals/workspace-smoke-0.8.log
mkdir -p "$(dirname "$JOURNAL")"
: >"$JOURNAL"
FAILS=0
check() {
  local label=$1
  shift
  if "$@" >>"$JOURNAL" 2>&1; then echo "PASS $label"; else echo "FAIL $label (see $JOURNAL)"; FAILS=$((FAILS + 1)); fi
}
for crate in uke drv_agent drv_hdff drv_intf mapp_unvrs observatory unvrs; do
  check "member $crate" test -f "$crate/Cargo.toml"
done
for module in uke drv_agent drv_hdff drv_intf mapp_unvrs observatory; do
  check "front door $module" test -f "$module/src/lib.rs"
  check "no alternate entry $module" test ! -f "$module/src/entry.rs"
done
check "C0: flight.rs deleted" test ! -f uke/src/kernel/flight.rs
check "C0: drv_obs folded into uke" bash -c 'test ! -d drv_obs && test -f uke/src/obs.rs'
check "no product src under behaviour-eval" bash -c '! find behaviour-eval -name "*.rs" -not -path "*/target/*" | grep -q .'
check "drv_intf binds no socket" bash -c '! grep -rn "UnixListener\|\.bind(" drv_intf/src'
check "drv_intf routes no handoff and holds no mailbox" bash -c '! grep -rn "enqueue_mail\|VecDeque<Mail>\|fn dispatch\|to_rank < source" drv_intf/src'
check "C11: no role prose in uke" bash -c '! grep -rniE "chief of staff|copilot|personal assistant|project lead|you are (an? )?(l1|l2|l3|unvrs)|outcomes, not mechanics" uke/src'
check "C11: the mapp owns the words" grep -q "impl uke::Mapp for UnvrsMapp" mapp_unvrs/src/lib.rs
check "C11: every journal event carries mapp and project" grep -q 'm.entry("mapp")' uke/src/kernel.rs
check "cargo fmt --check" cargo fmt --all --check
check "cargo clippy -D warnings" cargo clippy --workspace --all-targets -- -D warnings
check "cargo test --workspace" cargo test --workspace
check "cargo build -p unvrs" cargo build -p unvrs
check "unvrs --version is 0.8.0" bash -c 'target/debug/unvrs --version | grep -q "^version: 0\.8\.0$"'
headless() {
  local u
  u=$(mktemp -d /tmp/unvrs-ws-XXXX)
  UNVRS_HOME=$u UNVRS_OBSERVATORY_PORT=off target/debug/unvrs kernel start </dev/null &&
    UNVRS_HOME=$u target/debug/unvrs kernel status </dev/null | grep -q "kernel: running" &&
    UNVRS_HOME=$u target/debug/unvrs kernel stop </dev/null
  local r=$?
  rm -rf "$u"
  return $r
}
check "kernel runs with no TTY on a temp UNVRS_HOME" headless
echo "fails: $FAILS"
echo "journal: $JOURNAL"
[ "$FAILS" -eq 0 ] && echo "workspace-smoke-0.8: PASS" || { echo "workspace-smoke-0.8: FAIL"; exit 1; }
