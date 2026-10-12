# Security model

What each party can and cannot do, what the ledger enforces for you, and what your app must
get right. The full analysis is in [`../docs/design-v0.3.1.md`](../docs/design-v0.3.1.md)
("Trust model", "Invariants the ledger enforces"); the latest internal review is
[`../docs/review/close-pot-actions.md`](../docs/review/close-pot-actions.md).

## Who can do what

| Party | Can | Cannot |
| --- | --- | --- |
| **Operators (2 of 3)** | Stall deposits and payouts. A colluding quorum can sign a false credit (reconciliation then freezes the app) or spend the vault: the design has no bound on that yet | Move a ledger balance; as honest operators, pay a settlement anywhere but the committed address, or pay one twice (every payout spends the vault's head note); sign anything for a ledger whose code differs from their pin |
| **One operator** | Freeze an app as a guardian (a threshold unfreezes) | Sign alone; unfreeze |
| **Rollup validator** | Reorder or withhold rollup transactions; commit wrong seat amounts for pots it holds | Change who sits in a pot (the receipt stays on Solana); touch another pot; keep a pot forever (refund after the deadline) |
| **Your app** | Decide every outcome in its own pots; choose when to close them | Create value; add or replace a player; move a balance without the player's signature; touch another app; change a payout address; block an exit of unstaked funds |
| **Ledger admin** | Register settings within program caps, activate apps, set caps, pause; rotate the attestor key (bounded by reconciliation); hand over the admin role | Unfreeze; change guardians; move funds |
| **A player's key** | Play its balance, request an exit | Redirect a payout |

## What the ledger guarantees your players

- **No value created:** returned pots must sum to what went in; balances only move between a
  player, pots and exits.
- **Payouts only to the receipt's players**, in seat order, whoever calls `cash_out`.
- **Nobody trapped:** players leave an unstarted pot; a pot that doesn't return is refunded after
  its session deadline plus 65 minutes; a frozen app still returns pots and takes exit requests,
  and payouts resume once it's unfrozen.
- **Return actions are confined:** a `close_pot` action may call only the pot's own app, with
  bounded size; it runs signed only by the app's escrow.

## What your app must get right

The ledger checks that seats add up, **not that the right seat won**. Review your app for:

- [ ] **Outcomes** come only from sources you can defend (VRF, a bounded oracle read, revealed
      commitments), never from caller-supplied values.
- [ ] **Signers:** only seated players act for themselves (copy the receipt's players into
      rollup state at start); permissionless instructions are harmless in any order.
- [ ] **Timing:** anything decided by "the latest value" (a price) bounds *when* that value may
      be used; otherwise whoever calls picks it.
- [ ] **Liveness:** every table can come home without you (a return action, a scheduled
      closer), and abandoned rounds (a VRF that never answers) can be voided.
- [ ] **Rollup-only data** is read in the rollup (the price feed's Solana copy is stale).
- [ ] **Fees:** a delegated payer has a lamport buffer above rent and the validator's fee
      vault; escrows for return actions are funded and monitored.
- [ ] **Upgrades** keep account layouts compatible, or happen with no tables open.

## Known limitations

From the internal reviews; none breaks the guarantees above.

| Area | Limitation |
| --- | --- |
| Up/Down | Whoever settles still chooses among the prices published in the 2 s after expiry (a few updates) |
| Auction | Bids are public after the reveal; a seller can bid through a second key and decline to reveal (paying the penalty to the other bidders), which acts as a hidden reserve |
| Ledger | `move_seat` accepts an amount of 0; `request_exit` has no undo; a new app's deposit cap is 0 until set; deposit IDs are global across apps |
| Gateway | Registering an intent can store it yet return an error; an unknown app may answer 503 before 404 |
| Deployment | The testnet's three operators run on one host with a dealer-generated key; production needs separate hosts and providers and a distributed key generation |
| Review | The ledger has had internal review only; an external review is planned before mainnet |
