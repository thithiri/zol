# ZOL Design v0.2

**Real-time applications for shielded ZEC, executed on MagicBlock and settled on Zcash**

Updated October 10, 2026. Status: design draft, revised after an external review of v0.1 and three further adversarial passes on this version. Nothing is implemented, the ledger sketch is uncompiled, and several directions depend on external facts that are not yet verified; see Open decisions.

ZOL (Zcash Operating Layer) lets an application accept shielded ZEC, run a real-time session, and pay the result back to Zcash. The ZEC never leaves Zcash: it sits in a threshold-controlled vault while a ledger program on Solana records who is owed what. Sessions execute in MagicBlock ephemeral rollups, so apps are ordinary Solana programs and no Zcash virtual machine is needed.

V0.2 keeps the v0.1 structure (one vault per app, a shared ledger that never calls apps, pots as the only money in the rollup) and closes the gaps those reviews found in its money-safety rules. Deposits, pots and settlements are now specified as state machines, and three v0.1 claims are withdrawn as wrong or overstated.

## Changes from v0.1

Findings from the external review:

| Finding | V0.1 | V0.2 |
|---|---|---|
| Forged credits could drain honest depositors | Signers capped an app's payouts at the deposits they saw | Signers reconcile every credited deposit, field for field, against their own scan; any guardian can freeze the app |
| A withheld attestation could be relayed after a refund | Refund when "uncredited" after expiry | Credit and void are mutually exclusive on Solana; a refund requires a finalized void and spends the deposited note itself |
| A validator could swap a player's key in a returned pot | `cash_out` checked the seat total | Player keys never enter the rollup; the pot carries amounts only and the receipt names the players |
| A coordinator could send payouts nobody can decrypt | Signers checked output commitments | Signers recompute every derived field, including ciphertexts, and sign only a sighash they computed |
| "Zcash consensus enforces the reserve invariant" | Stated as an invariant | Withdrawn. The gateway enforces reserves with explicit accounting, fee funding and settlement states |
| "100 blocks is hard finality" | Stated | Withdrawn. Zcash has no protocol finality; depth is a policy backed by a per-app exposure cap |
| Fresh session keys were said to give unlinkability | Payout addresses were public on Solana | Solana holds only a keyed commitment to the address; the limits of the remaining privacy are stated |

Found by the further passes on drafts of this version:

| Gap | Resolution |
|---|---|
| A pot that had returned could still be refunded after its deadline, reversing the result | `refund_pot` applies only to a pot that has not validly returned |
| Buy-ins in a pot that was never started had no way out | `leave_pot` while open, plus an open deadline |
| Flags and app data were attested but not reconciled | The deposit stores a digest of the whole attestation, and signers compare it |
| Uncredited deposit notes inflated the reserve | Only notes of credited deposits, change and float are spendable or counted |
| A refund could be paid from other notes and the deposit then reorged away | A refund must spend the deposited note |
| One settlement sequence for all apps let one bad record block every payout | Sequence numbers are per app |
| Small payouts could never fund their network fee | Each payout carries a fixed network fee allowance, and exits have a minimum |
| The app page holds the session key, so "an app cannot take unstaked funds" was too strong | The guarantee is restated for the app program; the page is listed as trusted |
| A coordinator could collect signatures and withhold the transaction | Bounded expiry, proof checked before signing, shares sent to every signer, several vault notes |
| A player could leave a pot in the same transaction that started it, leaving the app's record of stakes stale | `buy_in` needs the app's signature, and `delegate_pot` must state the players and buy-ins the app expects |
| Crediting a deposit straight to a seat bypassed the app | Removed. Credits go to balances; the memo's pot ID is reserved |
| The admin could confiscate through fee settings, or undo a freeze | Fee limits are constants in the program, unfreezing needs a threshold of guardians, and activation is delayed |
| Rotating the deposit-ID salt could give one note two IDs | The deposit-ID key is fixed for the life of an app |
| A result committed just before the session deadline could be refunded | Refund waits for a grace period after the deadline |

## Design decisions

| Area | V0.2 direction |
|---|---|
| Settlement | Zcash L1, Orchard shielded pool |
| Execution | MagicBlock ephemeral rollups running Solana programs |
| Ledger | One shared Solana program (the kernel); apps are untrusted callers |
| Asset representation | Non-transferable balances in the ledger; no wrapped token |
| Custody | FROST threshold vault holding native ZEC |
| Vault scope | One vault per app, with keys derived from a single key generation |
| Signer set | One set for every vault for now; per-app sets are a later tier |
| Deposit | ZIP 321 payment link, one shielded send, binary memo |
| Deposit proof | Threshold Ed25519 attestation, verified natively on Solana |
| Deposit lifecycle | Credited or voided, never both, decided by one Solana account |
| Player identity | Ed25519 session key; no Solana wallet and no SOL |
| Payout address | Fixed at first deposit; only a keyed commitment to it is published |
| What enters the rollup | Pots holding seat amounts; player keys and balances stay on Solana |
| Conservation | Ledger rules in the rollup, re-checked against a frozen receipt on return |
| Kernel and apps | The ledger never calls app code; a relayer bundles app instructions in the same transaction |
| Payout construction | Every signer recomputes the transaction and its sighash before signing |
| Reserves | Gateway accounting per app; a payout is signed only if claims stay covered |
| Emergency stop | Any one guardian can freeze an app; unfreezing needs a threshold of guardians |
| Signer policy | Fixed kernel rules; programmable policy deferred |
| Privacy | Shielded at entry and exit, fixed denominations, optional TEE rollup for session state |
| Developer surface | Raw ledger calls first; macro, presets and eject later |
| Successor | ztxo replaces gateway custody with native Zcash VTXOs |

