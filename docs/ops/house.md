# The house: single-player coin flips

The pages (https://zol.cash on the testnet beta): the landing page `/`, the games page `/play.html`
(player key, deposit, withdraw, the games), `/coinflip.html` (`/house.html` redirects there),
`/updown.html`, `/auction.html`; the original two-player page is `/two-player.html`. The house
lets one person play against the house. Design and
reasoning: [plan-house-mode.md](../plan-house-mode.md).

## For players

1. **Back up your session** (one click; the page refuses a deposit until you do).
2. **Deposit 0.01 TAZ**:
   - Enter your Zcash testnet address for winnings, a unified address `utest1…` with an Orchard receiver.
   - Pay the QR code or link from a wallet on the NU7 testnet: exactly 0.01 TAZ. The memo is filled in; if your wallet drops it, the deposit still counts.
   - No wallet? The games page's **Get 0.01 TAZ free** card (shown while the player's balance is 0) registers a 0.01 TAZ deposit and asks the house to pay it from its funding wallet, once per session and once per IP per day. It's a real deposit, so the player's payout address is fixed by it and withdrawals work as usual. Measured 2026-10-12: credited 84 s after the house sent it (1 block). `web/house/faucet-smoke.ts` repeats the measurement.
   - The deposit is credited after 3 Zcash blocks.
3. **Flip the coin.** A stake is 99,700 zat (about 0.001 TAZ), a tenth of a 0.01 TAZ deposit after the entry fee.
   - The first flip opens a **session**: you and the house each put up to 10 stakes on one table, you sign once (no SOL needed), and the table moves into the rollup (about 5–10 s, once).
   - Every flip after that runs in the rollup only: one stake moves from the loser's seat to the winner's, the result shows in about 2–3 s.
   - The table closes (back to Solana, your seat into your balance) when a side runs out of stakes, after 2 minutes without a flip, near the session deadline, or when you withdraw. The rollup does this by itself (a scheduled `crank_close`), and the payout runs on Solana as the table returns (a Magic Action); the house only steps in if those fail.
4. **Withdraw** when you like: the page closes any open table first, then the house co-signs your withdrawal. It reaches your address within minutes (0.3% exit fee and a 0.0002 TAZ network fee; the smallest withdrawal is 0.001 TAZ). "Final" follows about 2 h later.

Every step appears in the timeline:
- **Solana and rollup steps** link to the Solana explorer.
- **Zcash transactions** show their confirmations from the operators' own node. No public Zcash testnet explorer showed our transactions.

### ZEC Up/Down (the second tab)

- Same table, same balance: "Start table" on the Up/Down tab opens an Up/Down table (`create_updown_table`).
- **Bet** Up or Down, one stake (0.001 TAZ), at the ZEC/USD price the rollup has (MagicBlock's Pyth Lazer feed, id 66). After 60 s it settles at a price published after expiry: a right call pays 1.9× (0.9 stake from the house's seat), a tie refunds. The page settles at expiry; the rollup's closer settles it too if the page is gone.
- The settling price must be published within 2 s after expiry (`SETTLE_PUBLISH_WINDOW_SECS`), and the call made within 10 s; otherwise the bet is void (nothing moves). The oracle keeps only its latest price, so whoever settles picks the moment: the window bounds that choice to a few price updates. The page and the house settle at expiry, the rollup's closer within ~5 s. The template this follows (MagicBlock's binary-prediction) settles at whatever price is current whenever someone calls it (review finding F4).

### Sealed-bid auction (`/auction.html`)

- The house sells a numbered certificate; anyone with a balance bids. Same player and balance as the game page.
- **Register** (45 s): pay the deposit, 0.005 TAZ (also the most you can bid), into the auction's pot; the house co-signs and pays the fee. At most 8 bidders.
- **Seal a bid** (45 s, in the rollup): only `sha256("ZOL/bid/v1" ‖ auction ‖ bidder ‖ amount ‖ salt)` goes on-chain; the salt comes from your player key, the amount is remembered by your browser.
- **Reveal** (45 s): the page reveals your bid; a sealed bid never revealed loses 10% of the deposit, shared by those who revealed.
- **Result:** the highest revealed bid pays its bid to the seller (first price; ties go to the earlier registration). The rollup ends the auction (`schedule_auction_end`) and the pot's return pays every seat (`cash_out_auction`); the auction account stays on Solana as the certificate. Nobody registered: cancelled, the ledger refunds the seller's 1-zat stake.
- Operators: `GET /house/auction`, `POST /house/auction/open`; the desk is `web/house/auction.ts`. The ledger's `max_seats` is 16 on devnet (`devnet set-max-seats`).

## For operators

- **Service:** `zol-house` is a Bun binary built from `web/house/server.ts`:
  ```sh
  cd web && bun build --compile --target=bun-linux-x64 house/server.ts --outfile dist-house/zol-house
  ```
  - It runs under the systemd unit `zol-house`, with its config at `/home/zol/ops/house/house.json`.
  - It listens on 127.0.0.1:8790; Caddy proxies `/house/*` to it.
  - Its state is SQLite (`house.db`): games, plus rate-limit counters.
