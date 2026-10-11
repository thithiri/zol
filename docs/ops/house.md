# The house: single-player coin flips

The page `house.html` (https://159-223-38-126.sslip.io/house.html on the testnet trial)
lets one person play against the house. The two-player page `/` is unchanged. Design and
reasoning: [plan-house-mode.md](../plan-house-mode.md).

## For players

1. **Back up your session** (one click; the page refuses a deposit until you do).
2. **Deposit 0.01 TAZ**:
   - Enter your Zcash testnet address for winnings, a unified address `utest1…` with an Orchard receiver.
   - Pay the QR code or link from a wallet on the NU7 testnet: exactly 0.01 TAZ. The memo is filled in; if your wallet drops it, the deposit still counts.
   - No wallet? **Fund me** pays 0.01 TAZ from the house's funding wallet, once per session and once per IP per day.
   - The deposit is credited after 3 Zcash blocks.
3. **Flip the coin.** A stake is 99,700 zat (about 0.001 TAZ), a tenth of a 0.01 TAZ deposit after the entry fee.
   - The first flip opens a **session**: you and the house each put up to 10 stakes on one table, you sign once (no SOL needed), and the table moves into the rollup (about 5–10 s, once).
   - Every flip after that runs in the rollup only: one stake moves from the loser's seat to the winner's, the result shows in about 2–3 s.
   - The house closes the table (back to Solana, your seat into your balance) when a side runs out of stakes, after 2 minutes without a flip, near the session deadline, or when you withdraw.
4. **Withdraw** when you like: the page closes any open table first, then the house co-signs your withdrawal. It reaches your address within minutes (0.3% exit fee and a 0.0002 TAZ network fee; the smallest withdrawal is 0.001 TAZ). "Final" follows about 2 h later.

Every step appears in the timeline:
- **Solana and rollup steps** link to the Solana explorer.
- **Zcash transactions** show their confirmations from the operators' own node. No public Zcash testnet explorer showed our transactions.

## For operators

- **Service:** `zol-house` is a Bun binary built from `web/house/server.ts`:
  ```sh
  cd web && bun build --compile --target=bun-linux-x64 house/server.ts --outfile zol-house
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
  - `maxRounds` 10 stakes per seat; `idleSecs` 120 before an idle table is closed; `sessionSecs` 1800, the ledger session deadline (closed 5 minutes before).
- **Limits** (`house.json`):
  - `maxOpenGames` 2 (open sessions);
  - `playsPerIpPerHour` 10;
  - fund-me: `fundPerDay` 3, `fundPerIpPerDay` 1, at `fundZat` 1,000,000 each.
- **How a session runs:** the house builds one transaction that creates the table, seats both the house and the player with the same number of stakes (`join_rounds`) and starts it (pot and table into the rollup). It is 1,155 bytes, so it fits; the house falls back to a separate `start` if it ever does not.
  - The player signs it; the house checks it's byte-for-byte the transaction it built, adds its signatures and sends it.
  - Either all of it happens or nothing does, so the house never strands an open table.
  - The page flips in the rollup and watches the table there. The house watches too and closes the table (finish; then cash out and close in one Solana transaction). Withdrawals go through `/house/stop` first.
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
