#!/bin/sh
set -eu
cd "$(dirname "$0")/.."

# Only this module may call the raw Unix signal API, including read-only probes.
if git grep -nE 'libc[[:space:]]*::[[:space:]]*kill|nix[[:space:]]*::.*signal' -- '*.rs' ':!uke/src/signals/ledger.rs'; then
    echo 'Raw signals must go through the ledger Signaller.' >&2
    exit 1
else
    result=$?
    [ "$result" -eq 1 ] || exit "$result"
fi

if git grep -nE 'os_pid[[:space:]]*:[[:space:]]*Some[[:space:]]*\([[:space:]]*[01][[:space:]]*\)' -- '*.rs'; then
    echo 'PID 0/1 fixtures can broadcast signals; use None or a spawned child.' >&2
    exit 1
else
    result=$?
    [ "$result" -eq 1 ] || exit "$result"
fi

# stdlib kill bypasses authorization too. All kernel/driver commands must register.
if git grep -nE '\.(spawn|output|status)[[:space:]]*\([[:space:]]*\)' -- 'uke/src/*.rs' 'drv_*/src/*.rs' ':!uke/src/signals/runtime.rs'; then
    echo 'Use tracked spawn/output/status and Signaller termination.' >&2
    exit 1
else
    result=$?
    [ "$result" -eq 1 ] || exit "$result"
fi

if git grep -nE '\.kill[[:space:]]*\(' -- '*.rs'; then
    echo 'Child::kill bypasses the ledger Signaller.' >&2
    exit 1
else
    result=$?
    [ "$result" -eq 1 ] || exit "$result"
fi
