#!/usr/bin/env bash
# Test tiers.
#
#   scripts/test.sh fast      unit, LiteSVM ledger and coin-flip, zol-zcash offline, web, wire.
#                             No external services. Run on every change.
#   scripts/test.sh regtest   fast + every Zakura-backed suite (zol-zcash regtest, gateway
#                             cross-process and multi-host). Starts the Zakura pair if needed.
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

step "Zakura regtest pair"
if ! curl -s -m 3 -X POST -H 'content-type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"getblockcount","params":[]}' http://127.0.0.1:18232/ >/dev/null; then
  infra/zakura/scripts/up.sh pair
fi

step "zol-zcashd release binary"
(cd crates/zol-zcash && cargo build --release --bin zol-zcashd)

step "zol-zcash regtest suites"
(cd crates/zol-zcash && cargo nextest run --release --profile regtest --run-ignored only --no-fail-fast)

step "gateway cross-process and multi-host"
cargo nextest run -p zol-gateway --profile regtest --run-ignored only --no-fail-fast

if [ "$TIER" = "regtest" ]; then
  step "regtest tier done"
  exit 0
fi

step "invariant fuzz, 2,000 cases (release)"
PROPTEST_CASES=2000 cargo nextest run -p zol-tests --release --test invariants

step "Zcash leg end to end (MagicBlock stack, VRF, three gateways)"
(cd spikes/magicblock/zcash-leg && ./run.sh)

step "full tier done"
cd "$ROOT"
