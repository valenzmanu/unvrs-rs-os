#!/bin/sh
# Release deployment shares native gates, full doctor and rollback recovery.
exec "$(dirname "$0")/deploy-drvecon.sh" "$@"
