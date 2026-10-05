#!/usr/bin/env bash
# Usage: mine.sh N [node=a]  -> mines N blocks with the regtest `generate` RPC, prints new tip.
set -euo pipefail
source "$(dirname "$0")/lib.sh"
n="${1:?number of blocks}"; node="${2:-a}"
rpc "$node" generate "[$n]" | result >/dev/null
echo "$node height=$(rpc "$node" getblockcount | result) tip=$(rpc "$node" getbestblockhash | result)"
