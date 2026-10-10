# Testnet deploy: vault keys and genesis funding

How to give `zol-zcashd` its vault keys and fund an app's genesis head on testnet, without
`--dev`. The node is in [zcash-node.md](zcash-node.md). Running operators on separate hosts is
in [multihost.md](multihost.md).

## Vault keys: `zol-zcashd keygen`

```sh
zol-zcashd keygen --network testnet --operators 3 --threshold 2 --out ~/zol/keys \
  [--app <app id hex>]...
```

- **Generation.** This is the dev key generation (`testing::dev_seed_keys`), but with `OsRng`
  instead of the seed:
  - a trusted-dealer FROST(Pallas, BLAKE2b-512) key set (`frost::dealer`), with the group key
    normalised to even Y (ỹ(`ak_group`) = 0) as Orchard needs;
  - a uniformly random 32-byte master viewing secret `K`.

  Every run gives a new group.
- **Files.** Each file is mode 0600, and `--out` is created 0700. Existing files are never
  overwritten: the command fails before writing anything.
  - `operator-<i>.keys.toml`: a `[keys]` section with operator *i*'s single share, the shared
    `public_key_package` and `K`. It is secret. One share means `zol-zcashd` runs in
    `operator` mode.
  - `group.toml`: public facts only, with no shares and no `K`: `network`, `operators`,
    `threshold`, `ak_group`, `master_view_fingerprint`, `public_key_package`, and the
    verifying share of each FROST identifier.
- **Output.** It prints the facts operators compare: `ak_group`, `master_view_fingerprint`
  (BLAKE2b-256 of `K`, personalisation `ZOL_mview_fp_v1_`), and the verifying shares. For each
  `--app` it also prints that app's vault UA.
  - `zol-zcashd` prints `ak_group` and `master_view_fingerprint` at startup and in
    `GET /health`.
  - The two values fix every app's vault address, so every operator must see the same pair.
- **Using a file.** Point the config at it with `keys_file = "/path/operator-1.keys.toml"`.
  You can also paste its `[keys]` section into the config, but not both.

**A dealer means one machine saw every share.** For this trial all operators run on one host,
so that is acceptable. With independent operators it is not: anyone holding that machine, or
its disk, could spend every vault. A Pallas DKG across hosts is future work; see the gaps in
[docs/phase3-zol-zcash.md](../phase3-zol-zcash.md). Until it exists, generate the keys on the
VPS that runs the operators, then delete any copy you made elsewhere.

## Funding wallet and genesis

The operator wallet is a `zol-wallet` key file:

```sh
zol-wallet new --network testnet --out keys/testnet-wallet.json   # 0600; refuses to overwrite
zol-wallet address --key keys/testnet-wallet.json                 # its UA
```

Send TAZ to that UA. It must arrive as an Ironwood note (any NU6.3-aware wallet or faucet).
Transparent and Sapling funds are not seen. Then look up the funding transaction's height
(or take it from a block explorer):

```sh
curl -s -d '{"jsonrpc":"2.0","id":1,"method":"getrawtransaction","params":["<txid>",1]}' \
  http://127.0.0.1:18232/ | jq .result.height
```

### Config

```toml
listen = "127.0.0.1:18300"          # must be loopback with [funding] (see below)
network = "testnet"
state_dir = "/var/lib/zol-zcashd/op1"
poll_ms = 2000
expiry_delta = 40
apps = ["<app id hex>"]
keys_file = "/home/zol/keys/operator-1.keys.toml"

[node]
url = "http://127.0.0.1:18232/"

[funding]
key_file = "/home/zol/keys/testnet-wallet.json"
start_height = 4474000              # at or below the funding tx's height
```

- **One scanner, one birthday.** On a fresh state dir the scanner starts at
  `min(start_height, [funding] start_height)`, where the top-level `start_height` defaults to
  node tip + 1.
  - The apps are scanned from that height too. That is harmless: their vault keys are new, so
    blocks before the genesis hold no vault notes, and the event log just carries more `Block`
    events.
  - The node must still have `z_gettreestate(birthday - 1)` and every block from the birthday
    on. On a pruned node that means the birthday must be at or above `pruneheight`. The daemon
    checks this at startup.
