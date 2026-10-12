# Deploying an app to the testnet

From code to a live app on the testnet beta: Zcash testnet, Solana devnet, MagicBlock's
devnet rollup. Facts come from the beta's runbooks:
[`testnet-deploy.md`](../docs/ops/testnet-deploy.md), [`house.md`](../docs/ops/house.md),
[`multihost.md`](../docs/ops/multihost.md).

| Step | Who | What |
| --- | --- | --- |
| 1 | You | [Build](#1-build) the program |
| 2 | You (you hold the upgrade authority) | [Deploy](#2-deploy-to-solana-devnet) it to Solana devnet |
| 3 | Anyone | [Register](#3-register-with-the-ledger) it: your `register` instruction CPIs `register_app` |
| 4 | Operators | [Add the app](#4-operators-add-the-app-vault-and-genesis) to every `zol-zcashd` and gateway; fund its genesis head |
| 5 | Ledger admin | [Activate](#5-activation) it with the vault address and genesis head, once the head is at full depth |
| 6 | Ledger admin | [Set a deposit cap](#6-deposit-cap) (a new app's cap is 0) |
| 7 | — | Wait the activation delay (1 h); any guardian can cancel in that time |
| 8 | You | [Run your app service](#8-run-an-app-service) and [publish your pages](#9-pages) |

On the testnet beta, onboarding is **admin-run**: deploy your program, then send the operators its
program id and source. They review it against the [security checklist](security.md#what-your-app-must-get-right),
register it if you haven't, and run steps 4 to 6 (their runbook:
[`onboard-app.md`](../docs/ops/onboard-app.md)).

## 1. Build

Requirements: solana-cli 3.0 with platform-tools **v1.52** (anchor-lang 1.2 needs rustc ≥ 1.89;
the v1.51 default is too old), Rust 1.97.1 for the host (`rust-toolchain.toml`).

```bash
./scripts/build-sbf.sh
```

[`../scripts/build-sbf.sh`](../scripts/build-sbf.sh) builds the repo's programs into
`target/deploy/` and copies their keypairs there from `keys/` (gitignored). Add your program to
its loop, or build it the same way:

```bash
(cd programs/<your-app> && CARGO_TARGET_DIR="$PWD/../../target/sbf" cargo build-sbf --tools-version v1.52 --sbf-out-dir ../../target/deploy)
```

The program id in `declare_id!` must be the public key of the keypair you deploy with.

Deploy from `target/deploy/` only. `target/deploy-dev/` holds a ledger with short test windows
that must never be deployed ([testing](testing.md#build-the-programs-first)).

IDL (the README's command for the ledger; use your crate's name for yours):

```bash
~/.avm/bin/anchor-1.2.1 idl build -p zol-ledger -o idl/zol_ledger.json
```

## 2. Deploy to Solana devnet

```bash
solana program deploy -u devnet -k <payer.json> --program-id keys/<your_app>-keypair.json target/deploy/<your_app>.so
```

- **Cost.** About rent for the program's size, and briefly twice that for the upload buffer. On the testnet beta: 3.67 SOL rent for the ledger, 2.56 SOL for `zol-coinflip`.
- **Upgrade authority.** `-k` is the default upgrade authority. Pass `--upgrade-authority <keypair>` to use another. Keep the upgrade authority keypair out of the repo.

**Upgrades** use the same command. The program id stays; the bytes change.

```bash
solana program deploy -u devnet -k <payer.json> --upgrade-authority <authority.json> \
  --program-id <PROGRAM_ID or keypair> target/deploy/<your_app>.so
```

- Live accounts keep their bytes. An upgrade that changes an account layout breaks every open table of the old layout, so keep layouts compatible or wait until none are open.
- **Size.** `solana program show -u devnet <PROGRAM_ID>` prints the data length. solana-cli 3.0 `program deploy` extends the program account automatically (unless `--no-auto-extend`). If an upgrade fails for size, extend first:

  ```bash
  solana program extend -u devnet -k <authority.json> <PROGRAM_ID> <ADDITIONAL_BYTES>
  ```

- A failed deploy leaves its SOL in a buffer: `solana program show -u devnet --buffers` lists them, and `solana program close` reclaims them.

**Verify the deploy.** The deployed program data is your `.so` followed by zero padding.
Compare the first N bytes, N being the `.so`'s length:

```bash
solana program dump -u devnet <PROGRAM_ID> deployed.so
N=$(wc -c < target/deploy/<your_app>.so)
head -c "$N" deployed.so | shasum -a 256
shasum -a 256 target/deploy/<your_app>.so            # must match
tail -c +"$((N + 1))" deployed.so | tr -d '\0' | wc -c   # must print 0
```

`solana-verify get-program-hash` gives the same hash without trailing padding. For builds that
reproduce across machines, use `solana-verify build` (Docker).

## 3. Register with the ledger

Your program needs a `register` instruction that CPIs the ledger's `register_app`, signed with
your `["zol-app"]` PDA ([build an app](build-an-app.md#2-register-and-get-activated)). Anyone
can send it and pay; the testnet `registration_bond` is 0. The app starts `Pending`, with a
deposit cap of 0.

If your `register` follows the template (that name, no arguments, the same accounts), the
admin tool sends it for any app: `devnet register --app <program id> --payer <keypair>`.
Otherwise send it with your own client.

## 4. Operators: add the app, vault and genesis

The vault is per app, derived from the operators' shared keys, so no new key ceremony is
needed. On the testnet beta, `zol-add-app <program id> --apply` does items 1 to 3 on the host
([`onboard-app.md`](../docs/ops/onboard-app.md)). The operators:

1. **Add the app id to every `zol-zcashd`** (`apps = ["<app id hex>"]` in each `zcashd.toml`) and restart. A new app is scanned from the current tip on, so do this **before** the genesis is broadcast. Removing an app later is refused.
2. **Add it to every gateway** (`serve.json`): `"apps": [{ "program": "<base58>", "gateway_master_secret": "<32-byte hex>" }]`, with the same secret on every operator: the operators' one master secret, generated once
   and reused for every app ([`onboard-app.md`](../docs/ops/onboard-app.md#the-gateway-master-secret)). Restart. Guardians only check, and gateways only credit, apps in their config.
3. **Read the vault address.** Every daemon must return the same:

   ```bash
   APP=<app id hex>
   curl -s "http://127.0.0.1:18300/vault?app=$APP" | jq '.ua'
   curl -s "http://127.0.0.1:18300/vault?app=$APP" | python3 -c 'import json,sys; print(bytes(json.load(sys.stdin)["address"]).hex())'   # 43-byte hex for activate
   ```

4. **Fund the genesis head** from the funding wallet (op0, the daemon with `[funding]`; `/admin/*` answers loopback only):

   ```bash
   python3 -c 'import json,sys; print(json.dumps({"app": list(bytes.fromhex(sys.argv[1])), "float": int(sys.argv[2])}))' \
     "$APP" 5000000 > genesis.json
   curl -s -X POST http://127.0.0.1:18300/admin/genesis -H 'content-type: application/json' -d @genesis.json | jq
   ```

   It returns `{head_cmx, txid, head_value}`. Record `head_cmx` as hex (`bytes(head_cmx).hex()`) for activation. The float caps credits at early depth: on the testnet beta, 5,000,000 zat let five 0.01 TAZ deposits through before later ones waited for full depth. Errors: 422 when the wallet is short (it never mines), 409 when the app already has a genesis or one is pending.

5. **Check it.** After it is mined, every daemon must show the same `chain[0]` and no fault:

   ```bash
   curl -s "http://127.0.0.1:18300/head?app=$APP&genesis=<head_cmx hex>" | jq '.chain[0], .fault'
   ```

To get the app id hex from its base58 program id (from `zcash-leg/run.sh`):

```bash
python3 -c "
a='123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'; n=0
for c in '<PROGRAM_ID>': n=n*58+a.index(c)
print(n.to_bytes(32,'big').hex())"
```

Setting up `zol-zcashd`, the funding wallet and keys from scratch is in
[`testnet-deploy.md`](../docs/ops/testnet-deploy.md).

## 5. Activation

`activate_app(vault_address: [u8; 43], genesis_head: [u8; 32])`, signed by the ledger admin.
The app goes `Pending` → `Activating`, with `activates_at = now + activation_delay_secs` (3,600 s
on the testnet). The vault address never changes once active.

**Wait for full depth first.** During the delay every guardian checks that the genesis head is
in its own scan, in the vault's internal scope, with a genesis memo for this app, at least
`depths.full` blocks deep (60 on the testnet beta since 2026-10-11; 300 before, and on mainnet). If not,
it cancels: one guardian's `freeze_app` puts an `Activating` app back to `Pending` and clears
the vault and head. The admin can then activate again.

```bash
D=spikes/magicblock/target/e2e/release/devnet
$D activate --app <program id> --admin <admin.json> --vault <vault hex43> --genesis <head_cmx hex32>
$D status --app <program id>
```

`activate` prints `chain_now` and `accepting_after`. Credits are refused (`AppNotActive`) until
then; the gateways' page API reports the app as accepting once `activates_at` has passed.

## 6. Deposit cap

A new app's `deposit_cap` is 0. A credit is refused (`OverCap`) unless
`liability + unpaid_total + net amount ≤ deposit_cap`. The ledger admin sets it, in any state:

```bash
$D set-cap --app <program id> --admin <admin.json> --cap <zat>
```

## The devnet admin tool

[`../spikes/magicblock/ledger-e2e/src/bin/devnet.rs`](../spikes/magicblock/ledger-e2e/src/bin/devnet.rs).
JSON lines out; exits 1 on a failed transaction.

```bash
(cd spikes/magicblock/ledger-e2e && cargo build --release --target-dir ../target/e2e --bin devnet)
D=spikes/magicblock/target/e2e/release/devnet
ZOL_RPC=https://api.devnet.solana.com $D status --app <program id>     # ZOL_RPC is the default
```

| Command | Signer | What |
| --- | --- | --- |
| `init --admin <kp> --attestor <b58> --guardians <b58,b58,b58>` | Ledger upgrade authority | `initialize`: testnet network, guardian threshold 2, the params in the file |
| `status [--app <b58>]` | — | Config (admin, attestor, guardians, paused, denominations, `min_exit`, `max_seats`); with `--app`, the app (program, app id hex, state, `activates_at`, cap, vault, genesis, books) |
| `register --app <b58> --payer <kp>` | Anyone | The app's `register`, for apps that follow the template |
| `activate --app <b58> --admin <kp> --vault <hex43> --genesis <hex32>` | Admin | `activate_app` |
| `set-cap --app <b58> --admin <kp> --cap <zat>` | Admin | `set_cap` |
| `set-min-exit --admin <kp> --zat <n>` | Admin | `set_config` with the live params, `min_exit` changed |
| `set-max-seats --admin <kp> --seats <n>` | Admin | `set_config` with the live params, `max_seats` changed |
| `settle --app <b58> --payer <kp> <session b58>…` | Anyone | `settle` balances that requested exit |
| `settlement --app <b58> <seq>` | — | A settlement's state and the app's books |
| `unfreeze --app <b58> --payer <kp> --guardians <g1.json,g2.json>` | A threshold of guardians | `unfreeze_app` |

Every app command takes `--app`, so a cap or an activation never lands on the wrong app.
`init`, `set-min-exit` and `set-max-seats` are ledger-wide; `init`'s params are the beta's
live values (`min_exit` 50,000, `max_seats` 16).

## Ledger upgrades and the code pin

Only the operators upgrade the ledger. App upgrades need none of this.

Each gateway pins the ledger's code: `serve.json` carries
`"ledger_code": {"sha256": "<hex>", "len": <bytes>}` for the release build. Every minute the
gateway compares the deployed ProgramData (after its 45-byte header, then only zero padding).
On a mismatch it signs nothing: no attestation, no payout, no answer to a peer's signing
request. Without `ledger_code` it warns and signs for whatever is deployed, which is for local
development only.

So a ledger upgrade means:

1. Build with `scripts/build-sbf.sh` and record the hash and length:

   ```bash
   shasum -a 256 target/deploy/zol_ledger.so
   wc -c < target/deploy/zol_ledger.so
   ```

2. Update the hash in the [README](../README.md#program-hash).
3. Deploy (as in [step 2](#2-deploy-to-solana-devnet)) and verify the dump.
4. Update `ledger_code` in **every** operator's `serve.json` and restart each gateway (`zol-gateway@<op>` under systemd on the testnet beta). Gateways whose pin and deployment disagree stop signing until they match.

## 8. Run an app service

An app holds no ZEC and no player keys, but it may need a service that pays Solana fees, builds
transactions for players to sign, and closes what players abandon. The beta's is the
**house**, `zol-house` ([`../web/house/server.ts`](../web/house/server.ts),
[`../web/house/house.ts`](../web/house/house.ts)): the counterparty in the single-player games.
It is written for `zol-coinflip`; use it as the pattern.

Build a single Bun binary and copy it to the host:

```bash
cd web && bun build --compile --target=bun-linux-x64 house/server.ts --outfile dist-house/zol-house
```

Run it:

```bash
zol-house --config /home/zol/ops/house/house.json
```

It drives open games every 1.5 s and keeps its state in SQLite (games, rate-limit counters).

`house.json` (`FileConfig` in `server.ts`, `HouseConfig` in `house.ts`):

| Field | What | Beta value |
| --- | --- | --- |
| `listen` | `host:port` | `127.0.0.1:8790` |
| `db` | SQLite file | `house.db` |
| `sessionSeedFile` | The house's player key, 64 hex chars. Its ledger balance is the bankroll | |
| `feePayerFile` | Solana keypair JSON; pays every game transaction, table rent and buffers | ~1.5 SOL on devnet |
| `baseRpc`, `erRpc` | Solana and rollup RPC | |
| `ledger`, `app` | Program ids | |
| `validator` | Rollup validator identity tables are delegated to | `MAS1Dt9q…` |
| `gateway` | This operator's page API (local) | |
| `zcashd` | This operator's `zol-zcashd`: `/head` for payout tracking, `/admin/send` for fund-me | op0's, on loopback |
| `zcashNode` | Zakura RPC, for transaction confirmations | |
| `genesisHead` | The app's genesis head, hex | |
| `maxOpenGames` | Open sessions at once | 2 |
| `playsPerIpPerHour` | | 10 |
| `fundZat`, `fundPerDay`, `fundPerIpPerDay` | Fund-me deposits from the funding wallet | 1,000,000, 3, 1 |
| `stake` (optional) | Zat per flip; must be a stake the app accepts | Default: the smallest |
| `maxRounds` (optional) | Stakes each seat buys into a session | 10 |
| `idleSecs` (optional) | The house finishes a session idle this long | 180 |
| `sessionSecs` (optional) | Ledger session deadline asked at start; finished 5 minutes before | 1800 |
| `flipAfterSecs` (optional) | Unused since sessions | |

Because fund-me and payout tracking use `/admin/*` and `/head` on `zol-zcashd`, the house runs
on the same host as op0's daemon.

Operations on the testnet beta ([`house.md`](../docs/ops/house.md)):

- **systemd unit** `zol-house`. **Caddy** proxies `/house/*` to `127.0.0.1:8790`. The service matches paths that start with `/house/`, so keep the prefix (Caddy `handle`, not `handle_path`). The beta's unit and Caddyfile are in [`../infra/testnet/`](../infra/testnet/README.md). Wrap Caddy rules in `route { … }` to keep their written order ([`testnet-deploy.md`](../docs/ops/testnet-deploy.md#layout-user-zol-everything-06000700)).
- **Bankroll**: ordinary deposits to the house session without auto-exit, `cd web && npx vite-node house/bankroll.ts -- <count>` with an SSH tunnel to op0's `zol-zcashd`.
- **Escrow**: return actions are paid from the delegation program escrow of the app's rent vault; the house tops it up from its fee payer.
- **Monitoring**: `zol-status` ([`../infra/testnet/zol-status.sh`](../infra/testnet/zol-status.sh)) reads `127.0.0.1:8790/house/info` and flags a bankroll below one stake or a fee payer below 0.3 SOL.

## 9. Pages

The pages are static: Vite builds them with the deployment baked in through `VITE_*`
variables.

| Variable | What | Default |
| --- | --- | --- |
| `VITE_GATEWAY` | Page API base URL(s). The two-player page takes a comma-separated list | `http://127.0.0.1:8787` |
| `VITE_BASE_RPC` | Solana RPC | `http://127.0.0.1:28899` |
| `VITE_ER_RPC` | Rollup RPC | `http://127.0.0.1:27799` |
| `VITE_LEDGER`, `VITE_APP` | Program ids | The beta's ledger and `zol-coinflip` |
| `VITE_VRF_QUEUE` | VRF queue | The local test queue `Sc9MJU…` |
| `VITE_VALIDATOR` | Rollup validator identity (from the endpoint's `getIdentity`) | Empty: ask the rollup |
| `VITE_HOUSE` | The house service | `/house` |

Build for the testnet beta:

```bash
cd web
python3 scripts/pages.py          # only after editing coinflip.html: regenerates play.html and updown.html
VITE_GATEWAY=https://<host>/op0 VITE_BASE_RPC=https://<host>/solana VITE_ER_RPC=https://devnet-as.magicblock.app \
  VITE_VALIDATOR=MAS1Dt9qreoRMQ14YQuhg8UTZMMzDdKhmkZMECCzk57 \
  VITE_VRF_QUEUE=5hBR571xnXppuCPveTrctfTU7tJLSN94nq7kv7FRK5Tc npm run build
rsync -a dist/ <user>@<host>:/var/www/zol/
```

- `npm run build` is `tsc --noEmit && node scripts/docs.mjs && vite build` (the docs pages come from `dev-docs/`). The pages are the inputs listed in [`../web/vite.config.ts`](../web/vite.config.ts); add yours there.
- `VITE_BASE_RPC=https://<host>/solana` is Caddy proxying to a dedicated devnet RPC, because browsers on the public devnet RPC get rate-limited. The proxy adds the API key server-side.
- `VITE_VRF_QUEUE` on devnet is `DEFAULT_EPHEMERAL_QUEUE` (`5hBR571x…`). The default `Sc9MJU…` is the local stack's.

## Checklist

- [ ] Built with `scripts/build-sbf.sh` (tools v1.52), deployed from `target/deploy/`, dump verified.
- [ ] Upgrade authority keypair stored safely, outside the repo.
- [ ] `register` sent; `status` shows the app `Pending`.
- [ ] Operators added the app to every `zol-zcashd` and gateway **before** the genesis.
- [ ] Every daemon shows the same vault and the same genesis `chain[0]`, no fault.
- [ ] Activated only after the genesis reached `depths.full`.
- [ ] Deposit cap set.
- [ ] After the delay: `status` shows the app accepting, and a small deposit is credited.
- [ ] Your service's fee payer is funded and monitored; your return-action escrow is funded.
