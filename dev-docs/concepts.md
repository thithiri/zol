# Concepts

## Apps and vaults

An **app** is a Solana program registered with the ledger (`register_app`). Each app has its
own Zcash **vault**: a shielded address whose spend key is split among the operators (FROST,
2 of 3). Keys for every app's vault come from one key generation, so a new app needs no new
ceremony; each app has its own viewing key, so one app's deposits are invisible to another's.

An app starts `Pending`. The ledger's admin activates it with its vault address and a
**genesis** note (the vault's first "head" note, mined on Zcash), after which it waits an
**activation delay** (1 hour) before taking deposits. A **deposit cap** bounds what the app
can hold.

## Session keys

A player is an **Ed25519 session key**, one per app. It signs the deposit memo and every
ledger action that touches the player's balance. It needs no SOL: the app (or a sponsor) pays
Solana fees, and rollup transactions are free.

Where the key lives is up to the page. The testnet pages derive it from a **passkey**
(WebAuthn PRF: same passkey, same key, on every device it syncs to) or keep a random **session
key** in the browser, which the player must save. See
[TypeScript client: session and passkey](reference/typescript-client.md).

## Deposits

1. The page builds a **memo** naming the session key, the player's **payout address** (a
   Zcash unified address), the vault, the network, an expiry height and flags, **signed by the
   session key**.
2. It **registers the intent** with every operator (`POST /v1/apps/{app}/intents`) and gets
   back a **session address**: a diversified receiver of the vault, unique to the session.
3. The player pays a **fixed amount** (the app's denominations: 0.01 or 0.1 TAZ on testnet) to
   that address from any Zcash wallet, through a ZIP 321 link that carries the memo as text
   (`ZOL1:…`). If the wallet drops the memo, the deposit still counts: the session address and
   the registered intent identify it.
4. Each operator sees the note in its own scan. Once it has enough confirmations, 2 of 3
   operators sign a **credit**, and a relayer submits it to the ledger, which adds the amount
   (less a 0.3% entry fee) to the player's **balance**.

**Confirmation depth:** testnet credits early, after 3 blocks (about 75 s), within a float the
operators keep; deeper credits wait for full depth (60 blocks). A deposit that is never
credited is **voided** and refunded to the payout address.

The **payout address is fixed at the first deposit**. The ledger stores only a keyed
commitment to it; the operators check every payout against it.

## Balances

A **balance** is per app and per session key: an amount, an `exiting` flag and an
`auto_exit` flag. Balances are not tokens: they cannot be transferred. They move only into
pots (through the app, with the player's signature), back from pots (cash-out), and out to
Zcash (exit).

## Pots, seats and receipts

A **pot** is where an app holds players' stakes during a session.

- `open_pot` (by the app) creates the pot and its **receipt**.
- `buy_in` (the player's session key **and** the app sign) moves an amount from the player's
  balance into a **seat**. Seats are indexes; the receipt records which player holds each seat
  and what they bought in.
- `leave_pot` lets a player take their buy-in back while the pot is still open.

The **receipt never leaves Solana**: it is the authoritative list of who is owed what. Only
the pot (an array of seat amounts) goes into the rollup. Whatever happens there, money can
only move between that pot's seats, and cash-out pays `amounts[i]` to `receipt.players[i]`.

Limits on the testnet: up to 16 seats per pot (a ledger setting), a pot open at most 1 hour
before it starts, a session of 1 to 120 minutes.

## The rollup session

- `delegate_pot` (through the app) hands the pot to a **MagicBlock validator** the app names,
  with a **session deadline**. The app delegates its own state (a table) alongside.
- In the rollup the app decides outcomes and calls `move_seat(from, to, amount)`. Rollup
  transactions are free and land in milliseconds. Randomness comes from MagicBlock's **VRF**;
  prices from its **oracle**; timed work from **scheduled tasks** (cranks).
- `close_pot` commits the pot back to Solana. It can attach a **return action**: an
  instruction of the app that the delegation program runs on Solana the moment the pot is
  back (e.g. to cash out and close the table, so no keeper is needed).
- `cash_out` (anyone) checks the returned pot (seat count, sum) and pays every seat into its
  player's balance.
- If the pot never returns validly, `refund_pot` (anyone) returns every buy-in after the
  session deadline plus a grace period (65 minutes).

## Exits and payouts

- `request_exit` (the player's key) marks a balance for payout; `settle` (anyone) gathers
  exiting balances into a numbered **settlement**.
- The operators build one Zcash transaction per settlement that spends the vault's **head**
  note, pays every payout to its committed address (less a 0.3% exit fee and a network-fee
  allowance), and makes a new head. Every payout spends the head, so two signer quorums can
  never both pay a settlement.
- Once the payout is deep enough on Zcash, the operators mark the settlement **paid** on the
  ledger.

**Smallest withdrawal:** 0.0005 TAZ on testnet (`min_exit`).

## Who pays what

| Cost | Paid by |
| --- | --- |
| Zcash deposit fee | The player's wallet |
| Entry fee (0.3%) and exit fee (0.3%) | Deducted from the amounts; kept by the app's fee pool |
| Zcash payout network fee | An allowance per payout (20,000 zat on testnet) |
| Solana fees and rent for app actions | The app or its service (a "house"), as fee payer |
| Rollup transactions | Free |
| A return action's Solana fee | An escrow the app funds with MagicBlock's delegation program |

## Guardians

Each operator also runs a **guardian**: it reconciles the ledger against the vault and can
**freeze** an app on a mismatch (one guardian freezes; a threshold unfreezes). A frozen app
takes no new value in but still lets players out. Separately, every operator **pins the
ledger's code** and signs nothing if the deployed program differs.
