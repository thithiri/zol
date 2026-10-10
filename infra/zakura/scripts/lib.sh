# Shared helpers. Source me.
ZDIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# ZOL_NODE_CONFIG=nu7 (default): every upgrade through NU7 at height 1 (regtest-nu7.toml).
# ZOL_NODE_CONFIG=pre-nu7: through NU6.3 only (regtest-pre-nu7.toml).
export ZOL_NODE_CONFIG="${ZOL_NODE_CONFIG:-nu7}"
case "$ZOL_NODE_CONFIG" in
  nu7|pre-nu7) ;;
  *) echo "ZOL_NODE_CONFIG must be nu7 or pre-nu7" >&2; exit 1 ;;
esac
RPC_PORT="${ZOL_NODE_RPC_PORT:-18232}"
CPREFIX="zol-zakura"              # containers zol-zakura-a / zol-zakura-b
P2P_NET="zol-zakura-p2p"
COMPOSE=(docker compose -f "$ZDIR/docker-compose.yml" --profile pair)

# rpc NODE METHOD [JSON_PARAMS]  -> prints the JSON-RPC response body.
# Runs curl *inside* the container so it works even while the node is partitioned.
rpc() {
  local node="$1" method="$2" params="${3:-[]}"
  docker exec "$CPREFIX-$node" curl -s --max-time 900 -H 'content-type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$method\",\"params\":$params}" \
    http://127.0.0.1:18232/
}

# Extract .result (or die with .error) using python3 on the host.
result() { python3 -c 'import json,sys; r=json.load(sys.stdin)
e=r.get("error")
if e: sys.exit("rpc error: %s" % e)
v=r["result"]; print(v if isinstance(v,(str,int)) else json.dumps(v))'; }

wait_rpc() {
  local node="$1"
  for _ in $(seq 1 120); do
    rpc "$node" getblockcount 2>/dev/null | grep -q '"result"' && return 0
    sleep 1
  done
  echo "node $node RPC not up" >&2; return 1
}
