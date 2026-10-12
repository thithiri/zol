# Gateway page API

Each ZOL operator runs `zol-gateway serve`, which serves a small public HTTP API to app pages
(browsers, scripts). This page documents that API as implemented, plus the deposit memo and
payment link a page builds before calling it.

Sources:

- Routes and handlers: [`gateway/src/server.rs`](../../gateway/src/server.rs)
- Request/response types: `zol_types::page` in [`crates/zol-types/src/lib.rs`](../../crates/zol-types/src/lib.rs)
- Intent validation: `Node::register_intent` in [`gateway/src/node.rs`](../../gateway/src/node.rs)
- Deposit decisions: [`gateway/src/decide.rs`](../../gateway/src/decide.rs)
- Memo format: [`crates/zol-wire/src/memo.rs`](../../crates/zol-wire/src/memo.rs), [`crates/zol-wire/src/text.rs`](../../crates/zol-wire/src/text.rs), TS twin [`crates/zol-wire/ts/wire.ts`](../../crates/zol-wire/ts/wire.ts) (symlinked as `web/src/wire.ts`)
- Page side: [`web/src/gateway.ts`](../../web/src/gateway.ts), [`web/src/deposit.ts`](../../web/src/deposit.ts), [`web/src/memo.ts`](../../web/src/memo.ts)

## Base URLs

Testnet beta (Zcash testnet, Solana devnet), three operators behind one Caddy:

| Operator | Base URL |
|---|---|
| op0 | `https://zol.cash/op0` |
| op1 | `https://zol.cash/op1` |
| op2 | `https://zol.cash/op2` |

Testnet app (zol-coinflip program): `7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo`.

Every operator lists all operators' base URLs in `AppInfo.operators`, so a page can start from
one URL and discover the rest.

## Conventions

- JSON request and response bodies.
- `{app}` in a path is the app's **program ID in base58** (Solana form).
- `{session}` is the 32-byte **session public key in hex** (64 hex chars, case-insensitive). It is
  an Ed25519 key; the same key is the player's Solana signer.
- Byte strings in bodies are hex (memo, address).
- Amounts are zatoshi (1 ZEC = 100,000,000 zat).
- Errors: 4xx/5xx with body `{"error": "<message>"}`.
- CORS is open on every response: `access-control-allow-origin: *`,
  `access-control-allow-headers: content-type`, `access-control-allow-methods: GET, POST, OPTIONS`.
  Any `OPTIONS` request returns `204`.
- Any path not listed below returns `404 {"error":"not found"}` (including a trailing slash, or the
  wrong method on a listed path).
- An app the operator does not serve returns `404 {"error":"unknown app"}`.

## Endpoints

| Method | Path | Returns |
|---|---|---|
| `GET` | `/health` | operator name and halt flag |
| `GET` | `/v1/apps/{app}` | `AppInfo` |
| `GET` | `/v1/apps/{app}/sessions/{session}` | `SessionInfo` |
| `POST` | `/v1/apps/{app}/intents` | `SessionInfo` |
| `GET` | `/v1/apps/{app}/sessions/{session}/deposits` | `DepositStatus[]` |
| `POST` | `/v1/apps/{app}/sessions/{session}/payout` | `PayoutCheckResult` |

`POST /peer/*` is the operator-to-operator API (attestation and spend signing rounds, chain
reports, intent gossip). It requires signed `x-zol-peer` / `x-zol-ts` / `x-zol-nonce` /
`x-zol-sig` headers from a configured peer guardian key and is **not public**; the testnet Caddy
returns `404` for `/opN/peer/*`. It is not documented here.

---

### `GET /health`

Liveness of this operator. Any method on `/health` answers the same.

Response `200`:

| Field | Type | Meaning |
|---|---|---|
| `ok` | bool | always `true` |
| `name` | string | operator name (`op0`, …) |
| `halted` | string \| null | reason this operator's node halted (it then signs nothing), or `null` |

```sh
curl -s https://zol.cash/op0/health
# {"halted":null,"name":"op0","ok":true}
```

---

### `GET /v1/apps/{app}`

Everything a page needs before building a deposit memo.

Response `200` — `AppInfo`:

