# Vendored Zcash crates: NU7 (ZIP 259) on the pinned stack

These are the crates.io releases ZOL pins, copied unchanged except for the patches below.
They are wired in through `[patch.crates-io]` in `../Cargo.toml`, which applies only to the
zol-zcash workspace. Remove the directory and the `[patch]` table when zol-zcash moves to
`zcash_protocol` 0.11 / `zcash_primitives` 0.31 / `pczt` 0.10 (see `docs/ops/nu7.md`).

| Crate | Version | Upstream commit (`.cargo_vcs_info.json`) | Cargo.lock checksum of the original |
| --- | --- | --- | --- |
| zcash_protocol | 0.10.6 | 28cf1143f932dae94d8626fc0d26a6c61b08823c | b20adb1cdb6ab551e83d1507461e89b41fc67e800eec9e59e0c8de96152e8c5c |
| zcash_primitives | 0.30.1 | 97aefdc39a037da9c4f19a0e8a450d2c7932f53e | 403d5be1e96339534be098e3377fb8a78d68ca7585b1780133d884b810277418 |
| pczt | 0.9.3 | 202f173b36f5eca9a7adbe1010f41bd8b8bb4535 | a5592f4f3eba7f9344cc423f45b6e65911e0630c9b89968d5a20792aadd5a0eb |

To check the copies, diff each one against the registry source of the same version, for
example `~/.cargo/registry/src/index.crates.io-*/zcash_protocol-0.10.6`. The only
differences are those listed here.

## Patches

1. **The NU7 code paths are enabled per crate, not globally.**
   - A new `build.rs` in each of the three crates prints
     `cargo:rustc-cfg=zcash_unstable="nu7"`.
   - Each `Cargo.toml` changes `build = false` to `build = "build.rs"`.
   - The three crates already declare this cfg value in their `check-cfg` lints. No other
     crate in this build checks a `zcash_unstable` cfg. That was checked across every
     registry package in `Cargo.lock`, including orchard 0.15.5, sapling-crypto 0.7,
     zcash_transparent 0.10, zcash_address 0.13 and zcash_script 0.4.
   - A global `RUSTFLAGS` cfg would work too, but it is silently dropped whenever
     `RUSTFLAGS` is set in the environment, and it would reach every crate.
2. **zcash_protocol gets the ZIP 259 constants:**
   - `BranchId::Nu7` is `0x77190AD9` in both directions; it was the placeholder
     `0xffff_ffff`.
   - `TEST_NETWORK` activates NU7 at 4,465,026; it was `None`.
   - Mainnet stays `None`.
   - These are the values of `zcash_protocol` 0.11.0-pre.0 and of Zebra 7.0.0-rc.0 /
     Zakura 1.6.0.

## What `zcash_unstable = "nu7"` turns on in these three crates

All of it was audited; nothing else is gated.

**zcash_protocol**
- The `NetworkUpgrade::Nu7` and `BranchId::Nu7` variants and the `LocalNetwork::nu7` field.
- Branch height ranges, with NU6.3 ending at NU7.
- `has_sprout() = false`, matching ZIP 2003. `has_sapling` and `has_orchard` are true.
- `orchard_protocol_revision() = V3`, the NU6.3 circuit, unchanged. This matches 0.11.0-pre.0.

**zcash_primitives**
- `TxVersion::suggested_for_branch(Nu7) = V6`.
- `valid_in_branch`:
  - v4 is invalid at NU7, matching ZIP 2003;
  - v5 and v6 are valid.
- The builder allows the Ironwood pool under NU7.
- Everything else behind the cfg is ZIP 233 (`zip233Amount`). It is gated
  `all(zcash_unstable = "nu7", feature = "zip-233")`, and the `zip-233` feature is **off**,
  so the v6 format, txid and sighash are byte-for-byte the NU6.3 ones apart from the branch
  ID. NU7 does not deploy ZIP 233 (ZIP 259).
- Tests.

**pczt**
- The Creator maps `Nu7` to v6.
- The Updater's anchor update accepts `Nu7`.
- The ZIP 233 field is again behind the `zip-233` feature, which is off.

## Diff against upstream

