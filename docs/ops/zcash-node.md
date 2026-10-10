# Zcash node for zol-zcashd: Zebra or Zakura

Status: 2026-10-10. Evaluated Zakura 1.6.0 (`zakuracore/zakura:1.6.0`, git `ebda15e`; source
read at upstream `a29a968`). The crates.io `zakura` 1.0.3 named in the request is old: 1.6.0 is
the current release (2026-10-01).

## Verdict

- **RPC: drop-in for zol-zcashd on mainnet and regtest, with one regtest-only gap.**
  - Every method the scanner and daemon use exists and returns the same encoding.
    `generatetoaddress` is missing; zol-zcash now falls back to `generate`.
  - On Zakura 1.6.0 regtest, `regtest_deposit_genesis_refund`, `zcashd_dev_flow` and
    `phase4_distributed_payouts_consolidation_and_double_spend` all pass, on both an archive
    node and a pruned one (`tx_retention = 1001`).
  - The two reorg tests were not run on Zakura. They drive the `zol-zebra-a/b` containers and
    the `spikes/zebra/scripts` partition scripts.
- **Pruning works for us**, provided the scanner starts near the tip and never falls further
  behind than the retention window. It cannot backfill from an old start height. The daemon
  now refuses to start in that case instead of failing every sync.
- **Testnet NU7 (update 2026-10-10): unblocked by a minimal patch.** Testnet activated NU7 at
  height 4,465,026 (ZIP 259, branch `0x77190ad9`). The pinned crates now recognise it through a
  small vendored patch, validated on Zebra 7.0.0-rc.0 regtest. See [nu7.md](nu7.md). Operators
  need an NU7-capable node: Zebra 7.0.0-rc.0 or later, or a Zakura release that follows NU7.
  Zebra 6.4.2 forks off at the activation height. Mainnet NU7 is targeted for November 5; its
  height must be added to the patch once it is set (October 20).

## RPC compatibility

| Method (our use) | Zebra 6.4.2 | Zakura 1.6.0 | Notes |
| --- | --- | --- | --- |
| `getblockchaininfo` | yes | yes | Same shape; `consensus.chaintip`, `valuePools` (incl. `ironwood`). Zakura adds `pruned`, `pruneheight`. |
| `getbestblockhash`, `getblockcount`, `getblockhash` | yes | yes | `getblockhash` works below `pruneheight` (headers are kept). |
| `getblock [height_str, 0]`, `getblock [hash, 0]` | yes | yes | Same raw hex. Below `pruneheight`: error -8 "Block not found". |
| `getrawtransaction [txid, 0/1]` | yes | yes | Wrappers only; neither the daemon nor the scanner calls it. |
| `sendrawtransaction` | yes | yes | Same -25/-26 errors; the double-spend test sees "any transaction with the same effects will be rejected". |
| `z_gettreestate` | yes | yes | Same JSON; empty tree is `"000000"`. Below `pruneheight`: error -8 "the requested block is not in the main chain". |
| `getrawmempool` | yes | yes | |
| `generate` (regtest) | yes | yes | Pays `[mining] miner_address`; a UA with an Orchard receiver gets an Ironwood coinbase note. |
| `generatetoaddress` (regtest) | yes | **no** (-32601) | Fallback: `ZebraClient::mine_to`. |
| `getblocksubsidy` | yes | yes | Same fields. |

Other Zakura behaviour that matters:

- **Reorg window.** The non-finalized window (`MAX_BLOCK_REORG_HEIGHT`) is 1,000 blocks
  (Zebra: 100). This is local policy, not consensus.
- **Mempool limits.** Mempool ancestors are limited to 2. The conventional fee is 400 zat
  per action; ours is 5,000 per action, which is above it.
- **NU6.1 lockbox disbursement on regtest.** If a node activates NU6.1 on regtest, it must
  set a lockbox disbursement (see below).
- **End-of-support halt.** The Zakura 1.6.0 binary halts on mainnet at height 3,536,468
  (about 2026-11-01), so operators must track releases. Testnet is exempt.
- **zcashd-compat mode is not needed.** `--zcashd-compat` runs a zcashd wallet sidecar
  behind Zakura, for exchanges. It needs about 30 GiB on testnet and 275 GiB on mainnet.
  Zakura's own RPC already covers everything we call.

## Regtest with Zakura