## System structure

```text
Player's Zcash wallet
        |
        |  shielded deposit, memo carries session key and payout address
        v
App vault on Zcash  <--------------------------------+
(Orchard, FROST key, one per app)                    |
        |                                            |  shielded payout or refund
        v                                            |
Gateway attestors                          Gateway vault signers
(scan, wait for depth, credit or void)     (reconcile, recompute, sign spend)
        |                                            ^
        v                                            |
ZOL ledger on Solana  -------------------------------+
(deposits, balances, pot receipts, settlements)
        |                         ^
        |  delegate pot           |  commit and return pot
        v                         |
MagicBlock rollup
(seat amounts and app state for the length of a session)
```

Only numbers move during a session. The vault is a liability backing the ledger, and Solana, not the rollup, is the source of truth for what the vault owes.

## Trust model

| Party | What a fault can do | Bound |
|---|---|---|
| Vault signers, at threshold | Take the ZEC in every vault | None yet. Rules are self-enforced; attested enclaves and ztxo are the path out |
| Attestors, at threshold | Forge a credit, misbind a real deposit, or void a real deposit | No ZEC reaches anyone but an original depositor: a forged or misbound credit fails reconciliation and freezes the app; a void refunds the depositor's own note to the depositor's own address. Liveness is lost until operators recover |
| Admin key | Pause, change caps and fees, rotate the attestor key, or activate a new app with a wrong vault address | Fee rates and the fee allowance are capped by constants in the program. A vault address never changes once set. An activation takes effect only after a delay in which any guardian can cancel it. A rotated attestor key has the attestors' bound. The admin cannot unfreeze |
| A single guardian | Freeze an app, or cancel a pending activation | Liveness only |
| Guardians, at threshold | Unfreeze an app that should stay frozen | Forged credit can be staked again, but signers still refuse to pay it |
| Coordinator | Propose a malformed payout, or withhold a signed one | Signers recompute it, and a transaction that differs by one byte has a different sighash. Withholding delays a payout until its bounded expiry |
| Rollup validator | Commit wrong seat amounts, or never return a pot | The pot's frozen total and frozen player list. Withholding turns a result into a refund, so a validator colluding with a player can cancel any result that player dislikes |
| App program | Pay the wrong seat, or never close a pot | The pot. It cannot create value, add a player, touch another app, or redirect a payout |
| App page | Anything the session key can do inside that app | The player's deposit into that app. The page creates the session key and builds the memo, so depositing into an app means trusting its front end |
| Ledger program | Over-credit or misallocate balances | Reconciliation and reserve rules at the signers, who also pin the program hash and stop on upgrade |
| Solana | Reorder or stall | Signers act only on finalized state; a stall delays payouts and nothing is signed |

Attestors and vault signers are the same operators holding two separate keys. The reconciliation rule exists so that a leaked attestor key cannot send ZEC to an attacker.

## Keys

| Key | Curve and scheme | Holder | Use |
|---|---|---|---|
| Session key | Ed25519 | Player's browser, created by the app page | Signs Solana and rollup actions; is the account identity |
| Attestor key | Ed25519, FROST | Gateway operators | Signs credit, void and paid messages |
| Guardian keys | Ed25519, one per operator | Each gateway operator | One freezes an app; a threshold unfreezes it |
| Vault spend key | Pallas, rerandomized FROST (ZIP 312) | Gateway operators | Authorizes Orchard spends |
| Vault viewing key | Orchard full viewing key | Operators; shareable per app | Scanning, proof creation, proof of reserves |
| Gateway secrets | Symmetric, fixed per app | Gateway operators | Keys for deposit IDs and payout commitments |

Per-app vault keys come from one key generation. Each app's spend key is the group key plus a public per-app tweak added to every signer's share; the viewing components are derived per app from a master secret. A new app needs no new ceremony, and one app's viewing key reveals nothing about another. This gives accounting isolation, not key isolation: the same signers hold every derived key.

ZIP 312 is a draft. It covers splitting the spend authorization key and re-randomized signing, leaves key generation out of scope, and says nothing about deriving several spend keys from one group key or about the other viewing-key components. The per-app derivation is therefore this design's own construction and needs its own review.