```diff
diff --git a/zcash_protocol-0.10.6/Cargo.toml b/zcash_protocol-0.10.6/Cargo.toml
index 69a26a2..fb5c24a 100644
--- a/zcash_protocol-0.10.6/Cargo.toml
+++ b/zcash_protocol-0.10.6/Cargo.toml
@@ -18,7 +18,7 @@ authors = [
     "Jack Grigg <jack@electriccoin.co>",
     "Kris Nuttycombe <kris@nutty.land>",
 ]
-build = false
+build = "build.rs" # ZOL patch: was false
 autolib = false
 autobins = false
 autoexamples = false
diff --git a/zcash_protocol-0.10.6/build.rs b/zcash_protocol-0.10.6/build.rs
new file mode 100644
index 0000000..4fe4176
--- /dev/null
+++ b/zcash_protocol-0.10.6/build.rs
@@ -0,0 +1,5 @@
+// ZOL patch: enable this crate's NU7 code paths (`cfg(zcash_unstable = "nu7")`) for this
+// crate only, instead of a global RUSTFLAGS cfg. See crates/zol-zcash/vendor/VENDOR_PATCHES.md.
+fn main() {
+    println!("cargo:rustc-cfg=zcash_unstable=\"nu7\"");
+}
diff --git a/zcash_protocol-0.10.6/src/consensus.rs b/zcash_protocol-0.10.6/src/consensus.rs
index d77bc22..be4f3c7 100644
--- a/zcash_protocol-0.10.6/src/consensus.rs
+++ b/zcash_protocol-0.10.6/src/consensus.rs
@@ -536,7 +536,7 @@ impl Parameters for TestNetwork {
             NetworkUpgrade::Nu6_2 => Some(BlockHeight(4_052_000)),
             NetworkUpgrade::Nu6_3 => Some(BlockHeight(4_134_000)),
             #[cfg(zcash_unstable = "nu7")]
-            NetworkUpgrade::Nu7 => None,
+            NetworkUpgrade::Nu7 => Some(BlockHeight(4_465_026)), // ZOL patch: ZIP 259
             #[cfg(zcash_unstable = "nutachyon")]
             NetworkUpgrade::NuTachyon => None,
         }
@@ -773,7 +773,7 @@ impl TryFrom<u32> for BranchId {
             0x5437_f330 => Ok(BranchId::Nu6_2),
             0x37a5_165b => Ok(BranchId::Nu6_3),
             #[cfg(zcash_unstable = "nu7")]
-            0xffff_ffff => Ok(BranchId::Nu7),
+            0x7719_0ad9 => Ok(BranchId::Nu7), // ZOL patch: ZIP 259
             #[cfg(zcash_unstable = "nutachyon")]
             0xffff_fffc => Ok(BranchId::NuTachyon),
             _ => Err("Unknown consensus branch ID"),
@@ -796,7 +796,7 @@ impl From<BranchId> for u32 {
             BranchId::Nu6_2 => 0x5437_f330,
             BranchId::Nu6_3 => 0x37a5_165b,
             #[cfg(zcash_unstable = "nu7")]
-            BranchId::Nu7 => 0xffff_ffff,
+            BranchId::Nu7 => 0x7719_0ad9, // ZOL patch: ZIP 259
             #[cfg(zcash_unstable = "nutachyon")]
             BranchId::NuTachyon => 0xffff_fffc,
         }
diff --git a/zcash_primitives-0.30.1/Cargo.toml b/zcash_primitives-0.30.1/Cargo.toml
index 4d05f25..736076e 100644
--- a/zcash_primitives-0.30.1/Cargo.toml
+++ b/zcash_primitives-0.30.1/Cargo.toml
@@ -18,7 +18,7 @@ authors = [
     "Jack Grigg <jack@z.cash>",
     "Kris Nuttycombe <kris@electriccoin.co>",
 ]
-build = false
+build = "build.rs" # ZOL patch: was false
 autolib = false
 autobins = false
 autoexamples = false
diff --git a/zcash_primitives-0.30.1/build.rs b/zcash_primitives-0.30.1/build.rs
new file mode 100644
index 0000000..4fe4176
--- /dev/null
+++ b/zcash_primitives-0.30.1/build.rs
@@ -0,0 +1,5 @@
+// ZOL patch: enable this crate's NU7 code paths (`cfg(zcash_unstable = "nu7")`) for this
+// crate only, instead of a global RUSTFLAGS cfg. See crates/zol-zcash/vendor/VENDOR_PATCHES.md.
+fn main() {
+    println!("cargo:rustc-cfg=zcash_unstable=\"nu7\"");
+}
diff --git a/pczt-0.9.3/Cargo.toml b/pczt-0.9.3/Cargo.toml
index 04a39e7..a65647b 100644
--- a/pczt-0.9.3/Cargo.toml
+++ b/pczt-0.9.3/Cargo.toml
@@ -15,7 +15,7 @@ rust-version = "1.88"
 name = "pczt"
 version = "0.9.3"
 authors = ["Jack Grigg <jack@electriccoin.co>"]
-build = false
+build = "build.rs" # ZOL patch: was false
 autolib = false
 autobins = false
 autoexamples = false
diff --git a/pczt-0.9.3/build.rs b/pczt-0.9.3/build.rs
new file mode 100644
index 0000000..4fe4176
--- /dev/null
+++ b/pczt-0.9.3/build.rs
@@ -0,0 +1,5 @@
+// ZOL patch: enable this crate's NU7 code paths (`cfg(zcash_unstable = "nu7")`) for this
+// crate only, instead of a global RUSTFLAGS cfg. See crates/zol-zcash/vendor/VENDOR_PATCHES.md.
+fn main() {
+    println!("cargo:rustc-cfg=zcash_unstable=\"nu7\"");
+}
```
