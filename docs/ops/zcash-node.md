# Zcash node for zol-zcashd: Zakura

Status: 2026-10-10. ZOL runs on **Zakura 1.6.0**, a fork of Zebra by Sean Bowe and Valar
Group. It is used for dev and test (`infra/zakura`) and is the node operators should run.

| Item | Value |
| --- | --- |
| Image | `zakuracore/zakura:1.6.0`, git `ebda15e` |
| Image digest (multi-arch index) | `sha256:bf53c178e9549217fce7cfaa21efc85c478129ba6dc5307d55640514affeb0ac` |
| NU7 | Follows NU7 (ZIP 259, branch `0x77190ad9`): testnet at 4,465,026, and `NU7 = 1` on regtest |
| Dev/test pair | `infra/zakura/` (below) |

What Zakura offers that matters to us:

- native pruning;
- pruned snapshots;
- the same RPC surface zol-zcashd uses.

Zakura keeps Zebra's 1,000-block rollback window.

## RPC used by zol-zcashd

`zol_zcash::node::NodeClient` calls only the methods below. All of them are in Zakura's
unauthenticated method set, so they work on a restricted listener without cookie auth.

| Method | Use | Notes |
| --- | --- | --- |
| `getblockchaininfo` | tip, `consensus.chaintip`, `upgrades` (regtest activation heights), `pruned` / `pruneheight` | |
| `getbestblockhash`, `getblockcount`, `getblockhash` | scanner | `getblockhash` works below `pruneheight` (headers are kept) |
| `getblock [height_str \| hash, 0]` | scanner (raw block) | error -8 "Block not found" below `pruneheight` |
| `z_gettreestate` | scanner birthday (`start_height - 1`) | An empty tree is `"000000"`. A missing Ironwood `finalState` is treated as an error, not as an empty tree. Error -8 below `pruneheight`. |
| `sendrawtransaction` | genesis, refunds, payouts | -25/-26 errors, e.g. "any transaction with the same effects will be rejected" |
| `getrawmempool`, `getrawtransaction` | helpers | |
| `getblocksubsidy`, `generate` | regtest dev funding and tests | `generate` pays `[mining] miner_address` |

Zakura behaviour to know about:

- **No `generatetoaddress`.** On regtest, `generate` pays `miner_address`. With a UA holding an
  Orchard receiver, that payment is an Ironwood coinbase note. `infra/zakura` sets
  `miner_address` to the dev wallet of dev seed `zol-dev`. That seed is zol-zcashd's default,
  and the tests use it too (`ZOL_DEV_SEED`).
  - `zol-zcashd --print-dev-keys --dev-seed <seed>` prints the UA for another seed.
  - A running dev daemon reports it as `dev_wallet_ua` in `/health`.
- **NU6.1 lockbox disbursement.** Zakura enforces the one-time NU6.1 lockbox output on every
  network. A regtest config therefore needs
  `lockbox_disbursements = [{ address = "t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v", amount = 0 }]`.
  Without it the first `generate` fails with "missing lockbox disbursements for NU6.1
  activation block".
- **Regtest peering.** Regtest peers are joined with the `addnode` RPC (`scripts/peer.sh`).
  The configs set `crawl_new_peer_interval = "5s"` so the pair connects within seconds
  instead of the default 61 s crawl.
- **Mempool:** at most 2 ancestors per transaction. The conventional fee is 400 zat per
  action; ours is 5,000.
- **Release end of support.** Each release halts on mainnet at a set height; for 1.6.0 that
  is 3,536,468, about 2026-11-01. Testnet is exempt. Operators must track releases; the
  mainnet NU7 activation (targeted for November 5) needs a newer release anyway.
- **Not needed:** `--zcashd-compat`, a zcashd wallet sidecar for exchanges.

## Dev and test: `infra/zakura`

A Zakura regtest pair runs in Docker, with ephemeral state.
- **Containers:** `zol-zakura-a` (RPC `127.0.0.1:18232`) and `zol-zakura-b` (RPC
  `127.0.0.1:18242`), on the p2p network `zol-zakura-p2p`.
