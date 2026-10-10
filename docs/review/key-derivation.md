# ZOL per-app vault keys: construction for external review

Version: `KEY_DERIVATION_VERSION = 1` (crates/zol-zcash `src/keys.rs`). Date: 2026-10-10.

This document supersedes `docs/spikes/0.8-per-app-key-derivation.md`. Its construction and vectors were "version 0": no network or version prefix, and `nk`/`rivk` derived ad hoc from the master secret. The tweak construction and the security discussion carry over.

Vectors: `crates/zol-zcash/tests/vectors/zol_keys_v1.json`, checked by `crates/zol-zcash/tests/key_vectors.rs` and by `tests/vectors/verify_zol_keys.py`.

---

## 1. What ZOL needs

ZOL keeps players' ZEC in one Orchard-protocol vault per app, in the Ironwood pool (NU6.3). The vaults are controlled by a set of operators with 2-of-3 rerandomized FROST over Pallas (ZIP 312). Three requirements follow:

1. **One key generation, many vaults.** One DKG yields a spend key and a full viewing key for every app. A new app needs no new ceremony.
2. **Viewing isolation.** One app's viewing key reveals nothing about another app's notes. Auditors and app operators may receive a single app's FVK.
3. **Quantum recoverability.** Ironwood notes are recoverable under ZIP 2005 if a vault's keys follow ZIP 2005 § 4.2.3 with `use_qsk = true` ("Usage with FROST").

This construction does not provide spend-key isolation. The same signers hold every app key; see § 5.1.

## 2. Notation

| Symbol | Meaning |
|---|---|
| `G` | Orchard spend-authorization base point, `repr_P = 63c975b8…55b7`. The test `generator_is_orchard_spendauth_base` checks that `[ask]G` reproduces orchard's `ak` for ordinary keys |
| `p`, `q` | Pallas base and scalar field orders |
| `repr_P` | 32-byte point encoding: little-endian x-coordinate, with the top bit set to ỹ |
| `ToScalar`, `ToBase` | `LEOS2IP_512(x) mod q` and `mod p` |
| `BLAKE2b-n(P, x)` | BLAKE2b with an n-byte output and 16-byte personalization `P` |
| `PRF^expand_k(t)` | `BLAKE2b-512("Zcash_ExpandSeed", k ‖ t)` |
| `H^qk(qsk)` | `BLAKE3.derive_key("Zcash ZIP 2005 qk-derivation v1", qsk)`, 32 bytes |
| `ak_group` | FROST group verification key from the DKG, after reddsa's even-ỹ normalization (`post_dkg` negates the key and every share if ỹ = 1) |
| `K` | 32-byte master viewing secret, uniformly random, held by every operator. It is not threshold-shared |
| `app_id` | The app's 32-byte Solana program ID (public) |
| `net` | 0 mainnet, 1 testnet, 2 regtest |

## 3. Construction (v1)

```text
D        = [0x01] || [net]                                   version, network
enc(app) = I2LEOSP_16(32) || app_id

for c = 0, 1, 2, … (u32):
    t_c   = ToScalar( BLAKE2b-512("ZOL_app_ak_tweak", D || ak_group || enc(app) || I2LEOSP_32(c)) )
    A_c   = ak_group + [t_c] G
    reject c if A_c = 𝒪 or ỹ(A_c) = 1                      (Orchard sign rule on ak)
    sk_c  = BLAKE2b-256("ZOL_app_view_key", D || K || enc(app) || I2LEOSP_32(c))
    -- ZIP 2005 § 4.2.3, use_qsk = true, with ak^P = A_c  ("Usage with FROST": the agreed sk is sk_c)
    nk    = ToBase  ( PRF^expand_{sk_c}([0x07]) )
    qsk   = truncate_32( PRF^expand_{sk_c}([0x0C]) )
    qk    = H^qk(qsk)
    rivk  = ToScalar( PRF^expand_{qk}([0x0D] || I2LEOSP_256(ak) || I2LEOSP_256(nk)) )   (rivk_ext)
    reject c if FullViewingKey::from_bytes(repr_P(A_c) || nk || rivk) fails   (ivk ∈ {0, ⊥})
accept the first such c:  t_app = t_c,  ak_app = A_c,  fvk_app = (ak_app, nk, rivk)
```