The session key has no link to the player's Zcash key. The only binding is that its public key sits in the memo of the player's own shielded deposit. A stolen session key can lose funds in play but cannot redirect a payout.

## State machines

Solana records every state that another party depends on. Each signer's durable log records progress on the Zcash side, decided from that signer's own scan. A signer acts only on finalized Solana state.

### Deposit

| State | Recorded in | Entered by | Guard |
|---|---|---|---|
| Seen | Signer log | The signer's scan | The note decrypts under the viewing key of an app that has a vault, frozen or not |
| Credited | Solana | `credit_deposit` | Valid credit attestation; no `Deposit` account exists for this ID |
| Voided | Solana | `void_deposit` | Valid void message; no `Deposit` account exists for this ID |
| Refunded | Signer log | Refund confirmed at payout depth | A finalized `Voided` record; the refund spends the deposited note |

Credit and void both create the same `Deposit` account, so Solana admits exactly one of them. A credit attestation that was signed and withheld is harmless: once the void lands it can never be relayed, and until the void is finalized no signer will refund.

Each attestor also records its decision durably before releasing a signature share and never signs both for one deposit ID. That is a second line; the account is the guarantee.

A deposit may be voided when its memo is invalid but a refund address parses, when its expiry height has passed without a credit, or when the app is frozen, paused or over its cap. A deposit whose memo yields no address stays `Seen` and is held until the sender proves the payment.

A refund transaction spends the deposited note itself and pays the address the signer read from that note's memo, less the network fee. If the deposit is later reorged away, the refund becomes invalid with it, so a refund needs no depth rule and can never be paid from other players' notes.

### Pot

| State | Entered by | Guard |
|---|---|---|
| Open | `open_pot` | Signed by the app's program address; sets an open deadline within the configured maximum |
| Delegated | `delegate_pot` | Pot is `Open`, its open deadline has not passed, and the receipt equals the players and buy-ins the app supplies; freezes the receipt; sets a session deadline within the configured minimum and maximum |
| Cashed out | `cash_out` | See below; consumes the receipt |
| Refunded | `refund_pot` | See below; consumes the receipt |

While a pot is `Open`, a player joins with `buy_in` and leaves with `leave_pot`, which returns their own buy-in. `buy_in` needs two signatures, the session key and the app's program address, so nobody enters a pot except through the app. `leave_pot` needs only the session key, so an app cannot trap a player in a pot that has not started. A pot has a maximum number of seats, and seats are unique: a second buy-in from the same session key adds to its existing seat.

Because a player can leave at any moment before the start, `delegate_pot` carries the players and buy-ins the app expects and fails unless they equal the receipt. An app must treat the receipt, not its own records, as the statement of who has staked what.

Player keys live only in the `PotReceipt`, which never leaves Solana. The delegated `Pot` holds an array of amounts, one per seat, in receipt order. Nothing the rollup commits can change who the players are.

`cash_out` requires all of the following, then credits `receipt.players[i]` with `pot.amounts[i]`:

- The pot account is owned by the ledger again, so it has returned from the rollup.
- Its address is the one derived from the receipt's app and pot ID.
- The number of seats equals the number of players in the receipt.
- The seat amounts sum to the receipt total.
- The receipt is `Delegated`. It becomes `Cashed out` in the same instruction and can never be used again.

`refund_pot` returns each buy-in from the receipt in exactly two cases:

- The pot is `Open` and its open deadline has passed.
- The pot is `Delegated`, its session deadline plus a grace period has passed, and it has not validly returned: the pot account is not owned by the ledger, or it is but fails the seat-count or sum check.

A pot that has returned and passes the checks can only be cashed out, whatever the time. A loser therefore cannot wait out the deadline to reverse a result that reached Solana.

The grace period must exceed the longest time MagicBlock can take between committing a pot and returning it. Otherwise a result committed just before the deadline, with its return still pending, could be refunded. That time is not yet known.

### Settlement

| State | Recorded in | Entered by | Guard |
|---|---|---|---|
| Unpaid | Solana | `settle` | Balances are ledger-owned and marked for exit |
| Signed | Signer log | Signers produce a spend | All payout rules pass; inputs are reserved; the expiry height is within a small bound |
| Expired | Signer log | Expiry height passes at depth without inclusion | Reservation released; the record may be signed again |
| Paid | Signer log, then Solana | Confirmed at payout depth, then `mark_paid` | The signer's own scan shows the transaction |

A re-signed payout must spend at least one note of the expired attempt, so the two can never both confirm.

`mark_paid` is a public record for auditors. It is posted only after payout depth, carries no Zcash transaction ID, and signers never rely on it. A reorg that removes a paid transaction below payout depth triggers the global halt.

## Entry: deposit and credit

### Deposit

The app page creates a session key and shows a ZIP 321 payment link with the app's vault address, the amount and the memo filled in. The player approves one shielded Orchard send from their own wallet. Amounts are fixed denominations.

