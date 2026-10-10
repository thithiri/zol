//! Text transport for the deposit memo (spike 0.10).
//!
//! Most wallets drop or mangle a binary `0xFF` memo taken from a ZIP 321 link, so
//! the page's link carries a text memo instead:
//!
//! ```text
//! "ZOL1:" ‖ base64url_nopad( canonical_memo[1..] with trailing zeros stripped )
//! ```
//!
//! It starts with ASCII, so wallets treat it as a UTF-8 text memo, and it fits
//! Zingo's 511-byte text limit. The canonical 512-byte binary memo (and so the
//! session signature and every digest) is unchanged; the scanner accepts either form.

use crate::memo::{classify, DepositMemo, MemoError, MemoKind, MEMO_ARBITRARY, OFF_DATA};
use crate::MEMO_LEN;

pub const TEXT_PREFIX: &[u8; 5] = b"ZOL1:";
/// Longest text memo some wallets accept.
pub const TEXT_MAX: usize = 511;
/// Longest payload (canonical memo without its first byte) that fits in `TEXT_MAX`.
pub const TEXT_PAYLOAD_MAX: usize = (TEXT_MAX - TEXT_PREFIX.len()) * 3 / 4;
/// App data that always fits in the text form.
pub const TEXT_APP_DATA_MAX: usize = TEXT_PAYLOAD_MAX - (OFF_DATA - 1);

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn b64_val(c: u8) -> Option<u8> {
    ALPHABET.iter().position(|x| *x == c).map(|p| p as u8)
}

/// Unpadded base64url into `out`; returns the length written.
pub fn b64url_encode(input: &[u8], out: &mut [u8]) -> Option<usize> {
    let n = (input.len() * 4).div_ceil(3);
    if n > out.len() {
        return None;
    }
    let mut o = 0;
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let v = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..chunk.len() + 1 {
            out[o] = ALPHABET[(v >> (18 - 6 * i) & 63) as usize];
            o += 1;
        }
    }
    Some(o)
}

/// Strict unpadded base64url decode: only the canonical encoding of a byte string
/// is accepted (checked by re-encoding).
pub fn b64url_decode(input: &[u8], out: &mut [u8]) -> Option<usize> {
    if input.len() % 4 == 1 {
        return None;
    }
    let mut o = 0;
    for chunk in input.chunks(4) {
        let mut v: u32 = 0;
        for (i, c) in chunk.iter().enumerate() {
            v |= (b64_val(*c)? as u32) << (18 - 6 * i);
        }
        for i in 0..chunk.len() - 1 {
            *out.get_mut(o)? = (v >> (16 - 8 * i)) as u8;
            o += 1;
        }
    }
    let mut check = [0u8; 4 * MEMO_LEN / 3 + 4];
    let n = b64url_encode(&out[..o], &mut check)?;
    (check[..n] == *input).then_some(o)
}

#[test]
fn b64_rejects_non_canonical() {
    let mut out = [0u8; 4];
    assert_eq!(b64url_decode(b"QQ", &mut out), Some(1)); // "A"
    assert_eq!(b64url_decode(b"QR", &mut out), None); // same byte, stray low bits
    assert_eq!(b64url_decode(b"Q", &mut out), None);
    assert_eq!(b64url_decode(b"QQ==", &mut out), None);
}

impl DepositMemo {
    /// The text memo, zero padded to 512 bytes, and its length before padding.
    /// Fails with `AppDataTooLong` if it would exceed `TEXT_MAX`.
    pub fn encode_text(&self) -> Result<([u8; MEMO_LEN], usize), MemoError> {
        let canon = self.encode();
        let mut end = MEMO_LEN;
        while end > 1 && canon[end - 1] == 0 {
            end -= 1;
        }
        let payload = &canon[1..end];
        let mut out = [0u8; MEMO_LEN];
        out[..TEXT_PREFIX.len()].copy_from_slice(TEXT_PREFIX);
        let n = b64url_encode(payload, &mut out[TEXT_PREFIX.len()..TEXT_MAX]).ok_or(MemoError::AppDataTooLong)?;
        Ok((out, TEXT_PREFIX.len() + n))
    }

