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
3. **Flip the coin.** One button:
   - The house matches your stake, and you sign your seat (no SOL needed).
   - The table moves into the rollup, and the page flips the coin for you. If the page is closed, the house flips after 60 s. The VRF decides either way.
4. **Paid out automatically.** If you win, about 0.0197 TAZ (1,994,000 zat less the 0.3% exit fee and the network allowance) goes to your address within minutes. "Final" follows about 2 h later.

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
  - One game needs one stake (997,000 zat).
  - `zol-status` warns when the bankroll is below one stake.
  - The house wins about half its games, so the bankroll drifts.
- **Limits** (`house.json`):
  - `maxOpenGames` 2;
  - `playsPerIpPerHour` 10;
  - fund-me: `fundPerDay` 3, `fundPerIpPerDay` 1, at `fundZat` 1,000,000 each.
- **How a game runs:** the house builds one transaction that creates the table and seats both the house and the player.
  - The player signs it; the house checks it's byte-for-byte the transaction it built, adds its signatures and sends it.
  - Either both are seated or nothing happens, so the house never strands an open table.
  - From there the house drives every step: start, flip if needed, finish, cash out, close, settle. It then follows the Zcash payout to the end.
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
