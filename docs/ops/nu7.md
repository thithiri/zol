# NU7 on the Zcash stack

Status: 2026-10-10.

- **zol-zcash is on the official NU7 pre-release crates.** These are `zcash_protocol
  0.11.0-pre.0`, `zcash_primitives 0.31.0-pre.1`, `pczt 0.10.0-pre.1` and `orchard 0.16.0`.
  The vendored patch (`crates/zol-zcash/vendor/`, `VENDOR_PATCHES.md`) and the
  `[patch.crates-io]` table are gone. Both were the interim fix that first validated NU7 on
  Zebra 7 and then on Zakura.
- **RedPallas FROST is our own ciphersuite** on frost-core 3.0, proven byte-identical to
  reddsa 0.5.2's (see [FROST](#frost)).
- **Mainnet NU7 is one constant**, [what to change on the 20th](#on-2026-10-20-mainnet-nu7-height).
- **Every Zakura-backed suite passes on both regtest configs**: `nu7`, everything through
  NU7 at height 1, and `pre-nu7`, an NU6.3 tip. See [Test results](#test-results).
- **Testnet:** zol-zcash parses and builds post-NU7 testnet transactions. It has not been run
  against testnet itself, because there was no disk for a sync.

## What NU7 changes

Checked against ZIP 259, Zebra v7.0.0-rc.0 (`6d1e414`), Zakura 1.6.0 and
`zcash_protocol 0.11.0-pre.0` / `zcash_primitives 0.31.0-pre.1`. The press summary is
right in substance.

| Change | Source | Effect on ZOL |
| --- | --- | --- |
| New consensus branch ID `0x77190AD9`. Testnet height 4,465,026; mainnet height "TBD (to be set on Oct 20)" | ZIP 259; Zebra 7 `network_upgrade.rs`; Zakura | **Breaking.** v5/v6 transactions carry the branch ID in the header, and ZIP 244 signatures commit to it. Our pinned crates rejected it on parse (scanner) and signed under the NU6.3 ID (builder). Fixed by the interim patch, now by the 0.11 / 0.31 / 0.10 pre-releases. |
| No new transaction format, no new tx version, no `zip233Amount` (ZIP 233 and 234 are **not** deployed) | ZIP 259 ("Not deployed: ZIP 233 and ZIP 234"); Zebra 7 and Zakura have no ZIP 233 code; `zcash_primitives` gates `zip233Amount` behind a feature | None. The v6 layout, txid and sighash are unchanged apart from the branch ID. |
| Orchard/Ironwood circuit unchanged | `zcash_protocol 0.11-pre`: `Nu7 => OrchardProtocolRevision::V3` (same as NU6.3); Zebra 7 `halo2.rs`: `Nu6_3 \| Nu7 => VERIFIER_NU6_3_ONWARD` | None. We keep `OrchardCircuitVersion::PostNu6_3`. |
| v4 transactions invalid (ZIP 2003); Sprout unspendable | ZIP 259; Zebra 7 `transaction.rs` | None. We only build v6. |
| 25 s target spacing (ZIP 218); subsidy and halving rescaled | ZIP 218; Zebra 7 `POST_NU7_POW_TARGET_SPACING = 25` | Every depth expressed in blocks is now 3× shorter in time (see below). |
| Per-block shielded action limits (ZIP 218): 330 Orchard and 330 Ironwood actions per block | Zebra 7 `block.rs` `ORCHARD_BLOCK_ACTION_LIMIT` | Our transactions use at most 32 actions. Under congestion, inclusion can be slower. |
| 60% of fees removed into the NSM (ZIP 235); halving-preserving reissuance (ZIP 237) | ZIP 259; Zebra 7 / Zakura `fees.rs` | None. This is a block-level coinbase rule, with no transaction field. The ZIP 317 fee is unchanged. |
| Funding-stream changes (ZIP 207 rev 2, 214 rev 3, 2008) | ZIP 259 | None. |
| Expiry rules unchanged (default delta still 40); reorg window 1,000 blocks; coinbase maturity 100 | Zebra 7 constants; `DEFAULT_TX_EXPIRY_DELTA = 40` in 0.31-pre | Block counts now mean 3× less time. |

## The port

### Versions

| Crate | Before (patched) | Now |
| --- | --- | --- |
| zcash_protocol | 0.10.6 + vendored NU7 patch | **0.11.0-pre.0** (`local-consensus`) |
| zcash_primitives | 0.30.1 + patch | **0.31.0-pre.1** |
| pczt | 0.9.3 + patch | **0.10.0-pre.1** |
| orchard | 0.15.5 | **0.16.0** |
| zcash_address | 0.13.0 | **0.14.0-pre.1** |
| zcash_note_encryption | 0.4.2 | **0.5.1** |
| incrementalmerkletree / shardtree | 0.8.2 / 0.7.1 | **0.9.0 / 0.8.0** |
| pasta_curves, ff, group | 0.5.2, 0.13, 0.13 | **0.6.1, 0.14, 0.14** |
| reddsa | 0.5.2 (`frost`) | **0.6.1** (no FROST; the FROST ciphersuite is ours) |
| frost-core, frost-rerandomized | 3.0.0 (via reddsa) | **3.0.0** (direct) |
| rand / rand_core | 0.8 / 0.6 | 0.10 / 0.10 for orchard, pczt and pasta (`rand10`, `rand_core10`); 0.8 / 0.6 kept for frost-core 3.0 and the seeded dev keys |

These are the newest published versions, as of 2026-10-10, that work together.

API changes absorbed:

- **RNGs.** pczt roles (`finalize_io`, `create_ironwood_proof`, `sign_ironwood`,
  `extract`), the zcash_primitives builder and the orchard builder now take an explicit
  rand_core 0.10 RNG. We pass the thread CSPRNG, `zol_zcash::sys_rng()`.
- **Unified addresses.** zcash_address 0.14 adds ZIP 316 revisions. We encode Revision 0
  (`u1…`/`utest1…`/`uregtest1…`), so the strings are byte-identical, and decode both R0 and
  R2.
- **Note encryption.** zcash_note_encryption 0.5 uses `NoteBytesData` ciphertexts, and orchard
  0.16 implements `ShieldedOutput` for PCZT actions, so our wrapper is gone.
- **`SpendingKey` is no longer `Copy`.**

### FROST

reddsa 0.6 removed its `frost` feature ("the FROST ciphersuites will be moved to the
ZcashFoundation/frost repository"). As of 2026-10-10 that repository has no RedPallas crate,
and no other maintained crate on crates.io provides one. The candidates were forks
(`causeway-reddsa`, `nam-reddsa`) or unrelated crates.

So `crates/zol-zcash/src/redpallas.rs` implements `frost_core::Ciphersuite` and
`frost_rerandomized::RandomizedCiphersuite` for Pallas. It is a port of reddsa 0.5.2's
`frost::redpallas` onto pasta_curves 0.6, about 300 lines, and keeps everything that shows
on the wire or in stored keys:

- `ID` = `"FROST(Pallas, BLAKE2b-512)"`. frost-core binds it into every serialized package,
  so existing `[keys]` configs (key packages, public key package) load unchanged.
- **Hash personalisations:**
  - H1 `FROST_RedPallasR`, H2 `Zcash_RedPallasH`, H3 `FROST_RedPallasN`;
  - H4 `FROST_RedPallasM`, H5 `FROST_RedPallasC`;
  - HDKG `FROST_RedPallasD`, HID `FROST_RedPallasI`, randomizer `FROST_RedPallasA`.

  All are BLAKE2b-512, with scalars reduced with `from_uniform_bytes`.
- The Orchard `SpendAuthSig` basepoint as generator, with the same encodings.
- The same even-Y normalisation after dealer key generation and DKG, so `ak` has ỹ = 0.
- **The same RNG consumption.** A random scalar is eight `next_u64` words, as pasta_curves
  0.5's `Field::random` drew them. Seeded key generation therefore reproduces: the dev keys
  of every seed are unchanged, and so is the `zol-dev` dev wallet that `infra/zakura` mines to.

**How byte-equivalence is proven.** `tests/frost_equivalence.rs` runs reddsa 0.5.2 (`frost`,
a dev-dependency only) next to our ciphersuite on 12 fixed ChaCha20 seeds and asserts equal
bytes for:

- dealer key packages and public key packages for (3,2), (5,3) and (2,2), including the raw
  VSS secret shares;
- DKG outputs, from the same 3-party flow as `frost::dkg_local`;
- identifiers, both `derive` (HID) and numeric;
- for every signer pair: nonces, commitments, signing packages, rerandomized signature
  shares with an explicit `alpha` (the PCZT's, the path ZOL uses), and aggregated
  signatures;
- seed-derived randomizers and randomized verifying keys (H "FROST_RedPallasA");
- plain (non-rerandomized) FROST shares and signatures, covering H1 to H5.

The test reads our serialized packages back as reddsa's types, so the stored formats are
interchangeable. In addition, `zol-zcashd --print-dev-keys` produces the same bytes before
and after the port for the seeds `zol-dev`, `phase4-test` and `x`.

### Key derivation unchanged

`KEY_DERIVATION_VERSION` stays 1. `tests/vectors/zol_keys_v1.json`,
`zip2005_use_qsk_zafe.json` and `verify_zol_keys.py` are unchanged. `tests/key_vectors.rs`
(5 tests) and the Python verifier (9 cases, 8 ZIP 2005 vectors) pass.

### Mainnet NU7 height (`params::MAINNET_NU7_HEIGHT`)

`zol_zcash::params::MAINNET_NU7_HEIGHT: Option<u32> = None` feeds `ChainParams::MAINNET`'s
activation table, through `params::mainnet_activation_height`. Every builder, parser and
verifier in the crate takes heights from `ChainParams`, which implements zcash_protocol's
`Parameters`.

`params::tests::mainnet_nu7_transition` builds a zcash_protocol `LocalNetwork` with mainnet's
heights and NU7 at a hypothetical height N, and checks:
- the branch is NU6.3 at N − 1 and NU7 (`0x77190AD9`) at N;
- a transaction targeted before N has its expiry capped at N − 1.

`TESTNET_NU7_HEIGHT` (4,465,026) sits beside it and is checked against the crate's testnet
table.

### On 2026-10-20: mainnet NU7 height

When the height H is published (ZIP 259 requires a multiple of 3):

1. `crates/zol-zcash/src/params.rs`: `pub const MAINNET_NU7_HEIGHT: Option<u32> = Some(H);`
2. `gateway/src/config.rs`: `pub const MAINNET_NU7_HEIGHT: Option<u32> = Some(H);`. The
   gateway weighs pre-NU7 blocks ×3 for depths. Both constants must change in the same
   release.
3. Run `cd crates/zol-zcash && cargo test params` (`branches` then asserts the switch at H),
   `cd gateway && cargo test nu7_heights_match` (fails if the two constants differ) and
   `scripts/test.sh fast`.
4. Ship zol-zcashd and zol-gateway to **all operators before H**. The `branch` failure
   stops a mixed set from signing anything that cannot be mined; capped expiry stops any
   transaction from outliving H.

If a later `zcash_protocol` release ships the mainnet height itself, our constant still wins
for zol-zcash. Keep the two equal.

### Other zol-zcash changes (kept from the patch)

- **Regtest heights:**
  - `RegtestHeights.nu7`; `REGTEST` has NU7 at 1, and `PRE_NU7` stops at NU6.3.
  - `ChainParams::regtest_from_upgrades` reads the heights from the node; zol-zcashd does
    this on regtest.
- **Expiry cap:** `tx::capped_expiry` keeps expiry below the next upgrade. It is now
  generic over `Parameters`.
- **Payout-verifier failure `branch`:** the wrong branch, or an expiry past the next upgrade.
- **Scanner snapshot version 3.**

### PCZT wire format and the operator upgrade

- **The serialized PCZT format did not change.**
  - Checked by experiment: a v6 payout proposal PCZT serialized by pczt 0.10.0-pre.1 (11,849
    bytes, encoding version 2) parses with pczt 0.9.3, and 0.9.3 re-serializes it to
    **identical bytes**.
  - pczt 0.10 added `EncodingVersion`, but a v6 PCZT still uses version 2.
  - Its changelog: secret fields (`dummy_sk`, `bsk`) are now zeroized types, with "their
    serialized encoding unchanged".
- **What did change, so all operators upgrade together:**
  - **NU7 branch.** PCZTs for heights at or above NU7 carry branch `0x77190AD9`. An
    unpatched pre-port build (pczt 0.9.3 without the vendored patch) refuses to sign them.
    A build with the interim patch agrees on the branch.
  - **Receipt memos.**
    - Payout outputs now carry `payout_memo(app, seq)` and refund outputs
      `refund_memo(app, deposit cmx)`.
    - An old verifier expects empty memos and fails new payouts with `payout_memo`.
    - An old refund check fails new refunds on the ciphertext comparison.
    - A new verifier rejects old-style empty-memo payouts.
  - **`GET /valid_receiver`** now returns `ReceiverCheck { valid, vault }`.
  - **Scanner snapshot version.** State directories from before the NU7 work are refused.

  Mixed versions fail closed: nothing is signed. They cannot pay wrongly. Upgrade every
  operator's zol-zcashd and zol-gateway in one window.

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

### On the pre-release stack, Zakura 1.6.0 (current)

`scripts/test.sh regtest`, run once per config with the pair restarted in between
(`infra/zakura/scripts/down.sh`, then `ZOL_NODE_CONFIG=… up.sh pair`):

| Suite | `nu7` (NU7 at 1) | `pre-nu7` (NU6.3 tip) |
| --- | --- | --- |
| root workspace: unit + LiteSVM | 118/118 | 118/118 |
| zol-zcash offline (incl. `frost_equivalence` 5, `key_vectors` 5, payout adversarial with `payout_memo` and `branch`, NU7 block fixture, mainnet NU7 transition) | 29/29 | 29/29 |
| web + wire (TypeScript) | pass | pass |
| zol-zcash regtest: `regtest` (3), `zcashd` (1, incl. `ReceiverCheck`), `phase4_regtest` (2) | 6/6 | 6/6 |
| gateway: `regtest_e2e` (2), `multihost` (1), `rpc_validator` (1) | 4/4 | 4/4 |

The frozen key vectors and the Python verifier pass unchanged. The interim patch's results
on Zakura and Zebra follow.

### With the interim patch (historical)

- **Before the move to Zakura,** the same suites passed on Zebra 7.0.0-rc.0 (NU7 at 1) and
  on Zebra 6.4.2 (pre-NU7).
- **The mined refund transaction** parsed as header `0x80000006`, version group
  `0xD884B698`, branch `0x77190AD9`.
- **A chain crossing NU7 mid-test** (`NU7 = 6`): the funding coinbase was at height 5 under
  NU6.3, and the deposit, genesis and refund followed under NU7. That chain passed
  `regtest_deposit_genesis_refund`, with every transaction built for the branch of its
  target height.

## Risks

1. **An in-house FROST ciphersuite.**
   - About 300 lines port reddsa 0.5.2's audited code onto pasta_curves 0.6. Equivalence is
     tested on fixed seeds across keygen, DKG and signing, but it is not independently
     audited.
   - It also depends on frost-core 3.0's internals (`Ciphersuite`, `post_generate`,
     `post_dkg`) staying stable. A drift would show up in `tests/frost_equivalence.rs`.
   - Replace it with the ZcashFoundation/frost crate when one is published.
2. **Pre-release crates.** `zcash_protocol 0.11.0-pre.0`, `zcash_primitives 0.31.0-pre.1`
   and `pczt 0.10.0-pre.1` may still change before final release. They are pinned with `=`.
3. **The mainnet NU7 height is ours to set.**
   - Until `MAINNET_NU7_HEIGHT` (and the gateway's twin) is set, mainnet builds **NU6.3
     transactions forever**.
   - Those would be rejected after activation ("transaction has wrong consensus branch id
     for block network upgrade"), and the scanner would fail on post-NU7 blocks.
   - Set both before activation; see [On 2026-10-20](#on-2026-10-20-mainnet-nu7-height).
4. **Two RNG ecosystems.** rand_core 0.10 serves orchard, pczt and pasta; rand_core 0.6
   serves frost-core 3.0 and the seeded dev keys.
   - Both are CSPRNG-backed in production paths: `rand::rng()` from 0.10, and `OsRng` from
     0.6.
   - The 0.6 line stays until frost-core moves to rand_core 0.10. That move would change
     nothing on the wire, but its scalar sampling must keep the eight-`next_u64` rule, or
     seeded dev keys change.
5. **Regtest heights come from the node.**
   - A malicious or misconfigured regtest node can only make the daemon build transactions
     that node then rejects.
   - Testnet and mainnet do not read heights from the node.
6. **Older state directories are refused** (snapshot version 3). This is fine today;
   production would need a migration.
7. **All operators must upgrade together**, because of the receipt memos, the NU7 branch
   and the `ReceiverCheck` shape (see the wire-format notes). Mixed versions fail closed:
   nothing gets signed.
8. **One-block tip skew at the activation height** can fail `branch` once. A retry clears it.

## 25-second blocks: block-denominated parameters

**Applied 2026-10-10:**

- gateway depths are counted in 25 s blocks and default to `Depths::NU7` (30/300/300). A block below the network's NU7 height counts three times (`policy::DepthRule`), so the same config gives 10/100/100 blocks before activation and switches by itself;
- `DEFAULT_EXPIRY_DELTA` is 120 (refunds, genesis, dev sends), and payouts keep the gateway's 40;
- `MAX_EXPIRY_DELTA` is 300;
- the payout verifier's anchor window is `ANCHOR_WINDOW` = 300;
- the example serve config uses the NU7 depths.

Zcash difficulty retargets to 25 s, so a block
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

- **Effective dates:** testnet switched at 4,465,026 (`gateway::config::TESTNET_NU7_HEIGHT`).
  Mainnet switches at `gateway::config::MAINNET_NU7_HEIGHT`, which is `None` until the height
  is announced; until then every mainnet block counts as a 75 s block. On the 20th, set it
  together with the zol-zcash constant.
- **Across activation** confirmations are a sum: pre-NU7 blocks × 3 plus post-NU7 blocks. The
  count only grows as the tip advances, so no credit or top-up drops back from full depth at
  the switch. `zol-zcashd` still counts plain blocks; the gateway converts (`DepthRule::blocks`)
  for the plan's `full_depth` and for the page's `AppInfo`.
- `GatewayConfig.nu7_height` overrides the network's height (tests, private chains). Regtest
  counts every block once.

## What is still open after the port

1. **The RedPallas ciphersuite is ours** until ZcashFoundation/frost publishes one. Then
   switch to it and keep `tests/frost_equivalence.rs`; reddsa 0.5.2 remains the reference.
2. **The crates are pre-releases** (`-pre`). Move to the final 0.11 / 0.31 / 0.10 releases
   when they ship. Expect small API changes, but no change in our encodings.
3. **ZIP 233 (`zip233Amount` in v6)** is not in NU7. If a later upgrade deploys it, it changes
   the v6 format; the maintained crates will carry it.
4. **Mainnet NU7 height:** see [On 2026-10-20](#on-2026-10-20-mainnet-nu7-height).