- **Config:** `ZOL_NODE_CONFIG` picks it.
  - `nu7` (the default) uses `regtest-nu7.toml`: every upgrade through NU7 at height 1.
  - `pre-nu7` uses `regtest-pre-nu7.toml`: everything through NU6.3 (Ironwood) at height 1,
    and no NU7.
- **More pairs:** `ZOL_NODE_PAIR` names a pair (default `zol-zakura`), and `ZOL_NODE_RPC_PORT`
  and `ZOL_NODE_B_RPC_PORT` set its ports, so several can run at once. `scripts/test.sh
  regtest` starts five, `zol-zk-*` and `zol-gw-*` on ports 18250–18340, one per lane, and
  removes them at the end (`KEEP_PAIRS=1` keeps them).

```sh
infra/zakura/scripts/up.sh pair          # or `up.sh` for node a only
infra/zakura/scripts/mine.sh 5 [a|b]
infra/zakura/scripts/height.sh [a|b]
infra/zakura/scripts/peer.sh             # (re)join b to a
infra/zakura/scripts/partition.sh        # cut b off the p2p network
infra/zakura/scripts/reorg.sh B_LEN A_LEN [HOLD_S] [TIMEOUT_S]
infra/zakura/scripts/down.sh
ZOL_NODE_CONFIG=pre-nu7 infra/zakura/scripts/up.sh pair
```

zol-zcashd reads the regtest activation heights from the node, so the same build works
against both configs. Test environment variables:

- `ZOL_NODE_URL` (default `http://127.0.0.1:18232/`);
- `ZOL_NODE_B_URL` (default `http://127.0.0.1:18242/`, for the reorg tests);
- `ZOL_DEV_SEED` (default `zol-dev`).

The reorg tests drive `zol-zakura-a/b` with `docker exec` and the scripts above. Measured on
the pair: a 3-vs-5-block partition reorg converged 16 s after healing.

## Pruning

### Configuration

- **Settings.** Pruning is set with `[state] storage_mode = "pruned"`, or with
  `[state.storage_mode.pruned] tx_retention = N`.
- **Minimum retention.** The minimum `tx_retention` is 10,000 on mainnet and testnet, and
  1,001 on regtest.
- **One-way.** A pruned database will not open in archive mode.

### What is kept and deleted

- **Deleted:** raw transaction bytes (the block bodies) older than the window.
- **Kept:**
  - headers and hashes;
  - UTXO, nullifier and anchor state;
  - the txid index.

### Measured on regtest

With `tx_retention = 1001`, at tip 2,300 the node reports `pruneheight = 300`.

- **Retention is counted from the finalized tip.** Pruning works on the finalized tip, which
  is the tip minus about 1,000. So the node actually serves bodies for about
  `tx_retention + 1000` blocks below the tip.
- **Below `pruneheight`:**
  - `getblock` (verbosity 0 and 1) fails.
  - **`z_gettreestate` fails.** Zakura loads the block first, so it errors instead of
    returning `null`.
  - `getblockhash` and `getblockheader` still work.

Upstream's historical-treestate work lists pruned nodes as a non-goal. A pruned node will not
serve old tree states.

### What zol-zcashd needs

- **Fresh state directory.** It needs `z_gettreestate(start_height - 1)` and every block from
  `start_height` on.
  - Leave `start_height` unset. The default is tip + 1, so the tree state is read at the tip.
  - Start zol-zcashd **before** the app's genesis head transaction is broadcast. The scanner
    needs nothing older than the app's genesis.
- **Steady state.** The scanner reads only new blocks. Reorgs are rewound from its own
  checkpoints, and neither the daemon nor the gateway fetches old transactions.
- **Restart.** It resumes at `next_height`, which must be at or above `pruneheight`.
  **The daemon must not be down longer than the retention window.**
  - With the 10,000 minimum that is about 11,000 blocks: about 76 h at 25 s blocks (testnet
    after NU7, mainnet after its NU7), or about 9 days at 75 s.
  - Past that, the vault state can only be rebuilt from an archive node.
