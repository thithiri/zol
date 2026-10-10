// ZOL patch: enable this crate's NU7 code paths (`cfg(zcash_unstable = "nu7")`) for this
// crate only, instead of a global RUSTFLAGS cfg. See crates/zol-zcash/vendor/VENDOR_PATCHES.md.
fn main() {
    println!("cargo:rustc-cfg=zcash_unstable=\"nu7\"");
}