| Field | Type | Meaning |
|---|---|---|
| `app` | string | program ID, base58 |
| `network` | string | `"mainnet"` \| `"testnet"` \| `"regtest"` (Zcash network) |
| `vault_address` | string | the app vault's external receiver at diversifier index 0, raw 43 bytes hex. The deposit memo signature covers it. **Not** the address to pay (see `SessionInfo.ua`). |
| `denominations` | number[] | permitted deposit amounts in zatoshi (ledger `Config.params.denominations`, zeros dropped). A deposit of any other value is voided. |
| `tip` | number | Zcash tip height this operator's scanner has seen (0 before the first block). Memo expiry is a height. |
| `early_depth` | number | blocks after which a deposit can be credited early (subject to cap/float) |
| `full_depth` | number | blocks for full depth |
| `state` | string | ledger `App` state: `"unregistered"` (no `App` account), `"pending"`, `"activating"`, `"active"`, `"frozen"`. `activating` is reported as `active` once its activation time has passed, even if the ledger has not flipped it yet. |
| `operators` | string[] | every operator's public page API base URL, in coordinator rotation order |

`early_depth` and `full_depth` are converted to plain block counts at the current tip. Internally
the gateway counts depth in 25 s blocks, with each pre-NU7 (75 s) block counting three times
(`DepthRule` in [`gateway/src/policy.rs`](../../gateway/src/policy.rs)), so these values can
change as the tip moves across the NU7 height.

Errors:

| Status | When |
|---|---|
| `400` | `{app}` is not base58 (`"app must be a base58 program id"`) |
| `503` | the ledger (Solana RPC) read failed (`"ledger unavailable, try again: …"`). Checked before the unknown-app check. |
| `404` | the operator does not serve this app |

```sh
curl -s https://zol.cash/op0/v1/apps/7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo
```

```json
{"app":"7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo","network":"testnet",
 "vault_address":"25e45332989eeaf6341def5f6596421d3ad7f384227d1ad6f30c17d86bc949a2e1874d27809b260f92ee05",
 "denominations":[1000000,10000000],"tip":4489919,"early_depth":3,"full_depth":60,"state":"active",
 "operators":["https://zol.cash/op0","https://zol.cash/op1","https://zol.cash/op2"]}
```

---

### `GET /v1/apps/{app}/sessions/{session}`

The session's own deposit address and whether an intent is registered for it on **this**
operator. Any 32-byte value is accepted as `{session}`; it is not checked to be a valid key.

Response `200` — `SessionInfo`:

| Field | Type | Meaning |
|---|---|---|
| `ua` | string | the session's own vault receiver as an Orchard-only unified address. Put this in the payment link. |
| `receiver_index` | string | 88-bit diversifier index of that receiver, decimal string (exceeds JS number precision). Derived as the first 11 bytes, little-endian, of BLAKE2b-256(personal `"ZOL_SessionRecv1"`, `app_id ‖ session_key`) (`session_receiver_index` in `zol-types`). |
| `intent_registered` | bool | this operator holds an intent for this session's receiver |

