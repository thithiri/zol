# Networks and addresses

## The testnet beta

| | |
| --- | --- |
| Zcash | testnet, NU7 active since height 4,465,026 (25 s blocks) |
| Solana | devnet |
| MagicBlock/SVM rollup | `https://devnet-as.magicblock.app` (validator `MAS1Dt9qreoRMQ14YQuhg8UTZMMzDdKhmkZMECCzk57`) |
| Pages | <https://159-223-38-126.sslip.io/> (landing), `/play.html` (deposit, withdraw, the games), `/coinflip.html`, `/updown.html`, `/auction.html`, `/two-player.html`, `/docs/` (these docs) |
| Gateway API (3 operators) | `https://159-223-38-126.sslip.io/op0`, `/op1`, `/op2` ([reference](gateway-api.md)) |
| House service (the games' counterparty) | `https://159-223-38-126.sslip.io/house` |

Everything runs on one host for the beta; the operators are meant to run on separate hosts
and providers before mainnet.

## Programs and accounts (Solana devnet)

| What | Address |
| --- | --- |
| `zol-ledger` | `3qh6GRqjmVSeVctB6R7oYYsdG68oFyRDwnXGvKamib3Y` |
| `zol-coinflip` (the games app: coin flip, Up/Down, auction) | `7G2o74qyLh477EuWUFdgumeB13kTCK1cuHgKoRqM1yUo` |
| MagicBlock delegation program | `DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh` |
| MagicBlock magic program / context | `Magic11111111111111111111111111111111111111` / `MagicContext1111111111111111111111111111111` |
| MagicBlock VRF program | `Vrf1RNUjXmQGjmQrQLvJHs9SNkvDJEsRVFPkfSQUwGz` |
| VRF queue (rollup) | `5hBR571xnXppuCPveTrctfTU7tJLSN94nq7kv7FRK5Tc` |
| MagicBlock price oracle program | `PriCems5tHihc6UDXDjzjeawomAwBduWMGAi8ZUjppd` |
| ZEC/USD feed (Pyth Lazer id 66) | `6XWQr2Y1XEpJrCdVbYGupnDeb3wkR4YXazVXdgn2Lwpg` (live **in the rollup only**) |

The ledger's pinned code hash is in the [README](../../README.md); every operator refuses to
sign if the deployed ledger differs.

## Ledger settings on the testnet

| Setting | Value |
| --- | --- |
| Deposit amounts (denominations) | 0.01 TAZ (1,000,000 zat), 0.1 TAZ (10,000,000 zat) |
| Entry fee / exit fee | 0.3% / 0.3% |
| Network-fee allowance per payout | 20,000 zat |
| Smallest withdrawal (`min_exit`) | 50,000 zat |
| Seats per pot (`max_seats`) | 16 |
| Pot open at most / session length | 1 hour / 1 minute to 2 hours |
| Refund grace after the session deadline | 3,900 s |
| App activation delay | 1 hour |
| Credit depth | 3 blocks early (within the operators' float), 60 blocks full; payouts final at 60 |

Read the live values from the ledger's `Config` account or `GET /v1/apps/{app}`.

## Getting TAZ

Many testnet faucets still follow the pre-NU7 fork, and their TAZ never reaches the current
chain. [Valar's faucet](https://faucet.testnet.valargroup.dev) is on the NU7 chain
(`/api/status` reports `chainHeight`; 0.125 TAZ per IP per day). Check any faucet's height
against your node's before using it.

## MagicBlock in ZOL

What ZOL uses from MagicBlock, and what we learned getting it to work on devnet.

| Feature | Used for | Notes |
| --- | --- | --- |
| Delegation | pots (by the ledger) and app state (by the app) move into the rollup | Name the validator explicitly; without it the delegation program picks a default and the account never appears in your rollup |
| VRF | coin-flip outcomes | Pin the oracle queue (`ALLOWED_QUEUES`) so a caller can't name a queue whose oracle they run. The oracle submits each answer more than once; later copies fail harmlessly |
| Price oracle | ZEC Up/Down | The feed account is updated **only in the rollup**; its Solana copy is stale. It is a Pyth `PriceUpdateV2` body: verification level at byte 40, feed id 41..73, price (`i64`) at 73, publish time (`i64`) at 93. The publish time trails the rollup clock by about a second |
| Scheduled tasks (cranks) | closing idle tables, ending auctions | A scheduled instruction may not include **any** signer account; sign with your program's PDAs inside it. The validator ran tasks about every 5 s even when 20 s was asked |
| Return actions (post-undelegate) | paying a pot out the moment it is back | Attach them to the account whose return matters (the ledger does this for pots via `close_pot`'s action); separate accounts return in separate transactions |

Gotchas:

- **Signer flags in CPIs.** The SDK copies each `AccountInfo`'s `is_signer` into the magic
  program's instruction; mark a PDA you sign for as a signer (`info.is_signer = true`) before
  passing it. Anchor CPIs use the callee's declared signer flags instead, so declare optional
  signers as `Option<Signer>`.
- **Delegated payers.** If the commit's payer is itself a delegated account (e.g. a table
  paying from a scheduled task), pass the validator's magic fee vault
  (`["magic-fee-vault", validator]` under the delegation program), and keep a lamport buffer
  above rent: the payer is charged the action fee.
- **Program size.** Upgrades often need `solana program extend` first.
