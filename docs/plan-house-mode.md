# Plan: single-player coin flip against the house

Goal: someone with only a Zcash wallet can try ZOL in a few minutes, without a second player,
without SOL and without reading logs, and can see each step happen.

Status: plan, 2026-10-11. Builds on the testnet trial ([ops/testnet-deploy.md](ops/testnet-deploy.md)).

## What the player sees

One page, one button per step, and a timeline that fills in as the steps happen, each with a link:

1. **Deposit.** The page shows a QR code and a "Pay 0.01 TAZ" link (ZIP 321) for the player's
   Zcash wallet. The player enters their own address for winnings and refunds once.
   - Timeline: "payment seen (txid ↗)", then "confirmations 4/30", then "credited: 0.00997 on the ledger (Solana tx ↗)".
2. **Flip against the house.** One button. The house matches the stake, the table moves into
   the rollup, and the VRF picks heads or tails.
   - Timeline: "house joined", "table in the rollup", "coin landed: you won 0.01994 (VRF proof ↗)".
3. **Paid out.** No button. The deposit carried the auto-exit flag, so the balance is
   exited and settled by itself after the game, and the operators pay it on Zcash.
   - Timeline: "settled (Solana tx ↗)", "paid to your wallet (Zcash tx ↗)", and later "final on both chains".

The player never handles SOL, a fee-payer key or a table ID. They keep the session key
backup, which the page already requires before a deposit.

## Design

### The house is a second player, not a new program

The coinflip program seats exactly two players at equal stakes. The house is an operator
service with its own session key and a ledger balance (the **bankroll**), and it takes the
other seat. **No program changes**, so the security argument stays the same: the house is
just a player that the ledger and the VRF treat like any other.

- **Bankroll:** deposits from the funding wallet credited to the house session, using the
  ordinary deposit path. For example 20 × 0.01 TAZ to begin; the house wins and loses at 50/50.
- **The house service** (`zol-house`, one process on the droplet; TypeScript reusing `web/src/chain.ts`):
  - **Opening a game:** creates a table for a player who asks for one, through `POST /house/play {session}`, and pays the Solana fees and rent itself.
  - **Seats:** joins the house seat, then waits for the player to join (their session signs; the house is the fee payer, below).
  - **Start and finish:** runs `start` once both are seated, and `finish`, `cash_out` and `close_table` after the coin lands, so the player is never stuck waiting on a button. The player still sends `flip` themselves; it's free in the rollup.
  - **Payout:** runs `settle` for the player's auto-exited balance.
  - **Limits:** one open game per session, a cap on concurrent games, and no new games once the bankroll falls below N stakes. Each refusal has a plain message.
- **Fairness and trust, stated on the page:**
  - The VRF decides; the house can't pick the outcome.
  - Like any player, the house trusts the rollup validator for the result, and the page links the validator and the VRF proof.

### Fees without SOL: the house is the fee payer

