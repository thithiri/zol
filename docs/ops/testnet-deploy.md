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
