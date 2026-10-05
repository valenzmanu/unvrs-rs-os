#!/bin/sh
# Agent protocol and health deploy; the shared wrapper owns native gates and recovery.
exec "$(dirname "$0")/deploy-drvecon.sh" "$@"
