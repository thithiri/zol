#!/usr/bin/env bash
# Two-node partition reorg experiment (spike 0.11).
# Usage: reorg.sh B_LEN A_LEN [HOLD_S=75] [TIMEOUT_S=900]
#   Both nodes share a prefix; b is cut off the p2p network; b mines B_LEN blocks, a mines A_LEN
#   blocks; the network is healed and we wait for b's tip to equal a's. b must reorg by B_LEN
#   blocks if A_LEN > B_LEN. Prints timings and evidence.
set -euo pipefail
source "$(dirname "$0")/lib.sh"
blen="${1:?b fork length}"; alen="${2:?a fork length}"; hold="${3:-75}"; timeout="${4:-900}"
ts() { date +%s; }
tip() { rpc "$1" getbestblockhash | result; }
h()   { rpc "$1" getblockcount | result; }
wait_equal() {
  local t0; t0=$(ts)
  until [[ "$(tip a)" == "$(tip b)" ]]; do
    (( $(ts) - t0 > timeout )) && { echo "TIMEOUT: a=$(h a)/$(tip a) b=$(h b)/$(tip b)"; return 1; }
    sleep 1
  done
  echo "converged after $(( $(ts) - t0 ))s at height $(h a)"
}

echo "== prefix: a=$(h a) b=$(h b)"
rpc a generate '[2]' | result >/dev/null
wait_equal
base_h=$(h a); base=$(tip a)
echo "common base height=$base_h hash=$base"

"$(dirname "$0")/partition.sh"
t0=$(ts)
bfork=$(rpc b generate "[$blen]" | result)
afork=$(rpc a generate "[$alen]" | result)
b_first=$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])[0])' "$bfork")
b_last=$(tip b)
echo "partitioned: a height=$(h a) tip=$(tip a) | b height=$(h b) tip=$b_last (b fork first=$b_first) mined in $(( $(ts) - t0 ))s"

# Hold the partition long enough for the nodes' heartbeats to drop the old TCP connections,
# then prove nothing leaked across.
sleep "$hold"
echo "after ${hold}s partition: a height=$(h a) tip=$(tip a) | b height=$(h b) tip=$(tip b) peers_b=$(rpc b getpeerinfo | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["result"]))')"
[[ "$(tip b)" == "$b_last" ]] || { echo "LEAK: b tip moved during partition"; exit 1; }

echo "== heal"
since=$(date -u +%Y-%m-%dT%H:%M:%SZ)
"$(dirname "$0")/peer.sh" >/dev/null
wait_equal
echo "-- b log during heal:"
docker logs --since "$since" "$CPREFIX-b" 2>&1 | grep -E "switched best chain|chain fork|reorg|rollback|error|ERROR|WARN" | cut -c1-220 | head -12
echo "b block $b_first now: $(rpc b getblock "[\"$b_first\",1]" | python3 -c 'import json,sys; r=json.load(sys.stdin); print(r.get("error") or {"height":r["result"].get("height"),"confirmations":r["result"].get("confirmations")})')"
echo "b height at base+1 now: $(rpc b getblockhash "[$((base_h+1))]" | result) (a: $(rpc a getblockhash "[$((base_h+1))]" | result))"
