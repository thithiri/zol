# ZOL Design v0.3

**Real-time applications for shielded ZEC, executed on MagicBlock and settled on Zcash**

Updated October 10, 2026. Status: design draft, revised after external reviews of v0.1 and v0.2 and further adversarial passes. Nothing is implemented, the ledger sketch is uncompiled, and several directions depend on external facts that are not yet verified; see Open decisions.

ZOL (Zcash Operating Layer) lets an application accept shielded ZEC, run a real-time session, and pay the result back to Zcash. The ZEC never leaves Zcash: it sits in a threshold-controlled vault while a ledger program on Solana records who is owed what. Sessions execute in MagicBlock ephemeral rollups, so apps are ordinary Solana programs and no Zcash virtual machine is needed.

V0.3 keeps the structure of v0.2 (one vault per app, a shared ledger that never calls apps, pots as the only money in the rollup, state machines for deposits, pots and settlements) and adds one element: a single head note per vault that every payout must spend. That makes Zcash itself decide which payout of a settlement is the real one, so safety no longer rests on any signer's local log.

## Changes from v0.2

Findings from the external review of v0.2:

| Finding | V0.2 | V0.3 |
|---|---|---|
| Two signing quorums that share only a malicious operator could each pay the same settlement | Each signer's local log said whether a record was already signed | Every payout spends the vault's head note, so two payouts of one settlement share a nullifier and only one can confirm |
| A retry had to reuse an input of the expired attempt, which a reorg could remove, stranding the payout | "A re-signed payout must spend at least one note of the expired attempt" | The head is the only required input. Other inputs can be replaced, so an app keeps running after an early-credit rollback |
| The payout checker rejected Orchard's ordinary dummy spends | Every spend had to be a scanned vault note under the vault key | Real spends and zero-valued dummy spends are checked by separate rules |
| Payout commitments gave an active address-testing oracle | Anyone could deposit into another player's session key | The memo must be signed by the session key |
| "One changed byte changes the sighash" | Stated | Corrected. The signature commits to the transaction's effects, not to its proofs or signatures |
| The ledger's trust row implied the signers bound its faults | Stated | It now says that allocation between legitimate users rests on the ledger alone |

The changes from v0.1 are recorded in `design-v0.2.md`.

## Design decisions

| Area | V0.3 direction |
|---|---|
| Settlement | Zcash L1, Orchard shielded pool |
| Execution | MagicBlock ephemeral rollups running Solana programs |
| Ledger | One shared Solana program (the kernel); apps are untrusted callers |
| Asset representation | Non-transferable balances in the ledger; no wrapped token |
| Custody | FROST threshold vault holding native ZEC |
| Vault scope | One vault per app, with keys derived from a single key generation |
| Signer set | One set for every vault for now; per-app sets are a later tier |
| Deposit | ZIP 321 payment link, one shielded send, binary memo signed by the session key |
| Deposit proof | Threshold Ed25519 attestation, verified natively on Solana |
| Deposit lifecycle | Credited or voided, never both, decided by one Solana account |
| Player identity | Ed25519 session key; no Solana wallet and no SOL |
| Payout address | Fixed at first deposit; only a keyed commitment to it is published |
| What enters the rollup | Pots holding seat amounts; player keys and balances stay on Solana |
| Conservation | Ledger rules in the rollup, re-checked against a frozen receipt on return |
| Kernel and apps | The ledger never calls app code; a relayer bundles app instructions in the same transaction |
| Payout agreement | One head note per vault; every payout spends it, so Zcash orders payouts |
| Payout construction | Every signer recomputes the transaction's effects and their hash before signing |
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
| Vault signers, below threshold | Get different honest signers to approve conflicting payouts | Conflicting payouts all spend the same head note, so at most one confirms |
| Attestors, at threshold | Forge a credit, misbind a real deposit, or void a real deposit | No ZEC reaches anyone but an original depositor: a forged or misbound credit fails reconciliation and freezes the app; a void refunds the depositor's own note to the depositor's own address. Liveness is lost until operators recover |
| Admin key | Pause, change caps and fees, rotate the attestor key, or activate a new app with a wrong vault address or genesis head | Fee rates and the fee allowance are capped by constants in the program. A vault address never changes once set. An activation takes effect only after a delay in which any guardian can cancel it. A rotated attestor key has the attestors' bound. The admin cannot unfreeze |
| A single guardian | Freeze an app, or cancel a pending activation | Liveness only |
| Guardians, at threshold | Unfreeze an app that should stay frozen | Forged credit can be staked again, but signers still refuse to pay it |
| Coordinator | Propose a malformed payout, or withhold a signed one | Signers recompute every effect of the transaction and sign a hash of those effects. Withholding delays a payout until its bounded expiry |
| Rollup validator | Commit wrong seat amounts, or never return a pot | The pot's frozen total and frozen player list. Withholding turns a result into a refund, so a validator colluding with a player can cancel any result that player dislikes |
| App program | Pay the wrong seat, or never close a pot | The pot. It cannot create value, add a player, touch another app, or redirect a payout |
| App page | Anything the session key can do inside that app | The player's deposit into that app. The page creates the session key and builds the memo, so depositing into an app means trusting its front end |
| Ledger program | Over-credit an app, or misallocate between its users | Signers bound the total: an app is never paid more than its vault holds, which is real deposits plus the float. A ledger fault can therefore cost the float. Signers do not check who is owed what inside that total, so allocation between legitimate users rests on the ledger's correctness alone. Signers pin the program hash and stop on upgrade |
| Solana | Reorder or stall | Signers act only on finalized state; a stall delays payouts and nothing is signed |

