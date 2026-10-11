# Plan: ZEC Up/Down, from MagicBlock's Binary Prediction template

Status: proposal, 2026-10-12. Template:
[binary-prediction](https://github.com/magicblock-labs/magicblock-engine-examples/tree/main/binary-prediction/anchor)
([docs](https://docs.magicblock.gg/pages/templates/binary-prediction)). Builds on the coin flip's
session tables and keeper-free closing ([ops/house.md](ops/house.md)).

Goal: bet that ZEC/USD is higher (Up) or lower (Down) after a short round, with a shielded ZEC
deposit in and out, each bet decided in the rollup against MagicBlock's live price feed.

## What the template does

| Template | What it is |
| --- | --- |
| `Pool` (one per market, not delegated) | mint, price feed and Pyth feed id, `bet_duration_seconds`, `min_stake`, `payout_bps` (e.g. 19000 = 1.9×) |
| Pool custody | SPL tokens in a delegated Ephemeral-SPL account; the pool is the counterparty, unhedged |
| `Bet` (per user, delegated) | `open_price`, `expiry_ts`, `direction`, `stake`, `is_open`; one open bet, reset after settlement |
| `place_bet(direction, stake)` (rollup) | session-key signed; reads the price (`PriceUpdateV2::get_price_no_older_than`), checks the pool can pay `stake × payout_bps`, moves the stake into the pool |
| `settle()` (rollup, anyone) | after expiry, reads the price again: a tie refunds, a right call pays `stake × payout_bps`, a wrong one pays nothing |
| Template's own caveats | "not a production risk engine": an unhedged pool, simple accounting, settlement manual |

## How it maps onto ZOL

| Template | ZOL |
| --- | --- |
| SPL token custody, Ephemeral SPL accounts, token approvals | **the ledger**: shielded ZEC deposit to a balance, a session table's pot, withdrawal to the bound shielded address |
| Pool liquidity | the **house's seat** in the player's table, funded at `start` (collateral) |
| Session keys plus a one-time SPL `approve` | the **ZOL session key** (passkey or saved key), already the ledger identity |
| `Bet` account | bet fields in the existing `Table` (a game-kind byte): one open bet per table |
| `place_bet` moves the stake into the pool | the stake stays in the player's seat; `settle` moves it with `move_seat` |
| Manual `settle` | **settled by the rollup itself**: `bet` schedules a one-shot `crank_close`-style task at expiry |
| Undelegate and withdraw | the table's keeper-free close: `crank_close` when idle, payout on the pot's return |
| Pyth receiver account | MagicBlock's oracle PDA for ZEC/USD (`6XWQr2Y1…`, Lazer id 66, 200 ms), live **only in the rollup** |

Same app program (`7G2o…`), so a player uses the same balance for coin flips and Up/Down.

## One fix to the template: settle at expiry, not whenever

In the template anyone may `settle` any time after expiry, at **the price at that moment**. A
player (or the house) can wait for a favourable price before settling. Fix:

- `settle` is allowed only within `SETTLE_WINDOW` (e.g. 5 s) after expiry, and only with an
  oracle update published **at or after expiry** (the feed's publish time, read from the account).
- The scheduled task settles at expiry; the page and the house can also call it.
- If nobody settles inside the window, the bet is **void**: the stake stays where it is
  (a refund), so missing the window never favours either side.

## Program changes (in `zol-coinflip`)

| Instruction | Runs on | Does |
| --- | --- | --- |
| `bet(direction)` | rollup, the seated player | table is an Up/Down table with no open bet; reads the price (fresh, right feed); requires house seat ≥ stake × (payout − 1) and player seat ≥ stake; records `open_price`, `expiry`, `direction`; schedules `settle_bet` at expiry |
| `settle_bet` | rollup, anyone (the scheduled task) | inside the window, with a post-expiry price: tie → nothing moves; right → `move_seat` house → player stake × (payout − 1); wrong → `move_seat` player → house stake; clears the bet; outside the window → void |
| `create_table(..., game)` | Solana | the game kind (coin flip or Up/Down) and, for Up/Down, `payout_bps` and `duration` |
| `crank_close` | rollup | unchanged rules; never closes a table with an open bet before it is settled or void |

Settings (program constants or table fields): `payout_bps` 19000, `duration` 30 s or 60 s,
`SETTLE_WINDOW` 5 s, `MAX_PRICE_AGE` 2 s, one stake (0.001 TAZ) per bet.

## House and page

- **House:** opens Up/Down tables with collateral for the payout (at 1.9×, 0.9 stake per player
  stake, so 9 house stakes for 10 player stakes); the existing fallbacks; nothing else to drive.
- **Page:** the live ZEC/USD price from the oracle account in the rollup (websocket), a small
  chart, a countdown, Up and Down buttons, the open bet with its entry price, the result; the
  same deposit and withdraw panels and Start table flow as the coin flip.

## Tests

- LiteSVM with a fixture oracle account (the template uses a test-mode oracle `.so`): win, loss,
  tie, the window (too early, inside, too late → void), a stale price, the wrong feed, the
  house-seat check, one open bet, `crank_close` waiting for an open bet.
- Devnet: a real session with the live feed; settlement by the scheduled task with the page closed.

## Phases

| # | Task | Size |
| --- | --- | --- |
| P1 | Program: game kind, `bet`, `settle_bet` with the window, oracle read, scheduled settle; tests | 1.5–2 d |
| P2 | House: Up/Down tables and collateral | 0.5 d |
| P3 | Page: price, countdown, Up/Down, result | 1.5–2 d |
| P4 | Devnet run, docs | 0.5 d |

About 4–5 days.

## Trust, stated on the page

The result depends on the price feed (Pyth Lazer via MagicBlock) and the rollup validator, as
the coin flip depends on the VRF and the validator. The house is the counterparty and keeps the
edge (5% at 1.9×).

## Decisions

1. Payout: 1.9× (a 5% house edge) or 2× (none) for the testnet?
2. Round length: 30 s or 60 s?
3. A tie refunds (template) — keep?
4. Stake per bet: one stake (0.001 TAZ), or let the player choose 1–5?
