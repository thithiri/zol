# ZOL Design v0.1

**Real-time applications for shielded ZEC, executed on MagicBlock and settled on Zcash**

Updated October 10, 2026. Status: design draft. Nothing is implemented, the ledger sketch is uncompiled, and the directions below are the latest proposals from design discussion. Several depend on external facts that are not yet verified; see Open decisions.

ZOL (Zcash Operating Layer) lets an application accept shielded ZEC, run a real-time session, and pay the result back to Zcash. The ZEC never leaves Zcash: it sits in a threshold-controlled vault while a ledger program on Solana records who is owed what. Sessions execute in MagicBlock ephemeral rollups, so apps are ordinary Solana programs and no Zcash virtual machine is needed.

V0.1 fixes three structural choices made after the first concept note: one vault per app, a shared ledger that apps call but that never calls apps, and pots as the only money that enters the rollup. It supersedes the V1 concept, in which custody was open between a gateway and pre-signed exits. Pre-signed exits are dropped: an exit signed at entry pays the entry balance, so it cannot follow a session's outcome.

## Design decisions

| Area | V0.1 direction |
|---|---|
| Settlement | Zcash L1, Orchard shielded pool |
| Execution | MagicBlock ephemeral rollups running Solana programs |
| Ledger | One shared Solana program (the kernel); apps are untrusted callers |
| Asset representation | Non-transferable balances in the ledger; no wrapped token |
| Custody | FROST threshold vault holding native ZEC |
| Vault scope | One vault per app, with keys derived from a single key generation |
| Signer set | One set for every vault in V0.1; per-app sets are a later tier |
| Deposit | ZIP 321 payment link, one shielded send, binary memo |
| Deposit proof | Threshold Ed25519 attestation, verified natively on Solana |
| Player identity | Ed25519 session key; no Solana wallet and no SOL |
| What enters the rollup | Pots with one seat per player; player balances stay on Solana |
| Conservation | Enforced by the ledger in the rollup and re-checked on Solana when a pot returns |
| Kernel and apps | The ledger never calls app code; a relayer bundles app instructions in the same transaction |
| Exit | Payout address fixed at first deposit; signers pay from settlement records |
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
        |                                            |  shielded payout
        v                                            |
Gateway attestors                          Gateway vault signers
(scan, wait for depth, sign attestation)   (verify settlement, sign spend)
        |                                            ^
        v                                            |
ZOL ledger on Solana  -------------------------------+
(balances, pot receipts, settlement records)
        |                         ^
        |  delegate pot           |  commit and return pot
        v                         |