Attestors and vault signers are the same operators holding two separate keys. The reconciliation rule exists so that a leaked attestor key cannot send ZEC to an attacker.

## Keys

| Key | Curve and scheme | Holder | Use |
|---|---|---|---|
| Session key | Ed25519 | Player's browser, created by the app page | Signs the deposit memo and all Solana and rollup actions; is the account identity |
| Attestor key | Ed25519, FROST | Gateway operators | Signs credit, void and paid messages |
| Guardian keys | Ed25519, one per operator | Each gateway operator | One freezes an app; a threshold unfreezes it |
| Vault spend key | Pallas, rerandomized FROST (ZIP 312) | Gateway operators | Authorizes Orchard spends |
| Vault viewing key | Orchard full viewing key | Operators; shareable per app | Scanning, proof creation, proof of reserves |
| Gateway secrets | Symmetric, fixed per app | Gateway operators | Keys for deposit IDs and payout commitments |

Per-app vault keys come from one key generation. Each app's spend key is the group key plus a public per-app tweak added to every signer's share; the viewing components are derived per app from a master secret. A new app needs no new ceremony, and one app's viewing key reveals nothing about another. This gives accounting isolation, not key isolation: the same signers hold every derived key.

ZIP 312 is a draft. It covers splitting the spend authorization key and re-randomized signing, leaves key generation out of scope, and says nothing about deriving several spend keys from one group key or about the other viewing-key components. The per-app derivation is therefore this design's own construction and needs its own review.

The session key has no link to the player's Zcash key. The only binding is that its public key sits in the memo of the player's own shielded deposit. A stolen session key can lose funds in play but cannot redirect a payout.

## State machines

Solana records every state that another party depends on, and Zcash itself records what has been spent. A signer's log only caches what its own scan shows; no rule relies on a log to prevent paying twice. A signer acts only on finalized Solana state.

### Deposit

| State | Recorded in | Entered by | Guard |
|---|---|---|---|
| Seen | Zcash | The signer's scan | The note decrypts under the viewing key of an app that has a vault, frozen or not |
| Credited | Solana | `credit_deposit` | Valid credit attestation; no `Deposit` account exists for this ID |
| Voided | Solana | `void_deposit` | Valid void message; no `Deposit` account exists for this ID |
| Refunded | Zcash | Refund confirmed at payout depth | A finalized `Voided` record; the refund spends the deposited note |

Credit and void both create the same `Deposit` account, so Solana admits exactly one of them. A credit attestation that was signed and withheld is harmless: once the void lands it can never be relayed, and until the void is finalized no signer will refund.

Each attestor also records its decision durably before releasing a signature share and never signs both for one deposit ID. That is a second line; the account is the guarantee.

A deposit may be voided when its memo is invalid (including a bad session-key signature) but a refund address parses, when its expiry height has passed without a credit, or when the app is frozen, paused or over its cap. A deposit whose memo yields no address stays `Seen` and is held until the sender proves the payment.

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
| Paid | Zcash, then Solana | A confirmed vault transaction whose head memo covers the record; then `mark_paid` | The transaction is at payout depth in the signer's own scan |