Memo layout, 512 bytes, unchanged from v0.1:

| Bytes | Field | Notes |
|---|---|---|
| 0 | `0xFF` | Marks arbitrary data so wallets do not render text |
| 1–3 | `"ZOL"` | Tag |
| 4 | Version | 1 |
| 5 | Flags | Bit 0: auto-exit after cash-out. Other bits reserved |
| 6–37 | Session public key | 32 bytes |
| 38–80 | Payout address | Raw Orchard address, 43 bytes |
| 81–84 | Expiry height | u32 little-endian; after it an uncredited deposit may be voided |
| 85–116 | Pot ID | 32 bytes; reserved for deposit-as-join and ignored in this version |
| 117–118 | App data length | u16 little-endian |
| 119–511 | App data | Up to 393 bytes, zero padded |

The vault address identifies the app, so the memo carries no program ID. The payout address is required because a shielded deposit does not reveal its sender. The note encryption authenticates the memo, so it needs no signature.

### Confirmation policy

Zcash has no protocol finality. Zebra keeps a rollback window of 1,000 blocks, about 21 hours at today's 75-second interval, and describes it as a local node policy that is not part of consensus. The external review cites zcashd as shutting down and asking for human intervention on a reorg deeper than 100 blocks. Neither is a network-wide guarantee.

Depth is therefore a policy with a stated exposure:

- **Early credit.** About 10 confirmations for deposits under a per-deposit cap. For each app, the sum of early credits not yet at full depth may not exceed the gateway float currently held in that app's vault.
- **Full depth.** A deeper threshold, to be chosen, after which a deposit stops counting against the cap.
- **Rolled-back early credit.** The loss is charged to the float and the app keeps running. The cap falls by the same amount until operators top the float up. The signer keeps its record of the note it accepted, so reconciliation still matches.
- **Global halt.** If a deposit past full depth or a paid transaction rolls back, if a rolled-back early credit exceeds what is left of an app's float, or if signers' nodes disagree on the chain, all credits and payouts stop until operators intervene.

Depths are stated in blocks here and must be restated if the block interval changes. Signers run Zebra; the version, the shutdown and recovery procedure, and the thresholds are open.

### Attestation and credit

Attestors scan the viewing key of every app that has a vault, frozen or not, with their own node, apply the confirmation policy and the memo checks, and produce one FROST Ed25519 signature over a 32-byte digest. Credit, void and paid messages use distinct domain tags, so one can never be replayed as another. A void message carries the network, the app and the deposit ID. A paid message carries the network, the app and the sequence number.

Credit attestation fields: domain tag, network, app, deposit ID, amount, session key, payout commitment, flags, pot ID, app data. The Zcash height is deliberately left out. Two fields are derived with gateway secrets so the public record gives nothing away:

- **Deposit ID:** a keyed hash of the note commitment. The key is fixed for the life of the app, so a note has exactly one ID.
- **Payout commitment:** a keyed hash of the payout address, the session key and the app. The address itself is never written to Solana.

Anyone can relay the attestation. The ledger checks that the preceding instruction is an Ed25519 verification of the attestor key over the digest, then:

1. Creates the `Deposit` account as `Credited`, storing the app, the index, the amount, the session key, the payout commitment, the pot ID and the attestation digest.
2. Adds the amount to the app's credited total and increments its deposit count.
3. Credits the balance with the amount less the entry fee. The entry fee joins the app's fee pool and is not a liability.

`Deposit.amount` is always the full note value, so it can be compared with the note.

A later deposit to the same session must carry the same payout commitment, which means the same payout address.

## Ledger program

The ledger is the only program the signers pin. A feature belongs in it only if it must be enforced against the app.

### Accounts

| Account | Keyed by | Holds | Leaves Solana |
|---|---|---|---|
| `Config` | singleton | Admin, attestor key, guardian keys, fee rates, network fee allowance, minimum exit, seat and deadline limits, pause flag | No |
| `App` | app program ID | State, vault address, deposit cap, liability, deposit count, credited total, fee pool, next settlement number, unpaid total | No |
| `Deposit` | deposit ID | State, app, index, amount, session key, payout commitment, pot ID, attestation digest | No |
| `Balance` | app + session key | Amount, payout commitment, exit and auto-exit flags | No |
| `Pot` | app + pot ID | One amount per seat | Yes, during a session |
| `PotReceipt` | pot | State, player keys, buy-ins, total, open deadline, session deadline | No |
| `Settlement` | app + sequence number | State and up to 16 payouts, each a payout commitment and an amount | No |

An app is `Pending` after registration. `activate_app` records the vault address the gateway derived, and the app becomes `Active` after a delay unless a guardian cancels the activation in that time. The vault address is immutable. A guardian can make an active app `Frozen`, and a threshold of guardians returns it to `Active`. A frozen app accepts no credits, buy-ins, delegations or settlements. Pots already in the rollup can still return and cash out, and deposits can still be voided and refunded.

### Instructions

