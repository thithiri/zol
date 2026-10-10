//! Offline: a real Zebra 6.4.2 regtest block (Ironwood shielded coinbase to the fixed key
//! `SpendingKey([7; 32])`, captured by `regtest_make_block_fixture`), and the JSON-RPC
//! client against a canned local HTTP responder.

use orchard::keys::{FullViewingKey, Scope, SpendingKey};
use zol_zcash::{
    block::parse_block,
    display_hex,
    scan::{parse_legacy_tree, Account, Pool, ScannerConfig},
    zebra::ZebraClient,
    ChainParams, Error, Scanner,
};

fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!("fixtures/coinbase_block.json")).unwrap()
}

#[test]
fn parses_and_scans_real_regtest_block() {
    let f = fixture();
    let raw = hex::decode(f["raw"].as_str().unwrap()).unwrap();
    let b = parse_block(&raw, &ChainParams::REGTEST).unwrap();
    let height = f["height"].as_u64().unwrap() as u32;
    assert_eq!(b.height, height);
    assert_eq!(display_hex(&b.hash), f["hash"].as_str().unwrap());
    assert_eq!(display_hex(&b.prev_hash), f["prev_hash"].as_str().unwrap());
    let cb = &b.txs[0];
    assert!(cb.ironwood_bundle().is_some() && cb.orchard_bundle().is_none());

    let fvk = FullViewingKey::from(&SpendingKey::from_bytes([7; 32]).unwrap());
    let frontier =
        parse_legacy_tree(&hex::decode(f["ironwood_final_state_prev"].as_str().unwrap()).unwrap())
            .unwrap();
    let mut sc = Scanner::new(
        ScannerConfig::new(ChainParams::REGTEST),
        vec![Account { id: [7; 32], fvk }],
        height,
        Some(b.prev_hash),
        Some(frontier),
    )
    .unwrap();
    let ev = sc.ingest_block(&raw).unwrap();
    assert_eq!(ev.received.len(), 1);
    let n = &ev.received[0];
    assert_eq!(
        (n.pool, n.value, n.scope, n.receiver_index),
        (Pool::Ironwood, 625_000_000, Scope::External, [0; 11])
    );
    // The witness roots to the tree state after this block.
    let sp = sc.spendable(&n.cmx, None).unwrap();
    assert_eq!(sp.anchor_height, height);
    // Re-ingesting the same height fails; a block not linked to the tip is a reorg.
    assert!(sc.ingest_block(&raw).is_err());
}

#[test]
fn wrong_prev_hash_is_a_reorg() {
    let f = fixture();
    let raw = hex::decode(f["raw"].as_str().unwrap()).unwrap();
    let height = f["height"].as_u64().unwrap() as u32;
    let fvk = FullViewingKey::from(&SpendingKey::from_bytes([7; 32]).unwrap());
    let frontier =
        parse_legacy_tree(&hex::decode(f["ironwood_final_state_prev"].as_str().unwrap()).unwrap())
            .unwrap();
    let mut sc = Scanner::new(
        ScannerConfig::new(ChainParams::REGTEST),
        vec![Account { id: [7; 32], fvk }],
        height,
        Some([1; 32]),
        Some(frontier),
    )
    .unwrap();
    assert!(matches!(sc.ingest_block(&raw), Err(Error::Reorg { .. })));
}

/// Serves one canned HTTP response per connection and returns the last request body.
async fn serve(responses: Vec<String>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", l.local_addr().unwrap());
    let h = tokio::spawn(async move {
        let mut bodies = vec![];
        for body in responses {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = vec![0u8; 65536];
            let mut n = 0;
            loop {
                n += s.read(&mut buf[n..]).await.unwrap();
                let txt = String::from_utf8_lossy(&buf[..n]).to_string();
                if let Some(i) = txt.find("\r\n\r\n") {
                    let len: usize = txt
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    if n >= i + 4 + len {
                        bodies.push(txt[i + 4..].to_string());
                        break;
                    }
                }
            }
            let resp = format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
            s.write_all(resp.as_bytes()).await.unwrap();
        }
        bodies
    });
    (url, h)
}

#[tokio::test]
async fn rpc_envelope_results_and_errors() {
    let (url, h) = serve(vec![
        r#"{"jsonrpc":"2.0","id":1,"result":215}"#.into(),
        r#"{"jsonrpc":"2.0","id":1,"result":"00ab"}"#.into(),
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":-26,"message":"bad-txns"}}"#.into(),
        r#"{"jsonrpc":"2.0","id":1,"result":{"chain":"test","blocks":5,"bestblockhash":"aa","consensus":{"chaintip":"37a5165b"},"valuePools":[{"id":"ironwood","chainValueZat":625000000}]}}"#.into(),
    ])
    .await;
    let c = ZebraClient::new(url);
    assert_eq!(c.get_block_count().await.unwrap(), 215);
    assert_eq!(
        c.get_block_raw_by_height(3).await.unwrap(),
        vec![0x00, 0xab]
    );
    let e = c.send_raw_transaction(&[1, 2]).await.unwrap_err();
    assert!(e.to_string().contains("bad-txns"));
    let info = c.get_blockchain_info().await.unwrap();
    assert_eq!(info.chaintip_branch().as_deref(), Some("37a5165b"));
    assert_eq!(info.pool_zat("ironwood"), Some(625_000_000));
    let bodies = h.await.unwrap();
    let req: serde_json::Value = serde_json::from_str(&bodies[1]).unwrap();
    assert_eq!(req["method"], "getblock");
    assert_eq!(req["params"], serde_json::json!(["3", 0]));
    let req: serde_json::Value = serde_json::from_str(&bodies[2]).unwrap();
    assert_eq!(req["params"], serde_json::json!(["0102"]));
}