Config: [`zakura-regtest.toml`](zakura-regtest.toml). It differs from
`spikes/zebra/regtest.toml` in two ways:

1. **Lockbox disbursement.** It sets
   `lockbox_disbursements = [{ address = "t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v", amount = 0 }]`.
   - Zakura enforces the one-time NU6.1 lockbox output in the NU6.1 activation block.
   - Without it, the first `generate` fails with "missing lockbox disbursements for NU6.1
     activation block".
   - A zero-value P2SH output satisfies the rule. Zakura's `local_genesis.rs` uses the same
     approach.
2. **Miner address.** `miner_address` is the dev wallet UA of dev seed `zol-zakura`.
   Zakura has no `generatetoaddress`, so funding goes through `generate`.

Run the node and the suites:

```sh
docker run -d --name zol-zakura-a -p 127.0.0.1:18252:18232 \
  -e CONFIG_FILE_PATH=/etc/zakura/regtest.toml \
  -v "$PWD/docs/ops/zakura-regtest.toml:/etc/zakura/regtest.toml:ro" zakuracore/zakura:1.6.0
cd crates/zol-zcash
export ZOL_ZEBRA_URL=http://127.0.0.1:18252/ ZOL_DEV_SEED=zol-zakura
cargo test --test regtest -- --ignored --exact regtest_deposit_genesis_refund
cargo test --test zcashd -- --ignored
cargo test --test phase4_regtest -- --ignored --exact phase4_distributed_payouts_consolidation_and_double_spend
```

To use another seed, print its UA and put it in `miner_address`:

```sh
zol-zcashd --print-dev-keys --dev-seed <seed>   # prints "# dev_wallet_ua (regtest ...) = uregtest1..."
```

A running daemon also reports the UA as `dev_wallet_ua` in `/health`. If funding stalls, the
dev-funding error message names this setting.

## Pruning

### Configuration

- **Settings.** Pruning is set with `[state] storage_mode = "pruned"`, or with
  `[state.storage_mode.pruned] tx_retention = N`.
- **Minimum retention.** The minimum `tx_retention` is 10,000 on mainnet and testnet, and
  1,001 on regtest.
- **One-way.** A pruned database will not open in archive mode.

### What is kept and deleted

- **Deleted:** raw transaction bytes, meaning the block bodies.
- **Kept:**
  - headers and hashes, so `getblockhash` and `getblockheader` still work;
  - UTXO, nullifier and anchor state;
  - the txid index.

### Measured on regtest

With `tx_retention = 1001`, at tip 2,300 the node reports `pruneheight = 300`.

- **Retention is counted from the finalized tip.** Pruning works on the finalized tip, which
  is the tip minus about 1,000. So the node actually serves bodies for about
  `tx_retention + 1000` blocks below the tip. On testnet with 10,000 that is about 11,000.
- **Below `pruneheight`:**
  - `getblock` (verbosity 0 and 1) fails.
  - **`z_gettreestate` fails.** Zakura loads the block first, so it errors instead of
    returning `null`.
  - `getblockhash` still works.
- **At or above `pruneheight`:** everything works.

Upstream's historical-treestate work (`docs/design/historical-treestate-serving.md`) lists
pruned nodes as a non-goal. Pruned nodes will not serve old tree states.

### What zol-zcashd needs

- **Fresh state directory.** It needs `z_gettreestate(start_height - 1)` and every block
  from `start_height` on.
  - Default: `start_height` = tip + 1, so the tree state is read at the tip, which is always
    retained.
  - **Do not set `start_height` on a pruned node unless it is at or above
    `getblockchaininfo.pruneheight` + 1.**
- **Steady state.** The scanner reads only new blocks. Reorgs are rewound locally from its
  own checkpoints, and neither the daemon nor the gateway fetches old transactions. The head
  chain and refunds come from scanner state.
- **Restart.** It resumes at `next_height`, which must be at or above `pruneheight`. **The
  daemon must not be down longer than the retention window.**
  - At about 11,000 blocks, that is about 76 hours with 25 s post-NU7 testnet blocks.
  - On mainnet (75 s blocks) it is about 9 days.
  - Past that, the node no longer has the blocks. The vault state directory cannot be
    rebuilt from that node; it needs an archive node.