| Instruction | Runs on | Signed by | Effect |
|---|---|---|---|
| `initialize`, `set_config`, `set_cap` | Solana | admin | Configuration. Fee rates and the fee allowance cannot exceed constants in the program |
| `register_app` | Solana | anyone, with a bond | Creates the app record as `Pending` |
| `activate_app` | Solana | admin | Records the vault address, once and permanently; the app becomes `Active` after a delay |
| `freeze_app` | Solana | any one guardian | Freezes an active app, or cancels a pending activation |
| `unfreeze_app` | Solana | a threshold of guardians | Returns a frozen app to `Active` |
| `credit_deposit` | Solana | relayer, with credit attestation | Creates the deposit as `Credited`; credits the balance |
| `void_deposit` | Solana | relayer, with void message | Creates the deposit as `Voided`; credits nothing |
| `open_pot` | Solana | app's program address | Creates an empty pot and its receipt |
| `buy_in` | Solana | session key and app's program address | Moves an amount from balance to seat; updates the receipt |
| `leave_pot` | Solana | session key | While the pot is `Open`, returns the player's buy-in to their balance |
| `delegate_pot` | Solana | app's program address | Checks the receipt against the expected players and buy-ins, freezes it, sets the session deadline, sends the pot to the rollup |
| `move_seat` | Rollup | app's program address | Moves an amount between two seats of one pot, by index |
| `close_pot` | Rollup | app's program address | Commits the pot and returns it to Solana |
| `cash_out` | Solana | anyone | Checks the pot against its receipt, credits each player, consumes the receipt |
| `refund_pot` | Solana | anyone | In the two cases above, returns each buy-in and consumes the receipt |
| `request_exit` | Solana | session key | Marks a balance of at least the minimum exit for payout |
| `settle` | Solana | anyone | Writes the app's next settlement record from exiting balances |
| `mark_paid` | Solana | relayer, with paid message | Marks a settlement `Paid` and reduces the app's unpaid total |

The app's program address is a program-derived address with a fixed seed, so only the app's code can produce that signature.

At `settle`, each payout is the exiting balance less the protocol fee and less a fixed network fee allowance from `Config`. Both deductions join the app's fee pool. The minimum exit keeps every payout above its own deductions.

### Invariants the ledger enforces

```text
Per app:  sum(Balance.amount) + sum(live PotReceipt.total) == App.liability
Per app:  App.credited_total == sum(Deposit.amount where state is Credited)
Per pot:  sum(Pot.amounts) == PotReceipt.total            (checked at cash_out)
```

The ledger keeps the first two by changing the totals only where value enters or leaves an app, and checks the third at `cash_out`. All three should be fuzzed off-chain.

The ledger cannot see Zcash, so it cannot enforce that the vault covers these liabilities. That is the gateway's job; see Reserve accounting.

### No calls into apps

Solana forbids re-entrancy, so a ledger-to-app hook could not call back into the ledger. There are no hooks. When a deposit should also update app state, the relayer places the app's own instruction after `credit_deposit` in the same transaction. The ledger therefore never executes untrusted code.

## Sessions in MagicBlock

An account is writable in one place at a time. Delegation passes ownership of the Solana copy to MagicBlock's delegation program, the rollup clones it, and a commit writes the latest state back. This is the same pattern as MagicBlock's Ephemeral SPL Token, with the Zcash vault in place of a Solana token vault.

### Table-shaped apps

1. Players buy in on Solana, through the app. Their stake moves from balance to seat.
2. The app delegates its own state and the pot to the same rollup validator, stating the stakes it expects. The receipt freezes.
3. In the rollup the app moves value between seats by index. Transactions are free and take milliseconds.
4. The app closes the session; the pot is committed and returned.
5. Anyone calls `cash_out`.

Because only seat amounts are delegated, balances stay live for deposits and exits mid-session, a rollup or app fault can only reshuffle money among that pot's frozen players, and MagicBlock's per-account session charge is paid per table, not per player.

Whoever controls whether a pot returns holds an option: an app or a validator can let a pot time out and refund in place of delivering a result. That is inside the stated bound, since the frozen players get their own buy-ins back, but it means a validator colluding with a player cannot lose. Whether MagicBlock's forced undelegation or slashing removes this option is open.

### Open apps

Apps that players join mid-session, such as a live market, need player balances inside the rollup. The intended pattern is a one-off voucher account per deposit, delegated and merged in the rollup. This is not designed yet, and it will need its own answer to the player-substitution problem that the frozen receipt solves for tables.

## Exit: settlement and payout

`settle` turns exiting balances of one app into that app's next `Settlement` record: a list of payout commitments and amounts. It accepts only balances owned by the ledger, so nothing still in the rollup can be paid. Sequence numbers are per app, so a record that cannot be signed blocks only its own app.

### Signer rules

Each signer checks every rule on its own machine, against its own Solana node and its own Zcash scan, and withholds its share if any fails.

