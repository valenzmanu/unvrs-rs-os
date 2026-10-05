#!/bin/sh
# Verified captain-go admission and research defaults: worker-built client,
# native gates, a 900-second drain without force, doctor and rollback recovery.
exec "$(dirname "$0")/deploy-drvecon.sh" "$@"
