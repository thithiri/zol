# Plan: Polymarket-style binary markets on ZOL, resolved by the ZOL operators

Status: proposal, 2026-10-11. Alternative to the per-table AMM in [plan-market.md](plan-market.md),
which can serve as a first milestone (see "Milestones").

Goal: YES/NO markets where players trade **with each other** on an order book in the
MagicBlock rollup, money comes in and goes out as shielded ZEC, and the result is signed by
the ZOL operators' 2-of-3 threshold key, the same key that credits deposits.

## Polymarket, mapped to ZOL

| Polymarket | ZOL |
| --- | --- |
| USDC collateral | a ledger balance, deposited from shielded ZEC, withdrawn to the bound shielded address |
| Conditional tokens: 1 USDC splits into 1 YES + 1 NO, merges back, the winner redeems 1 | the same, as app state in the rollup, backed 1:1 by a market vault |
| Order book matched off-chain by an operator, settled on Polygon | order book **on-chain in the rollup** (free, millisecond transactions); anyone can run the matching crank, the program checks every match |
| Three match types: transfer, mint (YES buy + NO buy), merge (YES sell + NO sell) | the same |
| UMA optimistic oracle resolves | **the ZOL operators sign the outcome** (FROST Ed25519, 2 of 3); invalid or unresolved markets pay 50/50 |
| Wallet address visible with every trade | a session key visible with every trade; never linked to a Zcash wallet |

## Why the ledger needs one new primitive

Players trading with each other move money **between different players' accounts in the
rollup**. Today the ledger allows moves only between seats of one pot, and a pot cannot take
new money once it is in the rollup, and has at most 16 seats. A market needs a pot that
anyone can enter or leave at any time, with no seat limit: a **market pot**.

### Market pot (ledger)

| Account | Where | Holds |
| --- | --- | --- |
| `MarketPot` | Solana only | app, market id, state, `total_in`, `total_out`, deadlines |
| `Position` (PDA `["position", market_pot, player, n]`) | delegated to the rollup | an amount only. **The owner is in the address**: a rollup cannot change who it pays, because the ledger re-derives the address at return |
| `PositionReceipt` (PDA per player and market) | Solana only | what this player put in and took out (for the refund path) |
| the market vault | a `Position` owned by the app's market, not a player | collateral behind outstanding YES+NO sets |

| Instruction | Runs on | Does |
| --- | --- | --- |
| `open_market_pot` | Solana | app only; sets deadlines (trading close, resolution, refund) |
| `enter_market(amount)` | Solana | session key + app: balance → a new `Position`, delegated to the market's validator. Allowed any time before close. A top-up is another `Position`, merged in the rollup |
| `market_transfer(from, to, amount)` | rollup | app only: between two positions of **the same market**; zero-sum |
| `leave_market` | rollup → Solana | the app commits and undelegates a position; on Solana the ledger credits the owner's balance, with `total_out + amount ≤ total_in` |
| `refund_market` | Solana | after the refund deadline, if positions never came back: each player gets what they put in minus what they took out, scaled down if the market is short |

**What the ledger still guarantees:** no value created (`total_out ≤ total_in`), payouts only
to the player the address names, then only to their bound Zcash address, and nobody trapped
(refund path). **What it cannot:** that the app or the rollup validator moved money correctly
inside the market; a bad validator can reshuffle a market's money, as it can a pot's today.
The blast radius is one market's collateral.

## The market program (in the existing app program)

Balances belong to an app, and a new app needs its own Zcash vault, so markets go into the
existing app program (`7G2o…`, upgraded in place): players trade with the balance they
already have.

- **`Market`** (delegated): question hash, spec hash (below), close time, resolution deadline,
  tick size (0.01), share unit (e.g. 0.0001 TAZ), state, outcome.
- **`Holdings`** per player (delegated, created with their first position): YES and NO
  shares, cash locked in open orders, up to 8 open orders.
- **`Book`** (delegated): one book on YES; bids and asks at 99 price levels, FIFO per level,
  fixed capacity (e.g. 256 resting orders). "Buy NO at q" is an ask on YES at 1 − q backed by
  cash, so one book serves both sides.
- **Instructions (rollup):**
  - `split` / `merge`: position ↔ vault, minting or burning YES+NO;
  - `place(side, price, size)` / `cancel`: lock or unlock cash or shares in `Holdings`;
  - `match`: anyone (the house runs the crank); takes the best bid and best ask, checks they
    cross and are the queue heads, settles one of the three match types with
    `market_transfer` and share moves. One transaction matches up to N maker orders;
  - `resolve`: verifies the operators' signature (below), sets the outcome, freezes the book;
  - `redeem`: winning shares × unit from the vault to the player's position (0.5 each side if
    `Invalid`);
  - then `leave_market` returns cash to the balance.