1. **Finalized.** The record is at Solana's finalized commitment, plus any challenge period MagicBlock applies to the commits behind it.
2. **Pinned program.** The ledger's code hash matches the configured one.
3. **In sequence, not already paid.** Every earlier record of this app is `Signed` or `Paid` in the signer's log, and this record is neither. An `Expired` record may be signed again under the re-signing rule, and it blocks later records of its app until it is.
4. **Reconciled.** As of the finalized slot at which the record was read, every `Deposit` credited to this app, from index zero to the app's deposit count, matches a note in the signer's scan: the amount equals the note value, and the stored attestation digest equals the digest the signer recomputes from that note and its memo. The app's credited total equals the sum of those notes. Every credited deposit is at full depth or inside the early-credit cap. Any mismatch freezes the app.
5. **Destinations resolved.** Each payout commitment maps to exactly one payout address in the signer's scan.
6. **Reserve covered.** After this payout the app's reserve still covers its claims.
7. **Transaction recomputed.** See below.
8. **Within limits.** Per-payout and per-hour caps apply; above them a delay and a larger quorum apply.

Rule 4 is what protects an honest depositor from a forged credit. An aggregate cap only limits the loss to the vault; reconciliation refuses to pay at all while any credit on Solana lacks a real note behind it. Because the digest covers every attested field, a real deposit re-attested with altered flags or app data fails too.

Signers also run reconciliation continuously, not only at payout. On a mismatch a signer's guardian key freezes the app at once, which stops forged credit from being staked in further pots. Results already decided with forged funds are not unwound automatically; the recovery procedure is open.

### Building and checking the payout

The coordinator proposes the transaction as a partially created Zcash transaction (the `pczt` format), with every note plaintext and every piece of randomness included. It also creates the proof, which needs only the viewing key, and the re-randomizer, which ZIP 312 requires to travel over a confidential channel.

A signer does not trust any derived value in the proposal. It recomputes them:

- **Inputs.** Every spent note is spendable for this app (see Reserve accounting), is unspent in the signer's own scan, and is not reserved by another pending transaction. Each action's nullifier is that note's nullifier.
- **Randomized key.** Each action's verification key follows from the vault key and the supplied re-randomizer.
- **Outputs.** Exactly the payouts of the records being paid, each to the address from rule 5 for the recorded amount. Every other output, whether change or padding, goes to this app's vault.
- **Commitments.** Each output's note commitment and value commitment follow from the supplied plaintext.
- **Ciphertexts.** Each output's ciphertext is the encryption of that plaintext to that recipient, recomputed and compared. A correct commitment with an undecryptable ciphertext would otherwise burn the payout, because the Orchard circuit does not constrain encryption.
- **Fee and expiry.** The fee is the standard fee for this transaction shape and no more than what is left in the app's fee pool. The value balance equals the fee. The expiry height is within a small bound of the current height.
- **Proof.** The proof verifies.
- **Sighash.** The signer computes the sighash itself from the fields it just checked and signs only that value. ZIP 312 requires signers to check that the sighash matches the data they were sent.

Liveness measures against a coordinator that collects signatures and withholds the transaction:

- Signature shares go to every signer, so any one of them can aggregate and broadcast.
- The bounded expiry limits how long inputs stay reserved.
- Change is split so the vault always holds several notes, and one pending payout cannot reserve them all.

If signers disagree or Solana stalls, nothing is signed. Funds wait, which is safe but not live.

## Reserve accounting

Zcash consensus stops an invalid spend. It knows nothing about liabilities on Solana, so valid payouts, refunds and network fees could still leave claims under-backed. Each signer therefore keeps a per-app account from its own Zcash scan and enforces it.

A vault note is spendable for payouts only if it is the note of a `Credited` deposit accepted under the confirmation policy, change from an earlier vault transaction, or float. The note of a `Seen` or `Voided` deposit is neither spendable nor counted: it can only be refunded to its sender or become spendable when its deposit is credited.

Both sides are computed on the projected state in which every `Signed` transaction confirms:

```text
Reserve(app) = unspent spendable notes
             - payouts and network fees of Signed transactions
               (their change is counted as retained)

Claims(app)  = App.liability
             + Settlement amounts not yet Paid or Signed

Rule:          sign a payout only if, with it included,
               Reserve(app) >= Claims(app)
```

If a `Signed` transaction expires, its outflow returns to the reserve and its settlement returns to the claims, so the rule is unaffected. Refunds touch neither side, because they spend only their own deposit's note.

Worked example, ignoring the entry fee. Alice's credited note of 1,000,000 zatoshi is the only spendable note, and Bob's 1,000,000 note is `Seen` but not credited. Alice exits: protocol fee 3,000, network fee allowance 10,000, payout 987,000. Before signing, the reserve is 1,000,000 and the claim is 987,000. The payout spends 987,000 plus a 10,000 network fee (two actions, no padding), so the projected reserve is 3,000 against claims of zero. Bob's note was never counted, so crediting him later adds 1,000,000 to both sides.

