#!/usr/bin/env bash
# Test tiers.
#
#   scripts/test.sh fast      unit, LiteSVM ledger and coin-flip, zol-zcash offline, web, wire.
#                             No external services. Run on every change.
#   scripts/test.sh regtest   fast + every Zakura-backed suite (zol-zcash regtest, gateway
#                             cross-process and multi-host), in five parallel lanes, each on
#                             its own fresh Zakura pair. KEEP_PAIRS=1 leaves them running.
#   scripts/test.sh full      regtest + the 2,000-case invariant fuzz and the full local
#                             Zcash-leg end to end (MagicBlock stack, VRF, three gateways).
#
# Uses cargo-nextest (parallel test processes) and sccache (via .cargo/config.toml).
# Keep target/ directories warm: incremental runs are much faster than cold ones.
set -euo pipefail
cd "$(dirname "$0")/.."
TIER="${1:-fast}"
ROOT="$PWD"
t0=$(date +%s)
step() { printf '\n== %s (%ss)\n' "$1" "$(( $(date +%s) - t0 ))"; }

need() { command -v "$1" >/dev/null || { echo "missing $1: $2" >&2; exit 1; }; }
need cargo-nextest "cargo install cargo-nextest --locked"
need sccache "brew install sccache"

step "build SBF programs (release and dev-timings)"
./scripts/build-sbf.sh >/dev/null

step "root workspace: unit + LiteSVM"
cargo nextest run --workspace --no-fail-fast

step "zol-zcash offline"
(cd crates/zol-zcash && cargo nextest run --release --no-fail-fast)

step "web + wire (TypeScript)"
(cd web && npm test --silent)
(cd crates/zol-wire/ts && bun test)

if [ "$TIER" = "fast" ]; then
  step "fast tier done"
  exit 0
fi

step "build regtest suites"
(cd crates/zol-zcash && cargo build --release --bin zol-zcashd && cargo nextest run --release --profile regtest --run-ignored only --no-run)
cargo nextest run -p zol-gateway --profile regtest --run-ignored only --no-run

# Each lane gets its own fresh Zakura pair (ZOL_NODE_PAIR, own RPC ports) and runs
# beside the others. Within a lane, tests run one at a time (profile regtest).
# The default pair (zol-zakura, :18232) is left alone for manual work.
LOGS="$ROOT/target/regtest-lanes"
mkdir -p "$LOGS"
# lane NAME PORT DIR FILTER [NEXTEST ARGS...]
lane() {
  local name="$1" port="$2" dir="$3" filter="$4"
  shift 4
  (
    export ZOL_NODE_PAIR="zol-$name" ZOL_NODE_RPC_PORT="$port" ZOL_NODE_B_RPC_PORT="$((port + 10))"
    export ZOL_NODE_URL="http://127.0.0.1:$port/" ZOL_NODE_B_URL="http://127.0.0.1:$((port + 10))/"
    infra/zakura/scripts/down.sh >/dev/null 2>&1 || true
    infra/zakura/scripts/up.sh pair
    rc=0
    (cd "$dir" && cargo nextest run "$@" --profile regtest --run-ignored only --no-fail-fast -E "$filter") || rc=$?
    [ -n "${KEEP_PAIRS:-}" ] || infra/zakura/scripts/down.sh >/dev/null 2>&1 || true
    exit $rc
  ) >"$LOGS/$name.log" 2>&1
}

step "regtest lanes in parallel (logs in target/regtest-lanes)"
pids=() names=()
lane zk-reorg  18250 crates/zol-zcash 'binary(regtest)' --release & pids+=($!) names+=(zk-reorg)
lane zk-phase4 18270 crates/zol-zcash 'binary(phase4_regtest)' --release & pids+=($!) names+=(zk-phase4)
lane zk-daemon 18290 crates/zol-zcash 'not (binary(regtest) | binary(phase4_regtest))' --release & pids+=($!) names+=(zk-daemon)
lane gw-multi  18310 . 'binary(multihost)' -p zol-gateway & pids+=($!) names+=(gw-multi)
lane gw-e2e    18330 . 'not binary(multihost)' -p zol-gateway & pids+=($!) names+=(gw-e2e)
failed=0
for i in "${!pids[@]}"; do
  if wait "${pids[$i]}"; then r=ok; else r=FAILED; failed=1; fi
  printf '  %-10s %-6s %s\n' "${names[$i]}" "$r" "$(grep -E 'Summary' "$LOGS/${names[$i]}.log" | tail -1 | sed 's/^ *//')"
done
if [ "$failed" = 1 ]; then
  echo "regtest lanes failed; see $LOGS" >&2
  exit 1
fi

if [ "$TIER" = "regtest" ]; then
  step "regtest tier done"
  exit 0
fi

step "invariant fuzz, 2,000 cases (release)"
PROPTEST_CASES=2000 cargo nextest run -p zol-tests --release --test invariants

step "Zakura default pair (for the Zcash leg)"
if ! curl -s -m 3 -X POST -H 'content-type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"getblockcount","params":[]}' http://127.0.0.1:18232/ >/dev/null; then
  infra/zakura/scripts/up.sh pair
fi

step "Zcash leg end to end (MagicBlock stack, VRF, three gateways)"
(cd spikes/magicblock/zcash-leg && ./run.sh)

step "full tier done"
cd "$ROOT"