**Derived and standard parts.**

- **ZIP 32 parts.** These are the standard Orchard/ZIP 32 derivations, computed by the `orchard` crate and re-derived independently in the tests:
  - internal `rivk_int = ToScalar(PRF^expand_rivk([0x83] || ak || nk))`;
  - `dk ‖ ovk = PRF^expand_rivk([0x82] || ak || nk)` (and the same with `rivk_int`);
  - `ivk = Commit^ivk_rivk(ak, nk)`;
  - diversified addresses.
- **Vault address:** external scope, diversifier index 0. **Head address:** internal scope, index 0.
- **Session receiver:** external scope, at diversifier index `BLAKE2b-256("ZOL_SessionRecv1", app_id || session_key)[0..11]` (little-endian, 88 bits).
- **Padding OVK:** `BLAKE2b-256("ZOL_padding_ovk1", fvk_app bytes)`. Anyone with the FVK can recompute it, so signers can check padding outputs' `out_ciphertext`.

**Spend shares.**

- Signer `i` holds a base share `s_i` from the DKG and derives per-app shares on the fly:
  - `s_i,app = s_i + t_app`;
  - `Y_i,app = Y_i + [t_app]G`;
  - `Y_app = ak_group + [t_app]G = ak_app`.
- This is correct for every signing set, because the Lagrange coefficients at 0 sum to 1.
- **Signing a spend.** It is rerandomized FROST (reddsa 0.5.2, frost-rerandomized 3.0.0) on the app shares, with randomizer α taken from the PCZT. The result is a RedPallas signature under `rk = ak_app + [α]G`.

**ZIP 2005 retention.** ZIP 2005 requires that `sk`, `use_qsk` and the FROST `ak` be retained for the Recovery Protocol. Here:

- `sk` for app X is `sk_c(X)`, rederivable from `K`, `app_id`, `net` and the accepted `c`;
- `ak` is `ak_app`, rederivable from `ak_group`;
- `use_qsk` is always `true`.

**Operators must back up `K` and `ak_group`.**

## 4. Verification status