Fee funding, so that fees never eat into claims:

- **Protocol fees and network fee allowances.** Deducted from liabilities at credit and at `settle`. They stay in the vault as ZEC and form the app's fee pool.
- **Payout network fee.** Paid from the fee pool, which the record's own allowances have just topped up. Signers track what has been spent from it. The float is never used for fees.
- **Fee shortfalls.** A network fee grows with the number of actions, and so with the number of input notes. The gateway consolidates vault notes in the background, paying from the fee pool, and operators can top up a pool that runs short. A shortfall delays that app's payouts and nothing else.
- **Refund network fee.** Deducted from the refund itself.
- **Reorg loss on early credits.** Charged to the float in that app's vault, up to the cap.

An auditor holding an app's viewing key can compare the vault against the liability, unpaid total and credited total that Solana shows for that app.

## Privacy

Privacy differs by observer, and the gateway sees everything.

| Observer | Can see | Cannot see |
|---|---|---|
| Public, on Zcash | That shielded transactions occurred, and how many actions each has | Amounts, addresses, memos |
| Public, on Solana | Session keys, amounts, pots, timing, payout commitments | Zcash addresses, and any stated link to a Zcash transaction |
| App page | The session key and its signing power; the payout address when it builds the link | Other apps' activity |
| Rollup validator | Session state, unless the app runs in the TEE rollup | Zcash addresses |
| Gateway operators | Every deposit, memo, payout address and amount | Nothing is hidden from them |

A public observer can still correlate the two chains by timing and by transaction shape: a settlement with a given number of payouts is followed by a Zcash transaction with a matching number of actions. The measures below reduce that; they do not remove it.

- Entry and exit are shielded Orchard transactions.
- Deposits use fixed denominations.
- Signers pay settlements on a schedule, several records per transaction, padded to fixed action counts. `settle` is open to anyone, so batching has to be a signer policy and not a property of the records.
- Deposit IDs and payout commitments are keyed with gateway secrets, so an outsider who knows a candidate address or note cannot test it against Solana. The deposit-ID key cannot be rotated without giving one note two IDs, so if it leaks, every past deposit ID of that app can be linked to its public note commitment.
- A fresh session key per app or session keeps Solana-side activity unlinkable to a public observer. Because the payout commitment includes the session key, reusing an address does not link sessions publicly.
- Wallets should still supply a fresh payout address for every deposit, since the gateway and the app page see addresses.
- Session state is public on standard rollup validators. MagicBlock's Private Ephemeral Rollup runs the same programs inside Intel TDX, at the cost of a hardware trust assumption.

## Apps and developer surface

An app is an Anchor program plus a web page. It holds no ZEC and no keys. A developer writes against the raw ledger calls; the worked example is a two-player coin flip with six instructions (`create_table`, `join`, `start`, `flip`, `land`, `finish`) using MagicBlock's randomness service, described in the ZOL app quickstart. The quickstart predates this version: `move_seat` now takes seat indexes, `buy_in` also needs the app's signature, and `delegate_pot` takes the expected stakes.

The ledger guarantees that an app program cannot create value, add or replace a player, reach another app, change a payout address, move a balance without the session key's signature, or block the exit of unstaked funds. Money in a pot is under the app's control: the ledger checks that seats add up, not that the right seat won.

Those guarantees are about the program. The app's page creates the session key, so it can sign anything that key can sign inside that app. Moving session-key creation into the wallet is the fix, and it is the same wallet integration that closes payout-address phishing.

Planned simplifications, to be extracted after a second app exists:

- A macro for table-shaped games that generates accounts, ledger calls, delegation and guarded callbacks.
- A single outcome call that sets final seat amounts in place of repeated `move_seat`.
- Deposit-as-join through the memo's reserved pot ID, once it can respect the app's control over who enters a pot.
- Auto cash-out as a post-commit action, and auto-exit.
- An `eject` command that writes out the generated code.
- Configurable presets for teams that write no program.
- A hosted relayer that pays Solana fees.

## Timing and costs

| Step | Time | Driver |
|---|---|---|
| Deposit to early credit | About 12 minutes | About 10 Zcash confirmations at 75 seconds |
| Deposit to full depth | To be set | Policy; see Confirmation policy |
| Actions in a session | Milliseconds | Rollup |
| Pot return to payout | At least 2 minutes | Solana finality and one Zcash confirmation, plus MagicBlock's challenge window, which is unknown, plus any wait for the payout schedule |

MagicBlock charges 0.003 SOL per delegated account per session and 0.001 SOL per commit after the first. An account without a fee payer stops after 10 commits. Rollup transactions are currently free. The app or a relayer pays Solana fees and account rent.

Protocol fees are proposed at 0.3% on entry and 0.3% on exit. The ledger sketch implements the exit fee only.

## Roadmap

