# Shared helpers. Source me.
ZDIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# ZOL_ZEBRA=zebra (default): the Zebra 6.4.2 pair (docker-compose.yml, pre-NU7).
# ZOL_ZEBRA=zebra7: the Zebra 7 NU7 pair (docker-compose.zebra7.yml, docs/ops/nu7.md).
ZOL_ZEBRA="${ZOL_ZEBRA:-zebra}"
case "$ZOL_ZEBRA" in
  zebra)  COMPOSE_FILE="$ZDIR/docker-compose.yml";        RPC_PORT="${ZOL_ZEBRA_RPC_PORT:-18232}" ;;
  zebra7) COMPOSE_FILE="$ZDIR/docker-compose.zebra7.yml"; RPC_PORT="${ZOL_ZEBRA7_RPC_PORT:-18272}" ;;
  *) COMPOSE_FILE="${ZOL_ZEBRA_COMPOSE_FILE:?ZOL_ZEBRA is zebra, zebra7, or set ZOL_ZEBRA_COMPOSE_FILE}"
     RPC_PORT="${ZOL_ZEBRA_RPC_PORT:-18232}" ;;
esac
CPREFIX="zol-$ZOL_ZEBRA"          # containers $CPREFIX-a / $CPREFIX-b
P2P_NET="zol-$ZOL_ZEBRA-p2p"
COMPOSE=(docker compose -f "$COMPOSE_FILE" --profile pair)

# rpc NODE METHOD [JSON_PARAMS]  -> prints the JSON-RPC response body.
# Runs curl *inside* the container so it works even while the node is partitioned.
rpc() {
  local node="$1" method="$2" params="${3:-[]}"
  docker exec "$CPREFIX-$node" curl -s --max-time 900 -H 'content-type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$method\",\"params\":$params}" \
    http://127.0.0.1:18232/
}

# Extract .result (or die with .error) using python3 on the host.
result() { python3 -c 'import json,sys; r=json.load(sys.stdin); 
e=r.get("error")
if e: sys.exit("rpc error: %s" % e)
v=r["result"]; print(v if isinstance(v,(str,int)) else json.dumps(v))'; }

wait_rpc() {
  local node="$1"
  for _ in $(seq 1 120); do
    rpc "$node" getblockcount >/dev/null 2>&1 && rpc "$node" getblockcount | grep -q '"result"' && return 0
    sleep 1
  done
  echo "node $node RPC not up" >&2; return 1
}
