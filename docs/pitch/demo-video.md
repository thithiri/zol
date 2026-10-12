# Demo video (up to 3:00)

A screen recording of the live testnet beta at https://zol.cash, with voice-over. The story:
a new player with no wallet gets free test ZEC, plays in real time on Solana, and withdraws
back to Zcash, shielded. About 330 words of voice-over over roughly 2:45 of footage.

Record each shot as its own clip. Waits (a Zcash block, a payout) are cut, and the voice-over
says so ("a minute later…"), so nothing is sped up or faked.

## Before recording

**Ask Claude to run the pre-flight** the same day. It checks and fixes:

- the operators, the house and the bankroll are healthy (`zol-status`), and no tables are open;
- the faucet: grants left today, and your address's daily limit cleared;
- the funding wallet can pay a grant, and no older withdrawal is still waiting to be paid.

**Your setup:**

- A desktop browser at 1920×1080, a clean profile (no bookmarks bar or extensions), zoom about 125%.
- Start the player with a **passkey** (Touch ID or Face ID): no seed phrase, and no "save your
  key" step before the faucet. A fresh session key also works, but the page then asks you to
  save it first.
- Decide the payout address:
  - **Your own Zcash testnet wallet:** strongest ending, the ZEC arrives in a wallet on camera.
  - **The house's address** (the faucet's checkbox): no wallet needed; the withdrawal is shown
    paid in the page's timeline, and the money goes back to the house.

## Shot list

| # | On screen | Do | Target | Voice-over |
| --- | --- | --- | --- | --- |
| 1 | zol.cash | Open the landing page; scroll once to "How it works" | 0:10 | This is ZOL: programmable Zcash privacy on Solana. Everything you'll see is live on testnet. |
| 2 | Games page | Click **Play on testnet**; **Start with a passkey**; confirm with Touch ID | 0:15 | A new player starts with a passkey. No seed phrase, no Solana wallet, and no SOL needed. |
| 3 | Faucet card | Enter your Zcash address, or tick **Use the house's address** so the deposit QR appears; click **Get 0.01 TAZ free** | 0:15 | No test ZEC yet? The house sends a real shielded deposit to the player, through the same vault anyone pays into. |
| 4 | Balance | **Cut** to the timeline saying credited and the balance at 0.00997 TAZ | 0:10 | A Zcash block later, the operators credit it on the Solana ledger, less a small fee. |
| 5 | Coin flip | Open **Coin flip**; **Start table**; wait for "flip away" | 0:15 | The coin flip opens a table and moves it into a MagicBlock rollup, a fast Solana runtime. |
| 6 | Coin flip | Pick **Heads**, flip four or five times in a row | 0:25 | Every flip is decided by verifiable randomness in the rollup. Neither the player nor the house can pick it, and each flip lands in about a second. |
| 7 | Up/Down | Open **ZEC Up/Down** (close the coin-flip table when asked); **Start table**; call **Up**; **cut** to the result | 0:25 | ZEC Up/Down reads a live price inside the rollup. Call it, and thirty seconds later the price settles the bet on its own. |
| 8 | Games page | Show the three games list briefly | 0:08 | The same Solana program also runs a sealed-bid auction, with hidden bids and no trusted hardware. |
| 9 | Withdraw | **Withdraw Zcash**: the panel shows the fixed payout address and the fees; click **Withdraw** | 0:15 | Cashing out: the payout goes only to the address fixed at the first deposit, and every operator checks it before signing. |
| 10 | Timeline | **Cut** to "paid on Zcash", or to the wallet showing the incoming shielded transaction | 0:12 | A few minutes later, the operators co-sign the payment, and it lands back on Zcash, shielded. |
| 11 | Explorer | Open the settlement's Solana transaction from the timeline | 0:12 | On Solana you'd see a session key and an amount. On Zcash, only a shielded transaction. Nothing public links the two. |
| 12 | Docs | Open zol.cash/docs; scroll the contents | 0:08 | Every app here is just a Solana program, and the docs show how to build your own. |
| 13 | End card | zol.cash, logo | 0:05 | ZOL. Build anything with Zcash privacy on Solana. |

Total target: about 2:55.

## Notes

- **Shot 4 and shot 10 are cuts across real waits** (one Zcash block; a payout of a few
  minutes). Say "a block later" and "a few minutes later" as written; don't speed the footage
  up to look instant.
- **Shot 9** says what is enforced: the ledger fixes the address and each operator checks it.
  It doesn't claim more; two colluding operators could still sign elsewhere, which the deck's
  guarantees slide covers if a judge asks.
- **If something stalls** (an RPC hiccup, a slow block), stop and retake that shot; every shot
  stands alone.
- Up/Down needs the coin-flip table closed first: the page offers a **Close your coin-flip
  table** button; closing takes a few seconds, so cut it.
