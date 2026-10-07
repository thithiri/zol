#!/usr/bin/env bash
# Builds the on-chain programs into target/deploy. anchor-lang 1.2 needs rustc >= 1.89,
# so platform-tools v1.52 (rustc 1.89) is required; v1.51 (the solana-cli 3.0 default) is too old.
set -euo pipefail
cd "$(dirname "$0")/.."
TOOLS="${SBF_TOOLS_VERSION:-v1.52}"
for p in zol-ledger zol-test-app; do
  (cd "programs/$p" && CARGO_TARGET_DIR="$PWD/../../target/sbf" cargo build-sbf --tools-version "$TOOLS" --sbf-out-dir ../../target/deploy)
done
cp keys/zol_ledger-keypair.json target/deploy/zol_ledger-keypair.json
cp keys/zol_test_app-keypair.json target/deploy/zol_test_app-keypair.json
ls -la target/deploy
