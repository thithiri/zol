# TypeScript client modules

The browser pages in `web/` are built from a handful of DOM-free modules in
[`web/src/`](../../web/src/). They are the reference client for ZOL: session keys, deposits
through the [gateway page API](gateway-api.md), and Solana / MagicBlock rollup transactions for
the ledger and the zol-coinflip program.

| Module | Purpose |
|---|---|
| [`session.ts`](#sessionts) | Ed25519 session key: create, store, sign, back up, restore |
| [`passkey.ts`](#passkeyts) | derive the session seed from a passkey (WebAuthn PRF) |
| [`ua.ts`](#uats) | parse a payout address (unified address or raw receiver) |
| [`gateway.ts`](#gatewayts) | typed client for the gateway page API |
| [`memo.ts`](#memots) | build and sign the v2 deposit memo |
| [`wire.ts`](#wirets) | byte-exact memo encoding, signing message, text form, ZIP 321 link |
| [`deposit.ts`](#depositts) | `prepareDeposit`: memo → intents → payment link |
| [`chain.ts`](#chaints) | instruction builders, PDAs, account decoders, `sendTx` / `sendSigned` |


## Using the modules

They are **not a published npm package**. They live in the `zol-coinflip-web` app
([`web/package.json`](../../web/package.json), `"private": true`). Two options:

1. **Import from the repo.** Scripts in the repo already do this, e.g.
   [`web/scripts/testnet-smoke.ts`](../../web/scripts/testnet-smoke.ts) imports
   `../src/gateway`, `../src/deposit`, `../src/session`, `../src/chain` and runs with
   `cd web && npx vite-node scripts/testnet-smoke.ts`.
2. **Copy the files** into your project. Copy with symlinks dereferenced: `web/src/wire.ts` is a
   symlink to [`crates/zol-wire/ts/wire.ts`](../../crates/zol-wire/ts/wire.ts)
   (`cp -L web/src/{chain,session,passkey,deposit,gateway,memo,ua,wire}.ts your/src/`).
   `wire.ts` is checked against [`crates/zol-wire/vectors/v0.3.json`](../../crates/zol-wire/vectors/v0.3.json).

Requirements:

- Dependencies (versions from `web/package.json`): `@noble/curves@^1.6`, `@noble/hashes@^1.5`
  (v1 import paths such as `@noble/hashes/sha256`), `@scure/base@^1.1`, `@solana/web3.js@^1.95`
  (v1 API), and `buffer@^6` in browsers.
- `chain.ts` builds instructions with `Buffer.from`. In a browser, install a global first, as the
  pages do:
  ```ts
  import { Buffer } from "buffer";
  (globalThis as any).Buffer ??= Buffer;
  ```
- ESM, TypeScript 5, `moduleResolution: "Bundler"` ([`web/tsconfig.json`](../../web/tsconfig.json)).
  With Vite, allow the symlinked file: `server: { fs: { allow: [".."] } }`
  ([`web/vite.config.ts`](../../web/vite.config.ts)).
- `passkey.ts` needs a browser (WebAuthn, `localStorage`, `location`). Everything else also runs
  in Node ≥ 20 / Bun (`creditDigest` uses `globalThis.crypto.subtle`).

Tests: [`web/test/`](../../web/test/) (`npm test` in `web/`).

### Testnet values

From [`docs/ops/testnet-deploy.md`](../../docs/ops/testnet-deploy.md) and
[`web/scripts/testnet-smoke.ts`](../../web/scripts/testnet-smoke.ts) (testnet beta; may change):

| Setting | Value |
|---|---|
| Gateways | `https://159-223-38-126.sslip.io/op0`, `/op1`, `/op2` |
| Ledger program | `3qh6GRqjmVSeVctB6R7oYYsdG68oFyRDwnXGvKamib3Y` |
| App (zol-coinflip) program | `7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo` |
| Solana (base) RPC | Solana devnet; the hosted page uses the proxy `https://159-223-38-126.sslip.io/solana` |
| Rollup (ER) RPC | `https://devnet-as.magicblock.app` |
| Rollup validator | `MAS1Dt9qreoRMQ14YQuhg8UTZMMzDdKhmkZMECCzk57` (the ER endpoint's `getIdentity`) |
| VRF queue | `5hBR571xnXppuCPveTrctfTU7tJLSN94nq7kv7FRK5Tc` |

The pages read these from `VITE_GATEWAY` (comma-separated), `VITE_BASE_RPC`, `VITE_ER_RPC`,
`VITE_LEDGER`, `VITE_APP`, `VITE_VALIDATOR`, `VITE_VRF_QUEUE`; their built-in defaults are local
devnet values.

---

## `session.ts`

Source: [`web/src/session.ts`](../../web/src/session.ts)

A session key is an Ed25519 key, one per app program. It signs the deposit memo and every
Solana and rollup action of the player; the ledger knows the player only by it. Its public key is
the `{session}` of the gateway API and the player's Solana signer.

| Export | Signature | Description |
|---|---|---|
| `KV` | `interface { getItem(k): string \| null; setItem(k, v): void }` | storage; `localStorage` satisfies it |
| `Session` | `interface { seed: Uint8Array; publicKey: Uint8Array; keypair: Keypair }` | 32-byte secret seed, 32-byte public key, same key as a web3.js `Keypair` |
| `hex` | `(b: Uint8Array) => string` | lowercase hex |
| `unhex` | `(s: string) => Uint8Array` | hex to bytes (no validation) |
| `sessionFromSeed` | `(seed: Uint8Array) => Session` | session from a 32-byte seed |
| `loadOrCreateSession` | `(app: string, kv: KV) => Session` | the app's stored session, or a new random one (stored) |
| `readSession` | `(app: string, kv: KV) => Session \| null` | the stored session, without creating one |
| `storeSession` | `(app, seed, kv, how: "passkey" \| "backup") => Session` | store a seed as the app's session; `"passkey"` also marks it backed up |
| `sessionKind` | `(app, kv) => "passkey" \| "backup"` | how the stored session is kept |
| `sign` | `(session: Session, msg: Uint8Array) => Uint8Array` | Ed25519 signature (64 bytes) |
| `exportSession` | `(app: string, s: Session) => string` | backup string |
| `parseSessionBackup` | `(app: string, text: string) => Session` | parse and check a backup string; throws a player-readable reason |
| `restoreSession` | `(app, text, kv) => Session` | parse, store as the app's session, mark backed up |
| `isBackedUp` | `(app, s, kv) => boolean` | whether this session was marked backed up |
| `markBackedUp` | `(app, s, kv) => void` | mark it backed up |

Storage keys (`app` = program ID base58):

| Key | Value |
|---|---|
| `zol:session:<app>` | seed, 64 hex chars |
| `zol:session-kind:<app>` | `passkey` or absent (= `backup`) |
| `zol:backedup:<app>:<first 16 hex of seed>` | `"1"` |

Backup string format:

```text
zolsess1:<app program id>:<seed hex, 64 chars>:<checksum>
checksum = hex(sha256("zolsess1:<app>:<seed hex>")[0..4])
```

`parseSessionBackup` rejects a wrong prefix or part count, a checksum mismatch, a backup for a
different app, and a malformed seed.

```ts
import { loadOrCreateSession, exportSession, hex, type KV } from "./session";

const APP = "7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo";
const s = loadOrCreateSession(APP, localStorage);       // browser

// Node / Bun: any KV works.
const m = new Map<string, string>();
const kv: KV = { getItem: (k) => m.get(k) ?? null, setItem: (k, v) => void m.set(k, v) };
const s2 = loadOrCreateSession(APP, kv);

console.log(hex(s.publicKey));          // the {session} for the gateway API
const backup = exportSession(APP, s);   // show to the player once
```

---

## `passkey.ts`

Source: [`web/src/passkey.ts`](../../web/src/passkey.ts)

Passkeys sign with P-256, but the ledger and rollup need Ed25519 from the session key. So the
passkey derives the seed through the WebAuthn PRF extension:

```text
salt = sha256("zol/session/v1/" ‖ app)                       // PRF input, fixed per app
seed = sha256("ZOL passkey session v1" ‖ app ‖ prf_output)   // session seed
```

The same passkey gives the same session on every device it syncs to. Without PRF support the
pages fall back to a random session with a backup string.

| Export | Signature | Description |
|---|---|---|
| `prfSalt` | `(app: string) => Uint8Array` | the PRF `eval.first` input |
| `seedFromPrf` | `(app: string, prf: Uint8Array) => Uint8Array` | session seed from the PRF output |
| `passkeysAvailable` | `() => boolean` | WebAuthn present in this browser |
| `prfSupported` | `() => Promise<boolean \| null>` | from `PublicKeyCredential.getClientCapabilities()`; `null` when the browser cannot say |
| `NO_PRF_HELP` | `string` | player-facing advice when PRF is unavailable |
| `seedFromPasskey` | `(app: string, credentialId?: string) => Promise<Uint8Array>` | `navigator.credentials.get` with PRF (user verification required; any passkey for this site, or the given base64 credential ID); throws `NO_PRF_HELP` without PRF |
| `createPasskeySeed` | `(app: string) => Promise<Uint8Array>` | creates a resident passkey (`rp.id = location.hostname`, ES256/RS256) with PRF, then derives the seed (a second prompt if PRF output is not returned at creation) |

Both functions store the credential ID (base64) in `localStorage` under `zol:passkey:<app>`.
The RP name is hard-coded to `"ZOL coin flip"`. A passkey is bound to the page's hostname, so the
derived session depends on serving the page from the same host.

```ts
import { createPasskeySeed, seedFromPasskey, prfSupported } from "./passkey";
import { storeSession } from "./session";

if ((await prfSupported()) !== false) {
  const seed = await createPasskeySeed(APP);          // new player
  // or: const seed = await seedFromPasskey(APP);     // returning player, new device
  const s = storeSession(APP, seed, localStorage, "passkey");
}
```

---

## `ua.ts`

Source: [`web/src/ua.ts`](../../web/src/ua.ts)

Payout address parsing: a ZIP 316 unified address (bech32m + F4Jumble + TLV items), or a raw
43-byte Orchard-format receiver as 86 hex chars. The memo carries the Orchard receiver
(typecode `0x03`); Ironwood uses the same receivers.

| Export | Signature | Description |
|---|---|---|
| `ZNetwork` | `"mainnet" \| "testnet" \| "regtest"` | |
| `UA_HRP` | `Record<ZNetwork, string>` | `u`, `utest`, `uregtest` |
| `f4jumble` / `f4jumbleInv` | `(m: Uint8Array) => Uint8Array` | ZIP 316 F4Jumble and inverse |
| `UaItem` | `interface { typecode: number; value: Uint8Array }` | |
| `decodeUa` | `(ua: string) => { network: ZNetwork; items: UaItem[] }` | full decode with padding, ordering and compactSize checks |
| `orchardReceiverFromUa` | `(ua: string, expected?: ZNetwork) => Uint8Array` | the 43-byte Orchard receiver; throws on wrong network or none |
| `parsePayoutAddress` | `(input: string, expected?: ZNetwork) => Uint8Array` | UA or 86-hex raw receiver → 43 bytes (raw hex is not network-checked) |
| `encodeOrchardUa` | `(receiver: Uint8Array, network: ZNetwork) => string` | Orchard-only UA (tests, display) |

```ts
import { parsePayoutAddress } from "./ua";
const payout = parsePayoutAddress("utest1…", "testnet"); // Uint8Array(43)
```

---

## `gateway.ts`

Source: [`web/src/gateway.ts`](../../web/src/gateway.ts). Endpoint semantics:
[gateway-api.md](gateway-api.md).

| Export | Signature | Description |
|---|---|---|
| `AppInfo` | interface | `app`, `network`, `vault_address`, `denominations: number[]`, `tip`, `early_depth`, `full_depth`, `operators?: string[]`, `state` |
| `SessionInfo` | interface | `ua`, `receiver_index` (decimal string), `intent_registered` |
| `DepositStatus` | interface | `cmx`, `value`, `height`, `confirmations`, `state`, `txid?` |
| `GatewayError` | `class extends Error { status: number }` | thrown on a non-2xx response; `message` is the body's `error` |
| `Gateway` | `new Gateway(base: string, fetchImpl?: typeof fetch)` | one operator's page API; `base` is the operator URL (trailing `/` ignored) |
| `Gateway#app` | `(app: string) => Promise<AppInfo>` | `GET /v1/apps/{app}` |
| `Gateway#session` | `(app, sessionHex) => Promise<SessionInfo>` | `GET /v1/apps/{app}/sessions/{session}` |
| `Gateway#registerIntent` | `(app, memoHex) => Promise<SessionInfo>` | `POST /v1/apps/{app}/intents` |
| `Gateway#payoutCheck` | `(app, sessionHex, addressHex) => Promise<{ bound: boolean; matches: boolean \| null }>` | `POST …/sessions/{session}/payout` |
| `Gateway#deposits` | `(app, sessionHex) => Promise<DepositStatus[]>` | `GET …/sessions/{session}/deposits` |

Network failures surface as the `fetch` error, not `GatewayError`. `denominations` and `value`
are JS numbers (safe: zatoshi amounts are far below 2^53).

```ts
import { Gateway } from "./gateway";
import { hex } from "./session";

const gw = new Gateway("https://159-223-38-126.sslip.io/op0");
const info = await gw.app(APP);
const all = (info.operators ?? [gw.base]).map((u) => new Gateway(u));
const deposits = await gw.deposits(APP, hex(s.publicKey));
```

---

## `memo.ts`

Source: [`web/src/memo.ts`](../../web/src/memo.ts). Format: [gateway-api.md § Deposit memo](gateway-api.md#deposit-memo).

| Export | Signature | Description |
|---|---|---|
| `NETWORK_BYTE` | `Record<ZNetwork, number>` | `mainnet: 0, testnet: 1, regtest: 2` |
| `MemoInput` | interface | `session`, `payoutAddress` (43 B), `vaultAddress` (43 B, `AppInfo.vault_address`), `network`, `expiryHeight`, `autoExit?`, `appData?` |
| `buildSignedMemo` | `(i: MemoInput) => DepositMemo` | fills the memo (pot ID zero, flags from `autoExit`) and signs it with the session key |
| `canonicalMemo` | `(m: DepositMemo) => Uint8Array` | 512-byte canonical encoding (`encodeMemo`) |
| `zecString` | `(zat: bigint) => string` | zatoshi → decimal ZEC for ZIP 321, no trailing zeros |

---

## `wire.ts`

Source: [`crates/zol-wire/ts/wire.ts`](../../crates/zol-wire/ts/wire.ts) (symlinked at
`web/src/wire.ts`). TypeScript twin of `zol-wire`; no imports.

| Export | Signature | Description |
|---|---|---|
| `MEMO_LEN`, `APP_DATA_MAX`, `FLAG_AUTO_EXIT`, `TEXT_MAX` | `512`, `329`, `0x01`, `511` | |
| `TAGS` | `{ memoSig, credit }` | 16-byte domain tags `ZOL/memo-sig/v2`, `ZOL/credit/v1` (NUL padded) |
| `DepositMemo` | interface | `flags`, `sessionKey`, `payoutAddress`, `expiryHeight`, `potId`, `signature`, `appData` |
| `encodeMemo` | `(m: DepositMemo) => Uint8Array` | canonical 512 bytes; throws on long app data or reserved flags |
| `memoSigningMessage` | `(m, network: number, vaultAddress: Uint8Array) => Uint8Array` | the 508 bytes the session key signs |
| `creditDigest` | `(a: {...}) => Promise<Uint8Array>` | SHA-256 credit attestation digest (WebCrypto) |
| `base64url` | `(b: Uint8Array) => string` | unpadded base64url |
| `encodeMemoText` | `(m: DepositMemo) => string` | `ZOL1:` text form; throws if over 511 chars |
| `depositLink` | `(vaultUa: string, amountZec: string, m: DepositMemo) => string` | `zcash:{ua}?amount={zec}&memo={base64url(utf8(text memo))}`; pass `SessionInfo.ua` as `vaultUa` |

---

## `deposit.ts`

Source: [`web/src/deposit.ts`](../../web/src/deposit.ts)

| Export | Signature | Description |
|---|---|---|
| `EXPIRY_BLOCKS` | `1_000` | memo expiry = first gateway's `tip` + this |
| `PreparedDeposit` | interface | `app: AppInfo`, `memo: DepositMemo`, `session: SessionInfo`, `link: string`, `amountZat: bigint`, `registered: string[]`, `failed: { gateway, error }[]` |
| `prepareDeposit` | `(gws: Gateway \| Gateway[], appProgram: string, s: Session, payoutInput: string, amountZat: bigint, autoExit = false) => Promise<PreparedDeposit>` | see below |

`prepareDeposit`:

1. `GET` app info from the first gateway; throws unless `state === "active"` and `amountZat` is
   one of `denominations`.
2. Parses `payoutInput` (`parsePayoutAddress`, network-checked).
3. Builds and signs the memo with expiry `tip + EXPIRY_BLOCKS`.
4. Registers the canonical memo (hex) with **every** gateway passed, in parallel. Throws if none
   accepted; throws if two operators return different session `ua`s.
5. Returns the ZIP 321 `link` to the session's UA.

It does not call the payout check. On a device where the session may already have a balance, call
`Gateway#payoutCheck` first (as `house-page.ts` does), since a different payout address voids the
deposit.

```ts
import { Gateway } from "./gateway";
import { prepareDeposit } from "./deposit";
import { hex } from "./session";
import { parsePayoutAddress } from "./ua";

const first = new Gateway("https://159-223-38-126.sslip.io/op0");
const gws = ((await first.app(APP)).operators ?? [first.base]).map((u) => new Gateway(u));

const c = await first.payoutCheck(APP, hex(s.publicKey), hex(parsePayoutAddress(payoutUa, "testnet")));
if (c.bound && !c.matches) throw new Error("use the payout address bound to this session");

const d = await prepareDeposit(gws, APP, s, payoutUa, 1_000_000n /* 0.01 TAZ */);
console.log(d.link, d.registered, d.failed);   // show d.link as a QR code
// then poll first.deposits(APP, hex(s.publicKey)) until state === "credited"
```

---

## `chain.ts`

Source: [`web/src/chain.ts`](../../web/src/chain.ts). Account orders follow
[`idl/zol_coinflip.json`](../../idl/zol_coinflip.json) and
[`idl/zol_ledger.json`](../../idl/zol_ledger.json). Instruction data is the 8-byte Anchor
discriminator `sha256("global:<name>")[0..8]` plus little-endian args.

Two connections are involved: **base** (Solana) and **ER** (the MagicBlock ephemeral rollup).
Tables, pots and auctions are delegated to the rollup while a game runs; read them there when the
base account's owner is `DLP` (the pages' `readLive` pattern). Rollup transactions cost no SOL:
the session key can be the fee payer. Solana transactions need a funded fee payer.

### Constants

| Export | Value / type | Description |
|---|---|---|
| `DLP` | `DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh` | MagicBlock delegation program |
| `MAGIC_PROGRAM`, `MAGIC_CONTEXT` | `Magic1111…`, `MagicContext1111…` | MagicBlock commit/undelegate accounts |
| `VRF_PROGRAM` | `Vrf1RNUj…` | MagicBlock VRF program |
| `PRICE_PROGRAM`, `ZEC_USD_FEED` | `PriCems5…`, `6XWQr2Y1…` | MagicBlock price oracle and its ZEC/USD feed (rollup only) |
| `MAX_SEATS` | `16` | pot seats |
| `UPDOWN` | `{ payoutBps: 19_000n, roundSecs: 30, settleWindowSecs: 10, publishWindowSecs: 2 }` | Up/Down terms |
| `STAKE_SPLIT` | `10n` | a deposit also splits into tenths |
| `TABLE_STATES` | `["Open","Started","Flipping","Landed","Finished"]` | |
| `POT_STATES` | `["Open","Delegated","CashedOut","Refunded"]` | |
| `AUCTION_STATES`, `AUCTION_SEATS` | `["Registering","Bidding","Ended"]`, `9` | |

### Helpers

| Export | Signature | Description |
|---|---|---|
| `decodePrice` | `(data: Uint8Array) => { price: bigint; published: number }` | oracle account: price (8 decimals), publish time (unix s) |
| `magicFeeVault` | `(validator: PublicKey) => PublicKey` | `["magic-fee-vault", validator]` under `DLP` |
| `allowedStakes` | `(p: LedgerParams) => bigint[]` | stakes `create_table` accepts: each denomination net of entry fee, then a tenth of each |
| `bidCommitment` | `(auction, bidder: PublicKey, amount: bigint, salt: Uint8Array) => Uint8Array` | `sha256("ZOL/bid/v1" ‖ auction ‖ bidder ‖ amount LE ‖ salt)` |
| `bidSalt` | `(sessionSeed: Uint8Array, auction: PublicKey) => Uint8Array` | `sha256("ZOL/bid-salt/v1" ‖ seed ‖ auction)`: deterministic, nothing to save |

### `Pdas`

`new Pdas({ ledger, app }: Programs)`; `Programs = { ledger: PublicKey; app: PublicKey }` (`app`
is the zol-coinflip program).

| Method | Seeds | Program |
|---|---|---|
| `config()` | `"config"` | ledger |
| `app()` | `"app", app program` | ledger |
| `balance(session)` | `"balance", app(), session` | ledger |
| `pot(potId)` | `"pot", app(), potId` | ledger |
| `receipt(pot)` | `"receipt", pot` | ledger |
| `appAuthority()` | `"zol-app"` | app |
| `table(potId)` | `"table", potId` | app |
| `identity()` | `"identity"` | app |
| `rentVault()` | `"rent-vault"` | app |
| `dlp(tag, acc)` | `tag, acc` | `DLP` |
| `buffer(acc, owner)` | `"buffer", acc` | `owner` |
| `pda(seeds, program)` | any | any |

`potId` is 32 random bytes chosen by the table creator.

### `Coinflip`

`new Coinflip(p: Programs)`; `.pdas: Pdas`. Each method returns a `TransactionInstruction`.
"Where" is where the pages send it.

| Method | Where | Signer(s) | Description |
|---|---|---|---|
| `createTable(payer, potId, stake: bigint, openSecs: bigint, validator, updown = false)` | Solana | payer | coin-flip (or Up/Down) table and its pot; `stake` must be in `allowedStakes` |
| `join(session, potId, rounds?: number)` | Solana | session | take a seat with one stake (`join`) or `rounds` stakes (`join_rounds`) |
| `start(payer, potId, sessionSecs: bigint)` | Solana | payer | delegate table and pot to the rollup (pages use a 400,000 CU limit) |
| `flip(payer, potId, queue, callerSeed: Uint8Array)` | rollup | payer | request a VRF flip (`callerSeed` 32 bytes) |
| `bet(payer, potId, up: boolean)` | rollup | payer | Up/Down: seated player bets for one round |
| `settleBet(potId)` | rollup | any | Up/Down: settle the open bet after its round |
| `finish(payer, potId)` | rollup | payer | finish the table; it returns to Solana (then `cashOut` / `cashOutTable` there) |
| `finishAndPay(payer, potId)` | rollup | payer | `finish` plus a Magic Action that runs `cash_out_table` on Solana |
| `scheduleCloser(payer, potId, validator)` | rollup | payer | schedule the rollup to close the table when idle, short, or near its deadline |
| `cashOutTable(potId, creator, players)` | Solana | any | cash a returned pot out to both seats and close the table |
| `actionEscrow()` | — | — | the escrow PDA paying Magic Action fees (`["balance", rentVault, 255]` under `DLP`) |
| `topUpActionEscrow(payer, lamports: bigint)` | Solana | payer | `DLP` `TopUpEphemeralBalance` for that escrow |
| `refundPot(potId, players)` | Solana (ledger) | any | refund buy-ins of a pot that never started, after its open deadline |
| `closeTable(potId, creator)` | Solana | any | table rent back to the creator once the pot is cashed out or refunded |
| `rescueTable(potId, validator)` | Solana | any | table stuck delegated after a refund: first call requests undelegation, a later call (after 9,000 slots) rolls it back |
| `cashOut(potId, players)` | Solana (ledger) | any | ledger `cash_out`; `players` in receipt order |
| `requestExit(session)` | Solana (ledger) | session | mark the session's balance for exit |
| `settle(payer, seq: bigint, sessions)` | Solana (ledger) | payer | put up to 16 exiting balances into settlement `seq` (= `App.next_settlement`); operators then pay it on Zcash |

### `Auctions`

`new Auctions(cf: Coinflip)`. Sealed-bid auction instructions.

| Method | Where | Description |
|---|---|---|
| `auction(potId)` | — | PDA `["auction", potId]` under the app program |
| `create(payer, seller, potId, deposit: bigint, secs: [register, commit, reveal], validator)` | Solana | create the auction (payer and seller sign) |
| `register(session, potId)` | Solana | register a bidder (a ledger buy-in of the auction's `deposit` from the session's balance; the session signs) |
| `start(payer, potId)` | Solana | delegate auction and pot to the rollup |
| `commit(bidder, potId, commitment)` | rollup | sealed bid (`bidCommitment`) |
| `reveal(bidder, potId, amount: bigint, salt)` | rollup | reveal it |
| `end(potId, validator)` | rollup | end and return the pot |
| `scheduleEnd(payer, potId, validator)` | rollup | schedule `end` on the rollup |
| `cashOut(potId, players)` | Solana | `cash_out_auction` |

### Decoders

Each checks the 8-byte account discriminator (`sha256("account:<Name>")[0..8]`) and throws
`not a <Name> account` otherwise.

| Export | Returns |
|---|---|
| `decodeBalance(data)` | `BalanceAcc { amount: bigint; exiting: boolean; autoExit: boolean }` |
| `decodeApp(data)` | `AppAcc { state, activatesAt, liability, feePool, nextSettlement, paidCount, unpaidTotal }` |
| `decodeLedgerParams(data)` | `LedgerParams { entryFeeBps, denominations: bigint[4], minExit? }` from the `Config` account (fixed offsets) |
| `decodeTable(data)` | `TableAcc` (pot, potId, stake, validator, creator, state, winner, flips, flipRequestedAt, players, sessionDeadline, results, game, bet, wins, ties); tolerates older, shorter layouts |
| `decodePot(data)` | `PotAcc { seatCount, amounts, delegated }` |
| `decodeReceipt(data)` | `ReceiptAcc { state, players, buyIns, total, validator }` |
| `decodeAuction(data)` | `AuctionAcc` (deadlines, state, players, commits, bids, committed, revealed, winner, price, …) |

### Sending

| Export | Signature | Description |
|---|---|---|
| `sendTx` | `(conn: Connection, ixs: TransactionInstruction[], feePayer: Keypair, signers: Keypair[] = [], cuLimit?: number) => Promise<string>` | builds a legacy `Transaction` (optional compute-unit limit first), signs with the fee payer and `signers`, then `sendSigned` |
| `sendSigned` | `(conn: Connection, raw: Uint8Array \| Buffer, lastValidBlockHeight: number) => Promise<string>` | sends with preflight, rebroadcasts every 2 s until `confirmed`/`finalized`; throws with the last 8 log lines on error, or when the blockhash expires |

`sendSigned` is for transactions signed by several parties, e.g. a server that is fee payer
partially signs after the player signs (see [`web/house/auction.ts`](../../web/house/auction.ts)).

### Examples

```ts
import { Buffer } from "buffer";
(globalThis as any).Buffer ??= Buffer;
import { Connection, Keypair, PublicKey } from "@solana/web3.js";
import { Coinflip, DLP, decodeBalance, decodeApp, decodeTable, sendTx } from "./chain";

const base = new Connection("https://api.devnet.solana.com", "confirmed");
const er = new Connection("https://devnet-as.magicblock.app", "confirmed");
const cf = new Coinflip({
  ledger: new PublicKey("3qh6GRqjmVSeVctB6R7oYYsdG68oFyRDwnXGvKamib3Y"),
  app: new PublicKey("7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo"),
});
const me = s.keypair.publicKey;          // s: Session from session.ts
const feePayer: Keypair = s.keypair;     // or a funded relayer key

// Read the session's ledger balance (zatoshi).
const acc = await base.getAccountInfo(cf.pdas.balance(me));
const bal = acc ? decodeBalance(acc.data) : null;

// Read a table where it lives (base, or the rollup while delegated).
async function readLive(key: PublicKey) {
  const b = await base.getAccountInfo(key);
  return b && b.owner.equals(DLP) ? (await er.getAccountInfo(key)) ?? b : b;
}
const t = decodeTable((await readLive(cf.pdas.table(potId)))!.data);

// Solana: take a seat (fee payer pays SOL, the session key signs).
await sendTx(base, [cf.join(me, potId)], feePayer, [s.keypair]);

// Rollup: flip once the table is delegated (free; the session key pays).
const queue = new PublicKey("5hBR571xnXppuCPveTrctfTU7tJLSN94nq7kv7FRK5Tc");
await sendTx(er, [cf.flip(me, potId, queue, crypto.getRandomValues(new Uint8Array(32)))], s.keypair);

// Exit: mark the balance for exit and settle it in one Solana transaction.
const app = decodeApp((await base.getAccountInfo(cf.pdas.app()))!.data);
await sendTx(base, [cf.requestExit(me), cf.settle(feePayer.publicKey, app.nextSettlement, [me])], feePayer, [s.keypair]);
```

## Notes and open points

- Balances below the ledger's `minExit` (`decodeLedgerParams`) cannot be withdrawn. The house
  page disables withdrawal below it; `main.ts` does not check it before `requestExit`.
- The decoders use fixed byte offsets matching the current program layouts; they are not
  generated from the IDL, so a program layout change needs a matching update here.