    /// Decodes a deposit memo in either form: canonical binary, or `ZOL1:` text.
    pub fn decode_any(m: &[u8; MEMO_LEN]) -> Result<Self, MemoError> {
        if m[0] == MEMO_ARBITRARY {
            return Self::decode(m);
        }
        if &m[..TEXT_PREFIX.len()] != TEXT_PREFIX {
            return Err(MemoError::NotArbitraryData);
        }
        let mut end = MEMO_LEN;
        while end > TEXT_PREFIX.len() && m[end - 1] == 0 {
            end -= 1;
        }
        let mut canon = [0u8; MEMO_LEN];
        canon[0] = MEMO_ARBITRARY;
        b64url_decode(&m[TEXT_PREFIX.len()..end], &mut canon[1..]).ok_or(MemoError::BadText)?;
        if classify(&canon)? != MemoKind::DepositV2 {
            return Err(MemoError::UnknownKind(canon[4]));
        }
        Self::decode(&canon)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memo::APP_DATA_MAX;
    use proptest::prelude::*;

    fn memo(data: &[u8]) -> DepositMemo {
        let mut m = DepositMemo::new(1, [0xAB; 32], [0xCD; 43], 7, [0; 32], data).unwrap();
        m.set_signature([0x5A; 64]);
        m
    }

    #[test]
    fn limits() {
        assert_eq!(TEXT_PAYLOAD_MAX, 379);
        assert_eq!(TEXT_APP_DATA_MAX, 197);
        let (t, n) = memo(&[1; TEXT_APP_DATA_MAX]).encode_text().unwrap();
        assert!(n <= TEXT_MAX && t[n..].iter().all(|b| *b == 0));
        assert!(t[..n].iter().all(|b| b.is_ascii()));
        assert_eq!(memo(&[1; TEXT_APP_DATA_MAX + 1]).encode_text(), Err(MemoError::AppDataTooLong));
        // Trailing zeros in app data are stripped and restored.
        let m = memo(&[0; APP_DATA_MAX]);
        assert_eq!(DepositMemo::decode_any(&m.encode_text().unwrap().0).unwrap(), m);
    }

    #[test]
    fn rejects_bad_text() {
        let (mut t, n) = memo(b"x").encode_text().unwrap();
        t[n - 1] = b'=';
        assert!(DepositMemo::decode_any(&t).is_err());
        let mut t2 = [0u8; MEMO_LEN];
        t2[..5].copy_from_slice(b"ZOL2:");
        assert!(DepositMemo::decode_any(&t2).is_err());
    }

    proptest! {
        #[test]
        fn text_roundtrip(data in proptest::collection::vec(any::<u8>(), 0..=TEXT_APP_DATA_MAX), sig in any::<[u8; 32]>()) {
            let mut m = memo(&data);
            let mut s = [0u8; 64];
            s[..32].copy_from_slice(&sig);
            m.set_signature(s);
            let (t, _) = m.encode_text().unwrap();
            prop_assert_eq!(&DepositMemo::decode_any(&t).unwrap(), &m);
            prop_assert_eq!(&DepositMemo::decode_any(&m.encode()).unwrap(), &m);
        }

        #[test]
        fn b64_roundtrip(bytes in proptest::collection::vec(any::<u8>(), 0..400)) {
            let mut e = [0u8; 600];
            let n = b64url_encode(&bytes, &mut e).unwrap();
            let mut d = [0u8; 400];
            let k = b64url_decode(&e[..n], &mut d).unwrap();
            prop_assert_eq!(&d[..k], &bytes[..]);
        }

        #[test]
        fn decode_any_never_panics(bytes in proptest::collection::vec(any::<u8>(), MEMO_LEN)) {
            let m: [u8; MEMO_LEN] = bytes.try_into().unwrap();
            let _ = DepositMemo::decode_any(&m);
        }
    }
}