The page builds each Solana transaction with **the house as fee payer**. The player's session
key signs its part, and the page posts the partly signed transaction to
`POST /house/cosign`. The house checks it and only then signs and submits:
- every instruction is one of an allow-list (`coinflip.join`, `ledger.request_exit` and the
  ledger's `settle`);
- the accounts belong to this app and this session;
- the session has no other transaction in flight.

That's the "hosted relayer" from the design, limited to this app. Rollup transactions cost
nothing already. Once this lands, the page's `feePayer` setting becomes a developer option.

### Zcash wallet support

- **Deposit:** a ZIP 321 `zcash:` link and QR code with a ZOL1 text memo (works today). Some wallets
  drop memos, so the page also registers the intent with the operators before showing the link (works today).
- **Payout address:** the player pastes a unified address. The page checks it for an Orchard
  receiver and the right network before signing anything (works today), and explains what to
  do if the wallet only shows a transparent or Sapling address.
- **Wallet matrix (to test):**
  - Zashi/Zodl, Zingo, Zkool and YWallet on the NU7 testnet. For each: does it open the link, keep the memo, pay an Orchard receiver, and show the payout?
  - Results go on the page as "known to work".
- **No wallet:** a "fund me" button sends 0.01 TAZ from the house's funding wallet to the
  player's session address, at most once per session per day. That lets someone try the flow
  without a wallet, as we did for player 2 today.

### Making it quick enough to try

| Step | Today | Plan |
| --- | --- | --- |
| Credit after payment | 30 blocks (about 12 min) | **3 blocks (about 75 s) on testnet**, for 0.01 TAZ deposits only: `depths.early = 3` with `early_credit_cap = 1,000,000` |
| Play | Seven manual steps | One click, with the house handling the rest |
| Payout | Exit and settle by hand | Auto-exit plus the house's `settle`: paid on Zcash about 3–5 min after the coin lands |
| Final on both chains | About 2 h (payout depth) | Unchanged, and shown as "final" later. The player already sees the Zcash payout transaction |

Credit at 3 blocks is a testnet trade-off: the float carries the risk of a reorg undoing a
credited deposit (design: "Reserve accounting"). With a 0.01 TAZ cap per deposit and a
0.05 TAZ float, at most 5 such credits can be pending at once. That is acceptable on testnet,
and the float can be topped up. Mainnet keeps the full depths.

## Tasks

| # | Task | Where | Size |
| --- | --- | --- | --- |
| H0 | **Float accounting:** don't count, as claims, settlements the walked head chain already shows as paid. The head shrinks at payout, but `unpaid_total` drops only at `mark_paid`, about 2 h later, so every payout is counted twice for 2 h and early credits stall. Reorg-consistent: if the payout disappears, the head chain reverts too | `gateway::reconcile::compute_reserve` | S |
| H1 | House session key, bankroll deposits (20 × 0.01 TAZ from the funding wallet) and bankroll monitoring in `zol-status` | ops, `zol-status` | S |
| H2 | `zol-house` service: game queue, create, seat, start, finish, cash out, close and settle; limits; restart-safe state (SQLite) | new `house/` (TS, reuses `web/src/chain.ts`) | L |
| H3 | `POST /house/cosign`: allow-listed fee sponsoring of the player's own transactions | `house/` | M |
| H4 | `/house/*` behind Caddy, with per-IP and per-session rate limits | Caddy | S |
| H5 | Page "vs house" mode: three steps, a timeline with explorer links, the house as fee payer, auto-exit on by default, and an advanced section keeping today's two-player page | `web/` | M |
| H6 | Faster testnet credit: `depths.early = 3`, `early_credit_cap = 1,000,000` | `serve.json` on the droplet | S |
| H7 | "Fund me" demo deposit: one per session per day, from the house funding wallet through `/admin/send` | `house/` | S |
| H8 | Wallet matrix: test the deposit and payout flow with 3–4 wallets on the NU7 testnet; publish the results on the page | manual | M |
| H9 | Tests: house service against the local stack (the zcash-leg harness), cosign allow-list (rejects anything else), page flow in the e2e script | tests | M |
| H10 | Docs: player guide (one page), operator notes for the house (bankroll, limits, refills) | docs | S |

**Order:** H0, H6 and H1 first, since they're quick and make today's page faster. Then H2, H3 and H4.
Then H5 and H7, then H9. H8 can run in parallel once H5 is up. **Estimate:** about 2–3 days of
focused work. H2 and H5 are most of it.

## Open questions

1. **House edge:** none on a fair coin. The operators earn only the ledger's entry and exit
   fees, which are 0.3% each. Fine for a test; a real house would want an edge or a cap.
2. **Bankroll exhaustion:** the house stops offering games below N stakes. Who refills, and from what?
3. **Abuse:** the cosign endpoint and "fund me" are free money and free fees to anyone. On
   testnet, rate limits are enough. Anything public needs sign-up or proof-of-work.
4. **One session per browser:** a player who clears storage without the backup loses access.
   The page already requires the backup; a later version could derive the session from a
   wallet signature instead.
