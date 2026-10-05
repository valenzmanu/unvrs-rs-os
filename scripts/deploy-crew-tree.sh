#!/bin/sh
# Observatory Crew worker tree (PID 182): native gates, the worker-built client, a 900-second
# drain without force, and full doctor recovery. Usage: deploy-crew-tree.sh <commit>
exec "$(dirname "$0")/deploy-drvecon.sh" "$@"
