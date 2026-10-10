#!/bin/bash
# Local MagicBlock stack on non-default ports (other work may use 8899/7799).
#   base solana-test-validator: http 28899 / ws 28900 (delegation program etc. preloaded)
#   ephemeral-validator (ER):   http 27799 / ws 27800
#   query-filtering-service:    http 26699 / ws 26700
# Env:
#   DLP=npm|devnet|mainnet  which delegation program binary to load (default npm = the one
#                           shipped in @magicblock-labs/ephemeral-validator 0.14.10).
#                           devnet/mainnet use `solana program dump` copies in ./dumps.
#   TICKS=<n>               --ticks-per-slot for the base validator (default 64 = ~400ms slots).
#   EXTRA_ACCOUNTS="pubkey=file.json ..."  genesis accounts (e.g. a preloaded ledger Config and App).
#   VRF_CLEAN=1             replace the local oracle's VRF queue dumps (full of stale requests in
#                           0.14.10, so no new request fits) with the empty copies in vrf-queues/.
#   ZOL=1                   also preload zol-ledger and zol-test-app as upgradeable programs with
#                           keys/payer.json as upgrade authority, from $ZOL_DIR (default
#                           ledger-e2e/artifacts; use ../../target/deploy for the current build).
set -euo pipefail
cd "$(dirname "$0")"
export MB_STACK_BASE_PORT=28899 MB_STACK_ER_PORT=27799 MB_STACK_PUBLIC_PORT=26699
AUTH=$(solana-keygen pubkey keys/payer.json)
DUMP=node_modules/@magicblock-labs/ephemeral-validator/bin/local-dumps/DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh.so
[ -f "$DUMP.npm-orig" ] || cp "$DUMP" "$DUMP.npm-orig"
case "${DLP:-npm}" in
  npm) cp "$DUMP.npm-orig" "$DUMP" ;;
  devnet) cp dumps/dlp-devnet.so "$DUMP" ;;
  mainnet) cp dumps/dlp-mainnet.so "$DUMP" ;;
  *) echo "bad DLP"; exit 1 ;;
esac
DUMPS=node_modules/@magicblock-labs/ephemeral-validator/bin/local-dumps
for q in Sc9MJUngNbQXSXGP3F67KvKwVnhaYn6kcioxXNVowYT GKE6d7iv8kCBrsxr78W3xVdjGLLLJnxsGiuzrsZCGEvb; do
  [ -f "$DUMPS/$q.json.npm-orig" ] || cp "$DUMPS/$q.json" "$DUMPS/$q.json.npm-orig"
  if [ "${VRF_CLEAN:-0}" = "1" ]; then cp "vrf-queues/$q.json" "$DUMPS/$q.json"; else cp "$DUMPS/$q.json.npm-orig" "$DUMPS/$q.json"; fi
done
echo "DLP=${DLP:-npm} VRF_CLEAN=${VRF_CLEAN:-0} sha256=$(shasum -a 256 "$DUMP" | cut -c1-16) TICKS=${TICKS:-64}"
EXTRA=()
ZD="${ZOL_DIR:-ledger-e2e/artifacts}"
if [ "${ZOL:-0}" = "1" ]; then
  EXTRA+=(--upgradeable-program "$ZD/zol_ledger-keypair.json" "$ZD/zol_ledger.so" "$AUTH")
  EXTRA+=(--upgradeable-program "$ZD/zol_test_app-keypair.json" "$ZD/zol_test_app.so" "$AUTH")
fi
for a in ${EXTRA_ACCOUNTS:-}; do EXTRA+=(--account "${a%%=*}" "${a#*=}"); done
# The spike 0.9 pot program, only if it is built (nothing else needs it).
if [ -f target/deploy/zol_pot_spike.so ] && [ -f keys/program.json ]; then
  EXTRA+=(--upgradeable-program keys/program.json target/deploy/zol_pot_spike.so "$AUTH")
fi
exec node_modules/.bin/mb-stack --reset "${EXTRA[@]}" \
  --ledger ./.local-ledger --faucet-port 29900 --ticks-per-slot "${TICKS:-64}"
