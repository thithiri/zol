#!/usr/bin/env bash
# Stops and removes both nodes and the p2p network (state is ephemeral, nothing left on disk).
set -euo pipefail
source "$(dirname "$0")/lib.sh"
"${COMPOSE[@]}" down --remove-orphans