- **New fail-fast check.** At startup the daemon reads `pruned`/`pruneheight`. If the
  resume height, or `start_height - 1` for a fresh directory, is below `pruneheight`, it
  exits with a clear error. Verified against the pruned regtest node.
- **Start height for a fresh testnet vault.** Leave `start_height` unset, which gives tip + 1.
  - Start zol-zcashd, and let its scanner see the tip, **before** the app's genesis head
    transaction is broadcast.
  - The scanner only needs blocks from the app's genesis onward, so nothing older is needed.
- **Recommended retention: `tx_retention = 30000`** (about 9 days at 25 s, plus the
  1,000-block window). Testnet blocks are small, so the extra 20,000 bodies should cost tens
  to a few hundred MB.
  - Raising the retention after restoring a 10,000-retention snapshot is allowed.
  - The node simply stops pruning until the larger window fills. It cannot recover bodies
    that were already pruned.

## Testnet NU7 (historical: resolved by the patch in [nu7.md](nu7.md))

Testnet activated NU7 (ZIP 259) at height 4,465,026. Zakura 1.6.0 and Zebra main (7.0.0-rc)
follow it. NU7 brings no new transaction format, but every v5 and v6 transaction after
activation commits to branch ID `0x77190ad9`.

| Component | Status |
| --- | --- |
| Zebra 6.4.2 (spikes pin) | No testnet NU7 height. It forks off or stalls at 4,465,026. |
| Zakura 1.6.0 | Follows testnet NU7. |
| `zcash_protocol 0.10.6` (our pin) | `BranchId::try_from(0x77190ad9)` returns an error, and NU7 exists only under `cfg(zcash_unstable = "nu7")` with a placeholder ID. |
| `zcash_primitives 0.30.1` | `Transaction::read` of any post-NU7 v5 or v6 transaction fails. So `scan::sync` fails, and `tx::build` / pczt would sign under the NU6.3 branch ID. |
| Upgrade path | Only pre-releases know ZIP 259: `zcash_protocol 0.11.0-pre.0`, `zcash_primitives 0.31.0-pre.1`, `pczt 0.10.0-pre.1`. **`orchard 0.16` drops the FROST/rerandomization API we use**, so this is a real port, not a version bump. |

NU7 also changes the target block spacing to 25 s, so every depth parameter expressed as time
needs revisiting:

- `expiry_delta = 40` drops from about 50 minutes to about 17 minutes;
- confirmation depths;
- the retention arithmetic above.

Until the port lands, ZOL on Zcash runs on regtest only. Mainnet is unaffected for now: no NU7
height is set, though Zakura 1.6.0's halt height means operators must upgrade anyway.

## Testnet operator setup (once the NU7 port lands)

### Snapshot

Valar publishes testnet pruned snapshots (retention 10,000). They are not listed on
zakura.com/snapshots, which shows mainnet only, but are served next to the mainnet ones:

- Index: `https://zakura.valargroup.dev/testnet-pruned/SHA256SUMS`
- Latest file, checked 2026-10-10: `zakura-testnet-pruned-20261009T212835Z-4471701.tar.zst`
  - **7.75 GB (7.22 GiB) compressed**;
  - height 4,471,701;
  - sha256 `4646be6d8f991306e9575a5718882b8da204d087ec63a7bc427a0efbeea429a8`.
- For comparison, the mainnet pruned snapshot is 11.6 GB, mainnet archive is 255 GiB, and
  there is no testnet archive snapshot.

Stream it straight into the state directory, so the archive file never touches disk and the
peak usage is the extracted size alone. With only about 14 GiB free, keeping the `.tar.zst`
next to its extraction would not fit.

```sh
mkdir -p ~/zakura-testnet/cache
curl -fsSL https://zakura.valargroup.dev/testnet-pruned/zakura-testnet-pruned-20261009T212835Z-4471701.tar.zst \
  | tee >(sha256sum > ~/zakura-testnet/snapshot.sha256) \
  | zstd -dc | tar -x -C ~/zakura-testnet/cache
cat ~/zakura-testnet/snapshot.sha256   # must match SHA256SUMS; if not, delete the state and redo
```

Pick the newest entry in `SHA256SUMS`. Its state version must match the binary: 1.6.0 runs
state v29.1.

### Zakura config (`~/zakura-testnet/zakura.toml`)

