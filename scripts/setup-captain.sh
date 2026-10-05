#!/bin/sh
# Kept for compatibility: the one command is now ./install.sh at the repo root (which
# builds and runs `unvrs setup`). Arguments pass through (--seed <dir>, --no-seed,
# --trust-hooks, …).
exec "$(cd "$(dirname "$0")/.." && pwd)/install.sh" "$@"
