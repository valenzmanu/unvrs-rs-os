#!/usr/bin/env bash
# A1 · workspace layout, fmt, clippy, tests, and the binary reports 0.6.0. Run from repo root.
set -euo pipefail
cd "$(dirname "$0")/../.."
JOURNAL=behaviour-eval/journals/workspace-smoke.log
mkdir -p "$(dirname "$JOURNAL")"
: >"$JOURNAL"
FAILS=0
check() { # check <label> <command...>
  local label=$1
  shift
  if "$@" >>"$JOURNAL" 2>&1; then echo "PASS $label"; else echo "FAIL $label (see $JOURNAL)"; FAILS=$((FAILS + 1)); fi
}
for crate in uke drv_agent drv_hdff drv_obs drv_intf unvrs; do
  check "member $crate" test -f "$crate/Cargo.toml"
done
check "old package roots removed" bash -c 'test ! -d crates && test ! -d bins'
for module in uke drv_agent drv_hdff drv_obs drv_intf; do
  check "front door $module" test -f "$module/src/lib.rs"
  check "no alternate entry $module" test ! -f "$module/src/entry.rs"
done
check "no product src under behaviour-eval" bash -c '! find behaviour-eval -name "*.rs" -not -path "*/target/*" | grep -q .'
check "cargo fmt --check" cargo fmt --all --check
check "cargo clippy -D warnings" cargo clippy --workspace --all-targets -- -D warnings
check "cargo test --workspace" cargo test --workspace
check "cargo build -p unvrs" cargo build -p unvrs
check "unvrs --version is 0.6.0" bash -c 'target/debug/unvrs --version | grep -q "^version: 0\.6\.0$"'
echo "fails: $FAILS"
echo "journal: $JOURNAL"
[ "$FAILS" -eq 0 ] && echo "workspace-smoke: PASS" || { echo "workspace-smoke: FAIL"; exit 1; }
