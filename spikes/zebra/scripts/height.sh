#!/usr/bin/env bash
# Usage: height.sh [node=a]
set -euo pipefail
source "$(dirname "$0")/lib.sh"
node="${1:-a}"
echo "$node height=$(rpc "$node" getblockcount | result) tip=$(rpc "$node" getbestblockhash | result)"