Whether a settlement has been paid is read from the vault's head chain on Zcash, described under The vault head. It is never read from a signer's log. A signer may remember that it signed a payout that is still pending, but only to avoid wasted work.

`mark_paid` is a public record for auditors. It is posted only after payout depth, carries no Zcash transaction ID, and signers never rely on it. A reorg that removes a paid transaction below payout depth triggers the global halt.

## Entry: deposit and credit

### Deposit

The app page creates a session key and shows a ZIP 321 payment link with the app's vault address, the amount and the memo filled in. The player approves one shielded Orchard send from their own wallet. Amounts are fixed denominations.

Memo layout, 512 bytes. Version 2 adds the signature and shortens the app data:

| Bytes | Field | Notes |
|---|---|---|
| 0 | `0xFF` | Marks arbitrary data so wallets do not render text |
| 1–3 | `"ZOL"` | Tag |
| 4 | Version | 2 |
| 5 | Flags | Bit 0: auto-exit after cash-out. Other bits reserved |
| 6–37 | Session public key | 32 bytes |
| 38–80 | Payout address | Raw Orchard address, 43 bytes |
| 81–84 | Expiry height | u32 little-endian; after it an uncredited deposit may be voided |
| 85–116 | Pot ID | 32 bytes; reserved for deposit-as-join and ignored in this version |
| 117–180 | Session signature | Ed25519, 64 bytes |
| 181–182 | App data length | u16 little-endian |
| 183–511 | App data | Up to 329 bytes, zero padded |

The vault address identifies the app, so the memo carries no program ID. The payout address is required because a shielded deposit does not reveal its sender.

The session key signs a domain tag, the network, the app's vault address and every other memo field. Note encryption already proves that the memo came from whoever sent the note, but not that the sender controls the session key it names. Without the signature, anyone could deposit into another player's session with a guessed payout address and learn from the outcome whether the guess was right.

A memo is valid when its tag and version are known, its signature verifies, its expiry is in the future and its payout address parses, and the note is a permitted denomination.

### Confirmation policy

Zcash has no protocol finality. Zebra keeps a rollback window of 1,000 blocks, about 21 hours at today's 75-second interval, and describes it as a local node policy that is not part of consensus. The external review cites zcashd as shutting down and asking for human intervention on a reorg deeper than 100 blocks. Neither is a network-wide guarantee.

Depth is therefore a policy with a stated exposure:

- **Early credit.** About 10 confirmations for deposits under a per-deposit cap. For each app, the sum of early credits not yet at full depth may not exceed that app's remaining float; see Reserve accounting.
- **Full depth.** A deeper threshold, to be chosen, after which a deposit stops counting against the cap.
- **Rolled-back early credit.** The loss falls on the float and the app keeps running. The cap falls by the same amount until operators top the float up. A pending payout that was spending the lost note is replaced by one that is not. The signer keeps its record of the note it accepted, so reconciliation still matches; a signer that has lost that record freezes the app, which is safe.
- **Global halt.** If a deposit past full depth or a paid transaction rolls back, if a rolled-back early credit exceeds an app's remaining float, or if signers' nodes disagree on the chain, all credits and payouts stop until operators intervene.

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
| `App` | app program ID | State, vault address, genesis head commitment, deposit cap, liability, deposit count, credited total, fee pool, next settlement number, unpaid total | No |
| `Deposit` | deposit ID | State, app, index, amount, session key, payout commitment, pot ID, attestation digest | No |
| `Balance` | app + session key | Amount, payout commitment, exit and auto-exit flags | No |
| `Pot` | app + pot ID | One amount per seat | Yes, during a session |
| `PotReceipt` | pot | State, player keys, buy-ins, total, open deadline, session deadline | No |
| `Settlement` | app + sequence number | State and up to 16 payouts, each a payout commitment and an amount | No |

An app is `Pending` after registration. `activate_app` records the vault address the gateway derived and the commitment of the vault's genesis head, and the app becomes `Active` after a delay unless a guardian cancels the activation in that time. Guardians cancel it if the address is not the one they derive, or if the genesis head is missing, shallow or wrongly formed. Both values are immutable. A guardian can make an active app `Frozen`, and a threshold of guardians returns it to `Active`. A frozen app accepts no credits, buy-ins, delegations or settlements. Pots already in the rollup can still return and cash out, and deposits can still be voided and refunded.

### Instructions