MagicBlock rollup
(pot seats and app state for the length of a session)
```

Only numbers move during a session. The vault is a liability backing the ledger, and Solana, not the rollup, is the source of truth for what the vault owes.

## Trust model

| Party | What a fault can do | Bound |
|---|---|---|
| Vault signers, at threshold | Take the ZEC in every vault | None in V0.1. Rules are self-enforced; attested enclaves and ztxo are the path out |
| Attestors, at threshold | Credit deposits that never happened | Payout signers keep their own scan and refuse to pay an app more than the deposits they saw |
| Rollup validator | Commit a wrong pot state | The pot's total, recorded on Solana. It can reshuffle money among that pot's players only |
| App program | Pay the wrong seat | The pot. It cannot create value, touch another app, or redirect a payout |
| Ledger program | Over-credit or misallocate balances | Per app: signers cap payouts at the deposits they saw for that app, pin the program hash, and stop on upgrade |
| Solana | Reorder or stall | Signers act only on finalized state; a stall delays payouts and nothing is signed |

Attestors and vault signers are the same operators holding two separate keys, so a leaked attestor key cannot spend ZEC directly.

## Keys

| Key | Curve and scheme | Holder | Use |
|---|---|---|---|
| Session key | Ed25519 | Player's browser | Signs Solana and rollup actions; is the account identity |
| Attestor key | Ed25519, FROST | Gateway operators | Signs deposit attestations |
| Vault spend key | Pallas, rerandomized FROST (ZIP 312) | Gateway operators | Authorizes Orchard spends |
| Vault viewing key | Orchard full viewing key | Operators; shareable per app | Scanning, proof creation, proof of reserves |

Per-app vault keys come from one key generation. Each app's spend key is the group key plus a public per-app tweak added to every signer's share, in the manner of a Taproot tweak; the viewing components are derived per app from a master secret. A new app needs no new ceremony, and one app's viewing key reveals nothing about another. This gives accounting isolation, not key isolation: the same signers hold every derived key.

The session key has no link to the player's Zcash key. The only binding is that its public key sits in the memo of the player's own shielded deposit. A stolen session key can lose funds in play but cannot redirect a payout.

## Entry: deposit and credit

### Deposit

The app page creates a session key and shows a ZIP 321 payment link with the app's vault address, the amount and the memo filled in. The player approves one shielded Orchard send from their own wallet. Amounts are fixed denominations.

Memo layout, 512 bytes:

| Bytes | Field | Notes |
|---|---|---|
| 0 | `0xFF` | Marks arbitrary data so wallets do not render text |
| 1–3 | `"ZOL"` | Tag |
| 4 | Version | 1 |
| 5 | Flags | Bit 0: auto-exit after cash-out. Bit 1: pot ID present |
| 6–37 | Session public key | 32 bytes |
| 38–80 | Payout address | Raw Orchard address, 43 bytes |
| 81–84 | Expiry height | u32 little-endian; uncredited deposits are refunded after it |
| 85–116 | Pot ID | 32 bytes, zero when unused |
| 117–118 | App data length | u16 little-endian |
| 119–511 | App data | Up to 393 bytes, zero padded |

The vault address identifies the app, so the memo carries no program ID. The payout address is required because a shielded deposit does not reveal its sender; fixing it at first deposit is what stops a stolen session key redirecting funds. The note encryption authenticates the memo, so it needs no signature.

### Attestation

1. Each attestor runs its own Zcash node and scans every registered app's viewing key.
2. It waits for depth. Zcash nodes refuse reorgs deeper than about 100 blocks, which is hard finality at roughly two hours. Small amounts are credited sooner, at about 10 confirmations under a per-deposit cap, and the gateway carries the reorg risk in between.
3. It checks the memo: valid tag and version, permitted denomination, future expiry, parseable payout address. Anything else is refunded less the fee, or held if no refund address parses.
4. Attestors produce one FROST Ed25519 signature over a 32-byte digest of the attestation.

Attestation fields: domain tag, network, deposit ID, amount, app, session key, payout address, Zcash height, flags, pot ID, app data. The deposit ID is a salted hash of the note commitment, so the public record does not link a shielded transaction to a Solana session.

### Credit

Anyone can relay the attestation. The ledger checks that the preceding instruction is an Ed25519 verification of the attestor key over the digest, creates a `Deposit` account keyed by deposit ID as the replay guard, and credits the session's balance. If the memo names a pot that is still on Solana, the ledger credits the player's seat directly; otherwise it credits the balance.

A later deposit to the same session must name the same payout address.

## Ledger program

The ledger is the only program the signers pin. A feature belongs in it only if it must be enforced against the app.

### Accounts

| Account | Keyed by | Holds | Leaves Solana |
|---|---|---|---|
| `Config` | singleton | Attestor key, protocol fee, next settlement number, pause flag | No |
| `App` | app program ID | Vault key, deposit cap, total liability | No |
| `Deposit` | deposit ID | Replay guard | No |
| `Balance` | app + session key | Amount, payout address, exit and auto-exit flags | No |
| `Pot` | app + pot ID | One seat (session key, amount) per player | Yes, during a session |
| `PotReceipt` | pot | Total, each player's buy-in, deadline, status | No |
| `Settlement` | sequence number | Up to 16 payouts for the signers | No |

### Instructions

| Instruction | Runs on | Signed by | Effect |
|---|---|---|---|
| `initialize`, `set_cap` | Solana | admin | Configuration |
| `register_app` | Solana | anyone, with a bond | Creates the app record and derives its vault |
| `credit_deposit` | Solana | relayer, with attestation | Credits a balance or a seat |
| `open_pot` | Solana | app's program address | Creates an empty pot and its receipt |
| `buy_in` | Solana | session key | Moves an amount from balance to seat; updates the receipt |
| `delegate_pot` | Solana | app's program address | Freezes the receipt total, sets the deadline, sends the pot to the rollup |
| `move_seat` | Rollup | app's program address | Moves an amount between two seats of one pot |
| `close_pot` | Rollup | app's program address | Commits the pot and returns it to Solana |
| `cash_out` | Solana | anyone | Requires seats to sum to the receipt total, then credits each seat to its balance |
| `refund_pot` | Solana | anyone, after the deadline | Returns each buy-in from the receipt and voids the pot |
| `request_exit` | Solana | session key | Marks a balance for payout |
| `settle` | Solana | anyone | Writes a settlement record from exiting balances |

The app's program address is a program-derived address with a fixed seed, so only the app's code can produce that signature.

### Invariants

```text
Per app:    sum(Balance.amount) + sum(open PotReceipt.total) == App.liability
Per pot:    sum(Pot.seats) == PotReceipt.total            (checked at cash_out)
Per vault:  App.liability + unpaid settlements <= ZEC in that app's vault
```

The ledger keeps the first by changing `App.liability` only where value enters or leaves an app, and checks the second at `cash_out`. Both should be fuzzed off-chain. Zcash consensus enforces the third, because a payout for one app can spend only that app's notes.

### No calls into apps

Solana forbids re-entrancy, so a ledger-to-app hook could not call back into the ledger. V0.1 has no hooks. When a deposit should also update app state, the relayer places the app's own instruction after `credit_deposit` in the same transaction. The ledger therefore never executes untrusted code.

## Sessions in MagicBlock

An account is writable in one place at a time. Delegation passes ownership of the Solana copy to MagicBlock's delegation program, the rollup clones it, and a commit writes the latest state back. This is the same pattern as MagicBlock's Ephemeral SPL Token, with the Zcash vault in place of a Solana token vault.

### Table-shaped apps

1. Players buy in on Solana. Their stake moves from balance to seat.
2. The app delegates its own state and the pot to the same rollup validator.
3. In the rollup the app moves value between seats. Transactions are free and take milliseconds.
4. The app closes the session; the pot is committed and returned.
5. Anyone calls `cash_out`. The ledger checks the pot against its receipt and credits balances.

Because only the pot is delegated, balances stay live for deposits and exits mid-session, a rollup or app fault is confined to that pot's players, and MagicBlock's per-account session charge is paid per table, not per player.

If a pot has not been cashed out by its deadline, `refund_pot` returns each buy-in from the receipt, which never left Solana. A pot that returns afterwards is ignored. An app can therefore force a refund by never closing a pot, which is within the trust already placed in the app over that pot.

### Open apps

Apps that players join mid-session, such as a live market, need player balances inside the rollup. The intended pattern is a one-off voucher account per deposit, delegated and merged in the rollup, following MagicBlock's own top-up idiom. This is not designed in V0.1.

## Exit: settlement and payout

`settle` turns exiting balances of one app into a `Settlement` record: a sequence number and a list of payout addresses and amounts, net of the protocol fee. It accepts only balances owned by the ledger, so nothing still in the rollup can be paid.

Each signer checks every rule on its own machine and withholds its share if any fails:

1. **Finalized.** The record is at Solana's finalized commitment, read from the signer's own node, plus any challenge period MagicBlock applies to the commits behind it.
2. **Pinned program.** The ledger's code hash matches the configured one.
3. **In sequence, never seen.** The sequence number is the next expected and the record is not in the signer's log.
4. **Known destinations.** Every payout address was registered in a deposit the signer itself scanned.
5. **Covered.** Payouts for the app do not exceed the confirmed deposits the signer itself saw, less earlier payouts.
6. **Within limits.** Per-payout and per-hour caps apply; above them a delay and a larger quorum apply.

The coordinator builds the Zcash transaction deterministically from the record and creates the proof, which needs only the viewing key. It sends each output's plaintext so signers can recompute the note commitments and confirm they match the record, with change returning to the vault. Signers then produce the FROST spend authorization. Every payout carries an expiry height; a replacement is signed only after the first has provably expired, and it spends the same notes so both cannot confirm.

If signers disagree or Solana stalls, nothing is signed. Funds wait, which is safe but not live.

## Privacy

- Entry and exit are shielded Orchard transactions.
- Fixed denominations and batched exits blunt amount and timing correlation between Zcash and Solana.
- Deposit IDs are salted, and each app's viewing key is separate.
- A fresh session key per app or session keeps Solana-side activity unlinkable across sessions.
- Session state is public on standard rollup validators. MagicBlock's Private Ephemeral Rollup runs the same programs inside Intel TDX and lets a program shield state, at the cost of a hardware trust assumption.

## Apps and developer surface

An app is an Anchor program plus a web page. It holds no ZEC and no keys. In V0.1 a developer writes against the raw ledger calls; the worked example is a two-player coin flip with six instructions (`create_table`, `join`, `start`, `flip`, `land`, `finish`) using MagicBlock's randomness service, described in the ZOL app quickstart.

The ledger guarantees that an app cannot create value, reach another app, change a payout address, take a balance a player has not staked, or block the exit of unstaked funds. Money in a pot is under the app's control: the ledger checks that seats add up, not that the right seat won.

Planned simplifications, to be extracted after a second app exists:

- A macro for table-shaped games that generates accounts, ledger calls, delegation and guarded callbacks.
- A single outcome call that sets final seat amounts in place of repeated `move_seat`.
- Deposit-as-join through the memo's pot ID, auto cash-out as a post-commit action, and auto-exit.
- An `eject` command that writes out the generated code.
- Configurable presets for teams that write no program.
- A hosted relayer that pays Solana fees.

## Timing and costs

| Step | Time | Driver |
|---|---|---|
| Deposit to credit | About 12 minutes for small amounts | About 10 Zcash confirmations at 75 seconds |
| Deposit to credit, large | About 2 hours | 100-block reorg limit |
| Actions in a session | Milliseconds | Rollup |
| Pot return to payout | About 2 minutes | Solana finality, then one Zcash confirmation |

MagicBlock charges 0.003 SOL per delegated account per session and 0.001 SOL per commit after the first. An account without a fee payer stops after 10 commits. Rollup transactions are currently free. The app or a relayer pays Solana fees and account rent.

Protocol fees are proposed at 0.3% on entry and 0.3% on exit. The ledger sketch implements the exit fee only.

## Roadmap

| Stage | Change |
|---|---|
| V0.1 | Kernel ledger, per-app vaults, pot sessions, one flagship app on raw calls |
| Next | Signers in attested enclaves, so the signing rules become verifiable code |
| Next | Per-app signer sets as a tier for large apps |
| Next | Macro, presets and hosted relayer |
| Later | Zero-knowledge proof of note inclusion in place of the attestor signature |
| V2 | ztxo: vaults become shared VTXO outputs and gateway custody is removed |

The session program should emit its settlement as a list of owner and amount pairs from the start. The V0.1 signers pay from that list and a ztxo server can sign VTXO transitions from the same list, so apps do not change.

## Open decisions

Unverified external facts:

- How long MagicBlock's fraud-proof window is before a commit can be treated as final. Their documentation names the mechanism but gives no period.
- Whether a session key holding no SOL can be the fee payer for rollup transactions.
- How accounts that are not delegated, such as `App` and `Config`, are read inside the rollup.
- Whether ZIP 312 supports the additive per-app tweak as described, including Orchard's sign rule on the spend validating key.
- How target Zcash wallets handle binary memos supplied through a payment link.
- Anchor 1.0 cross-program call signatures; the code sketches have not been compiled.

Design choices still open:

- Signer threshold, operators, and whether the vault signer is built on the PairKey core. Orchard needs FROST over Pallas, which is a different scheme from PairKey's Ed25519 and ECDSA roots.
- Confirmation tiers, per-deposit caps and the denomination set.
- How an app takes a fee. The likely design is a house seat whose payout address is set at registration.
- A recovery or expiry path for a lost session key.
- The registration bond, sized to the scanning cost each app adds.
- Payout-address phishing: a malicious page can insert its own address in a binary memo. The fix is a wallet that fills that field itself.
- `move_seat` against a single outcome call.
- The open-app model with vouchers.
- Programmable signer policy. The shelved design stores a content-addressed WebAssembly module in a Solana account and lets it add restrictions only.
- Regulatory position for privacy, markets and custody together.

## Related material

- `zol_ledger.rs`: uncompiled Anchor sketch of the ledger. It predates this document and still uses `escrow` and `pay`, one vault, and a program ID in the memo.
- ZOL app quickstart: developer walkthrough for the coin flip, written against the calls listed here.
- `ztxo/round-design.md`: the V2 custody design.

## References

- [MagicBlock: ephemeral rollups, delegation and commits](https://docs.magicblock.gg/pages/ephemeral-rollups-ers/introduction/ephemeral-rollup.md)
- [MagicBlock: quickstart](https://docs.magicblock.gg/pages/ephemeral-rollups-ers/how-to-guide/quickstart)
- [MagicBlock: fees, commits and refunds](https://docs.magicblock.gg/pages/ephemeral-rollups-ers/introduction/fees-and-commit-economics.md)
- [MagicBlock: Ephemeral SPL Token](https://docs.magicblock.gg/pages/ephemeral-spl-token/overview)
- [MagicBlock: Private Ephemeral Rollup](https://docs.magicblock.gg/pages/tools/tee/introduction)
- [MagicBlock: randomness quickstart](https://docs.magicblock.gg/pages/verifiable-randomness-functions-vrfs/how-to-guide/quickstart.md)
