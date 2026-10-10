//! Writes web/test/ua-vectors.json: Orchard-only unified addresses from
//! zol_zcash::keys::encode_ua, and multi-receiver UAs built with zcash_address
//! (the library encode_ua uses), for the page's UA decoder tests.
use orchard::keys::{FullViewingKey, Scope, SpendingKey};
use rand::{rngs::StdRng, RngCore, SeedableRng};
use zcash_address::unified::{self, Encoding};
use zol_zcash::keys::encode_ua;
use zol_zcash::params::ChainParams;

fn main() {
    let mut rng = StdRng::seed_from_u64(0x5a01);
    let nets = [("mainnet", ChainParams::MAINNET), ("testnet", ChainParams::TESTNET), ("regtest", ChainParams::REGTEST)];
    let mut out = Vec::new();
    for (name, params) in nets {
        for i in 0..4u32 {
            let sk = loop {
                let mut b = [0u8; 32];
                rng.fill_bytes(&mut b);
                if let Some(sk) = Option::<SpendingKey>::from(SpendingKey::from_bytes(b)) {
                    break sk;
                }
            };
            let fvk = FullViewingKey::from(&sk);
            let recv = fvk.address_at(i, Scope::External).to_raw_address_bytes();
            out.push(serde_json::json!({"network": name, "kind": "orchard-only (encode_ua)", "ua": encode_ua(&params, &recv), "orchard": hex::encode(recv)}));

            // Orchard + Sapling-shaped + P2PKH items (receivers need not be valid points for the
            // UA encoding itself; the decoder must just find the Orchard item).
            let mut sapling = [0u8; 43];
            rng.fill_bytes(&mut sapling);
            let mut p2pkh = [0u8; 20];
            rng.fill_bytes(&mut p2pkh);
            let ua = unified::Address::try_from_items(vec![
                unified::Receiver::Orchard(recv),
                unified::Receiver::Sapling(sapling),
                unified::Receiver::P2pkh(p2pkh),
            ])
            .unwrap();
            use zcash_protocol::consensus::Parameters;
            out.push(serde_json::json!({"network": name, "kind": "orchard+sapling+p2pkh", "ua": ua.encode(&params.network_type()), "orchard": hex::encode(recv)}));
        }
        // A UA without an Orchard receiver: the decoder must refuse it.
        let mut sapling = [0u8; 43];
        rng.fill_bytes(&mut sapling);
        let mut p2pkh = [0u8; 20];
        rng.fill_bytes(&mut p2pkh);
        let ua = unified::Address::try_from_items(vec![unified::Receiver::Sapling(sapling), unified::Receiver::P2pkh(p2pkh)]).unwrap();
        use zcash_protocol::consensus::Parameters;
        out.push(serde_json::json!({"network": name, "kind": "no-orchard", "ua": ua.encode(&params.network_type()), "orchard": null}));
    }
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}