| Instruction | Runs on | Signed by | Effect |
|---|---|---|---|
| `initialize`, `set_config`, `set_cap` | Solana | admin | Configuration. Fee rates and the fee allowance cannot exceed constants in the program |
| `register_app` | Solana | anyone, with a bond | Creates the app record as `Pending` |
| `activate_app` | Solana | admin | Records the vault address and the genesis head commitment, once and permanently; the app becomes `Active` after a delay |
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

## The vault head

Threshold signing does not give agreement by itself. With two-of-three signing and one malicious operator, that operator could show each honest signer a different payout of the same settlement, spending different notes. Each honest signer would see a valid first payment and sign, and both transactions could confirm. V0.3 removes the problem by making Zcash the arbiter.

Each app's vault has exactly one head note at any time.

- **Genesis is pinned on Solana.** The gateway creates the first head from its float before the app is activated, with a memo at position zero that records no settlement paid and no fees spent. `activate_app` writes that note's commitment into the `App` account, permanently. Guardians cancel the activation unless the note exists, is at full depth and carries that memo, and signers check the same before their first signature.
- **The chain is defined by nullifiers, not by memos.** The transaction that reveals the current head's nullifier is the next transition, and the new head is that transaction's only output addressed to the vault. A signer finds the current head by walking from the commitment on Solana. An output counts as addressed to the vault if it decrypts under the vault's viewing keys, for any diversified address and in both the external and internal scopes. Signers apply that same test when signing and when walking the chain, and halt if a transition has two such outputs. A note that merely looks like a head, whether sent from outside or created as padding, is not on the chain and is never accepted as one.
- **Every payout spends the head.** A vault transaction that pays settlements, or that only consolidates notes, must spend the current head and create exactly one output addressed to the vault, which holds all the change. Padding outputs are worth zero and go to throwaway addresses.
- **The head memo is the payment record.** The new head's memo carries the app, the head's position in the chain, the sequence number of the last settlement paid so far, a digest of the records this transaction pays, and two running totals: network fees spent and fee top-ups swept in.
- **Refunds stay outside.** A deposit refund spends only its own deposit note and creates no output addressed to the vault, so it is the one vault transaction that does not touch the head.

What follows from this:

- **No double payment.** Two transactions that pay the same settlement both spend the same head, so they share a nullifier and Zcash accepts only one. This holds whichever signers approved them.
- **No safety-critical signer state.** A signer reads the current head and the last paid sequence number from its own scan. A lost or rolled-back log cannot make it pay twice.
- **Retries need no special rule.** A new attempt spends the same head as the expired one. Its other inputs are free to change, so if a reorg removes an early-credited deposit that a pending payout was spending, the replacement draws on the float and the app keeps running.
- **Reorgs are consistent.** A reorg that removes a payout also restores the head it spent, so the settlement is unpaid again and can be paid exactly once.
- **Consolidation cannot race a payout.** It is a head transition that pays nothing.

The cost is throughput. An Orchard note can be spent only after the block that created it, so each app gets one vault transaction per block. Paying several settlement records in one transaction absorbs this.

The assumption is that fewer signers are malicious than the threshold, so every signed transaction was checked by at least one honest signer.

## Exit: settlement and payout

`settle` turns exiting balances of one app into that app's next `Settlement` record: a list of payout commitments and amounts. It accepts only balances owned by the ledger, so nothing still in the rollup can be paid. Sequence numbers are per app, and an app's records are paid strictly in order.

### Signer rules

Each signer checks every rule on its own machine, against its own Solana node and its own Zcash scan, and withholds its share if any fails.

1. **Finalized.** Every record being paid is at Solana's finalized commitment, plus any challenge period MagicBlock applies to the commits behind it.
2. **Pinned program.** The ledger's code hash matches the configured one.
3. **Next in the chain.** The transaction spends the app's current head as the signer's own scan shows it, and pays exactly the records that follow the last paid sequence number in that head's memo, in order and with none skipped. A transition that pays nothing is signed only when no finalized record is waiting.
4. **Reconciled.** As of the finalized slot at which the records were read, every `Deposit` credited to this app, from index zero to the app's deposit count, matches a note in the signer's scan: the amount equals the note value, and the stored attestation digest equals the digest the signer recomputes from that note and its memo, having checked the memo's tag, version, signature and payout address. Expiry is judged once, by the attestors at credit time. The app's credited total equals the sum of those notes. Every credited deposit is at full depth or inside the early-credit cap. Any mismatch freezes the app.
5. **Destinations resolved.** Each payout commitment maps to exactly one payout address in the signer's scan.
6. **Reserve covered.** After this payout the app's reserve still covers its claims.
7. **Transaction recomputed.** See below.
8. **Within limits.** Per-payout and per-hour caps apply; above them a delay and a larger quorum apply. Transitions that pay nothing are limited to a set interval, and padding never exceeds the fixed action count for the batch.