- **Persistence.**
  - The funding wallet is a scanner account. Its id is derived from the wallet's viewing key,
    and its notes are never written to the event log.
  - Once a state dir tracks a funding wallet, `[funding]` must stay configured with the same
    key, or the daemon refuses to start (as it does for a removed app).
  - Adding `[funding]` to an existing state dir works only if `start_height` is at or above the
    scanner's next block. Otherwise use a fresh `state_dir`, which restarts the event log at
    seq 1, so start it before any gateway reads it.
- **Network.** The wallet file's `network` must match the config's.
- **Loopback only.** With `[funding]`, the daemon refuses to start unless `listen` is a loopback
  address. `/admin/*` also answers only loopback peers. `--allow-remote-admin` lifts both
  checks, so keep the port off the network if you use it.

### Admin endpoints (only with `[funding]`)

```sh
# Did the TAZ arrive? UA, balance, spendable, notes (height, txid, pending), scanner and node tip.
curl -s http://127.0.0.1:18300/admin/funding | jq

# The app's genesis head: float in zatoshi.
curl -s -X POST http://127.0.0.1:18300/admin/genesis \
  -H 'content-type: application/json' \
  -d '{"app":[<32 app id bytes as a JSON array>],"float":10000000}'

# Optional: a test payment (e.g. a deposit to a vault UA, with a text ZOL1: memo).
curl -s -X POST http://127.0.0.1:18300/admin/send -H 'content-type: application/json' \
  -d '{"to":"utest1…","value":200000,"memo":"ZOL1:…"}'
```

`api::GenesisRequest` takes `app` as a 32-byte array. To build the body from hex:

```sh
APP=<app id hex>
python3 -c 'import json,sys; print(json.dumps({"app": list(bytes.fromhex(sys.argv[1])), "float": int(sys.argv[2])}))' \
  "$APP" 10000000 > genesis.json
curl -s -X POST http://127.0.0.1:18300/admin/genesis -H 'content-type: application/json' -d @genesis.json | jq
```

- **`GET /admin/funding`** returns:
  - `ua`;
  - `balance`: all unspent Ironwood notes;
  - `spendable`: `balance` minus the notes used by this daemon's unmined transactions;
  - `notes`: for each, `cmx`, `value`, `height`, `txid` (display order), `scope` and `pending`;
  - `start_height`, `scanner_tip` and `node_tip`.
- **`POST /admin/genesis {app, float}`** returns `GenesisResponse {head_cmx, txid, head_value}`.
  It uses `genesis::build_multi`, the same code as `/dev/genesis`.
  - **Inputs.** It picks free funding notes, largest first and at most 40, until they cover
    `float` plus the ZIP 317 fee. Change goes back to the wallet's internal address.
  - **It never mines.** If funds are short it answers 422 with what is needed, what is free and
    what is pending.
  - **One genesis per app.** It answers 409 when the scanner already holds a genesis head for
    the app, or while a genesis it broadcast is unmined and unexpired.
  - **After the broadcast** the inputs are held as pending until they are mined or expire
    (`expiry_delta`). Pending is in memory only, so do not retry right after a restart; check
    `GET /head` and `/admin/funding` first.
  - Record `head_cmx` for the app's activation (`activate_app`).
- **`POST /admin/send {to, value, memo?}`** pays a UA on this network that has an Orchard
  receiver. `memo` is UTF-8 text of up to 512 bytes. It never mines, and returns `{txid}`.

After the genesis is mined, check it:

```sh
curl -s "http://127.0.0.1:18300/head?app=$APP&genesis=<head_cmx hex>" | jq '.chain[0], .fault'
```

## VPS sequence (one host, three operators)

1. Node synced past the funding transaction ([zcash-node.md](zcash-node.md)).
2. `zol-zcashd keygen --network testnet --operators 3 --threshold 2 --out ~/zol/keys --app $APP`.
   Keep `group.toml`, and note `ak_group` and `master_view_fingerprint`.
3. Three daemons, each with its own `state_dir`, port and `keys_file = operator-<i>.keys.toml`.
   Give exactly one of them (say operator 1) the `[funding]` section.
4. Check that `/health` on each shows the same `ak_group` and `master_view_fingerprint`, and
   that `/vault?app=$APP` shows the same UA.
