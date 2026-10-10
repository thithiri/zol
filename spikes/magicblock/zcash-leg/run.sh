#!/bin/bash
# The coin flip with the Zcash leg, all local:
#   Zebra regtest (spikes/zebra, :18232)  ->  3 x zol-zcashd (--dev, one share each, :18310-18312)
#   3 x zol-gateway serve (:18710-18712, authenticated HTTP between them)
#   solana-test-validator + ephemeral-validator (start-local.sh, :28899 / :27799) + VRF oracle
#   the page's own code (web/scripts/zcash-leg.ts) as the player.
# Config and App are preloaded active (as gateway/tests/multihost.rs does) with the vault and
# genesis head from zol-zcashd, so there is no 3600 s activation wait.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
MB="$HERE/.."
ROOT="$(cd "$MB/../.." && pwd)"
ZEBRA="${ZEBRA:-http://127.0.0.1:18232}"
ZCASHD_BIN="${ZCASHD_BIN:-$ROOT/crates/zol-zcash/target/release/zol-zcashd}"
GATEWAY_BIN="${GATEWAY_BIN:-$ROOT/target/release/zol-gateway}"
CF="$MB/target/e2e/release/coinflip"
APP=7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo
W="$HERE/work"
# Stop a previous run: daemons, gateways, the local stack, the oracle.
[ -f "$W/pids" ] && xargs kill < "$W/pids" 2>/dev/null || true
pkill -f "node_modules/.bin/mb-stack --reset" || true
pkill -f "vrf-oracle-darwin-arm64" || true
while pgrep -f "ledger ./.local-ledger" >/dev/null || pgrep -f "listen 127.0.0.1:27799" >/dev/null; do sleep 1; done
rm -rf "$W" "$MB/.local-ledger" "$MB/magicblock-test-storage"; mkdir -p "$W/keys" "$W/logs"
APP_HEX=$(python3 -c "
a='123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'; n=0
for c in '$APP': n=n*58+a.index(c)
print(n.to_bytes(32,'big').hex())")
zebra() { curl -s "$ZEBRA" -H 'content-type: application/json' -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$1\",\"params\":${2:-[]}}"; }
jq_py() { python3 -c "import json,sys; d=json.load(sys.stdin); print($1)"; }
H=$(zebra getblockcount | jq_py 'd["result"]')
echo "zebra height $H"

# --- Zcash: three single-share daemons
for i in 0 1 2; do
  mkdir -p "$W/z$i"
  cat > "$W/z$i/zcashd.toml" <<TOML
listen = "127.0.0.1:1831$i"
network = "regtest"
state_dir = "$W/z$i/state"
start_height = $H
poll_ms = 300
apps = ["$APP_HEX"]
[zebra]
url = "$ZEBRA/"
TOML
  "$ZCASHD_BIN" --config "$W/z$i/zcashd.toml" --dev --dev-seed zol-coinflip --dev-share $i > "$W/logs/zcashd$i.log" 2>&1 &
  echo $! >> "$W/pids"
done
for i in 0 1 2; do until curl -sf "http://127.0.0.1:1831$i/health" >/dev/null; do sleep 0.5; done; done
VAULT=$(curl -s "http://127.0.0.1:18310/vault?app=$APP_HEX" | jq_py 'bytes(d["address"]).hex()')
GEN=$(curl -s -X POST "http://127.0.0.1:18310/dev/genesis" -H 'content-type: application/json' \
  -d "{\"app\": $(python3 -c "print(list(bytes.fromhex('$APP_HEX')))"), \"float\": 30000000}" | jq_py 'bytes(d["head_cmx"]).hex()')
zebra generate '[101]' >/dev/null
T=$(zebra getblockcount | jq_py 'd["result"]')
for i in 0 1 2; do until [ "$(curl -s "http://127.0.0.1:1831$i/health" | jq_py 'd.get("log_tip") or 0')" -ge "$T" ]; do sleep 0.5; done; done
echo "vault $VAULT genesis head $GEN (synced to $T)"

# --- Keys: FROST attestor (DKG), guardians, relayers
GROUP=$("$GATEWAY_BIN" keygen --operators 3 --threshold 2 --out "$W/keys" | sed -n 's/.*: //p')
for i in 0 1 2; do
  solana-keygen new --no-bip39-passphrase -s -o "$W/keys/guardian$i.json" >/dev/null
  solana-keygen new --no-bip39-passphrase -s -o "$W/keys/relayer$i.json" >/dev/null
done
G=$(for i in 0 1 2; do solana-keygen pubkey "$W/keys/guardian$i.json"; done | tr '\n' ' ')
echo "attestor group key $GROUP"

# --- Solana + rollup with Config and App preloaded
ACC=$("$CF" preload "$W/accounts" "$GROUP" "$VAULT" "$GEN" $G | awk '{printf "%s=%s ", $1, $2}')
cd "$MB"
(ZOL=1 ZOL_DIR=../../target/deploy DLP=devnet VRF_CLEAN=1 EXTRA_ACCOUNTS="$ACC" RUST_LOG=info ./start-local.sh > "$W/logs/mb-stack.log" 2>&1 &)
until grep -q -E "MagicBlock stack is ready|exited before|failed" "$W/logs/mb-stack.log"; do sleep 1; done
grep -q "MagicBlock stack is ready" "$W/logs/mb-stack.log" || { tail -30 "$W/logs/mb-stack.log"; exit 1; }
U=http://127.0.0.1:28899
solana airdrop 100 -k keys/payer.json -u $U >/dev/null
for i in 0 1 2; do solana airdrop 10 "$(solana-keygen pubkey "$W/keys/relayer$i.json")" -u $U >/dev/null; done
solana program deploy ../../target/deploy/zol_coinflip.so --program-id ../../keys/zol_coinflip-keypair.json -k keys/payer.json -u $U | grep Signature
(VRF_ORACLE_SKIP_PREFLIGHT=true RPC_URL=http://127.0.0.1:27799 WEBSOCKET_URL=ws://127.0.0.1:27800 VRF_ORACLE_HTTP_PORT=28181 RUST_LOG=info \
  node_modules/@magicblock-labs/vrf-oracle-darwin-arm64/bin/vrf-oracle > "$W/logs/vrf-oracle.log" 2>&1 &)

# --- Three gateways
names=(op0 op1 op2)
for i in 0 1 2; do
  peers=""
  for j in 0 1 2; do [ $j = $i ] && continue
    peers+="{\"name\":\"op$j\",\"url\":\"http://127.0.0.1:1871$j\",\"guardian\":\"$(solana-keygen pubkey "$W/keys/guardian$j.json")\"},"
  done
  cat > "$W/gw$i.json" <<JSON
{
  "name": "op$i", "listen": "127.0.0.1:1871$i", "network": 2,
  "depths": { "early": 10, "full": 100, "payout": 100 },
  "early_credit_cap": 10000000, "denominations": [1000000, 10000000], "tip_stall_secs": 1800,
  "payout": { "max_records_per_tx": 4, "max_inputs": 8, "action_sizes": [4, 8, 16, 32], "interval_secs": 0,
    "batch_records": 1, "payout_cap": 100000000000, "large_delay_secs": 3600, "hourly_cap": 18446744073709551615,
    "consolidate_interval_secs": 3600, "consolidate_at_notes": 16, "expiry_delta": 40 },
  "solana_rpc": "$U", "relayer_keypair": "$W/keys/relayer$i.json", "store": "$W/store$i.db",
  "zcashd": "http://127.0.0.1:1831$i", "guardian_keypair": "$W/keys/guardian$i.json",
  "attestor_share": "$W/keys/attestor-share-$i.json", "attestor_pubkeys": "$W/keys/attestor-pubkeys.json", "threshold": 2,
  "apps": [{ "program": "$APP", "gateway_master_secret": "5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e" }],
  "peers": [${peers%,}], "order": ["op0", "op1", "op2"], "epoch_secs": 8, "poll_ms": 400, "send_commitment": "confirmed"
}
JSON
  "$GATEWAY_BIN" serve --config "$W/gw$i.json" > "$W/logs/gateway$i.log" 2>&1 &
  echo $! >> "$W/pids"
done
for i in 0 1 2; do until curl -sf "http://127.0.0.1:1871$i/v1/apps/$APP" >/dev/null; do sleep 0.5; done; done
echo "gateways up"

# --- The player, through the page's code
cd "$ROOT/web" && npx vite-node scripts/zcash-leg.ts 2>&1 | tee "$W/logs/zcash-leg.jsonl"
