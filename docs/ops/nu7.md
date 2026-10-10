# NU7 on the pinned Zcash stack

Status: 2026-10-10.

- **Verdict: the patch works.** Every zol-zcash regtest test and both gateway cross-process
  suites pass on the Zakura 1.6.0 regtest with NU7 active from height 1
  (`infra/zakura/regtest-nu7.toml`). Mined transactions are v6 with branch ID `0x77190AD9`.
  The patch was first validated on Zebra 7.0.0-rc.0 regtest. ZOL has since moved to Zakura
  and Zebra was removed.
- **Pre-NU7 chains still work.** The same build passes on a regtest where NU7 is not active
  (`ZOL_NODE_CONFIG=pre-nu7`).
- **Testnet:** this makes zol-zcash able to parse and build transactions on testnet after
  NU7. It has not been run against testnet itself, because there was no disk for a sync.

## What NU7 changes

Checked against ZIP 259, Zebra v7.0.0-rc.0 (`6d1e414`), Zakura 1.6.0 and
`zcash_protocol 0.11.0-pre.0` / `zcash_primitives 0.31.0-pre.1`. The press summary is
right in substance.

| Change | Source | Effect on ZOL |
| --- | --- | --- |
| New consensus branch ID `0x77190AD9`. Testnet height 4,465,026; mainnet height "TBD (to be set on Oct 20)" | ZIP 259; Zebra 7 `network_upgrade.rs`; Zakura | **Breaking.** v5/v6 transactions carry the branch ID in the header, and ZIP 244 signatures commit to it. Our pinned crates rejected it on parse (scanner) and signed under the NU6.3 ID (builder). Fixed by the patch. |
| No new transaction format, no new tx version, no `zip233Amount` (ZIP 233 and 234 are **not** deployed) | ZIP 259 ("Not deployed: ZIP 233 and ZIP 234"); Zebra 7 and Zakura have no ZIP 233 code; `zcash_primitives` gates `zip233Amount` behind a feature | None. The v6 layout, txid and sighash are unchanged apart from the branch ID. |
| Orchard/Ironwood circuit unchanged | `zcash_protocol 0.11-pre`: `Nu7 => OrchardProtocolRevision::V3` (same as NU6.3); Zebra 7 `halo2.rs`: `Nu6_3 \| Nu7 => VERIFIER_NU6_3_ONWARD` | None. We keep `OrchardCircuitVersion::PostNu6_3`. |
| v4 transactions invalid (ZIP 2003); Sprout unspendable | ZIP 259; Zebra 7 `transaction.rs` | None. We only build v6. |
| 25 s target spacing (ZIP 218); subsidy and halving rescaled | ZIP 218; Zebra 7 `POST_NU7_POW_TARGET_SPACING = 25` | Every depth expressed in blocks is now 3× shorter in time (see below). |
| Per-block shielded action limits (ZIP 218): 330 Orchard and 330 Ironwood actions per block | Zebra 7 `block.rs` `ORCHARD_BLOCK_ACTION_LIMIT` | Our transactions use at most 32 actions. Under congestion, inclusion can be slower. |
| 60% of fees removed into the NSM (ZIP 235); halving-preserving reissuance (ZIP 237) | ZIP 259; Zebra 7 / Zakura `fees.rs` | None. This is a block-level coinbase rule, with no transaction field. The ZIP 317 fee is unchanged. |
| Funding-stream changes (ZIP 207 rev 2, 214 rev 3, 2008) | ZIP 259 | None. |
| Expiry rules unchanged (default delta still 40); reorg window 1,000 blocks; coinbase maturity 100 | Zebra 7 constants; `DEFAULT_TX_EXPIRY_DELTA = 40` in 0.31-pre | Block counts now mean 3× less time. |

## The patch

The patch has two parts: vendored crates in `crates/zol-zcash/vendor/`, and changes to
zol-zcash itself. The full diff and the audit of what the cfg enables are in
[`crates/zol-zcash/vendor/VENDOR_PATCHES.md`](../../crates/zol-zcash/vendor/VENDOR_PATCHES.md).