Errors: `400` (`"session must be 32-byte hex"`, bad app), `404` (unknown app), `502` (the
operator's `zol-zcashd` could not derive the receiver).

```sh
curl -s https://zol.cash/op1/v1/apps/7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo/sessions/00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff
# {"ua":"utest14zzmgm5v…","receiver_index":"178960308578357930303823353","intent_registered":false}
```

---

### `POST /v1/apps/{app}/intents`

Registers a signed deposit memo ("intent") **before** the player pays. A wallet that drops or
mangles the memo can then still be credited: the gateway matches the note by the session
receiver it was paid to and uses the registered memo instead.

Request body — `Intent`:

```json
{ "memo": "<hex>" }
```

`memo` is the hex of either the canonical 512-byte binary memo or the bytes of its `ZOL1:` text
form (see [Deposit memo](#deposit-memo)). Up to 512 bytes; shorter input is zero-padded to 512.

Validation (`Node::register_intent`): the memo must decode as a v2 deposit memo (binary or text),
and its session signature must verify against this operator's network and the app's
`vault_address`. Expiry, denomination and payout address are **not** checked here; they are
checked when the deposit note arrives. The memo is stored in canonical binary form, keyed by
(app, session receiver index): registering again for the same session **replaces** the earlier
intent.

On success the operator passes the intent on to every other operator over the peer API
(`gossip_intent` in `server.rs`): in background threads, retrying after 1, 4, 16 and 64 s, and
giving up silently if a peer stays unreachable or refuses it. A receiving operator stores it but
does not gossip further. Gossip is best-effort; see [Registering with every operator](#registering-with-every-operator).

Response `200` — `SessionInfo` for the memo's session key (`intent_registered` is `true`).

Errors:

| Status | Body `error` |
|---|---|
| `400` | `body must be {"memo": hex}`, `memo hex`, `memo longer than 512 bytes` |
| `400` | `bad intent: not a deposit memo`, `bad intent: bad session signature`, `store: …` |
| `404` | `unknown app` |
| `502` | intent stored (and gossiped) but `zol-zcashd` failed to return the session receiver |

```sh
curl -s -X POST -H 'content-type: application/json' \
  -d "{\"memo\":\"$MEMO_HEX\"}" \
  https://zol.cash/op0/v1/apps/7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo/intents
```

`$MEMO_HEX` is produced by `hex(canonicalMemo(buildSignedMemo(...)))`
(see [typescript-client.md](typescript-client.md#memots)).

---

### `GET /v1/apps/{app}/sessions/{session}/deposits`

Deposit notes this operator has scanned for the session: notes paid to the session's receiver,
or notes anywhere in the vault whose memo names this session key. Rolled-back notes are omitted.
Read errors in the operator's store yield `[]`.

Response `200` — array of `DepositStatus`:

| Field | Type | Meaning |
|---|---|---|
| `cmx` | string | note commitment, hex (internal byte order) |
| `value` | number | zatoshi |
| `height` | number | Zcash block height of the note |
| `confirmations` | number | `tip - height + 1` in plain blocks (0 if above tip); compare with `AppInfo.early_depth`/`full_depth` |
| `state` | string | see below |
| `txid` | string | paying transaction, hex in display order (as wallets show it) |

`state`:

| Value | Meaning |
|---|---|
| `seen` | scanned, no decision yet (waiting for depth or a signing round) |
| `held` | no usable memo/intent, or a payout address that cannot be refunded to (invalid, or a vault's own receiver); waits for the sender to prove the payment and an operator to release it |
| `credited` | a credit attestation was signed; the amount is on the session's ledger balance |
| `voided` | a void attestation was signed; a refund to the memo's payout address is pending |
| `refunded` | voided, and the refund spend has been seen |

Void reasons (from `decide.rs`): bad session signature, expired (`tip >= expiry_height`), amount
not a denomination, app not accepting deposits, over the deposit cap, payout address different
from the one already bound to the session's balance, or released by an operator. The reason is
not returned by this endpoint.

Errors: `400` (bad session/app), `404` (unknown app).

```sh
curl -s https://zol.cash/op0/v1/apps/7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo/sessions/$SESSION_HEX/deposits
# [{"cmx":"…","value":1000000,"height":4489900,"confirmations":4,"state":"credited","txid":"…"}]
```

---

### `POST /v1/apps/{app}/sessions/{session}/payout`

Asks whether an address is the payout address bound to the session's ledger balance. Answers
only yes/no; the bound address is never returned. Use it on a new device before signing a deposit
memo: a deposit naming a different payout address than the bound one is voided.

Request body — `PayoutCheck`:

```json
{ "address": "<43-byte raw receiver, hex>" }
```

Response `200` — `PayoutCheckResult`:

| Field | Type | Meaning |
|---|---|---|
| `bound` | bool | the session has a ledger `Balance` account, so a payout commitment is bound |
| `matches` | bool \| null | whether `address` is the bound address; `null` when `bound` is `false` |

The check recomputes the payout commitment from `address`, the session and the app using this
operator's app secret and compares it with `Balance.payout_commitment` on the ledger.

Errors:

| Status | Body `error` |
|---|---|
| `400` | `session must be 32-byte hex`, `body must be {"address": hex}`, `address must be a 43-byte receiver in hex` |
| `503` | ledger read failed (checked before the unknown-app check) |
| `404` | `unknown app` |

```sh
curl -s -X POST -H 'content-type: application/json' \
  -d '{"address":"<86 hex chars>"}' \
  https://zol.cash/op0/v1/apps/7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo/sessions/$SESSION_HEX/payout
# {"bound":false,"matches":null}
```

## Registering with every operator

Each operator decides each deposit from its own scan and its own store. A note that carries its
memo on chain needs no intent. A note whose memo the wallet dropped can only be credited by
operators that hold the intent for its session receiver, and a credit needs a threshold of
operators.

What the code does:

- The gateway gossips every intent it accepts to all its peers (best-effort, background,
  4 retries; `server.rs` `gossip_intent`). The `AppInfo.operators` doc comment says: "An intent
  registered with one is passed on to the others; a page can also register with each itself."
- `prepareDeposit` in `web/src/deposit.ts` accepts one `Gateway` or an array and posts the intent to
  every one given, reporting which accepted (`registered`) and which failed (`failed`). It requires
  at least one success and checks that all responding operators return the same `ua`.

Recommendation: register with every URL in `AppInfo.operators` and check
`intent_registered` on each (via `GET …/sessions/{session}`) rather than relying on gossip
alone. The two-player page (`main.ts`) registers with every gateway in its comma-separated
`VITE_GATEWAY`; the games pages (`house-page.ts`) register with every URL in
`AppInfo.operators`.

## Deposit memo

Canonical form: 512 bytes, version 2 (`DepositMemo` in `zol-wire`).

| Bytes | Field | Notes |
|---|---|---|
| 0 | `0xFF` | ZIP 302 "arbitrary data" marker |
| 1–3 | `"ZOL"` | |
| 4 | kind/version | `0x02` = deposit v2 (`0x10` head, `0x11` top-up are vault-internal) |
| 5 | flags | bit 0 `FLAG_AUTO_EXIT` (exit automatically after cash-out); other bits must be 0 |
| 6–37 | session public key | Ed25519, 32 bytes |
| 38–80 | payout address | raw 43-byte Orchard-format receiver; refunds and payouts go here |
| 81–84 | expiry height | u32 LE; the deposit is voided if the tip reaches it before a decision |
| 85–116 | pot ID | reserved; the page writes zeros |
| 117–180 | session signature | Ed25519, 64 bytes |
| 181–182 | app data length | u16 LE, ≤ 329 |
| 183–511 | app data | then zero padding (non-zero padding is rejected) |

### What the session key signs

Ed25519 (pure, no prehash; verified with `verify_strict`) over the 508-byte message
(`DepositMemo::signing_message`):

```text
"ZOL/memo-sig/v2\0"        16 bytes, domain tag
network                     1 byte: 0 mainnet, 1 testnet, 2 regtest
vault_address              43 bytes: AppInfo.vault_address
memo[0..117]              117 bytes: header, flags, session key, payout address, expiry, pot ID
memo[181..512]            331 bytes: app data length and app data
```

So the signature binds every memo field except itself, plus the network and the app's vault.

### Text form (`ZOL1:`)

Many wallets drop or mangle a binary `0xFF` memo from a ZIP 321 link, so the link carries a text
memo instead:

```text
"ZOL1:" ‖ base64url_nopad( canonical_memo[1..] with trailing zero bytes stripped )
```

It is ASCII, at most 511 bytes (Zingo's text limit), which limits app data to 197 bytes in this
form (`TEXT_APP_DATA_MAX`). Decoding is strict (only canonical base64url). The scanner and
`POST /intents` accept either form; the signature and all digests are over the canonical binary
memo.

## Payment link

Built by `depositLink` in `wire.ts`, called from `prepareDeposit`:

```text
zcash:{SessionInfo.ua}?amount={ZEC decimal}&memo={base64url_nopad(utf8("ZOL1:…"))}
```

- Address: the **session's** receiver UA from `SessionInfo.ua` (returned by `POST /intents`), not
  `vault_address`. Paying to the session's own diversified receiver is what lets the gateway find
  the intent when the memo is missing.
- `amount`: decimal ZEC with no trailing zeros (`zecString`, e.g. `1000000` zat → `0.01`). Must be
  one of `AppInfo.denominations`.
- `memo`: ZIP 321 carries memo bytes as base64url; the bytes are the UTF-8 text memo, so the
  payload is base64url of an already base64url-bearing string.
- Expiry in the memo: `prepareDeposit` uses `AppInfo.tip + EXPIRY_BLOCKS` (1,000 blocks).

## Typical flow

1. `GET /v1/apps/{app}` → check `state == "active"`, pick an amount in `denominations`, read
   `network`, `vault_address`, `tip`, `operators`.
2. (New device, existing session) `POST …/sessions/{session}/payout` → if `bound && !matches`,
   ask for the original payout address.
3. Build and sign the memo; `POST /v1/apps/{app}/intents` to each operator.
4. Show the `zcash:` link / QR with `SessionInfo.ua`.
5. Poll `GET …/sessions/{session}/deposits` until `credited` (or `voided`/`refunded`), then read the
   session's `Balance` account on Solana.

## Open points

- `confirmations` uses plain block counts while operators decide by the time-weighted
  `DepthRule`; `AppInfo.early_depth`/`full_depth` are converted at the current tip, so comparing
  the two is approximate for notes mined before NU7 activation.
- The `/v1` API has no rate limiting or authentication in the gateway itself.
