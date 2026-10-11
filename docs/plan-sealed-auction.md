# Plan: a sealed-bid auction paid in shielded ZEC, from MagicBlock's Sealed-Bid Auction template

Status: built 2026-10-12 as **commit-reveal** (no TEE), first price, a certificate lot, deposit 0.005 TAZ, 10% non-reveal penalty to those who revealed, 5/5/5-minute phases, up to 8 bidders ([ops/house.md](ops/house.md)). The TEE design below is kept for reference. Template:
[sealed-auction](https://github.com/magicblock-labs/magicblock-engine-examples/tree/main/sealed-auction)
([docs](https://docs.magicblock.gg/pages/templates/sealed-bid-auction)), on a Private Ephemeral
Rollup ([PER quickstart](https://docs.magicblock.gg/pages/private-ephemeral-rollups-pers/how-to-guide/quickstart)).

Goal: a seller auctions a lot; bidders pay with their ZOL balance (shielded ZEC in, shielded ZEC
out). Bids stay hidden until the auction ends, losing bids are never published, and nobody can
link a bidder's session key to their Zcash wallet.

## What the template does

| Step | Template |
| --- | --- |
| `initialize_auction` | the seller's lot (Token A) in an auction-owned token account on Solana; a Token B escrow delegated to the rollup; lamports to sponsor bid accounts |
| `delegate_auction`, `init_auction_permission` | the auction state (counts only, no amounts or bidders) moves to the private rollup, public |
| `place_bid` | creates a rollup-only `Bid` account (sponsored by the auction) and moves Token B from the bidder's rollup balance into the auction escrow, fully collateralised |
| `init_bid_permission` | makes that `Bid` private: members **the auctioneer and the bidder** |
| `end_auction` | after the deadline, scans exactly `bid_count` bids **inside the private rollup** and records the highest (first price) |
| `settle_winning_bid`, `claim_refund` | pays the seller from escrow, refunds losers, closes bid accounts |
| `undelegate_auction`, `finalize` | back to Solana; the lot goes to the winner |
| Limits | `MAX_BIDDERS` 5; the seller can read every bid (it is a member) |

## How it maps onto ZOL

| Template | ZOL |
| --- | --- |
| Token B balances on the rollup | **a ledger pot**: each bidder buys in a **fixed deposit D** (the auction's bid cap) on Solana. Equal deposits reveal nothing about bids |
| Auction-owned Token B escrow | the pot's seats; the seller holds a seat too (a minimal buy-in) to receive the price |
| `Bid` amounts moved into escrow | the bid amount is only recorded in a private `Bid` account; money moves once, at the end |
| `settle_winning_bid`, `claim_refund` | one `move_seat`: winner's seat → seller's seat, the price. Then the pot returns and the **pot's return action** pays everyone: seller the price, winner D − price, losers D (keeper-free, as the coin flip) |
| Bid permission members: auctioneer + bidder | **the bidder only** (the program reads bids inside the rollup without being a member), so the seller cannot see bids and shill-bid against them. To verify in P0 |
| `MAX_BIDDERS` 5 | up to 15 bidders (pot seats; the ledger's `max_seats` raised from 8 to 16, a setting) |
| First price | first price, or **second price** (Vickrey: the winner pays the second-highest bid), which the program can compute as it sees all bids |
| SPL lot | an SPL token or NFT escrowed by the auction on Solana, released to an address the winner names; or, for a first demo, a lot the house sells |

Same app program (`7G2o…`), so bidders use the balance they already have.

What appears on-chain: everyone's deposit D (equal), the winner's seat change (the price,
which a sealed-bid auction publishes anyway), and session keys. Losing bids never leave the
private rollup.

## Flow

1. **Open** (Solana): the seller creates the auction (lot escrowed, bid cap D, deadline,
   first or second price) and its pot.
2. **Register** (Solana, until bidding starts): each bidder buys in D, one co-signed
   transaction like opening a table. The seller buys in a minimal seat.
3. **Start** (Solana): pot and auction delegated to the private rollup's validator; a task is
   scheduled for the deadline.
4. **Bid** (private rollup): each registered bidder places or changes a private bid ≤ D. Their
   page reads it back with a token signed by their session key.
5. **End** (private rollup, the scheduled task at the deadline): scans the bids, picks the
   winner and price, `move_seat` winner → seller, closes the bid accounts, finishes with the
   payout action attached to the pot's return.
6. **Settle** (Solana): the action pays every seat; `finalize` sends the lot to the winner's
   named address. No bids → the lot returns to the seller, deposits refunded.

## Do we need the TEE?

| Option | Hides bids from | Trust | Fit |
| --- | --- | --- | --- |
| **Private rollup** (the template) | everyone, including the operators and, with the membership change, the seller; losing bids never revealed | Intel TDX hardware and MagicBlock's TEE validator; its ingress enforces IP geofencing and sanctions lists | this plan |
| Commit, then reveal | everyone until the reveal; all bids public after | none new | fallback if P0 fails (+2 d): hash commitments in a public rollup, a reveal window, a penalty for not revealing |
| Bids encrypted to the ZOL operators (2 of 3) | everyone and any single operator | the same 2-of-3 as deposits | later; needs threshold decryption (1–2 weeks) |

The page states which one is in use and what it trusts.

## Phases

| # | Task | Size |
| --- | --- | --- |
| P0 | **Private rollup spike** on `devnet-tee.magicblock.app` (validator `MTEWGu…`, permission program `ACLseo…`): delegate a ledger pot and an app account there; a private account with **only the bidder** as member, read by the program in `end`; a page token from a ZOL session key (ed25519); the scheduled task and the pot's return action from the TEE validator | 1–1.5 d |
| P1 | Program: auction, register (pot + deposit), start, private bid, end (first/second price), the lot escrow and `finalize`, no-bid path; LiteSVM tests for everything but privacy | 2.5–3 d |
| P2 | House: run demo auctions as seller (a devnet NFT or SPL lot), schedule ends, fallbacks; raise `max_seats` to 16 | 1 d |
| P3 | Page `auction.html`: lot, countdown, register, private bid, "only you can see this bid", result and payout | 2 d |
| P4 | Devnet run with two or three bidders, docs | 0.5 d |

About 7–8 days; +2 days for the commit-reveal fallback if P0 fails.

## Decisions

1. First price (template) or second price?
2. The lot for the demo: a devnet NFT the house sells, an SPL token, or something else?
3. Bid cap D (the deposit): e.g. 0.01 TAZ?
4. Private rollup (TEE) first, accepting its trust and ingress rules, or commit-reveal first?
