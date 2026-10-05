#!/bin/sh
# Research routing uses native gates, a 900-second drain and full doctor recovery.
exec "$(dirname "$0")/deploy-drvecon.sh" "$@"