5. On operator 1, `GET /admin/funding` must show the TAZ. Then `POST /admin/genesis`.
6. Once the genesis is mined, check that `GET /head` on every daemon shows the same
   `chain[0]`, then activate the app with `head_cmx`.

## The first testnet deployment (2026-10-10)

What was actually run for the trial, and what it taught us. One host carries all three
operators, so this shows the system runs; it is not a security setup.

### Host

| | |
| --- | --- |
| Provider | DigitalOcean, SGP1, Basic $18: 2 vCPU, 2 GB RAM, 60 GB SSD, plus 4 GB swap (`vm.swappiness=10`) |
| OS | Ubuntu 24.04, key-only SSH, unattended upgrades, `net.ipv4.tcp_slow_start_after_idle=0` (Zakura asks for it) |
| Firewall (ufw) | 22, 80 and 443 (Caddy), 18233 (Zakura testnet P2P). Everything else is on 127.0.0.1 |
| Public name | `159-223-38-126.sslip.io` (no domain needed; Caddy gets a Let's Encrypt certificate for it) |
| Memory in use | about 1.5 GB with Zakura (about 1 GB) + three `zol-zcashd` + three gateways + Caddy |

Binaries are cross-compiled on the developer's Mac and copied up, so the host never builds
Rust:

```sh
cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.39 -p zol-gateway --bin zol-gateway
(cd crates/zol-zcash && cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.39 --bin zol-zcashd)
```

### Layout (user `zol`, everything 0600/0700)

```text
/home/zol/zakura-testnet/          zakura.toml, cache/ (pruned state, about 9 GB)
/home/zol/ops/shared/              app.hex, attestor/attestor-pubkeys.json, pallas/ (keygen output),
                                   gateway-master-secret.hex, pubkeys.json, testnet-wallet.json
/home/zol/ops/op{0,1,2}/           guardian.json, relayer.json, attestor-share.json,
                                   serve.json, zcashd.toml, zcashd-state/, gateway.db
/etc/systemd/system/               zol-zcashd@.service, zol-gateway@.service (instance = operator)
/etc/caddy/Caddyfile               /op{0,1,2}/* -> 127.0.0.1:870{0,1,2}; /peer/* -> 404; / -> the page
```

- **Ports:** `zol-zcashd` on 127.0.0.1:1830N and gateways on 127.0.0.1:870N. Peers call each other on localhost.
- **Funding:** only op0 has `[funding]`.
- **Keys:** the Solana guardian and relayer keypairs were generated on the host (PyNaCl). The attestor key came from `zol-gateway keygen` and the Pallas shares from `zol-zcashd keygen`; both are dealer-generated on this one host.

**Caddy rule order.** With `respond @peer 404` next to `handle_path` blocks, Caddy sorts
`handle_path` first, so `/op0/peer/...` was proxied. Wrapping everything in `route { ... }`
keeps the written order. Check with `curl -o /dev/null -w '%{http_code}' https://<host>/op0/peer/x`,
which must print 404.

### Solana devnet

```sh
solana program deploy -u devnet -k keys/devnet-relayer.json --program-id keys/zol_ledger-keypair.json target/deploy/zol_ledger.so
solana program deploy -u devnet -k keys/devnet-relayer.json --program-id keys/zol_coinflip-keypair.json target/deploy/zol_coinflip.so
D=spikes/magicblock/target/e2e/release/devnet   # (cd spikes/magicblock/ledger-e2e && cargo build --release --target-dir ../target/e2e --bin devnet)
$D init --admin keys/devnet-relayer.json --attestor <group key> --guardians <g0,g1,g2>
$D register --payer keys/devnet-relayer.json
# after the genesis is at full depth:
$D activate --admin keys/devnet-relayer.json --vault <vault hex43> --genesis <head_cmx>
$D status
```

- **Cost:** about 6.3 SOL for both programs, at rent of 3.67 SOL (ledger) plus 2.56 SOL (coinflip). A deploy briefly needs twice its rent for the upload buffer. Fund each relayer with 0.4 SOL; guardians need none, because the relayer pays for `freeze_app`.
- **Verify the deploy:** `solana program dump` the ledger and compare its sha256 with the release hash in the README.
- **Rate limits:** the public devnet RPC sends 429s when three gateways share one IP. `RpcLedger` retries with backoff, and `poll_ms` is 5000. A dedicated endpoint, such as Helius or QuickNode Solana devnet, is a one-line `solana_rpc` change in each `serve.json`.
- **The page:** built with `VITE_GATEWAY=https://<host>/op0`, `VITE_BASE_RPC=https://api.devnet.solana.com`, `VITE_ER_RPC=https://devnet-as.magicblock.app`, `VITE_VALIDATOR=MAS1Dt9qreoRMQ14YQuhg8UTZMMzDdKhmkZMECCzk57` (from the endpoint's `getIdentity`) and `VITE_VRF_QUEUE=5hBR571xnXppuCPveTrctfTU7tJLSN94nq7kv7FRK5Tc` (`DEFAULT_EPHEMERAL_QUEUE`). Then `rsync dist/` to `/var/www/zol`.
- **Fees:** players' Solana fees come from the session key, or from a fee-payer key pasted into the page settings.

### Zcash testnet

- **Snapshot:** Zakura 1.6.0 rejects the post-NU7 pruned snapshots (see [zcash-node.md](zcash-node.md)). Use the Oct 1 pre-NU7 snapshot; the node crossed NU7 itself without trouble.
  - **Speed:** syncing about 60,000 blocks took about 75 minutes, between 8 and 26 blocks/s.
  - **The tip:** the snapshot's file timestamp is when it was published, not when its last block was mined. Use a block's `time` to estimate where the tip is.
- **TAZ:** many faucets still run on the stale pre-NU7 fork.
  - **fauzec's TAZ never reached our chain.** Its receipt showed block 4,472,783 on Oct 10, a height the NU7 chain had passed on Oct 6.
  - **Valar's faucet works.** At `https://faucet.testnet.valargroup.dev` (`/api/status` reports `chainHeight`), it pays 0.125 TAZ per IP per day, on the NU7 chain.
  - **Check any faucet first:** its height must match your node's.
- **Genesis:** a 0.05 TAZ float, mined at 4,486,000 with a 10,000 zat fee. All three daemons showed the same `chain[0]` and no fault.
  - **Activation:** activate only after 300 confirmations (about 2 h at 24 s/block). Guardians cancel an activation whose genesis is shallower. The ledger then waits `activation_delay_secs` (1 h) before taking deposits.

### The first game (2026-10-10, `web/scripts/testnet-smoke.ts`)

| Step | Time (UTC) | Notes |
| --- | --- | --- |
| App active | 20:54:38 | Page API flipped at `activates_at` (lazy activation reported correctly) |
| Two deposits of 0.01 TAZ from the funding wallet | 20:56:59, 20:57:36 | p0 with its ZOL1 memo; p1 memo dropped, intent registered with op0 only and gossiped to op1 and op2 at once |
| Credited at early depth (30 blocks) | 21:10:18, 21:11:39 | 12.7 min after the first payment; coordinators op2 and op1 |
| Flip on MagicBlock devnet (`devnet-as`, VRF queue `5hBR571x…`) | 21:11:48 | Flip to landed 963 ms; finish to pot back on Solana 2.3 s |
| Cash-out, table closed, exit, settle | 21:11:52–21:11:55 | Winner 1,994,000; settlement 0 pays 1,968,018 after the exit fee and network allowance |
| Payout mined on Zcash | 21:15:13 | Height 4,486,495, coordinator op0; head value 3,011,982; no fault. The payout reached the funding wallet |

- **Books:** credited 3,000,000 = liability 997,000 + fee pool 34,982 + unpaid 1,968,018.
- **Health:** no halts or failed polls, and no restarts of Zakura, the daemons or the gateways. Memory peaked at about 1.6 GB of 2 GB with 91 MB of swap, so the payout proof fits the $18 droplet.
- **A stranded 0.01 TAZ.** The first run of the script crashed before its second deposit. Its single-note funding wallet had the change still unmined, so `/admin/send` correctly returned 422. The script had kept the sessions' keys only in memory, so p0's deposit was credited to a session nobody holds: the 997,000 of liability above. The script now writes session seeds to `keys/smoke-sessions-*.json` before any money moves, and waits for spendable change before each send. **A page must do the same: the session key is the only way to use or exit a balance.** The web page keeps it in the browser, so losing browser storage strands the balance in the same way.