Rule 3 replaces any reliance on a signer's memory of what it has signed. Rule 4 is what protects an honest depositor from a forged credit: an aggregate cap only limits the loss to the vault, while reconciliation refuses to pay at all while any credit on Solana lacks a real note behind it. Because the digest covers every attested field, a real deposit re-attested with altered flags or app data fails too.

Signers also run reconciliation continuously, not only at payout. On a mismatch a signer's guardian key freezes the app at once, which stops forged credit from being staked in further pots. Results already decided with forged funds are not unwound automatically; the recovery procedure is open.

### Building and checking the payout

The coordinator proposes the transaction as a partially created Zcash transaction (the `pczt` format), with every note plaintext and every piece of randomness included. It also creates the proof, which needs only the viewing key, and the re-randomizer, which ZIP 312 requires to travel over a confidential channel.

Orchard pairs every spend with an output, so a transaction with more outputs than real inputs carries dummy spends: zero-valued notes under throwaway keys. A payout from the head alone, with one payout and the new head as outputs, already needs one. In the `pczt` flow the coordinator signs the dummy spends itself before signers see the proposal, and a signer rejects any proposal that still carries a dummy spending key.

A signer does not trust any derived value in the proposal. It recomputes them:

- **Head.** Exactly one spend is the app's current head, found by walking the chain from the commitment on Solana. Exactly one output is addressed to this app's vault, and it is the new head: it holds all the change, and its memo advances the old one by exactly what this transaction does.
- **Real spends.** A spend whose verification key follows from the vault key and the supplied re-randomizer is a real spend. Apart from the head, it must be the note of a credited deposit of this app, or a top-up note, that is unspent in the signer's own scan, and the action's nullifier must be that note's nullifier. These are the only spends the signers authorize.
- **Dummy spends.** Any other spend must be worth zero. The signer confirms this by recomputing the action's value commitment from the paired output's value and the supplied randomness with a spend value of zero, and it verifies the throwaway-key signature already attached. A dummy cannot spend a vault note, because that needs the vault key.
- **Outputs.** Exactly the payouts of the records being paid, each to the address from rule 5 for the recorded amount, and the new head. Any other output must be worth zero and must not be addressed to the vault.
- **Commitments.** Each output's note commitment and value commitment follow from the supplied plaintext.
- **Ciphertexts.** Each output's ciphertext is the encryption of that plaintext to that recipient, recomputed and compared. A correct commitment with an undecryptable ciphertext would otherwise burn the payout, because the Orchard circuit does not constrain encryption.
- **Fee and expiry.** The fee is the standard fee for this transaction shape and no more than what is left in the app's fee pool. The value balance equals the fee. The expiry height is within a small bound of the current height.
- **Proof.** The proof verifies.
- **Effects hash.** The signer computes the signature hash itself from the fields it just checked and signs only that value. Under ZIP 244 this hash commits to the transaction's effects, meaning its nullifiers, commitments, ciphertexts, value balance and expiry, and not to its proofs or signatures. That is why the proof and the dummy signatures are checked separately. ZIP 312 requires signers to check that the hash matches the data they were sent.

Liveness measures against a coordinator that collects signatures and withholds the transaction:

- Signature shares go to every signer, so any one of them can aggregate and broadcast.
- The coordinator role rotates. If an attempt expires, the next signer in a fixed order proposes, so one operator cannot stall payouts by coordinating forever.
- A pending payout ties up the head, and with it the app's other payouts, only until it confirms or its bounded expiry passes.

If signers disagree or Solana stalls, nothing is signed. Funds wait, which is safe but not live.

## Reserve accounting

Zcash consensus stops an invalid spend. It knows nothing about liabilities on Solana, so valid payouts and network fees could still leave claims under-backed. Each signer therefore computes a per-app account from its own Zcash scan and enforces it.

