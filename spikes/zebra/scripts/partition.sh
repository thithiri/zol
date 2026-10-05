#!/usr/bin/env bash
# Cut node b off the p2p network (container keeps running; RPC via docker exec still works).
set -euo pipefail
source "$(dirname "$0")/lib.sh"
docker network disconnect zol-zebra-p2p zol-zebra-b
echo "b partitioned"
