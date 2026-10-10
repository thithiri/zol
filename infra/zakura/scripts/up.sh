#!/usr/bin/env bash
# Usage: up.sh [a|pair]   (default a: one node; pair: nodes a and b, peered via addnode)
set -euo pipefail
source "$(dirname "$0")/lib.sh"
mode="${1:-a}"
if [[ "$mode" == pair ]]; then
  "${COMPOSE[@]}" up -d zakura-a zakura-b
  wait_rpc a; wait_rpc b
  "$(dirname "$0")/peer.sh"
else
  "${COMPOSE[@]}" up -d zakura-a
  wait_rpc a
fi
echo "$CPREFIX-a RPC: http://127.0.0.1:$RPC_PORT  height=$(rpc a getblockcount | result)"
