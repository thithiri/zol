# Plan: a second ZOL app, and what makes a good demo

Status: proposal, 2026-10-11. Follows the house coin flip ([plan-house-mode.md](plan-house-mode.md)).

## What a demo has to show

ZOL's claim is that shielded ZEC can be spent in a fast Solana app without the app, Solana
or anyone watching learning which Zcash wallet is playing. A demo should make each of these
visible, not just true:

| Capability | Where it shows | The coin flip today |
| --- | --- | --- |
| Shielded entry and exit; Solana sees only an unlinkable session key | deposit and withdraw | yes, but nothing on the page *shows* the unlinkability |
| Payouts only to the address bound at the first deposit | withdraw | yes (stated in "How it works") |
| Real-time play in a rollup, settled on Solana once | the game loop | yes, since sessions (2–3 s a flip) |
| Many players' money in one pot, moved by app rules | `buy_in` / `move_seat` with up to 16 seats | no: two seats |
| Outside facts deciding outcomes (an oracle) | resolution | no: VRF only |
| Operators attesting Zcash facts to Solana (FROST 2-of-3) | the attestor | only for deposits, invisible to players |

The coin flip covers the first three. A second app should add the last three, and the page
should add a **"what each chain sees" panel** (below), which is cheap and lands the privacy
point for every app.

## Candidates

1. **ZEC Up/Down: 60-second price rounds** (recommended). Bet that ZEC/USD is higher or
   lower in 60 s.
   - *Shows:* an oracle in the rollup, real-time rounds, a market people understand at a
     glance, and why privacy matters (nobody wants their trades linked to their wallet).
   - *Risk:* needs a price feed readable inside the MagicBlock rollup with about 1 s
     freshness. To verify in a spike before anything else (P0).
2. **Zcash block market, resolved by the ZOL operators.** For example "will the next Zcash
   block arrive within 25 s?" or "will block N carry an Orchard action?", rounds of about
   25 s.
   - *Shows:* the part only ZOL has: the same 2-of-3 FROST attestor that credits deposits
     signs a Zcash fact, and the app checks it on Solana.
   - *Risk:* outcomes are near coin flips and less intuitive to a newcomer; needs a new
     attested message type (zol-wire) and a gateway signing path.
3. **Shielded raffle**: up to 15 players buy tickets, the VRF draws a winner every few
   minutes, the prize goes to the winner's shielded address.
   - *Shows:* multi-seat pots, VRF, a crowd. Smallest new program.
   - *Risk:* weak demo when only one or two people are online.

**Recommendation: 1, with 2 as the fallback** if no price feed is usable in the rollup.
Build it as **fixed odds against the house** first, not as a pool among players:

- **Fixed odds** (each player against the house, paid 1.9× on a win, refunded on an
  unchanged price): reuses the session table that the coin flip has now. A player opens a
  table once and bets every round inside the rollup; "land" reads the price instead of the
  VRF. It scales to any number of players, one table each. The house edge (5% here) pays
  for the bankroll's risk and can be set to zero on testnet.
- **Pool among players** (winners split the losers' stakes): truer to a market, but every
  player of a round must share one pot, so at most 15 players a round (`MAX_SEATS` 16, the
  ledger's `max_seats` 8 today), and bets must be placed on Solana before the pot moves
  into the rollup. That's phase 2, once there's a crowd.

## Design (fixed odds, v1)

- **Program `zol-updown`**, registered with the ledger like the coin flip; the ledger is
  unchanged (`buy_in`, `move_seat`, `close_pot`, `cash_out` already fit):
  - `open_table` / `join_rounds` / `start`: as the coin flip's session table, player and
    house seats with N stakes each; the house's seat holds N × (odds − 1) stakes;
  - `bet(direction, round)`: in the rollup, by the seated player, before the round's lock
    time; at most one open bet;
  - `resolve(round)`: anyone, after the round's end; reads the oracle's price at lock and at
    end (stored by the round keeper, below), moves stakes between the two seats, records the
    result;
  - `finish`: as the coin flip (creator or a player; anyone after the deadline).
- **Rounds:** global, every 60 s on a fixed clock (lock at :00, resolve at :60). A small
  `Round` account per round in the rollup holds the lock and end prices, written by a
  keeper instruction that only copies from the oracle account. Players never supply a price.
- **House service:** the existing `zol-house` gains an Up/Down mode: opens tables, keeps
  rounds ticking, resolves, closes idle tables. Same co-signing, same limits.
- **Page:** a live price line, the current round with a countdown, Up and Down buttons, the
  player's open bet and result dots, and the same deposit and withdraw panels as the coin
  flip (shared module).

### "What each chain sees" panel (both apps)

A collapsible panel on the page, filled from real data:

- **Zcash sees:** a shielded payment to a vault address; amount and sender hidden (links the
  txid; the operators' node shows confirmations only).
- **Solana sees:** session key `2ZoG…` credited 0.00997, its bets and results, a settlement
  to a *commitment*, not an address.
- **Nobody sees:** which Zcash wallet is behind `2ZoG…`. The page states the limits honestly:
  amounts are fixed denominations, and timing between a deposit and a credit can correlate.

## Phases

| # | Task | Size |
| --- | --- | --- |
| P0 | **Oracle spike:** read ZEC/USD (Pyth on Solana devnet, or MagicBlock's price feed in the rollup) from a rollup instruction; measure freshness and gaps over an hour. Go/no-go for candidate 1 | 0.5 d |
| P1 | "What each chain sees" panel on the coin flip page | 0.5 d |
| P2 | `zol-updown` program from the session table (tests in LiteSVM like the coin flip's), register and activate on devnet | 2 d |
| P3 | House Up/Down mode and round keeper | 1 d |
| P4 | Up/Down page (share deposit and withdraw with `house.html`) | 1.5 d |
| P5 | Docs, ops (bankroll sized for odds), a headless smoke script | 0.5 d |
| P6 | Later: pool among players (needs `max_seats` raised to 16) or candidate 2 | — |

About 6 days. P1 is worth doing first regardless: it improves the coin flip demo on its own.

## Open questions

1. **House edge:** 1.9× (5%) on mainnet-like terms, or 2× (none) for the testnet demo?
2. **Ties:** refund the stake (proposed) or count as a loss?
3. **Oracle trust:** the result depends on the feed and on the rollup validator, as the coin
   flip depends on the VRF and the validator. The page should say so.
