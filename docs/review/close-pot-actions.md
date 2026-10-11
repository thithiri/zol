# Review: return actions on `close_pot`, and the keeper-free games built on them

Date: 2026-10-12. Scope: the ledger change in `64bbc80` (`close_pot` with an optional
`PotAction` and fee vault), and its uses in zol-coinflip: `finish_and_pay`, `crank_close`,
`cash_out_table`, ZEC Up/Down (`bet`, `settle_bet`) and the sealed-bid auction
(`end_auction`, `cash_out_auction`). Internal review by the implementer; an external review
is still wanted before mainnet.

## What changed in the ledger

`close_pot(action: Option<PotAction>)` gained two optional accounts: MagicBlock's fee vault
(for a delegated payer) and an escrow authority (a signer). With an action, the ledger asks
the magic program to run one instruction on Solana right after the pot is back, signed only
by the escrow PDA the delegation program controls for that authority.

## The ledger's guarantees, checked again

| Guarantee | Still holds? | Why |
| --- | --- | --- |
| No value created | yes | The action is an ordinary transaction on Solana after the return. `cash_out` still checks the returned pot (seat count, sum equal to the receipt total) before paying anyone |
| Payouts only to the receipt's players | yes | Unchanged: `cash_out` pays `amounts[i]` to `receipt.players[i]`'s balance, whoever calls it |
| An app touches only its own pots | yes | `close_pot` still needs the pot's app authority to sign; the action's program must be **that** app (`PDA(["zol-app"], program) == authority`) |
| Nobody trapped | yes | If the action fails, never runs, or is never attached, `cash_out` stays permissionless and the refund path is unchanged |
| The ledger signs nothing new | yes | The action is signed by the delegation program's escrow PDA, never by a ledger or app PDA. An app gains no power it did not have: it could already call its own instructions on Solana at any time |

## Findings

### F1 (low) — the action's account list is the app's choice

The ledger forwards whatever accounts the app names (at most 16). The destination is the app
itself, so this gives the app nothing it could not do in a transaction of its own; the ledger
instructions it might call still check their own signers. **No change.**

### F2 (low) — who pays

The action's Solana fee comes from the escrow of the authority the app names. zol-coinflip
names its rent vault, whose escrow the house tops up. A third party can only spend its own
escrow. Anyone can trigger `crank_close` on an idle table, which spends about 5,000 lamports
of the coin flip's escrow per closed table: bounded by the number of tables. **No change;**
the house monitors the escrow (hourly top-up).

### F3 (low) — a wrong fee vault

`magic_fee_vault` is passed through unchecked by the ledger. The magic program accepts only
the validator's real vault; any other account is treated as an account to commit and fails
the call. An app can only break its own close. **No change.**

### F4 (medium, fairness) — Up/Down: whoever settles chooses among a few seconds of prices

`settle_bet` takes the oracle price current at the moment of the call, if published after
expiry and within `SETTLE_WINDOW_SECS` (10 s). The oracle keeps only its latest price, so the
caller picks the moment. The scheduled closer settles within about 5 s of expiry, so a bettor
watching the price can settle at once when it favours them, or wait (up to the closer's next
run) when it does not; the house could do the same in its favour. With 1.9× payouts the
option is worth a little, and it grows with volatility.

Options, in order of effort:
1. **Narrow the accepted publish time** to `[expiry, expiry + 1 s]`: the price used is almost
   exactly the first after expiry; a late settle voids the bet (stake back). The page settles
   at expiry; the house should too (it runs every 1.5 s). Small program change.
2. **A one-shot scheduled settle at expiry** from `bet` (needs a devnet check of when a
   scheduled task first runs).
3. **A price at a fixed time** (an oracle that keeps history), not available in MagicBlock's
   feed today.

**Recommendation:** 1, before Up/Down is shown as fair to anyone but testers.

### F5 (low) — the sealed-bid auction under commit-reveal

- The seller can register a second key as a bidder (shill). Under first price a shill can
  only win the seller's own lot, so it gains nothing; it could, with a second key, decline to
  reveal after seeing other reveals, paying the 10% penalty to the other revealers. That
  amounts to a hidden reserve the seller pays for. Acceptable for the demo; note it for
  any real auction (a public reserve price would remove the motive).
- All bids are public after the reveal, as designed.
- An auction whose end never runs is refunded by the ledger after its session deadline and
  grace; its account stays delegated (rent only).

### F6 (low) — anyone can schedule closers

`schedule_closer` and `schedule_auction_end` take any payer, so anyone can add scheduled tasks
for any table (one per payer, task id from the pot id). The tasks only run permissionless
instructions that do nothing unless their conditions hold. Costs the rollup validator work,
not anyone's money. **No change.**

## Payout planner (follow-up, deferred)

The "bigger action size" follow-up came from a payout round refused with "inputs within
max_inputs do not cover payouts + fee, or the fee exceeds the fee pool". It has not recurred
in the last 72 hours of logs, and payouts are being made. Changing the code that spends from
the vault needs a reproducing test first: deferred to the pre-mainnet review of the planner.
