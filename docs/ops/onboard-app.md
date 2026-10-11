# Onboarding an app (testnet trial)

On the trial, onboarding is **admin-run**. A developer deploys their program and asks; the
operators add it, fund its vault's genesis, and the ledger admin activates it and sets its cap.
Nobody else can: `activate_app` and `set_cap` need the ledger admin's signature. The
developer's side is in [`dev-docs/deploying.md`](../../dev-docs/deploying.md).

| Step | Where | Tool |
| --- | --- | --- |
| 1. Review the request | — | — |
| 2. Register (if the developer hasn't) | anywhere | `devnet register --app` |
| 3. Add the app to every operator | the host, as root | `zol-add-app` |
| 4. Fund the genesis head | the host | op0's `/admin/genesis` |
| 5. Wait for full depth, check | the host | `/head` on every daemon |
| 6. Activate | the admin key's machine | `devnet activate --app` |
| 7. Set the deposit cap | the admin key's machine | `devnet set-cap --app` |
| 8. Tell the developer | — | — |

`devnet` is the admin tool ([`devnet.rs`](../../spikes/magicblock/ledger-e2e/src/bin/devnet.rs)).
On the Mac it's `spikes/magicblock/target/e2e/release/devnet` (build:
`(cd spikes/magicblock/ledger-e2e && cargo build --release --target-dir ../target/e2e --bin devnet)`).
On the host it's `/usr/local/bin/zol-devnet`. The ledger admin key is `keys/devnet-relayer.json`
on the developer's Mac and isn't on the host, so steps 6 and 7 run on the Mac.

```bash
P=<program id, base58>
D=spikes/magicblock/target/e2e/release/devnet
```

## 1. Review the request

Ask for the program id and the source. Check before onboarding:

- The program is deployed, and who holds its upgrade authority. Whoever holds it can change
  the app's outcomes at any time.
- It passes the checklist in [`dev-docs/security.md`](../../dev-docs/security.md#what-your-app-must-get-right).
  The ledger checks that seats add up, not that the right seat won.
- Its `register` follows the template in
  [`dev-docs/build-an-app.md`](../../dev-docs/build-an-app.md#2-register-and-get-activated)
  (that name, no arguments, the same accounts). Otherwise the developer registers with their
  own client.
- The funding wallet holds the genesis float plus fees (step 4).

## 2. Register

```bash
$D register --app $P --payer keys/devnet-relayer.json
$D status --app $P     # state Pending; note app_id_hex
```

## 3. Add the app to every operator

On the host, as root:

```bash
zol-add-app $P           # dry run: shows the change, writes nothing
zol-add-app $P --apply
```

[`zol-add-app`](../../infra/testnet/zol-add-app.sh) refuses unless the ledger shows the app
`Pending`, and unless every gateway already holds the shared master secret. It adds the app
id to each `zcashd.toml` and `{program, gateway_master_secret}` to each `serve.json`, restarts
the three daemons and gateways, and prints the vault. It also checks that all three daemons
agree on the vault, and prints its 43-byte hex for activation.

A daemon scans a new app from the current tip on, so add it **before** the genesis is
broadcast. A daemon refuses to drop an app later, so check the program id first.

## 4. Fund the genesis head

From op0's daemon, the one with `[funding]` (`/admin/*` answers loopback only). The float caps
credits at early depth; the coin flip's is 5,000,000 zat:

```bash
APP=<app_id_hex>
python3 -c 'import json,sys; print(json.dumps({"app": list(bytes.fromhex(sys.argv[1])), "float": int(sys.argv[2])}))' \
  "$APP" 5000000 > /tmp/genesis.json
curl -s -X POST http://127.0.0.1:18300/admin/genesis -H 'content-type: application/json' -d @/tmp/genesis.json
```

It returns `{head_cmx, txid, head_value}`. Keep `head_cmx` as hex (`bytes(head_cmx).hex()`).
It answers 422 when the wallet is short and 409 when the app already has a genesis.

## 5. Wait for full depth, then check

Guardians cancel an activation whose genesis head is shallower than `depths.full`: 60 blocks
on the trial, about 25 minutes. Every daemon must show the same `chain[0]` and no fault:

```bash
for i in 0 1 2; do curl -s "http://127.0.0.1:1830$i/head?app=$APP&genesis=<head_cmx hex>" | jq -c '.chain[0], .fault'; done
```

## 6. Activate

```bash
$D activate --app $P --admin keys/devnet-relayer.json --vault <vault hex43> --genesis <head_cmx hex>
```

It prints `accepting_after`: credits start an hour later (`activation_delay_secs`). In that
hour any guardian can cancel, which puts the app back to `Pending`; fix the cause and
activate again.

## 7. Set the deposit cap

A new app's cap is 0, so nothing is credited until it's set. The cap bounds
`liability + unpaid_total + net deposit`. The coin flip's is 20,000,000 zat (0.2 TAZ):

```bash
$D set-cap --app $P --admin keys/devnet-relayer.json --cap 20000000
$D status --app $P
```

## 8. Tell the developer

Send the app's state, `activates_at`, its cap, and the vault's unified address. The page API
is the same for every app: `https://<host>/op0/v1/apps/<program>`. The developer runs their
own service and pages ([`dev-docs/deploying.md`](../../dev-docs/deploying.md#8-run-an-app-service)).

## The gateway master secret

Each gateway keys two things per app with secrets derived from one 32-byte master secret
([`gateway/src/config.rs`](../../gateway/src/config.rs), `AppSecrets`):

- **deposit IDs:** a keyed hash of the deposit note's commitment;
- **payout commitments:** a keyed hash of the player's payout address.

Each app's keys are `BLAKE2b-256(key = master, personal = "ZOL_GatewaySec_1", network ‖ tag ‖ app id)`,
so a new app reuses the master secret: no new secret, no ceremony.

- **Generate it once per operator set**, before the first app:

  ```bash
  umask 077; openssl rand -hex 32 > /home/zol/ops/shared/gateway-master-secret.hex
  ```

  On the trial it's that file, `zol:zol 0600`, and it's in the encrypted daily backup.
- **Every operator holds the same value**, in each app's `gateway_master_secret` in
  `serve.json`. Operators on separate hosts exchange it over an authenticated, encrypted
  channel, never by email or chat.
- **Never rotate it.** A new deposit-ID key would give one note two deposit IDs, so a note
  already credited could be credited again under its new ID.
- **Keep it among the operators.** With it, anyone can match deposit IDs on Solana to notes
  on Zcash, and test a guessed payout address against a player's commitment. That breaks the
  link-hiding the design depends on. It can't move funds.