| Stage | Change |
|---|---|
| First build | Kernel ledger with the three state machines, per-app vaults, pot sessions, one flagship app on raw calls |
| Before the flagship app | Adversarial tests for every row in Changes from v0.1 |
| Next | Signers in attested enclaves, so the signing rules become verifiable code |
| Next | Per-app signer sets as a tier for large apps |
| Next | Wallet-held session keys and wallet-filled payout addresses |
| Next | Macro, presets and hosted relayer |
| Later | Zero-knowledge proof of note inclusion in place of the attestor signature |
| V2 | ztxo: vaults become shared VTXO outputs and gateway custody is removed |

The session program should emit its settlement as a list of owner and amount pairs from the start. Today's signers pay from that list and a ztxo server can sign VTXO transitions from the same list, so apps do not change.

## Open decisions

Unverified external facts:

- How long MagicBlock's fraud-proof window is before a commit can be treated as final. Their documentation names the mechanism but gives no period. The payout time estimate depends on it.
- Whether MagicBlock offers forced undelegation or slashing that would remove a validator's option to let a pot time out.
- The longest time between MagicBlock committing an account and returning it, which sets the refund grace period.
- Whether a session key holding no SOL can be the fee payer for rollup transactions.
- How accounts that are not delegated, such as `App` and `Config`, are read inside the rollup.
- Whether an Orchard output ciphertext is byte-for-byte reproducible from the note plaintext, memo and recipient, as the recompute rule assumes.
- Which checks the `pczt` signer role already performs, and which the gateway must add.
- zcashd's behaviour on a deep reorg, taken from the external review and not checked here.
- How target Zcash wallets handle binary memos supplied through a payment link.
- Anchor 1.0 cross-program call signatures; the code sketches have not been compiled.

Design choices still open:

- Review of the per-app key derivation, including Orchard's sign rule on the spend validating key. ZIP 312 does not cover it.
- Signer threshold, operators, and whether the vault signer is built on the PairKey core. Orchard needs FROST over Pallas, which is a different scheme from PairKey's Ed25519 and ECDSA roots.
- Numeric parameters: depth thresholds, the per-deposit cap, each app's float, the fee constants, the network fee allowance, the minimum exit, the seat maximum, the open and session deadline limits, the refund grace period, the activation delay, the payout expiry bound.
- Sizing the network fee allowance against note fragmentation, and the consolidation policy.
- The recovery procedure after a freeze: a real deposit misbound by a forged credit, and pot results decided with forged funds.
- Custody of the admin and guardian keys, the guardian threshold, and whether the admin sits behind a timelock.
- Custody of the gateway keys behind deposit IDs and payout commitments, which cannot be rotated in place.
- The payout schedule and the fixed action counts used for padding.
- How an app takes a fee. The likely design is a house seat whose payout address is set at registration.
- A recovery or expiry path for a lost session key, which also strands that player's buy-in in an open pot until the open deadline.
- The registration bond, sized to the scanning cost each app adds.
- `move_seat` against a single outcome call.
- The open-app model with vouchers.
- Deposit-as-join: how a deposit can seat a player without bypassing the app.
- Programmable signer policy. The shelved design stores a content-addressed WebAssembly module in a Solana account and lets it add restrictions only.
- Regulatory position for privacy, markets and custody together.

## Related material

- `design-v0.1.md`: the previous version, kept unchanged.
- `zol_ledger.rs`: uncompiled Anchor sketch of the ledger. It predates v0.1 and still uses `escrow` and `pay`, one vault, and a program ID in the memo.
- ZOL app quickstart: developer walkthrough for the coin flip, written against v0.1.
- `ztxo/round-design.md`: the V2 custody design.

## References

- [ZIP 312: FROST for spend authorization multisignatures](https://zips.z.cash/zip-0312)
- [pczt crate documentation](https://docs.rs/pczt/latest/pczt/)
- [Zebra: `MAX_BLOCK_REORG_HEIGHT`](https://docs.rs/crate/zebra-chain/11.3.0/source/src/parameters/constants.rs)
- [Orchard circuit source](https://github.com/zcash/orchard/blob/main/src/circuit.rs), cited by the external review and not opened here
- [MagicBlock: ephemeral rollups, delegation and commits](https://docs.magicblock.gg/pages/ephemeral-rollups-ers/introduction/ephemeral-rollup.md)
- [MagicBlock: quickstart](https://docs.magicblock.gg/pages/ephemeral-rollups-ers/how-to-guide/quickstart)
- [MagicBlock: fees, commits and refunds](https://docs.magicblock.gg/pages/ephemeral-rollups-ers/introduction/fees-and-commit-economics.md)
- [MagicBlock: Ephemeral SPL Token](https://docs.magicblock.gg/pages/ephemeral-spl-token/overview)
- [MagicBlock: Private Ephemeral Rollup](https://docs.magicblock.gg/pages/tools/tee/introduction)
- [MagicBlock: randomness quickstart](https://docs.magicblock.gg/pages/verifiable-randomness-functions-vrfs/how-to-guide/quickstart.md)
