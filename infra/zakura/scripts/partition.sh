#!/usr/bin/env bash
# Cut node b off the p2p network (container keeps running; RPC via docker exec still works).
set -euo pipefail
source "$(dirname "$0")/lib.sh"
docker network disconnect "$P2P_NET" "$CPREFIX-b"
echo "b partitioned"
