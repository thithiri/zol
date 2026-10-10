#!/usr/bin/env bash
# Checks whether a Zcash JSON-RPC endpoint can serve zol-zcashd on NU7 testnet.
#   scripts/probe-zcash-rpc.sh <url> [user:password]
# Pass: tip branch is NU7 (77190ad9), recent blocks average ~25 s, raw getblock
# works, and z_gettreestate returns an Ironwood tree. Uses 5 requests.
set -euo pipefail
URL="$1"; AUTH="${2:-}"
rpc() {
  local args=(-s -m 30 -X POST -H 'content-type: application/json' --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$1\",\"params\":$2}")
  [ -n "$AUTH" ] && args+=(-u "$AUTH")
  curl "${args[@]}" "$URL"
}
info=$(rpc getblockchaininfo '[]')
tip=$(echo "$info" | python3 -c 'import json,sys; r=json.load(sys.stdin)["result"]; print(r["blocks"])')
echo "$info" | python3 -c '
import json,sys
r=json.load(sys.stdin)["result"]
print("chain:", r["chain"], "height:", r["blocks"], "tip branch:", r["consensus"]["chaintip"])
nu7=[v for k,v in r.get("upgrades",{}).items() if "7" in v.get("name","")]
print("NU7 entry:", nu7 or "none")
print("NU7 tip branch:", "yes" if r["consensus"]["chaintip"]=="77190ad9" else "NO")'
t1=$(rpc getblock "[\"$((tip-200))\",1]" | python3 -c 'import json,sys; print(json.load(sys.stdin)["result"]["time"])')
t2=$(rpc getblock "[\"$tip\",1]" | python3 -c 'import json,sys; print(json.load(sys.stdin)["result"]["time"])')
echo "avg block interval over last 200 blocks: $(( (t2-t1)/200 )) s (NU7 target 25 s, before NU7 75 s)"
raw=$(rpc getblock "[\"$tip\",0]" | python3 -c 'import json,sys; r=json.load(sys.stdin).get("result"); print(len(r) if isinstance(r,str) else "no")')
echo "raw getblock hex length: $raw"
rpc z_gettreestate "[\"$tip\"]" | python3 -c '
import json,sys
d=json.load(sys.stdin); r=d.get("result") or {}
print("z_gettreestate pools:", sorted(k for k in r if isinstance(r.get(k),dict)) or d.get("error"))
print("ironwood tree:", "yes" if "ironwood" in r else "NO")'