### Vendored crates

`zcash_protocol 0.10.6`, `zcash_primitives 0.30.1` and `pczt 0.9.3` are copied from crates.io
and wired in through `[patch.crates-io]` in `crates/zol-zcash/Cargo.toml`. The diff against
upstream is 21 added and 6 removed lines:

- **`build.rs` in each crate.** Each emits `cfg(zcash_unstable = "nu7")` for that crate only.
  - This turns on upstream's own NU7 paths: the `Nu7` enum variants, v6 for NU7, Ironwood
    allowed at NU7, and v4 invalid at NU7.
  - These are the same rules the 0.31 / 0.11 pre-releases ship.
  - The only other code behind this cfg is ZIP 233, which also needs the `zip-233` feature;
    it stays off.
  - No other crate in the build reads a `zcash_unstable` cfg; orchard 0.15.5 has none.
  - The cfg is not set globally, so `RUSTFLAGS` cannot drop it and it cannot leak into
    other crates.
- **`zcash_protocol`:**
  - `BranchId::Nu7` ⇄ `0x7719_0AD9`, replacing the placeholder `0xffff_ffff`;
  - testnet NU7 at 4,465,026;
  - mainnet NU7 stays `None`.

### zol-zcash changes

- **Regtest heights** (`params.rs`):
  - `RegtestHeights` has `nu7: Option<u32>`.
  - `REGTEST` / `ALL_AT_ONE` now activates NU7 at 1, and `RegtestHeights::PRE_NU7` is the
    NU6.3-only layout (`regtest-pre-nu7.toml`).
  - `RegtestHeights::from_upgrades` and `ChainParams::regtest_from_upgrades` read the
    heights from `getblockchaininfo.upgrades`.
  - `ChainParams::nu7_active` was added.
  - `TESTNET` gets NU7 from the patched crate; mainnet is unchanged.
- **zol-zcashd on regtest takes its activation heights from the node** and logs them. One
  binary therefore builds NU6.3 transactions on a pre-NU7 regtest and NU7 ones on an NU7
  regtest. Testnet and mainnet use the built-in heights.
- **Expiry never crosses a network upgrade** (`tx::capped_expiry`):
  - It caps `target + expiry_delta` at the block before the next upgrade, as zcashd does.
  - A transaction signed under NU6.3 and left unmined at the upgrade could never be mined.
    The cap lets its inputs go free at once instead of up to 40 blocks later.
  - This matters for the mainnet activation.
- **New payout-verifier failure `branch`.** It fires when the PCZT's branch ID is not the
  branch of `tip + 1`, or when its expiry crosses the next upgrade. Signers then refuse a
  transition that can never be mined. It is added to `payout::FAILURES`; the gateway passes
  failure names through as strings.
- **Scanner snapshot version 3**, because `ChainParams` is part of the snapshot.
  - A version-2 `state.bin` is refused with "recreate the state directory".
  - Only regtest and dev state directories exist today.
- **Tests:**
  - `tests/regtest.rs` takes the chain parameters from the node.
  - The block fixture is a Zakura NU7 block; it records the heights it was mined under.
  - The Orchard-pool test disables NU7.
  - New unit and adversarial tests cover the branch ID, the testnet height, the expiry cap
    and the `branch` failure.

### Regtest nodes

The dev/test node is the Zakura regtest pair in `infra/zakura/` ([zcash-node.md](zcash-node.md)).

- **Image:** `zakuracore/zakura:1.6.0`, pinned by digest
  `sha256:bf53c178e9549217fce7cfaa21efc85c478129ba6dc5307d55640514affeb0ac`. Zakura 1.6.0
  supports `NU7 = 1` on regtest.
- **Configs:** `regtest-nu7.toml` (default) and `regtest-pre-nu7.toml`, selected with
  `ZOL_NODE_CONFIG=nu7|pre-nu7`. Both carry the zero-value NU6.1 lockbox entry and
  `miner_address` = the `zol-dev` dev wallet.
- **Subsidy:** `getblocksubsidy` reports 2.083 ZEC per block under NU7, the 25 s-rescaled
  subsidy.

