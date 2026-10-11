# Example apps

The testnet's games are one app program, [`zol-coinflip`](../programs/zol-coinflip/src/lib.rs),
so players use one balance for all of them. Each game shows a different way to decide an
outcome inside the rollup. [Build an app](build-an-app.md) walks through the coin flip's
table life cycle, which the other two reuse.

| Game | Decides with | Page | Shows |
| --- | --- | --- | --- |
| Coin flip | MagicBlock VRF | [`/coinflip.html`](https://159-223-38-126.sslip.io/coinflip.html) | Sessions, verifiable randomness, keeper-free closing |
| ZEC Up/Down | MagicBlock's price oracle (Pyth) | [`/updown.html`](https://159-223-38-126.sslip.io/updown.html) | Reading a live feed in the rollup, time-bounded settlement |
| Sealed-bid auction | Commit, then reveal | [`/auction.html`](https://159-223-38-126.sslip.io/auction.html) | Many players in one pot, hidden bids without a TEE, a certificate on Solana |

## Coin flip

- **Table:** the player and the house each buy in up to 10 stakes (`join_rounds`); one stake
  is a tenth of a 0.01 TAZ deposit after the entry fee (99,700 zat).
- **Flip:** a seated player calls `flip` in the rollup, which requests randomness from
  MagicBlock's VRF with the pinned oracle queue. The oracle calls back `land`, which moves one
  stake from the loser's seat to the winner's and records the result.
- **Repeat** while both seats can cover a stake. The table keeps the last 64 results.
- **Close:** the house, a player, or the rollup's scheduled `crank_close` (idle 120 s, a short
  seat, the deadline near) ends it; the pot's return action pays both seats.

Trust: the VRF oracle and the rollup validator decide; neither player nor house can pick the
outcome. A flip the VRF never answers can be abandoned after 60 s (stakes stay put).

## ZEC Up/Down

Adapted from MagicBlock's
[binary-prediction](https://github.com/magicblock-labs/magicblock-engine-examples/tree/main/binary-prediction/anchor)
template, with the ledger instead of SPL token custody.

- **Table:** `create_updown_table`, otherwise the coin flip's table. The house's seat is the
  counterparty and must cover a win.
- **Bet:** `bet(up)` by a seated player, in the rollup. It reads ZEC/USD from MagicBlock's
  oracle account (address, owner, feed id, full verification, at most 5 s old), records the
  entry price and an expiry 30 s ahead, and requires the house's seat to hold 0.9 stake.
- **Settle:** `settle_bet`, by anyone, with a price **published within 2 s after expiry**
  (`SETTLE_PUBLISH_WINDOW_SECS`). A right call is paid 0.9 stake by the house (1.9× in all), a
  wrong one pays its stake to the house, an unchanged price moves nothing. A later price, or no
  call within 10 s, voids the bet.
- **Who settles:** the player's page and the house at expiry; `crank_close` every ~5 s as a
  backstop (it settles a bet before it ever closes a table).

Why the window: the oracle account holds only its latest price, so whoever settles picks the
moment. The template settles at whatever price is current whenever someone calls it, which
lets a player wait for a good price; the 2 s window limits that to a few price updates (review
finding F4 in [`../docs/review/close-pot-actions.md`](../docs/review/close-pot-actions.md)).

```rust
// settle_open_bet (abridged)
let (price, published) = read_price(oracle, now)?;
if published < bet.expiry { return err!(PriceNotYet); }                         // too early: retry
if published > bet.expiry + SETTLE_PUBLISH_WINDOW_SECS { /* void */ return Ok(()); }
let (from, to, amount) = if price == bet.open_price { /* tie */ }
    else if (price > bet.open_price) == bet.up { (house_seat, bettor_seat, updown_win(stake)) }
    else { (bettor_seat, house_seat, stake) };
zol_ledger::cpi::move_seat(signed(..), from, to, amount)?;
```

## Sealed-bid auction

Commit-reveal on a ledger pot. MagicBlock's
[sealed-auction](https://github.com/magicblock-labs/magicblock-engine-examples/tree/main/sealed-auction)
template hides bids in a Private Ephemeral Rollup (a TEE); this version needs no new trust,
at the cost of publishing all bids at the reveal.

| Phase | Where | What happens |
| --- | --- | --- |
| Open | Solana | `create_auction`: the seller (seat 0, a 1-zat stake) opens a pot and an `Auction` with a deposit D (the bid cap) and absolute deadlines |
| Register (45 s) | Solana | `register_bidder`: each bidder pays D into the pot. Equal deposits reveal nothing. At most 8 bidders |
| Start | Solana | `start_auction` at the register deadline: pot and auction into the rollup |
| Bid (45 s) | rollup | `commit_bid(sha256("ZOL/bid/v1" ‖ auction ‖ bidder ‖ amount ‖ salt))`; replaceable until the deadline |
| Reveal (45 s) | rollup | `reveal_bid(amount, salt)`, checked against the commitment, amount ≤ D |
| End | rollup | `end_auction` (anyone, or the scheduled task): the highest revealed bid wins (ties: earlier registration) and pays its bid to the seller; each sealed bid never revealed pays D/10 to those who revealed; the pot returns with `cash_out_auction` attached |
| Certificate | Solana | The `Auction` account stays, with the winner and the price |

The salt is `sha256("ZOL/bid-salt/v1" ‖ session seed ‖ auction)`, so a page can always
recompute it from the player's key; only the amount must be remembered (the page stores it).
`bid_commitment` has a test vector shared by the Rust and TypeScript implementations.

Why the penalty: without one, a bidder could watch others reveal and then decide whether to
reveal. It goes to the bidders who revealed, not the seller, so a seller bidding through a
second key cannot use a non-reveal as a free cancel.

## The house

The games' counterparty is an ordinary player: a session key with a ledger balance (the
bankroll), run by a service ([`../web/house/`](../web/house/)). It builds the transactions
players sign (and pays their fees), seats itself, opens auctions, settles bets, and closes
tables as a fallback. To the ledger and the VRF it is a player like any other; the ledger's
guarantees do not depend on it.
