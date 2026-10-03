//! Golden vectors shared with other implementations (the web page's TypeScript).
//! Regenerate with `ZOL_REGEN=1 cargo test -p zol-wire --test vectors`.

use ed25519_dalek::{Signer, SigningKey};
use serde_json::{json, Value};
use zol_wire::digest::{paid_digest, record_digest, records_digest, void_digest, Sha256};
use zol_wire::keyed::{deposit_id, payout_commitment};
use zol_wire::memo::FLAG_AUTO_EXIT;
use zol_wire::*;

fn build() -> Value {
    let sk = SigningKey::from_bytes(&[0x11; 32]);
    let session = sk.verifying_key().to_bytes();
    let payout_addr = [0x22u8; ORCHARD_ADDR_LEN];
    let vault = [0x33u8; ORCHARD_ADDR_LEN];
    let app = [0x44u8; 32];
    let app_data = b"table=7;seat=any";
    let mut memo = DepositMemo::new(FLAG_AUTO_EXIT, session, payout_addr, 3_000_000, [0; 32], app_data).unwrap();
    let msg = memo.signing_message(Network::Testnet, &vault);
    memo.set_signature(sk.sign(&msg).to_bytes());

    let memo_text = memo.encode_text().unwrap();
    assert_eq!(DepositMemo::decode_any(&memo_text.0).unwrap(), memo);

    let gw_dep = [0x55u8; 32];
    let gw_pay = [0x66u8; 32];
    let cmx = [0x77u8; 32];
    let dep_id = deposit_id(&gw_dep, &cmx);
    let pcmt = payout_commitment(&gw_pay, &payout_addr, &session, &app);
    let credit = CreditAttestation {
        network: Network::Testnet as u8,
        app,
        deposit_id: dep_id,
        amount: 10_000_000,
        session_key: session,
        payout_commitment: pcmt,
        flags: memo.flags,
        pot_id: memo.pot_id,
        app_data: memo.app_data(),
    };
    let payouts = [Payout { commitment: pcmt, amount: 9_960_000 }, Payout { commitment: [0x88; 32], amount: 1 }];
    let r0 = record_digest::<Sha256>(&app, 0, &payouts);
    let r1 = record_digest::<Sha256>(&app, 1, &payouts[..1]);
    let rs = records_digest::<Sha256>(&app, 0, &[r0, r1]);
    let head0 = HeadMemo::genesis(Network::Testnet, app);
    let head1 = head0
        .advance(&Transition { records_paid: 2, records_digest: rs, network_fee: 15_000, fee_topups_swept: 0 })
        .unwrap();
    let topup = TopUpMemo { network: Network::Testnet, app, purpose: TopUpPurpose::FeePool };

    json!({
        "inputs": {
            "session_secret": hex::encode([0x11u8; 32]),
            "session_key": hex::encode(session),
            "payout_address": hex::encode(payout_addr),
            "vault_address": hex::encode(vault),
            "app": hex::encode(app),
            "network": Network::Testnet as u8,
            "expiry_height": 3_000_000u32,
            "flags": FLAG_AUTO_EXIT,
            "app_data_utf8": std::str::from_utf8(app_data).unwrap(),
            "gateway_deposit_key": hex::encode(gw_dep),
            "gateway_payout_key": hex::encode(gw_pay),
            "note_commitment": hex::encode(cmx),
            "amount": 10_000_000u64,
        },
        "memo_signing_message": hex::encode(msg),
        "memo": hex::encode(memo.encode()),
        "memo_text": std::str::from_utf8(&memo_text.0[..memo_text.1]).unwrap(),
        "deposit_id": hex::encode(dep_id),
        "payout_commitment": hex::encode(pcmt),
        "credit_digest": hex::encode(credit.digest::<Sha256>()),
        "void_digest": hex::encode(void_digest::<Sha256>(1, &app, &dep_id)),
        "paid_digest_seq0": hex::encode(paid_digest::<Sha256>(1, &app, 0)),
        "record_digest_0": hex::encode(r0),
        "record_digest_1": hex::encode(r1),
        "records_digest": hex::encode(rs),
        "head_genesis": hex::encode(head0.encode()),
        "head_1": hex::encode(head1.encode()),
        "topup_fee": hex::encode(topup.encode()),
    })
}

#[test]
fn golden_vectors() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/vectors/v0.3.json");
    let v = build();
    if std::env::var("ZOL_REGEN").is_ok() {
        std::fs::write(path, serde_json::to_string_pretty(&v).unwrap() + "\n").unwrap();
    }
    let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(path).expect("run with ZOL_REGEN=1 once")).unwrap();
    assert_eq!(on_disk, v, "wire format changed; bump a version and regenerate deliberately");
}