- **Invariants checked in tests:** vault = outstanding sets × unit; positions + vault =
  `total_in − total_out`; locked cash and shares never spent twice; a match never trades
  through a better price.

## Resolution signed by the ZOL operators

The gateways already run a FROST Ed25519 2-of-3 attestor. Each operator signs a statement
(`Credit`, `Void`, `Paid`) **only if its own view gives the same statement**, and its log
refuses to sign a conflicting one. A result is one more statement:

- **`Statement::Resolve { app, market, outcome }`**, digest
  `SHA-256("ZOL/resolve/v1" ‖ network ‖ app ‖ market ‖ spec_hash ‖ outcome)`. The log keys it by
  market, so no operator ever signs both YES and NO.
- **Market spec:** a small document (question, source, rule, close time, resolution deadline)
  whose hash is in the `Market` account. Operators sign only for specs on their allow-list
  (testnet: the house's markets), so nobody can make them sign arbitrary questions.
- **How each operator decides, by market type:**
  - **Zcash facts** ("NU7 active on mainnet by block H", "block H has an Orchard action"):
    from the operator's own Zcash node. This is where ZOL is strongest: the same independent
    views that guard deposits.
  - **Prices** ("ZEC/USD above $X at T"): from a named source in the spec (e.g. the Pyth price
    at T), fetched by each operator independently.
  - **Anything else:** each operator's human answer, entered through an operator command;
    two must agree.
- **Verification on-chain:** an Ed25519 instruction by the ledger's `attestor` key over the
  digest, checked like a deposit credit. In the rollup if MagicBlock supports the Ed25519
  program there (P0); otherwise on Solana into a small `Resolution` account the rollup reads.
- **No answer by the resolution deadline:** anyone may resolve `Invalid` (50/50).

## Lifecycle

1. The house opens a market: spec registered with the operators, `MarketPot` and `Market`,
   `Book` delegated.
2. Players `enter_market` (Solana, one co-signed transaction), then everything else is in the
   rollup: split, place, cancel, matches, merge, leave with cash at any time.
3. The house's market-maker bot quotes both sides from its own position, as any player.
4. Close: the book freezes. The operators sign the outcome; anyone submits it.
5. Players redeem and leave; the market pot closes when every position is back.

## Trust and privacy, as the page states them

| Who | Can | Cannot |
| --- | --- | --- |
| Operators (2 of 3) | sign a wrong outcome | move money; sign two outcomes for one market |
| Rollup validator | reorder or withhold transactions; commit wrong positions (bounded by the market's collateral) | pay anyone outside the market |
| App / house | run the crank, quote, open markets | match against the book's rules, take locked funds |
| Observers | see every order and trade per session key | link a session key to a Zcash wallet |

## Milestones

| | What | Ledger change | Size |
| --- | --- | --- | --- |
| **M1** | Operator-signed resolution (`Statement::Resolve`, spec allow-list, on-chain check) on the per-table AMM of [plan-market.md](plan-market.md): players trade against the house | no | about 10 d |
| **M2** | Market pot + order book: players trade with each other | yes | about 15 d more |

M1 already shows the parts only ZOL has (shielded in and out, operator-signed results, the
rollup). M2 makes it a real exchange. Going straight to M2 is about 18 days.

## Phases (straight to M2)

| # | Task | Size |
| --- | --- | --- |
| P0 | **Rollup spikes:** Ed25519 program in the rollup; creating and delegating new accounts to a validator while others are live; a `match` touching 8–10 accounts (size and compute); reading an updated Solana account from the rollup | 1.5 d |
| P1 | **Ledger market pot:** the five instructions, refund path, guardian and reserve accounting of market pots as claims, LiteSVM tests, an invariants test; upgrade on devnet with no pots open | 4 d, then a review |
| P2 | **Operator resolution:** `Statement::Resolve` in zol-wire and the gateway, spec allow-list, Zcash-fact and price evaluators, operator command for manual specs, FROST signing through the existing peer protocol | 3 d |
| P3 | **Market program:** market, holdings, book, split/merge, place/cancel/match (three match types), resolve, redeem, invalid; property tests | 5 d |
| P4 | **House:** open markets, market-maker bot, matching crank, closer | 2 d |
| P5 | **Page:** market list, book depth, price chart, limit and market orders, positions and P&L, redeem; shared deposit and withdraw | 3 d |
| P6 | Devnet run with real deposits, docs (player guide, operator runbook for resolution) | 1 d |

About 18–20 days.

## Decisions needed

1. **M1 first** (about 10 days, demoable sooner) **or straight to M2**?
2. **First market type:** a Zcash fact (strongest showcase of operator signing) or a price?
3. **Unit and tick:** 0.0001 TAZ a share, 0.01 tick?
4. **Fees:** none (as Polymarket today) or a small taker fee to the house that runs the crank?
5. **Testnet terms:** the house's markets only (a spec allow-list) until the resolution flow
   has run for a while?