- **Fail-fast check.** At startup the daemon compares the height it needs with `pruneheight`
  and exits with a clear error instead of failing every sync.
- **Recommended: `tx_retention = 30000`.** That covers about 8.7 days at 25 s blocks plus the
  1,000-block window. The extra bodies cost tens to a few hundred MB on testnet.
  - Raising the retention after restoring a 10,000-retention snapshot is allowed. The node
    just stops pruning until the window fills.
- **Keep one archive node per operator set** for recovery. Also mix node implementations in
  production; see the open item in `docs/design-v0.3.1.md`.

## Operator setup (testnet)

### Snapshot

Valar publishes testnet pruned snapshots with retention 10,000. They are not listed on
zakura.com/snapshots (mainnet only) but are served alongside the mainnet ones:

- Index: `https://zakura.valargroup.dev/testnet-pruned/SHA256SUMS`
- Latest file, checked 2026-10-10: `zakura-testnet-pruned-20261009T212835Z-4471701.tar.zst`
  - **7.75 GB (7.22 GiB) compressed**;
  - height 4,471,701, past NU7;
  - sha256 `4646be6d8f991306e9575a5718882b8da204d087ec63a7bc427a0efbeea429a8`.
- For comparison: mainnet pruned is 11.6 GB, and mainnet archive is 255 GiB. There is no
  testnet archive snapshot.

Stream it straight into the state directory, so the archive never lands on disk:

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
cache_dir = "/home/zebra/.cache/zakura"   # the image's home directory is /home/zebra

[state]
cache_dir = "/home/zebra/.cache/zakura"

[state.storage_mode.pruned]
tx_retention = 30000          # >= 10000; see "Pruning"

[rpc]
listen_addr = "0.0.0.0:18232" # published on 127.0.0.1 only (below)
enable_cookie_auth = false    # restricted listener: still serves every method zol-zcashd uses

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
  zakuracore/zakura:1.6.0@sha256:bf53c178e9549217fce7cfaa21efc85c478129ba6dc5307d55640514affeb0ac
```

Check that `getblockchaininfo` shows three things:
- `"pruned": true`;
- a `pruneheight` about 10,000 below the snapshot height;
- `consensus.chaintip = "77190ad9"` (NU7).

### zol-zcashd config

```toml
listen = "127.0.0.1:18300"
network = "testnet"
state_dir = "/var/lib/zol-zcashd"
# start_height unset: tip + 1. Start the daemon BEFORE the app's genesis head tx.
poll_ms = 2000
expiry_delta = 40   # see docs/ops/nu7.md for 25 s-block defaults
apps = ["<app id hex>"]

[node]
url = "http://127.0.0.1:18232/"
# user = "__cookie__"; password = "<cookie file contents>"  only if Zakura cookie auth is on

[keys]
# operator share, see zol-zcashd --print-dev-keys for the format
```

Monitor `/health`: `last_error`, and `scanner_tip` against the node tip. Alert well before
the daemon has been down for the retention window (about 3 days at the minimum retention).

### Disk estimate (pruned testnet node)

| Item | Size |
| --- | --- |
| Zakura image | 0.5 GB |
| Extracted pruned testnet state | **about 8–10 GiB (estimate)** from a 7.2 GiB zstd archive. Not measured: the 7.75 GB download was not done. |
| Extra retention 10,000 → 30,000 bodies | under 0.5 GiB (estimate) |
| Growth (consensus state, indexes) | small; about 3,500 blocks/day at 25 s |
| zol-zcashd state dir | MBs |
| **Total** | **about 10–11 GiB** |

Run `du -sh ~/zakura-testnet/cache` after extraction and correct this table.

Syncing from scratch with pruning avoids the download. But it writes and compacts the full
history as it goes, so peak usage is higher and the sync is far slower.

## Mainnet

The setup is the same with `network = "Mainnet"` and the mainnet pruned snapshot (11.6 GB
compressed, retention 10,000; listed at zakura.com/snapshots).

Before mainnet NU7 two things are needed:
- the mainnet NU7 height in the vendored crate patch (`docs/ops/nu7.md`);
- a Zakura release past 1.6.0's end-of-support height.