- **Keys:**
  - **House session seed** (`house-session.hex`): the house's player key. Its ledger balance is the **bankroll**.
  - **House fee payer** (`house-fee-payer.json`): about 1.5 SOL on devnet. It pays every Solana transaction of a game, including table rent, which `close_table` returns.
- **Bankroll:** ordinary deposits credited to the house session, without auto-exit:
  ```sh
  cd web && npx vite-node house/bankroll.ts -- <count>   # SSH tunnel to op0's zol-zcashd needed
  ```
  - A session takes up to `maxRounds` stakes (default 10, 997,000 zat) from it while the table is open.
  - `zol-status` warns when the bankroll is below one stake.
  - The house wins about half its games, so the bankroll drifts.
- **Sessions** (`house.json`, all optional):
  - `stake` zat per flip, default the coinflip's smallest allowed stake (a tenth of the smallest deposit's net value);
  - `maxRounds` 10 stakes per seat; `idleSecs` 180 before the house closes an idle table (the rollup's closer does it at 120); `sessionSecs` 1800, the ledger session deadline (closed 5 minutes before).
- **Limits** (`house.json`):
  - `maxOpenGames` 2 (open sessions);
  - `playsPerIpPerHour` 10;
  - fund-me: `fundPerDay` 10 (3 before 2026-10-12), `fundPerIpPerDay` 1, at `fundZat` 1,000,000 each. Each grant spends 0.01 TAZ plus fees from op0's funding wallet: keep it topped up.
- **How a session runs:** the house builds one transaction that creates the table, seats both the house and the player with the same number of stakes (`join_rounds`) and starts it (pot and table into the rollup). It is 1,155 bytes, so it fits; the house falls back to a separate `start` if it ever does not.
  - The player signs it; the house checks it's byte-for-byte the transaction it built, adds its signatures and sends it.
  - Either all of it happens or nothing does, so the house never strands an open table.
  - The page flips in the rollup and watches the table there. Withdrawals go through `/house/stop` first.
  - **Closing and paying need no keeper** (verified on devnet 2026-10-12):
    - the house schedules `crank_close` once per table (`schedule_closer`); the rollup runs it every ~5 s and closes a table idle for 120 s, with a short seat, or near its deadline. The table pays its own commits (with the validator's magic fee vault), from a 0.001 SOL buffer `start` puts in it;
    - every close attaches `cash_out_table` to the **pot's** return through the ledger's `close_pot` (a Magic Action, paid from the rent vault's escrow `6AD3QX…`, which the house tops up from its fee payer). It pays both seats the moment the pot is back and closes the table if it is back too;
    - the house is the fallback: it closes an idle table at 180 s with `finish_and_pay` (plain `finish` if refused) and pays with `cash_out_table` if the action did not.
- **A game stuck** (in `failed`): the timeline says so. Use the two-player page's **Rescue stuck table** or the ledger's refund path. The pot is refunded by the ledger after the session deadline and grace.

## Lessons from bringing it up (2026-10-11)

- **The payout double count froze the app.** A payout already mined but not yet `mark_paid` was counted twice, as gone from the head and still as a claim. That made the float look short, and a guardian froze the app at 06:09 UTC (`EarlyOverFloat`).
  - **Fixed** in `ffac5b5`.
  - **Recovery:** two guardians unfroze it (`zol-devnet unfreeze`).
  - **Monitoring gap:** `zol-status` now reads the app's state from the ledger. Its log-based check had stopped showing the freeze after 15 minutes.
- **Helius devnet returned stale account data:** about 40% of `getAccountInfo` answers were 70+ minutes old, at a *current* context slot. So the slot-freshness check (`MAX_SLOT_LAG`, `5841a30`) can't catch it. The gateways and the house went back to the public devnet RPC, with retries and 15 s polling. The page calls the public RPC from each browser.
- **Zcash testnet mined only empty blocks,** every ~6 s from about 07:56 UTC. No transaction confirmed: a deposit expired unmined after its 40-block expiry (4 minutes at that pace). `expiry_delta` is now 300 for payouts and the daemons' own sends. Wait for blocks that carry transactions before testing.

## First games against the house (2026-10-11, `web/house/house-smoke.ts`)

| Game | Deposit credited | Seated → landed | Result | Payout |
| --- | --- | --- | --- | --- |
| 1 | 223 s (3 blocks) | 33 s: co-signed create+join, start, rollup, player flip, VRF | the house won; pot to the house, table closed | none (nothing to pay) |
| 2 | 415 s | 12 s | the player won; auto-exit, so the house settled it as settlement 3, 37 s after the win | mined on Zcash in block 4,488,262, 2 min 48 s after the settle; **11.0 min from deposit to coins in the wallet** |

Rate limiting on the shared IP showed up as retries in game 1's timeline. Retries are now silent, don't count toward giving up, and are hidden from the page (`bced3cb`).