Running the suites:

```sh
infra/zakura/scripts/up.sh pair          # ZOL_NODE_CONFIG=pre-nu7 for the NU6.3 chain
(cd crates/zol-zcash && cargo build --release --bin zol-zcashd &&
 cargo test --test regtest --test zcashd --test phase4_regtest -- --ignored --test-threads=1)
cargo test -p zol-gateway --test regtest_e2e -- --ignored --test-threads=1
cargo test -p zol-gateway --test multihost -- --ignored --test-threads=1
```

## Test results

### On Zakura 1.6.0 (current)

| Suite | `nu7` (NU7 at 1) | `pre-nu7` (NU6.3 tip) |
| --- | --- | --- |
| zol-zcash offline (`cargo test`): unit, offline, payout adversarial incl. `branch`, NU7 block fixture | pass | n/a |
| `regtest`: deposit/genesis/refund, reorg drops deposit, (fixture regen skipped) | 3/3 | 3/3 |
| `zcashd`: dev flow | 1/1 | 1/1 |
| `phase4_regtest`: distributed payouts, consolidation, double spend; reorged payout | 2/2 | 2/2 |
| gateway `regtest_e2e`: phase 3 and phase 4 end to end | 2/2 | 2/2 |
| gateway `multihost`: 3 hosts over HTTP, real solana-test-validator | 1/1 | 1/1 |
| `spikes/magicblock/zcash-leg/run.sh`: the whole app with the Zcash leg | pass | not run |

On `nu7` every daemon logged `nu7: Some(1)` and the tip branch was `77190ad9`. On `pre-nu7`
they logged `nu7: None` and the tip branch was `37a5165b`.

### Earlier validation on Zebra (historical)

- **Before the move to Zakura,** the same suites passed on Zebra 7.0.0-rc.0 (NU7 at 1) and
  on Zebra 6.4.2 (pre-NU7).
- **The mined refund transaction** parsed as header `0x80000006`, version group
  `0xD884B698`, branch `0x77190AD9`.
- **A chain crossing NU7 mid-test** (`NU7 = 6`): the funding coinbase was at height 5 under
  NU6.3, and the deposit, genesis and refund followed under NU7. That chain passed
  `regtest_deposit_genesis_refund`, with every transaction built for the branch of its
  target height.

## Risks

