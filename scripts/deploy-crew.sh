#!/bin/sh
# Crew diagram deploy (PID 159): native deploy owns failed gates; this wrapper adds full
# doctor and recovery. The worker-built client runs 'deploy'; the installed client runs
# doctor and rollback.
set -eu
if [ "$#" -ne 1 ]; then
    echo "usage: $0 <commit>" >&2
    exit 2
fi
if [ -n "${UNVRS_DRIVEN_PID:-}" ]; then
    echo "Refused: deployment cannot run inside driven PID $UNVRS_DRIVEN_PID; run it outside a driven turn." >&2
    exit 2
fi
repo=$(cd "$(dirname "$0")/.." && pwd -P)
commit=$(git -C "$repo" rev-parse --verify --end-of-options "$1^{commit}")
bin="${UNVRS_HOME:-$HOME/.unvrs}/bin/unvrs"
builder="$repo/target/debug/unvrs"
if [ ! -x "$builder" ]; then
    builder="$repo/target/release/unvrs"
fi
if [ ! -x "$builder" ]; then
    echo "Missing worker-built CLI: run cargo build --locked -p unvrs in $repo." >&2
    exit 2
fi

# On failure native deploy either leaves the old slot live or rolls back itself.
"$builder" deploy "$commit" --repo "$repo" --drain-secs 900 --force-requeue
if "$bin" doctor; then
    exit 0
fi
echo "Doctor failed after cutover; restoring the previous slot." >&2
if "$bin" rollback; then
    if "$bin" doctor; then
        echo "Previous slot restored and doctor passed; Crew diagram cutover failed." >&2
    else
        echo "Rollback completed, but doctor still fails; inspect native deployment status." >&2
    fi
else
    echo "Rollback failed; inspect native deployment status and its recovery commands." >&2
fi
exit 1