| Item | How it is checked | Status |
|---|---|---|
| ZIP 2005 `use_qsk` (`nk`, `qsk`, `qk`, `rivk_ext`) | `zip2005_use_qsk(sk, ak)` reproduces 8 third-party vectors byte for byte. The vectors are Zafe's `zip2005_use_qsk.json` (commit 6baa678), the same set proposed to ZcashFoundation/frost-tools in PR #610 (open). Our check also covers the 96-byte FVK, the default address and the testnet UFVK. Our own Python implementation (its own BLAKE3) also reproduces their `nk`/`qsk`/`qk`/`rivk_ext`/FVK | **Agrees** with the only other implementation found |
| Agreement with an **official** reference | None exists. Checked on 2026-10-10: `orchard` 0.16.0 (latest on crates.io) and orchard main `616a669`; librustzcash main `3c76c2d`; `zip32` 0.3.0 and main `185326d`; zcash-test-vectors main `78321be` (open PR #129 adds only QR note commitments); `zcash_spec` (PR #14 adds only the rcm domain). Orchard PR #475 (open) adds a FROST FVK constructor that is explicitly *incompatible* with quantum recoverability. ZcashFoundation's `zcash-sign` still uses it | **Unverified against upstream.** Implemented from ZIP 2005 text at zips main (last changed 840b3f3, 2026-08-25; status Proposed): § 4.2.3 "Orchard Key Components" (`H^nk` 0x07, `H^qsk` 0x0C with truncation to ℓ_qsk = 256, `H^qk` BLAKE3 with the quoted context, `H^rivk_ext` 0x0D over I2LEOSP_256(ak) ‖ I2LEOSP_256(nk)), and ZIP 32's unchanged internal derivation (0x83) |
| ZOL tweak, counter, `sk_c`, FVK, OVKs, padding OVK, session indices | `independent_rederivation` (a second Rust implementation in the test file, not calling `zol_zcash::keys`) and `verify_zol_keys.py` (standard-library Python, own BLAKE3 and Pallas arithmetic) reproduce all 9 cases, including 11 rejected counters | Verified (three implementations agree) |
| Diversified addresses (vault, head, session receivers) | `orchard` crate only (FF1 + Sinsemilla); not re-derived independently | Relies on orchard, which is tested upstream with official vectors |
| End to end | Regtest: Zebra 6.4.2 accepted deposits to session receivers built this way, the scanner decrypted them, and 2-of-3 FROST under `ak_app` spent them (`tests/regtest.rs`, `tests/phase4_regtest.rs`) | Works on regtest |

**The steps that remain unverified against an upstream reference are exactly these:**

1. The ZIP 2005 functions `H^qsk`, `H^qk` and `H^rivk_ext`, and their composition when `ak` comes from FROST.
2. The ZIP 2005 statement that such keys will pass the future Recovery Protocol, which is not yet specified in final form.

The ZIP constants are matched literally, and two independent implementations agree with ours. **If ZIP 2005 changes before activation of the Recovery Protocol, every vault address changes with the next `KEY_DERIVATION_VERSION`.**

## 5. Threat model and security notes

### 5.1 The tweak is public: no key isolation

`t_app` is computable by anyone who knows `ak_group` and `app_id`. Holding a threshold of base shares means holding every app's spend key. This is accepted ("accounting isolation, not key isolation", design v0.3.1 "Keys").

### 5.2 Signing isolation: the cross-app relabelling attack

For any apps A and B and any α:

```
ak_B + [α]G = ak_A + [α + t_B − t_A]G
```

A coordinator can therefore present app B's spend to app A's signers with α' = α + t_B − t_A. The `rk` test passes, and the FROST transcript under app A's shares is a valid spend authorization for app B's note.

Spike 0.8 showed this end to end. The Phase 4 adversarial test `malicious_coordinator_is_rejected_by_name` reproduces it against the payout verifier.

**The defence is the nullifier binding.** A signer authorizes action j for app X only if:

- `rk_j = ak_X + [α_j]G`, and
- `nf_j` equals, under `fvk_X`, the nullifier of a spendable app-X note in the signer's **own** scan: the current head, a credited deposit, or a full-depth top-up.

App B's note has a nullifier computed with `nk_B`, so the check fails with `real_spend_unknown_note`. Reviewers should confirm that the nullifier binding is sufficient. It relies on `nk` being secret per app and on the circuit binding the nullifier to the spent note's `nk`.

### 5.3 Coordinator-chosen α

The Orchard builder picks α when it constructs the PCZT. `rk = ak + [α]G` is part of the transaction effects and of the sighash, so α is fixed **before** FROST round 1.

- **What frost-rerandomized now recommends:** the 3.x API `RandomizedParams::new_from_commitments` / `sign_with_randomizer_seed` derives the randomizer from a seed and the round-1 commitments. Open ZIP 312 PR zcash/zips#895 would make that derivation a MUST.
- **Why ZOL cannot use it yet:** that derivation cannot produce the PCZT's α.
- **What ZOL uses instead:** the deprecated `frost_rerandomized::sign(…, Randomizer::from_scalar(α))` and `RandomizedParams::from_randomizer`.
- **Same as the Zcash Foundation tooling:** ZcashFoundation frost-tools (`frost-client` participant round 2; `zcash-sign` reads each spend's α from the PCZT) does the same.
- **Protection in ZOL:**
  - Every signer recomputes `rk` from α and its own `ak_app`.
  - The nullifier binding (§ 5.2) bounds what a malicious α can achieve.
  - α travels in the PCZT over the operators' authenticated channel, which ZIP 312 requires to be confidential.
- **Reviewer question:** does coordinator-chosen α weaken unforgeability, or only unlinkability (the reason the upstream API changed)?
- **Possible fix:** run round 1 first, derive α from the commitments, then build the PCZT with that α. This needs the builder to accept a chosen α.

### 5.4 Related keys and RedPallas

- The app tweak is an additive shift of the same kind as the rerandomizer.
- RedPallas hashes `rk` into the challenge, `c = H*(R ‖ rk ‖ m)`. This key-prefixing rules out the generic additive related-key forgeries that apply to unprefixed Schnorr.
- Security should reduce to rerandomized FROST unforgeability (ZIP 312 / the FROST rerandomized paper) with one extra fixed, public shift per app.
- Nonces are single use and bound to `(session, sighash)` in `zol-zcashd`.

### 5.5 Viewing isolation

- **`sk_c` per app:** it is a PRF output of `K` on distinct, injectively encoded inputs (fixed-width version, network and counter, length-prefixed app_id).
- **Everything else from `sk_c`:** `nk`, `qsk`, `qk` and `rivk` follow from `sk_c` (and `ak_app`) through ZIP 2005.
- **Under the PRF assumption,** `fvk_A` therefore reveals nothing about `sk_c(B)`, `nk_B` or `rivk_B`.
- **Tests:** app A's IVKs (both scopes) do not decrypt app B's notes.
- **`ak_group` hidden:**
  - `t_app` hashes `ak_group`, so the holder of `fvk_A` alone cannot compute `ak_group` or `ak_B`. Keep `ak_group` private to the operators.
  - Even with `ak_B`, finding app B's addresses needs `nk_B` and `rivk_B`.

### 5.6 The sign rule and the counter

- **Why ỹ matters:** Orchard requires ỹ(ak) = 0 at the encoding level (`SpendValidatingKey::from_bytes`, FVK and UFVK). ZIP 2005 asks for "any suitably secure method that ensures the last bit of repr_P(ak^P) is 0".
- **ZOL's method:** retry the counter. The expected number of tries is 2; the vectors include cases with c = 1 and c = 7.
- **Soundness:** the tweak is public, and only the public ỹ bit steers the choice.
- **Alternative:** negating the tweaked key and every share is also sound, by linearity, but was not chosen. That keeps one formula for `s_i,app`.
- **`rk`:** it has no sign rule.

### 5.7 What every operator holds

- **`K`, hence every app's `qsk`.**
  - ZIP 2005 notes that a quantum adversary holding `qsk` (and able to break discrete logs) could steal recoverable funds without the RedDSA threshold.
  - With a single `K`, compromising **one** operator's `K` puts every app at that risk.
  - Before quantum attacks, `K` only grants viewing.
- **Session receivers are linkable to the FVK holder.**
  - The diversifier index is public-derivable from `app_id` and the session key, both public on Solana.
  - Mapping an index to a receiver needs `dk` (from the FVK). Holders of an app's FVK can therefore link session receivers to Solana session keys. That is expected for operators and auditors.
- **Padding outputs** are readable by any FVK holder through the padding OVK, but they carry nothing.

## 6. Assumptions

- **ZIP 312** (Draft) rerandomized FROST, with reddsa's FROST(Pallas, BLAKE2b-512) ciphersuite:
  - DKG security;
  - single-use nonces;
  - signers check the sighash themselves;
  - α confidential.
- **ZIP 2005** (Proposed): the § 4.2.3 derivations, `use_qsk = true`, and the "Usage with FROST" rule that every participant derives the same `nk`, `qsk` and `rivk_ext` from an agreed `sk`. In ZOL that is `sk_c`, from the shared `K`.
- **RedPallas** EUF-CMA under key rerandomization, with key-prefixed challenges.
- **BLAKE2b and BLAKE3** as PRFs and random oracles in their personalised and keyed uses; `ToScalar`/`ToBase` bias is negligible.
- **ZIP 326:** `use_qsk = true` keys are generated only after NU6.3 Mainnet activation (2026-07-28), which holds. Senders need only the 43-byte receiver, so payments from `use_qsk = false` wallets work (shown on regtest).

## 7. Open questions for the reviewer

1. Is the per-app ZIP 2005 `sk` acceptable: `sk_c` derived from a shared `K` rather than agreed per vault in a DKG, as COCKTAIL-DKG would do (zcash/zips#895)? Will such vaults pass the Recovery Protocol? Upstream answer so far (frost#1094): "very unlikely" to change, but not committed.
2. Is coordinator-chosen α (§ 5.3) acceptable for unforgeability? Should ZOL move to commitment-derived α before mainnet, if open ZIP 312 PR #895 lands as written?
3. Is the nullifier binding (§ 5.2) a complete defence against cross-app relabelling, given that all apps share one signer set?
4. **Composition:** rerandomized FROST plus a public per-app additive tweak. Is a proof by reduction to rerandomized FROST unforgeability straightforward, or does the tweak's dependence on `ak_group` matter?
5. **Single `K`:** should `K` be split per app or threshold-shared, given the § 5.7 quantum caveat? Is the counter rejection (`ỹ` and FVK validity) unbiased enough?
6. **Domain separation:** the two 16-byte personalizations `ZOL_app_ak_tweak` and `ZOL_app_view_key`, the version byte and the network byte. Is anything missing?

## 8. Vectors

`crates/zol-zcash/tests/vectors/zol_keys_v1.json`:

- **Inputs:** `ak_group = [0x5a5a5a5a]G`, negated to ỹ = 0; `K = 0x11…11`.
- **Cases:** 3 app IDs (`a1…a1`, `b2…b2`, `01…01`) × mainnet, testnet and regtest = 9 cases. 5 of them need a counter retry (11 rejected counters in all), including one with c = 7.
- **Per case:**
  - rejected counters (tweak and candidate `ak`);
  - accepted `counter`, `tweak`, `ak_app`;
  - `view_secret` (`sk_c`), `nk`, `qsk`, `qk`, `rivk_ext`, `fvk`;
  - external and internal OVK, `padding_ovk`;
  - vault and head addresses;
  - two session receivers with their indices.

`frozen_vectors` fails if any value changes. A change requires bumping `KEY_DERIVATION_VERSION` and regenerating with `ZOL_REGEN_VECTORS=1 cargo test --test key_vectors frozen_vectors`.

`tests/vectors/zip2005_use_qsk_zafe.json` holds the third-party ZIP 2005 vectors (MIT/Apache-2.0, from zafe-cash/zafe 6baa678). They are not official.

Example (mainnet, `app_id = a1…a1`, c = 0):

```text
tweak      5eb2d7f72b48d8662884e299a5169b52d158f31f6acf542ad2de8c8f0e356727
ak_app     12a65d8963c270d4242a26952439ff8621a7f545515b851bf69e82c73803ab11
sk_c       1588e6eadecc0f463291a71d71e2a737bea76122aba32ba97d21744fe656ba50
nk         91318d7db164b3b71877f987468af4c8d3e9305e9c8bf9902c48cdb80ca5f506
qsk        040b495a8ee1cdab741a44f42a4479fa11e932715a0e717b1c2a86d19477e501
qk         76aedc6a87f3be0e9dd4215760dc2585f6b725294ce3d93b2e0c49fc8ab2f6c8
rivk_ext   8fd08f362d96cd151293430382529e3148b9c330ae11359c6281f9c1aaad882e
vault addr ae41b70729a88447bfb9a49d31ef0d5aaa5faab6cb3e8bea2cfe9cf5525861254311aded270bd57e383eb2
```

Reproduce with `cd crates/zol-zcash && cargo test --test key_vectors -- --nocapture`. That runs the Rust re-derivation and the Python verifier (`python3 -I tests/vectors/verify_zol_keys.py tests/vectors/zol_keys_v1.json tests/vectors/zip2005_use_qsk_zafe.json`).