1. **Hand-carried consensus constants.**
   - The branch ID and testnet height are copied from ZIP 259 and the 0.11 pre-release, and
     cross-checked against the Zebra 7 and Zakura sources.
   - The mainnet NU7 height is not in the patch: it is to be set on Oct 20. Until it is
     added (one line in `vendor/zcash_protocol-0.10.6/src/consensus.rs` `MainNetwork`),
     mainnet builds **NU6.3 transactions forever**.
   - Those would be rejected after mainnet activation ("transaction has wrong
     consensus branch id for block network upgrade" in the Zebra-derived verifier). The scanner would also fail to parse
     post-NU7 blocks.
   - **The mainnet height must be patched and shipped before activation.**
2. **Upstream's unstable NU7 code is enabled.**
   - The cfg turns on paths upstream marked unstable in 0.30.1. They were audited above and
     match the pre-release crates line for line.
   - Upstream does not test that combination of versions. Nothing else in our build is
     affected.
3. **Regtest heights come from the node.**
   - A malicious or misconfigured regtest node can only make the daemon build transactions
     that node then rejects.
   - Testnet and mainnet do not read heights from the node.
4. **Older state directories are refused** after the upgrade (snapshot version 3). This is
   fine today; production would need a migration.
5. **Mixed operators near an upgrade.**
   - A patched operator and an unpatched one disagree on the branch, and the `branch`
     failure blocks signing.
   - Operators whose tips differ by one block at the activation height can also disagree
     for a block. A retry clears it.
6. **The vendored copies freeze those crates at 0.30.1 / 0.9.3 / 0.10.6.** Upstream security
   fixes in that line need re-vendoring.

## 25-second blocks: block-denominated parameters

**Applied 2026-10-10:**

- gateway `Depths::default()` is now `Depths::NU7` (30/300/300), with `Depths::PRE_NU7` (10/100/100) for a pre-NU7 deployment;
- `DEFAULT_EXPIRY_DELTA` is 120 (refunds, genesis, dev sends), and payouts keep the gateway's 40;
- `MAX_EXPIRY_DELTA` is 300;
- the payout verifier's anchor window is `ANCHOR_WINDOW` = 300;
- the example serve config uses the NU7 depths.

Depths are not yet height-dependent, so a mainnet deployment switches its config at NU7 activation. Zcash difficulty retargets to 25 s, so a block
carries about a third of the work and wall-clock time of a pre-NU7 block. Keeping today's
security and timing needs about 3× the block counts.

| Parameter (where) | Before | Pre-NU7 time | Post-NU7 time | NU7-era default (applied) |
| --- | --- | --- | --- | --- |
| `depths.early` (gateway `config.rs`) | 10 | 12.5 min | 4.2 min | **30** |
| `depths.full` (gateway; also the deep-reorg halt threshold) | 100 | 2.1 h | 42 min | **300**, still below the node's 1,000-block reorg window |
| `depths.payout` (gateway) | 100 | 2.1 h | 42 min | **300** |
| `head_depth` (PayoutPlan) | 1 | — | — | 1 (head ordering only) |
| `expiry_delta` (gateway `PayoutPolicy`, zol-zcashd config, `DEFAULT_EXPIRY_DELTA`) | 40 | 50 min | 16.7 min | **40 for payouts** (fast release of a stuck head); **120** for refunds and genesis. Raising either above 100 needs `zol_zcash::MAX_EXPIRY_DELTA` raised in the same change, because the verifiers enforce it. |
| Payout-verifier anchor window (`payout.rs`, `tip - 100`) | 100 | 2.1 h | 42 min | 300 |
| Scanner `max_reorg_depth` | 1,000 | 20.8 h | 6.9 h | 1,000, matching Zakura's reorg window |
| Zakura `tx_retention` (docs/ops/zcash-node.md) | ≥ 10,000 | 8.7 d | 69 h | 30,000 (≈ 8.7 days), as recommended there |
| Gateway time-based settings (`interval_secs`, `large_delay_secs`, …) | seconds | — | — | unchanged |

- **Effective dates:** testnet switched at 4,465,026. Mainnet switches at its NU7 height, so
  the gateway should make the depths height-dependent or switch them at activation.
- **A parameter change is needed in the gateway or zol-types.** Depths are plain
  configuration, so no type changes. Raising `MAX_EXPIRY_DELTA` is a zol-zcash constant.

## What the full port to the pre-release crates is still needed for

The full port means `zcash_protocol 0.11`, `zcash_primitives 0.31`, `pczt 0.10` and
`orchard 0.16`. The mainnet target is November 5.

1. **Supported upstream.** The patch keeps us on a line upstream no longer maintains
   (0.30 / 0.9 / 0.10.x). Security fixes and any late NU7 change land only in 0.31 / 0.11.
   Mainnet NU7 constants (activation height, NSM reissuance height) would also have to be
   hand-copied each time.
2. **orchard 0.16 dropped the FROST rerandomization API** that our spend authorization uses:
   reddsa 0.5.2 frost plus `orchard::keys::SpendValidatingKey` rerandomization. The port has
   to replace it. Either use the new upstream FROST path, if one exists by then, or vendor
   the removed pieces. **This is the bulk of the port.**
3. **pczt 0.10 format.** The serialized PCZT may change. The distributed signing wire
   (`/sign/*`) carries PCZT bytes, so all operators must upgrade together.
4. **ZIP 233 (`zip233Amount` in v6).** It is not in NU7. If a later upgrade deploys it, it
   changes the v6 format; only the maintained crates will carry it.
5. **Before November 5, the patch alone needs:**
   - the mainnet height in the vendored `MainNetwork`, once published (Oct 20);
   - the `TESTNET`/`MAINNET` tests updated to match;
   - a release.

   That is about a day of work. The port is the durable fix and should not wait for
   mainnet.
