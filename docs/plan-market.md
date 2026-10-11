# Plan: a binary prediction market in the MagicBlock rollup

Status: proposal, 2026-10-11. Builds on the coin-flip session tables ([plan-house-mode.md](plan-house-mode.md)).

Goal: a YES/NO market ("Will ZEC/USD be above $X at 18:00 UTC?") that anyone can join at
any time, trade in and out of in real time inside the rollup, and that pays winners to their
ledger balance, then out to their shielded Zcash address.

## The idea: the house is the market maker, every player has a table with it

A pot cannot take new money once it is in the rollup (`buy_in` runs on Solana before
delegation), and it holds at most 16 seats. An open market seems to need one big pot that
anyone can enter. It does not:

- In an automated prediction market (Gnosis FPMM, LMSR, Polymarket's AMM pools) traders never
  trade with each other. **The market maker is the counterparty to every trade**; the shared
  state is only the pricing curve.
- So each player gets **their own two-seat pot with the house**, the coin flip's session
  table, and one shared **`Market` account** in the rollup holds the curve. A trade reads and
  updates the curve, then moves the price between the player's seat and the house's seat.
- Joining at any time is opening a table (one co-signed Solana transaction, about 3 s, as the
  coin flip). There is no seat cap: one pot per player, any number of players.

**No ledger change.** Every money movement is a `move_seat` within one player's pot; the
ledger's guarantees hold as they are (no value created, payouts only to the receipt's
players, refunds if the rollup withholds).

**Same app, same balances.** Balances and the vault belong to an app (`[balance, app,
session]`), and a new app would need its own vault genesis. So the market goes into the
existing app program (`7G2o…`, today's coin flip, upgraded in place): players use the balance
they already have.

## Design

### Accounts

| Account | Where | Holds |
| --- | --- | --- |
| `Market` (PDA `["market", id]`) | delegated to the rollup for its life | question hash, close and resolve times, resolver, AMM pools, total shares out, status, outcome |
| `Table` (today's, plus a market id) | delegated with its pot | the player's YES and NO shares and what they paid |
| Pot and receipt | ledger, as today | seat 0 the house, seat 1 the player |

All of a market's tables and its `Market` account are delegated to the **same validator**, so
one rollup transaction can touch both.

### The curve: fixed product (FPMM), integer math only

- One share pays one `unit` (e.g. 10,000 zat = 0.0001 TAZ) if its side wins.
- Pools `y` (YES) and `n` (NO), seeded by the house's liquidity `L`: `y = n = L`. Price of YES
  = `n / (y + n)`.
- **Buy YES with `c` units:** add `c` to both pools (minting `c` complete sets), then take out
  YES until `y · n` is back to its old value `k`. The player gets `(y + c) − k / (n + c)` YES
  (about `2c` at an even price).
  Sell is the inverse. All integer, rounding always against the trader.
- Why FPMM over LMSR: no exp or log on-chain, same pricing behaviour, and the house's worst
  loss is bounded by the liquidity it seeded.

### Instructions (rollup unless noted)

| Instruction | By | Does |
| --- | --- | --- |
| `create_market` (Solana) | house | creates `Market`, seeds pools, then delegates it |
| `open_table` (Solana) | house + player, co-signed | today's create + `join_rounds` + `start`, naming the market; the house's seat is the collateral |
| `buy(side, cost)` / `sell(side, shares)` | the player | prices on the curve; moves `cost` player seat → house seat (or the proceeds back); updates shares and pools |
| `resolve(outcome)` | the resolver (v1: the house key; v2: oracle or operator attestation) | after `close_time`; sets the outcome |
| `settle_table` | anyone | after resolve: moves `winning shares × unit` house seat → player seat |
| `finish` | as today | before close: the player's shares are sold back at the curve first; after resolve: needs `settle_table` first |

Then, on Solana as today: cash out and close the table.

**The one invariant that keeps every payout funded:** in each table, at all times,

    house seat ≥ max(YES shares, NO shares) × unit

`buy` refuses a trade that would break it. The house funds each table's seat at opening
(e.g. 10× the player's buy-in); a player who wants a bigger position opens a bigger table.

### Time limits

- A table lives at most the ledger's `max_session_secs` (2 h on devnet). Either markets close
  within a table's life (demo markets of 30 min to 2 h), or the admin raises
  `max_session_secs` (a setting, e.g. 24 h; it also lengthens how long a withholding rollup can
  hold a pot before refund).
- A player can leave any time before close: their shares are sold at the curve and the table
  closes.

### Resolution

- **v1:** the house key resolves, after `close_time`, and the page shows the source and the
  transaction. Honest about trust: the house is also the counterparty.
- **v2, price markets:** read the price account (Pyth) in the rollup at `close_time`; needs
  the P0 spike.
- **v2, Zcash facts:** the operators' 2-of-3 attestor signs the outcome, verified like a
  deposit credit; only ZOL can offer this.

### Privacy, stated on the page

Trades and positions are public on Solana and in the rollup, tied to a session key, never
to a Zcash wallet; money enters and leaves shielded. A player can use a fresh session per
market.

## Phases

| # | Task | Size |
| --- | --- | --- |
| P0 | **Spikes:** (a) 20 tables and one shared `Market` account delegated to the same validator, concurrent trades: latency and conflicts; (b) Pyth ZEC/USD readable in the rollup, freshness | 1 d |
| P1 | Program: `Market`, FPMM math with property tests (conservation, rounding against the trader, the house-seat invariant), `buy`/`sell`/`resolve`/`settle_table`, finish rules; LiteSVM tests; upgrade on devnet | 3 d |
| P2 | House: create and seed markets, open tables with collateral, a keeper that resolves, settles every table and closes idle ones; bankroll sizing | 1.5 d |
| P3 | Page `market.html`: question, price chart (from the `Market` account in the rollup), buy and sell YES/NO, position and P&L, leave; shared deposit and withdraw | 2 d |
| P4 | Devnet run with real deposits (needs TAZ), docs | 0.5 d |
| Later | Oracle and attestor resolution; markets longer than a session; an order book or a shared-collateral pot, which needs a ledger change | — |

About 8 days.

## Limits of this design

- **The house carries the risk:** it is the counterparty, bounded by the liquidity it seeds
  per market and the collateral it puts in each table. Fine for a demo; a real market wants
  outside liquidity providers (a later ledger primitive).
- **Collateral per table** is more capital than a shared pool would need.
- **Markets are as long as a session** unless `max_session_secs` is raised.

## Decisions needed

1. First market: a price market (needs the P0 oracle spike to resolve automatically, or the
   house resolves) or a Zcash fact?
2. Market length for the demo: 1 h? (Fits today's 2 h session limit.)
3. Share unit and table sizes: unit 10,000 zat; player buy-in up to 10 stakes; house collateral 10×?
4. House liquidity per market `L`, and whether the house takes a fee on trades (e.g. 1%).
