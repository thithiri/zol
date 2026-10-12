# FAQ, glossary and troubleshooting

## FAQ

**Does ZEC move to Solana?** No. ZEC stays in the app's shielded vault on Zcash. The ledger on
Solana records who is owed what; payouts are Zcash transactions from the vault.

**Is there a wrapped ZEC token?** No. Balances are non-transferable ledger entries.

**Do players need SOL or a Solana wallet?** No. They need a Zcash wallet and a session key
(usually held by a passkey). Apps pay Solana fees; rollup transactions are free.

**Can an app steal deposits?** It can decide outcomes in its own pots (so a dishonest app can
make players lose), but it cannot create value, pay anyone outside a pot's players, or change
where withdrawals go. Players should trust an app's game logic as they would any game.

**Why fixed deposit amounts?** So deposits don't fingerprint players, and so the operators can
credit a deposit whose wallet dropped the memo.

**How long does a deposit take?** On the testnet, usually 1–2 minutes (1 block) for an early credit,
within the operators' float; larger or later credits wait for 60 blocks.

**How long does a withdrawal take?** A few minutes for the Zcash payout; the ledger marks it
paid at payout depth (60 blocks).

**Can my app call other Solana programs?** In the rollup, programs that are delegated or
cloned there (VRF, price oracle). On Solana, anything. The ledger never calls apps.

**Can I run my own operators?** Not yet: the testnet beta has one operator set for every app.
Per-app signer sets are a later tier of the design.

## Glossary

| Term | Meaning |
| --- | --- |
| App | A Solana program registered with the ledger; signs ledger calls with its `["zol-app"]` PDA |
| Attestation | An Ed25519 signature by the operators' 2-of-3 key over a credit, void or paid statement |
| Balance | A player's per-app amount on the ledger, keyed by their session key |
| Buy-in | Moving an amount from a balance into a pot seat (`buy_in`) |
| Cash-out | Paying a returned pot's seats into the players' balances (`cash_out`) |
| Commitment (payout) | A keyed hash of the player's payout address; the ledger stores it instead of the address |
| Delegation | Handing a Solana account to a MagicBlock validator; it lives in the rollup until committed back |
| Denomination | An allowed deposit amount (0.01 or 0.1 TAZ on testnet) |
| Gateway | An operator's decision process: credits, attestations, relaying, guarding, the page API |
| Guardian | An operator role that reconciles the ledger with the vault and can freeze an app |
| Head note | The vault note every payout must spend; it orders payouts on Zcash |
| Intent | A signed deposit memo registered with the operators before paying |
| Pot | Seat amounts for one session, delegated to the rollup |
| Receipt | The pot's player list and buy-ins, kept on Solana |
| Return action | An app instruction the delegation program runs on Solana when a pot comes back |
| Session key | A player's Ed25519 key for one app |
| Settlement | A numbered batch of exiting balances that the operators pay in one Zcash transaction |
| Stake | The amount a table moves per round |
| TAZ | Testnet ZEC |
| Vault | An app's shielded Zcash address, its spend key split among the operators |

## Troubleshooting

| Symptom | Likely cause | What to do |
| --- | --- | --- |
| A deposit never shows | The wallet is on the pre-NU7 testnet fork, or the amount isn't a denomination | Check the wallet's chain height; pay exactly 0.01 or 0.1 TAZ |
| `app is pending, not active` | The app isn't activated yet, or is in its activation delay | Wait for activation (1 hour after) |
| Credited nothing for a new app | Its deposit cap is still 0 | The admin sets it (`set_cap`) |
| `BadStake` on `create_table` | The stake isn't a denomination's net value or a tenth of it | Read the allowed stakes from the ledger config (`allowedStakes`) |
| `MissingRequiredSignature` from the magic program | A PDA passed to a MagicBlock CPI isn't marked as a signer | Set `is_signer = true` on the `AccountInfo` before building the intent |
| `account required by the instruction is missing` on a commit | A delegated payer without the validator's fee vault | Pass `["magic-fee-vault", validator]` (delegation program) |
| `InsufficientFundsForRent` on a commit | A delegated payer at exactly its rent | Keep a lamport buffer in it |
| A return action says "pot not back" | It was attached to an account that returns before the pot | Attach it to the pot (`close_pot`'s action) |
| A price read is stale | Reading the oracle on Solana | Read it in the rollup |
| A scheduled task is rejected | Its instruction includes a signer account | Sign with PDAs inside the instruction instead |
| `process.env` is empty in browser code | The web app's Vite config blanks it for browser bundles | Pass settings as `VITE_*` variables at build time (`import.meta.env`); scripts run with `vite-node` see the real environment |
