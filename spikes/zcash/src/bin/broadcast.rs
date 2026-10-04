//! Broadcast stub for spike 0.3.
//!
//! Usage:
//!   broadcast <raw-tx-hex-file> <endpoint> [--send]
//!
//! <endpoint>:
//!   zebra+http://HOST:PORT      Zebra / zcashd JSON-RPC `sendrawtransaction` (plain HTTP)
//!   lightwalletd://HOST:PORT    lightwalletd gRPC `CompactTxStreamer/SendTransaction`
//!                               (prints a grpcurl command; no gRPC client is linked)
//!
//! The transaction is always verified offline first (proof, spend-auth and binding
//! signatures against the recomputed ZIP 244 sighash). Without `--send` nothing leaves
//! the machine: the request that would be sent is printed.
//!
//! We have no funded testnet ZEC and no synced node, so this was never run with --send.

use std::io::{Read, Write};

use zcash_protocol::consensus::BranchId;
use zol_zcash_spikes::{pool::Pool, verify};

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: broadcast <raw-tx-hex-file> <zebra+http://HOST:PORT | lightwalletd://HOST:PORT> [--send]");
        std::process::exit(2);
    }
    let send = args.iter().any(|a| a == "--send");
    let hex_str = std::fs::read_to_string(&args[1]).expect("read tx file");
    let raw = hex::decode(hex_str.trim()).expect("hex");
    if raw.len() < 12 {
        panic!("too short");
    }
    // v5/v6 header: header(4) | nVersionGroupId(4) | nConsensusBranchId(4) | ...
    let branch = u32::from_le_bytes(raw[8..12].try_into().unwrap());
    let pool = match BranchId::try_from(branch) {
        Ok(BranchId::Nu6_2) => Pool::OrchardNu62,
        Ok(BranchId::Nu6_3) => Pool::IronwoodNu63,
        other => panic!("unsupported branch id {branch:#x}: {other:?}"),
    };
    let rep = verify::verify_raw(&raw, pool).unwrap_or_else(|e| panic!("offline verification failed: {e}"));
    println!("offline verification OK: txid {} ({} actions, {} bytes, {:?})", rep.txid, rep.n_actions, rep.raw_len, pool);

    let ep = &args[2];
    if let Some(hostport) = ep.strip_prefix("zebra+http://") {
        let body = format!(
            r#"{{"jsonrpc":"2.0","id":"zol","method":"sendrawtransaction","params":["{}"]}}"#,
            hex::encode(&raw)
        );
        let req = format!(
            "POST / HTTP/1.1\r\nHost: {hostport}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        if !send {
            println!("[dry run] would POST to http://{hostport}/ :\n{}...", &req[..req.len().min(400)]);
            return;
        }
        let mut s = std::net::TcpStream::connect(hostport).expect("connect");
        s.write_all(req.as_bytes()).unwrap();
        let mut resp = String::new();
        s.read_to_string(&mut resp).unwrap();
        println!("{resp}");
    } else if let Some(hostport) = ep.strip_prefix("lightwalletd://") {
        let cmd = format!(
            "grpcurl -d '{{\"data\":\"{}\",\"height\":0}}' {hostport} cash.z.wallet.sdk.rpc.CompactTxStreamer/SendTransaction",
            b64(&raw)
        );
        println!("{}", if send { "run:" } else { "[dry run] command:" });
        println!("{}", &cmd[..cmd.len().min(300)]);
        println!("... ({} chars total)", cmd.len());
    } else {
        panic!("unknown endpoint scheme: {ep}");
    }
}
