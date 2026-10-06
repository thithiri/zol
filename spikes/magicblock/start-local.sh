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
#   ZOL=1                   also preload zol-ledger and zol-test-app (ledger-e2e/artifacts) as
#                           upgradeable programs with keys/payer.json as upgrade authority.
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
echo "DLP=${DLP:-npm} sha256=$(shasum -a 256 "$DUMP" | cut -c1-16) TICKS=${TICKS:-64}"
EXTRA=()
if [ "${ZOL:-0}" = "1" ]; then
  EXTRA+=(--upgradeable-program ledger-e2e/artifacts/zol_ledger-keypair.json ledger-e2e/artifacts/zol_ledger.so "$AUTH")
  EXTRA+=(--upgradeable-program ledger-e2e/artifacts/zol_test_app-keypair.json ledger-e2e/artifacts/zol_test_app.so "$AUTH")
fi
exec node_modules/.bin/mb-stack --reset "${EXTRA[@]}" \
  --ledger ./.local-ledger --faucet-port 29900 --ticks-per-slot "${TICKS:-64}" \
  --upgradeable-program keys/program.json target/deploy/zol_pot_spike.so "$AUTH"
