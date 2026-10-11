# App design patterns

Patterns that worked on the testnet, with the trade-offs we hit.

## Sessions: many rounds per table

Moving a pot into the rollup and back costs a few seconds and several Solana transactions.
Do it once per **session**, not once per round:

- each seat buys in **several stakes** (`join_rounds(n)`), and seats are equal multiples of
  the stake;
- every round moves **one stake** between seats (`move_seat`);
- rounds continue while both seats can cover a stake;
- the table closes when a seat runs short, the player stops, it sits idle, or its session
  deadline nears.

Measured on devnet: about 3 s from the player's click to the table in the rollup (one
transaction creates, seats and starts it), then under 1 s per coin flip.

## A counterparty service ("the house")

Single-player games need someone on the other side. Run it as a player:

- a session key with a ledger balance (the bankroll), funded by ordinary deposits;
- a service that builds transactions for players to sign and pays their fees, so players
  need no SOL. It co-signs only the exact transaction it built (`checkSigned` in
  [`../web/house/house.ts`](../web/house/house.ts));
- limits: open tables, plays per IP, the bankroll per table.

The ledger treats it like any player, so it can't do anything a player couldn't. Keep it out
of the trust path: anything it does should also be possible for the player or the rollup.

## Closing without a keeper

A table should come home even if every client is gone. Layer it:

1. **The return action** on `close_pot`: the payout runs on Solana the moment the pot is back.
2. **A scheduled closer** in the rollup (`ScheduleTask`) runs a permissionless close every
   few seconds, acting only when a close is due (idle, short seat, deadline).
3. **The service** as a fallback, a little later than the closer.
4. **The ledger's refund** after the session deadline plus grace, if the rollup never returns
   the pot. The refund returns buy-ins, not results, so keep 1 to 3 working.

Every permissionless instruction (close, settle, cash out) must be harmless when called
early, late or twice: return quietly when there is nothing to do.

## Deciding outcomes

| Source | Use for | Trust | Notes |
| --- | --- | --- | --- |
| VRF (MagicBlock) | chance | the oracle and the rollup validator | Pin the oracle queue. Handle an answer that never comes (a timeout that abandons the round) |
| Price oracle | markets, bets on prices | the feed and the validator | Live only in the rollup. Bound *when* a price may be used (a publish-time window), or whoever settles picks the price |
| Commit-reveal | hidden choices (auctions, rock-paper-scissors) | none new | Two phases; penalise not revealing; everything is public after the reveal |
| Operator attestation (planned) | facts about Zcash, or any agreed result | the operators (2 of 3) | The same threshold that credits deposits; see [`../docs/plan-polymarket.md`](../docs/plan-polymarket.md) |
| Private rollup (TEE) | hidden state | Intel TDX and MagicBlock's TEE validator | Not used on the testnet; hides losing bids forever, adds hardware trust and ingress rules |

## Many players in one pot

A pot has a fixed number of seats (`max_seats`, 16 on the testnet) and its return action must
name every seat's balance (at most 16 accounts), so the auction caps bidders at 8. For open
membership (a market anyone can join any time), give each player their own pot with the
counterparty, as in [`../docs/plan-market.md`](../docs/plan-market.md), or wait for the
market pot in [`../docs/plan-polymarket.md`](../docs/plan-polymarket.md).

## Hiding amounts with equal deposits

Deposits come in fixed denominations, and buy-ins are visible on Solana. Where an amount
should stay private (a bid), have everyone buy in the **same** amount and keep the real value
in app state until it's needed.

## What to copy into rollup state

The rollup cannot read the receipt (it stays on Solana). Copy what rollup instructions must
check, such as the seated players and the session deadline, into your app's account when the
table starts.