Three kinds of vault note count toward an app's reserve and can be spent in a head transition: the head, the note of a `Credited` deposit accepted under the confirmation policy that has not yet been swept into the head, and a top-up note. A top-up is a note sent to the vault with a top-up memo that says whether it refills the float or the fee pool. Anyone can send one and it only ever adds value. It is spendable, and counts toward anything, only once it is at full depth. The note of a `Seen` or `Voided` deposit is neither spendable nor counted: it can only be refunded to its sender, or become spendable when its deposit is credited. The gateway float sits in the head.

Every payout spends the head, so at most one pending vault transaction per app can ever confirm. The rule is therefore evaluated on the state the proposed transaction would leave:

```text
Reserve_after = value of the new head
              + credited deposit notes and top-up notes
                the transaction leaves unspent

Claims_after  = App.liability
              + Settlement amounts not paid by the head chain
                or by this transaction

Rule:           sign only if Reserve_after >= Claims_after
```

Refunds touch neither side, because they spend only their own deposit's note.

Worked example, ignoring the entry fee. The head holds a float of 100,000 zatoshi. Alice's credited note of 1,000,000 is spendable, and Bob's 1,000,000 note is `Seen` but not credited. Alice exits: protocol fee 3,000, network fee allowance 10,000, payout 987,000. The transaction spends the head and Alice's note, pays 987,000 and a network fee of 10,000 (two actions, without padding), and creates a new head of 103,000. Claims afterwards are zero and the reserve is 103,000: the float plus the 3,000 left in the fee pool. Bob's note was never counted, so crediting him later adds 1,000,000 to both sides.

Fee funding, so that fees never eat into claims or float:

- **Protocol fees and network fee allowances.** Deducted from liabilities at credit and at `settle`. They stay in the vault as ZEC and form the app's fee pool.
- **Payout network fee.** Paid from the fee pool. What is left in the pool is the amount accrued on Solana, plus fee top-ups swept in, less network fees spent. The head memo carries both running totals.
- **Fee shortfalls.** A network fee grows with the number of actions, and so with the number of input notes. Payouts sweep credited deposit notes into the head as they go, the gateway can run consolidation transitions, and operators can send a top-up when a pool runs short. A shortfall delays that app's payouts and nothing else.
- **Refund network fee.** Deducted from the refund itself.

Float and early credits. An app's remaining float is its reserve, counting deposit notes accepted at early or full depth and top-ups only at full depth, less its claims and less what is left in its fee pool. With no losses this equals the float the gateway put in. Early credits not yet at full depth may not exceed the remaining float.

For example, take a float of 100,000, Alice's full-depth deposit of 1,000,000 and one early credit of 100,000, each with a 0.3% entry fee. The reserve is 1,200,000, the claims are 1,096,700 and the fee pool is 3,300, so the remaining float is 100,000 and the early credit just fits. If that early credit is rolled back, the reserve falls to 1,100,000 while the claims and the booked fee stay, so the remaining float is zero: the float has absorbed the loss, entry fee included, and no further early credit is allowed until a top-up arrives.

An auditor holding an app's viewing key can compare the vault against the liability, unpaid total and credited total that Solana shows for that app.

## Privacy

Privacy differs by observer, and the gateway sees everything. The claims below are about passive observers, with one active attack closed as noted.

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
- Deposit IDs and payout commitments are keyed with gateway secrets, so an outsider who knows a candidate address or note cannot test it against Solana by looking. The signed memo closes the active version of that test: nobody can deposit into a session without its key, so no deposit's outcome reveals another player's address. The deposit-ID key cannot be rotated without giving one note two IDs, so if it leaks, every past deposit ID of that app can be linked to its public note commitment.
- A fresh session key per app or session keeps Solana-side activity unlinkable to a public observer. Because the payout commitment includes the session key, reusing an address does not link sessions publicly.
- Wallets should still supply a fresh payout address for every deposit, since the gateway and the app page see addresses.
- Session state is public on standard rollup validators. MagicBlock's Private Ephemeral Rollup runs the same programs inside Intel TDX, at the cost of a hardware trust assumption.

## Apps and developer surface

An app is an Anchor program plus a web page. It holds no ZEC and no keys. A developer writes against the raw ledger calls; the worked example is a two-player coin flip with six instructions (`create_table`, `join`, `start`, `flip`, `land`, `finish`) using MagicBlock's randomness service, described in the ZOL app quickstart. The quickstart predates this version: `move_seat` now takes seat indexes, `buy_in` also needs the app's signature, `delegate_pot` takes the expected stakes, and the deposit memo carries a session-key signature.

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
| Pot return to payout | At least 2 minutes | Solana finality and one Zcash confirmation, plus MagicBlock's challenge window, which is unknown, plus any wait for the payout schedule. Each app gets one vault transaction per block |

