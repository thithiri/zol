# Quickstart

About 15 minutes, on the testnet beta: create a player key, deposit shielded testnet ZEC
(TAZ), flip coins against the house in the MagicBlock rollup, and withdraw back to your
Zcash wallet. Everything runs from one script, [`../web/examples/quickstart.ts`](../web/examples/quickstart.ts),
using the TypeScript client in [`../web/src/`](../web/src/).

Prefer clicking? The same flow is on the pages at
<https://159-223-38-126.sslip.io/play.html>.

## You need

- **Node.js 22** and this repository.
- **A Zcash testnet wallet on the NU7 chain** with at least 0.0102 TAZ (0.01 to deposit plus
  the wallet's fee), and its **unified address** (`utest1…`) with an Orchard receiver.
  [Valar's faucet](https://faucet.testnet.valargroup.dev) pays 0.125 TAZ a day and is on the
  right chain (many older faucets are not: check that a faucet's height matches the current
  testnet).

```bash
cd web
npm install
```

## 1. A player key

```bash
npx vite-node examples/quickstart.ts new
```

This creates a random **session key** and saves its seed in `zol-session.json` (mode 600).
The key is your identity in the app: it signs your deposit memo and every action on your
balance. Keep the file; whoever holds it holds your balance.

## 2. Deposit

```bash
npx vite-node examples/quickstart.ts deposit utest1…your-address…
```

The script:

1. reads the app from an operator (`GET /v1/apps/{app}`: network, vault, denominations, tip),
2. builds a **memo** naming your key and your payout address, signed by your key,
3. registers it with **all three operators** (`POST /v1/apps/{app}/intents`), which return your
   **session address**, a receiver of the app's vault unique to you,
4. prints a ZIP 321 link:

```text
pay exactly 0.01 TAZ with this link (any Zcash testnet wallet):

zcash:utest1s4c7…?amount=0.01&memo=Wk9MMTpX…

session address: utest1s4c7…
registered with 3 of 3 operators
```

Open the link in your wallet (or paste the address, amount and memo) and send. The address
you passed is now fixed as your **payout address** for this app.

## 3. Wait for the credit

```bash
npx vite-node examples/quickstart.ts status
```

```text
deposit 0.01 TAZ: seen, 2 confirmations
balance: none yet
```

After 3 Zcash blocks (about 75 seconds) two operators attest the deposit and it is credited:

```text
deposit 0.01 TAZ: credited, 3 confirmations
balance: 0.00997 TAZ
```

The 0.3% entry fee is gone; the rest is your **balance** on the ledger.

## 4. Play in the rollup

```bash
npx vite-node examples/quickstart.ts flip 3
```

```text
flip 1: you won
flip 2: the house won
flip 3: you won
table closed; balance 0.010967 TAZ
```

What happened:

1. **One Solana transaction** (built by the house, signed by you, co-signed and paid by the
   house) created a table, seated you and the house with up to 10 stakes each
   (`join_rounds`), and moved the pot and the table into the MagicBlock rollup.
2. **Each flip** was a free rollup transaction signed by your key. MagicBlock's VRF picked the
   winner in under a second, and the program moved one stake (0.000997 TAZ) between seats.
3. **Closing:** the table returned to Solana, and its return action paid your seat into your
   balance.

## 5. Withdraw

```bash
npx vite-node examples/quickstart.ts withdraw
```

Your key signs `request_exit` and `settle`; the operators then pay your balance, less the
0.3% exit fee and a 0.0002 TAZ network fee, to your payout address in a single shielded
transaction (a few minutes). Check your wallet.

## What to read next

- [Concepts](concepts.md): what each step did, in ZOL's terms.
- [Build an app](build-an-app.md): the program behind the coin flip.
- [Gateway HTTP API](reference/gateway-api.md) and [TypeScript client](reference/typescript-client.md).
