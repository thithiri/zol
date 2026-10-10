//! Memos on outputs the vault pays to players (design v0.3.1, "fixed payout memo
//! format"). Plain UTF-8 text so wallets display them, fully determined by the
//! transaction's purpose so every signer can recompute the exact bytes:
//!
//! ```text
//! payout: "ZOL payout v1 app=<first 8 bytes of app id, hex> seq=<settlement sequence>"
//! refund: "ZOL refund v1 app=<first 8 bytes of app id, hex> note=<first 8 bytes of cmx, hex>"
//! ```
//!
//! zero padded to 512 bytes. They carry nothing a recipient doesn't already know,
//! and only the recipient can decrypt them.

use crate::MEMO_LEN;

const HEX: &[u8; 16] = b"0123456789abcdef";

struct Buf {
    b: [u8; MEMO_LEN],
    n: usize,
}

impl Buf {
    fn new() -> Self {
        Self { b: [0; MEMO_LEN], n: 0 }
    }
    fn put(&mut self, s: &[u8]) {
        self.b[self.n..self.n + s.len()].copy_from_slice(s);
        self.n += s.len();
    }
    fn hex8(&mut self, v: &[u8; 32]) {
        for x in &v[..8] {
            self.put(&[HEX[(x >> 4) as usize], HEX[(x & 15) as usize]]);
        }
    }
    fn dec(&mut self, mut v: u64) {
        let mut d = [0u8; 20];
        let mut i = d.len();
        loop {
            i -= 1;
            d[i] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        self.put(&d[i..]);
    }
}

/// Memo on every payout output of settlement record `sequence`.
pub fn payout_memo(app: &[u8; 32], sequence: u64) -> [u8; MEMO_LEN] {
    let mut b = Buf::new();
    b.put(b"ZOL payout v1 app=");
    b.hex8(app);
    b.put(b" seq=");
    b.dec(sequence);
    b.b
}

/// Memo on the output of a refund of the deposit note `cmx`.
pub fn refund_memo(app: &[u8; 32], cmx: &[u8; 32]) -> [u8; MEMO_LEN] {
    let mut b = Buf::new();
    b.put(b"ZOL refund v1 app=");
    b.hex8(app);
    b.put(b" note=");
    b.hex8(cmx);
    b.b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(m: &[u8; MEMO_LEN]) -> &str {
        let end = m.iter().position(|b| *b == 0).unwrap_or(MEMO_LEN);
        assert!(m[end..].iter().all(|b| *b == 0));
        core::str::from_utf8(&m[..end]).unwrap()
    }

    #[test]
    fn formats() {
        let mut app = [0u8; 32];
        app[..8].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef, 0, 1, 2, 3]);
        assert_eq!(text(&payout_memo(&app, 0)), "ZOL payout v1 app=deadbeef00010203 seq=0");
        assert_eq!(text(&payout_memo(&app, u64::MAX)), "ZOL payout v1 app=deadbeef00010203 seq=18446744073709551615");
        assert_eq!(text(&refund_memo(&app, &[0xab; 32])), "ZOL refund v1 app=deadbeef00010203 note=abababababababab");
        // A text memo per ZIP 302: first byte <= 0xF4.
        assert!(payout_memo(&app, 1)[0] <= 0xF4);
    }
}