MagicBlock charges 0.003 SOL per delegated account per session and 0.001 SOL per commit after the first. An account without a fee payer stops after 10 commits. Rollup transactions are currently free. The app or a relayer pays Solana fees and account rent.

Protocol fees are proposed at 0.3% on entry and 0.3% on exit. The ledger sketch implements the exit fee only.

## Roadmap

| Stage | Change |
|---|---|
| First build | Kernel ledger with the three state machines, per-app vaults with head notes, pot sessions, one flagship app on raw calls |
| Before the flagship app | Adversarial tests for every row in the change tables of v0.2 and v0.3, and the one-input payout with a dummy spend run through the real `pczt` pipeline |
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
- Which checks the `pczt` signer role already performs, and which the gateway must add. Its documentation says only that signers must reject a proposal carrying a dummy spending key.
- Whether a dummy spend's zero value can be confirmed from the fields a `pczt` exposes to a signer, as the dummy rule assumes.
- zcashd's behaviour on a deep reorg, taken from the external review and not checked here.
- How target Zcash wallets handle binary memos supplied through a payment link.
- Anchor 1.0 cross-program call signatures; the code sketches have not been compiled.

Design choices still open:

- Review of the per-app key derivation, including Orchard's sign rule on the spend validating key. ZIP 312 does not cover it.
- The exact head memo and top-up memo formats.
- Signer threshold, operators, and whether the vault signer is built on the PairKey core. Conflicting payouts are now excluded for any threshold, provided fewer signers are malicious than the threshold. Orchard needs FROST over Pallas, which is a different scheme from PairKey's Ed25519 and ECDSA roots.
- Numeric parameters: depth thresholds, the per-deposit cap, each app's float, the fee constants, the network fee allowance, the minimum exit, the seat maximum, the open and session deadline limits, the refund grace period, the activation delay, the payout expiry bound, the interval between transitions that pay nothing.
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

- `design-v0.2.md` and `design-v0.1.md`: the previous versions, kept unchanged. The first lists the changes from v0.1.
- `zol_ledger.rs`: uncompiled Anchor sketch of the ledger. It predates v0.1 and still uses `escrow` and `pay`, one vault, and a program ID in the memo.
- ZOL app quickstart: developer walkthrough for the coin flip, written against v0.1 and now behind on several calls.
- `ztxo/round-design.md`: the V2 custody design.

## References

- [ZIP 312: FROST for spend authorization multisignatures](https://zips.z.cash/zip-0312)
- [ZIP 244: transaction identifier and signature digests](https://zips.z.cash/zip-0244), cited by the review and not opened here
- [RFC 9591: FROST](https://www.rfc-editor.org/rfc/rfc9591.html), cited by the review and not opened here
- [pczt crate documentation](https://docs.rs/pczt/latest/pczt/) and its [Orchard spend fields](https://docs.rs/pczt/latest/pczt/orchard/struct.Spend.html)
- [Orchard builder source](https://github.com/zcash/orchard/blob/main/src/builder.rs), cited by the review and not opened here
- [Zebra: `MAX_BLOCK_REORG_HEIGHT`](https://docs.rs/crate/zebra-chain/11.3.0/source/src/parameters/constants.rs)
- [Orchard circuit source](https://github.com/zcash/orchard/blob/main/src/circuit.rs), cited by the external review and not opened here
- [MagicBlock: ephemeral rollups, delegation and commits](https://docs.magicblock.gg/pages/ephemeral-rollups-ers/introduction/ephemeral-rollup.md)
- [MagicBlock: quickstart](https://docs.magicblock.gg/pages/ephemeral-rollups-ers/how-to-guide/quickstart)
- [MagicBlock: fees, commits and refunds](https://docs.magicblock.gg/pages/ephemeral-rollups-ers/introduction/fees-and-commit-economics.md)
- [MagicBlock: Ephemeral SPL Token](https://docs.magicblock.gg/pages/ephemeral-spl-token/overview)
- [MagicBlock: Private Ephemeral Rollup](https://docs.magicblock.gg/pages/tools/tee/introduction)
- [MagicBlock: randomness quickstart](https://docs.magicblock.gg/pages/verifiable-randomness-functions-vrfs/how-to-guide/quickstart.md)