```toml
[network]
network = "Testnet"
listen_addr = "0.0.0.0:18233"
cache_dir = "/home/zebra/.cache/zakura"

[state]
cache_dir = "/home/zebra/.cache/zakura"

[state.storage_mode.pruned]
tx_retention = 30000          # >= 10000; see "Pruning"

[rpc]
listen_addr = "0.0.0.0:18232" # published on 127.0.0.1 only (below)
enable_cookie_auth = false    # or keep cookies and set [zebra] user/password in zol-zcashd

[mining]
internal_miner = false

[tracing]
use_color = false
```

```sh
docker run -d --name zakura-testnet --restart unless-stopped \
  -p 18233:18233 -p 127.0.0.1:18232:18232 \
  -e CONFIG_FILE_PATH=/etc/zakura/zakura.toml \
  -v ~/zakura-testnet/zakura.toml:/etc/zakura/zakura.toml:ro \
  -v ~/zakura-testnet/cache:/home/zebra/.cache/zakura \
  zakuracore/zakura:1.6.0
```

Check that `getblockchaininfo` shows `"pruned": true`, a `pruneheight` about 10,000 below
the snapshot height, and `consensus.chaintip = "77190ad9"`.

### zol-zcashd config

```toml
listen = "127.0.0.1:18300"
network = "testnet"
state_dir = "/var/lib/zol-zcashd"
# start_height unset: tip + 1. Start the daemon BEFORE the app's genesis head tx.
poll_ms = 2000
expiry_delta = 40   # revisit for 25 s blocks
apps = ["<app id hex>"]

[zebra]
url = "http://127.0.0.1:18232/"
# user/password only if Zakura cookie auth is on (user "__cookie__", password from the cookie file)

[keys]
# operator share, see zol-zcashd --print-dev-keys for the format
```

Monitor `/health`: `last_error`, and `scanner_tip` against the node tip. Alert well before
the daemon has been down for 3 days.

### Disk estimate (pruned testnet node)

| Item | Size |
| --- | --- |
| `zakuracore/zakura:1.6.0` image | 0.5 GB |
| Extracted pruned testnet state | **about 8–10 GiB (estimate)**, from a 7.2 GiB zstd archive of RocksDB SSTs; not measured, because the 7.75 GB download was not done |
| Extra retention 10,000 → 30,000 bodies | under 0.5 GiB (estimate) |
| Growth (consensus state, indexes) | small; about 3,500 blocks/day at 25 s |
| zol-zcashd state dir | MBs |
| **Total** | **about 10–11 GiB** |

- **Fits the 14 GiB free only with streaming extraction**, and with little headroom for
  RocksDB compaction.
- To make room, remove the Zebra regtest images (`zfnd/zebra:6.4.2`, 0.3 GB) and stopped
  containers.
- Run `du -sh ~/zakura-testnet/cache` after extraction and correct this table.
- Syncing testnet from scratch with pruning avoids the download, but writes and compacts
  the full history as it goes. Peak usage is higher and the sync takes much longer, so the
  snapshot is the better route.

## Code changes made for Zakura (crates/zol-zcash)

- **`ZebraClient::mine_to(n, addr)`.** Calls `generatetoaddress`, and on -32601 falls back to
  `generate`; `zebra::is_method_not_found` detects the error. Dev funding (`/dev/genesis`,
  `/dev/send`) and `tests/regtest.rs` use it.
- **`testing::dev_seed_keys(seed)` and `testing::dev_wallet_ua(params, seed)`.** The dev seed
  derivation moved here from the daemon unchanged, so tests and operators can compute the UA
  for `miner_address`. `/health` reports `dev_wallet_ua`, and `--print-dev-keys` prints it.
- **`BlockchainInfo::prune_height()`**, plus the daemon startup check against it (above).
- **`Scanner::from_zebra` refuses a missing Ironwood `finalState`** at heights where NU6.3 is
  active. Both nodes encode an empty tree as `"000000"`, so a missing field means the node
  lacks that tree, for example in Zakura's fast-synced band. Treating it as empty would
  silently corrupt every anchor.
- **Test settings.** `tests/zcashd.rs` and `tests/phase4_regtest.rs` read `ZOL_ZEBRA_URL` and
  `ZOL_DEV_SEED`. `tests/regtest.rs` reads `ZOL_DEV_SEED`, falling back to a random wallet.
