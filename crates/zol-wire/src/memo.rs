//! Deposit memo, version 2 (design v0.3, "Deposit").
//!
//! | Bytes   | Field              |
//! |---------|--------------------|
//! | 0       | 0xFF               |
//! | 1–3     | "ZOL"              |
//! | 4       | kind/version (2)   |
//! | 5       | flags              |
//! | 6–37    | session public key |
//! | 38–80   | payout address     |
//! | 81–84   | expiry height (LE) |
//! | 85–116  | pot ID (reserved)  |
//! | 117–180 | session signature  |
//! | 181–182 | app data length    |
//! | 183–511 | app data, zero pad |

use crate::{tags, Network, MEMO_LEN, ORCHARD_ADDR_LEN};

pub const MEMO_ARBITRARY: u8 = 0xFF;
pub const TAG: [u8; 3] = *b"ZOL";

pub const KIND_DEPOSIT_V2: u8 = 0x02;
pub const KIND_HEAD_V1: u8 = 0x10;
pub const KIND_TOPUP_V1: u8 = 0x11;

/// Bit 0: auto-exit after cash-out. Other bits reserved and must be zero.
pub const FLAG_AUTO_EXIT: u8 = 0x01;
pub const FLAGS_KNOWN: u8 = FLAG_AUTO_EXIT;

pub const APP_DATA_MAX: usize = 329;

const OFF_FLAGS: usize = 5;
const OFF_SESSION: usize = 6;
const OFF_ADDR: usize = 38;
const OFF_EXPIRY: usize = 81;
const OFF_POT: usize = 85;
const OFF_SIG: usize = 117;
const OFF_LEN: usize = 181;
pub(crate) const OFF_DATA: usize = 183;

/// Length of the session-key signing message: tag, network, vault address,
/// memo bytes 0..117 and 181..512 (everything except the signature itself).
pub const SIGNING_MESSAGE_LEN: usize = 16 + 1 + ORCHARD_ADDR_LEN + OFF_SIG + (MEMO_LEN - OFF_LEN);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoError {
    NotArbitraryData,
    BadTag,
    UnknownKind(u8),
    ReservedFlags,
    AppDataTooLong,
    NonZeroPadding,
    BadNetwork,
    BadPurpose,
    BadSignature,
    BadText,
}

/// What a ZOL memo is, from bytes 0–4.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoKind {
    DepositV2,
    HeadV1,
    TopUpV1,
}

/// Classifies a memo without fully parsing it.
pub fn classify(memo: &[u8; MEMO_LEN]) -> Result<MemoKind, MemoError> {
    if memo[0] != MEMO_ARBITRARY {
        return Err(MemoError::NotArbitraryData);
    }
    if memo[1..4] != TAG {
        return Err(MemoError::BadTag);
    }
    match memo[4] {
        KIND_DEPOSIT_V2 => Ok(MemoKind::DepositV2),
        KIND_HEAD_V1 => Ok(MemoKind::HeadV1),
        KIND_TOPUP_V1 => Ok(MemoKind::TopUpV1),
        k => Err(MemoError::UnknownKind(k)),
    }
}

pub(crate) fn header(kind: u8) -> [u8; 5] {
    [MEMO_ARBITRARY, TAG[0], TAG[1], TAG[2], kind]
}

pub(crate) fn all_zero(b: &[u8]) -> bool {
    b.iter().all(|x| *x == 0)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepositMemo {
    pub flags: u8,
    pub session_key: [u8; 32],
    pub payout_address: [u8; ORCHARD_ADDR_LEN],
    pub expiry_height: u32,
    pub pot_id: [u8; 32],
    pub signature: [u8; 64],
    app_data_len: u16,
    app_data: [u8; APP_DATA_MAX],
}

impl DepositMemo {
    /// Builds an unsigned memo. Call [`DepositMemo::signing_message`], sign it with
    /// the session key, then [`DepositMemo::set_signature`].
    pub fn new(
        flags: u8,
        session_key: [u8; 32],
        payout_address: [u8; ORCHARD_ADDR_LEN],
        expiry_height: u32,
        pot_id: [u8; 32],
        app_data: &[u8],
    ) -> Result<Self, MemoError> {
        if flags & !FLAGS_KNOWN != 0 {
            return Err(MemoError::ReservedFlags);
        }
        if app_data.len() > APP_DATA_MAX {
            return Err(MemoError::AppDataTooLong);
        }
        let mut data = [0u8; APP_DATA_MAX];
        data[..app_data.len()].copy_from_slice(app_data);
        Ok(Self {
            flags,
            session_key,
            payout_address,
            expiry_height,
            pot_id,
            signature: [0u8; 64],
            app_data_len: app_data.len() as u16,
            app_data: data,
        })
    }

    pub fn app_data(&self) -> &[u8] {
        &self.app_data[..self.app_data_len as usize]
    }

    pub fn auto_exit(&self) -> bool {
        self.flags & FLAG_AUTO_EXIT != 0
    }

    pub fn set_signature(&mut self, sig: [u8; 64]) {
        self.signature = sig;
    }

    pub fn encode(&self) -> [u8; MEMO_LEN] {
        let mut m = [0u8; MEMO_LEN];
        m[..5].copy_from_slice(&header(KIND_DEPOSIT_V2));
        m[OFF_FLAGS] = self.flags;
        m[OFF_SESSION..OFF_ADDR].copy_from_slice(&self.session_key);
        m[OFF_ADDR..OFF_EXPIRY].copy_from_slice(&self.payout_address);
        m[OFF_EXPIRY..OFF_POT].copy_from_slice(&self.expiry_height.to_le_bytes());
        m[OFF_POT..OFF_SIG].copy_from_slice(&self.pot_id);
        m[OFF_SIG..OFF_LEN].copy_from_slice(&self.signature);
        m[OFF_LEN..OFF_DATA].copy_from_slice(&self.app_data_len.to_le_bytes());
        m[OFF_DATA..].copy_from_slice(&self.app_data);
        m
    }

    /// Structural decode. Does not check the signature, the expiry, whether the
    /// payout address parses as an Orchard address, or the note denomination;
    /// those need the vault address, the chain height and Orchard (gateway side).
    pub fn decode(m: &[u8; MEMO_LEN]) -> Result<Self, MemoError> {
        if classify(m)? != MemoKind::DepositV2 {
            return Err(MemoError::UnknownKind(m[4]));
        }
        let flags = m[OFF_FLAGS];
        if flags & !FLAGS_KNOWN != 0 {
            return Err(MemoError::ReservedFlags);
        }
        let len = u16::from_le_bytes([m[OFF_LEN], m[OFF_LEN + 1]]);
        if len as usize > APP_DATA_MAX {
            return Err(MemoError::AppDataTooLong);
        }
        if !all_zero(&m[OFF_DATA + len as usize..]) {
            return Err(MemoError::NonZeroPadding);
        }
        let mut out = Self {
            flags,
            session_key: [0; 32],
            payout_address: [0; ORCHARD_ADDR_LEN],
            expiry_height: u32::from_le_bytes(m[OFF_EXPIRY..OFF_POT].try_into().unwrap()),
            pot_id: [0; 32],
            signature: [0; 64],
            app_data_len: len,
            app_data: [0; APP_DATA_MAX],
        };
        out.session_key.copy_from_slice(&m[OFF_SESSION..OFF_ADDR]);
        out.payout_address.copy_from_slice(&m[OFF_ADDR..OFF_EXPIRY]);
        out.pot_id.copy_from_slice(&m[OFF_POT..OFF_SIG]);
        out.signature.copy_from_slice(&m[OFF_SIG..OFF_LEN]);
        out.app_data.copy_from_slice(&m[OFF_DATA..]);
        Ok(out)
    }

    /// The exact bytes the session key signs (Ed25519, no prehash): domain tag,
    /// network, the app's vault address, and every memo byte except the signature.
    pub fn signing_message(
        &self,
        network: Network,
        vault_address: &[u8; ORCHARD_ADDR_LEN],
    ) -> [u8; SIGNING_MESSAGE_LEN] {
        let m = self.encode();
        let mut out = [0u8; SIGNING_MESSAGE_LEN];
        let mut i = 0;
        for part in [
            &tags::MEMO_SIG[..],
            &[network as u8][..],
            &vault_address[..],
            &m[..OFF_SIG],
            &m[OFF_LEN..],
        ] {
            out[i..i + part.len()].copy_from_slice(part);
            i += part.len();
        }
        debug_assert_eq!(i, SIGNING_MESSAGE_LEN);
        out
    }

    /// Verifies the session-key signature (gateway side).
    #[cfg(feature = "ed25519")]
    pub fn verify_signature(
        &self,
        network: Network,
        vault_address: &[u8; ORCHARD_ADDR_LEN],
    ) -> Result<(), MemoError> {
        use ed25519_dalek::{Signature, VerifyingKey};
        let vk = VerifyingKey::from_bytes(&self.session_key).map_err(|_| MemoError::BadSignature)?;
        let sig = Signature::from_bytes(&self.signature);
        vk.verify_strict(&self.signing_message(network, vault_address), &sig)
            .map_err(|_| MemoError::BadSignature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use proptest::prelude::*;

    fn sample(app: &[u8]) -> (SigningKey, DepositMemo) {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let memo = DepositMemo::new(
            FLAG_AUTO_EXIT,
            sk.verifying_key().to_bytes(),
            [3u8; ORCHARD_ADDR_LEN],
            1_234_567,
            [0u8; 32],
            app,
        )
        .unwrap();
        (sk, memo)
    }

    #[test]
    fn layout_offsets() {
        let (_, memo) = sample(b"hi");
        let m = memo.encode();
        assert_eq!(&m[..5], &[0xFF, b'Z', b'O', b'L', 2]);
        assert_eq!(m[5], 1);
        assert_eq!(&m[38..81], &[3u8; 43]);
        assert_eq!(&m[81..85], &1_234_567u32.to_le_bytes());
        assert_eq!(&m[181..183], &2u16.to_le_bytes());
        assert_eq!(&m[183..185], b"hi");
        assert!(m[185..].iter().all(|b| *b == 0));
        assert_eq!(SIGNING_MESSAGE_LEN, 508);
    }

    #[test]
    fn sign_verify_and_bind() {
        let (sk, mut memo) = sample(b"seat=1");
        let vault = [9u8; 43];
        let sig = sk.sign(&memo.signing_message(Network::Testnet, &vault));
        memo.set_signature(sig.to_bytes());
        let back = DepositMemo::decode(&memo.encode()).unwrap();
        assert_eq!(back, memo);
        back.verify_signature(Network::Testnet, &vault).unwrap();
        // Bound to network, vault and every field.
        assert!(back.verify_signature(Network::Mainnet, &vault).is_err());
        assert!(back.verify_signature(Network::Testnet, &[8u8; 43]).is_err());
        let mut tampered = back.clone();
        tampered.payout_address[0] ^= 1;
        assert!(tampered.verify_signature(Network::Testnet, &vault).is_err());
        let mut tampered = back.clone();
        tampered.flags = 0;
        assert!(tampered.verify_signature(Network::Testnet, &vault).is_err());
    }

    #[test]
    fn rejects_malformed() {
        let (_, memo) = sample(b"x");
        let good = memo.encode();
        let mut m = good;
        m[0] = 0xF6;
        assert_eq!(DepositMemo::decode(&m), Err(MemoError::NotArbitraryData));
        let mut m = good;
        m[2] = b'X';
        assert_eq!(DepositMemo::decode(&m), Err(MemoError::BadTag));
        let mut m = good;
        m[4] = 1;
        assert_eq!(DepositMemo::decode(&m), Err(MemoError::UnknownKind(1)));
        let mut m = good;
        m[5] = 0x02;
        assert_eq!(DepositMemo::decode(&m), Err(MemoError::ReservedFlags));
        let mut m = good;
        m[181..183].copy_from_slice(&330u16.to_le_bytes());
        assert_eq!(DepositMemo::decode(&m), Err(MemoError::AppDataTooLong));
        let mut m = good;
        m[511] = 1;
        assert_eq!(DepositMemo::decode(&m), Err(MemoError::NonZeroPadding));
        assert_eq!(DepositMemo::new(0, [0; 32], [0; 43], 0, [0; 32], &[0; 330]), Err(MemoError::AppDataTooLong));
    }

    proptest! {
        #[test]
        fn roundtrip(flags in 0u8..2, sk in any::<[u8;32]>(), exp in any::<u32>(), pot in any::<[u8;32]>(),
                     sig in proptest::collection::vec(any::<u8>(), 64), data in proptest::collection::vec(any::<u8>(), 0..=APP_DATA_MAX)) {
            let mut memo = DepositMemo::new(flags, sk, [5u8; 43], exp, pot, &data).unwrap();
            memo.set_signature(sig.try_into().unwrap());
            let enc = memo.encode();
            prop_assert_eq!(DepositMemo::decode(&enc).unwrap(), memo);
        }

        #[test]
        fn decode_never_panics(bytes in proptest::collection::vec(any::<u8>(), MEMO_LEN)) {
            let m: [u8; MEMO_LEN] = bytes.try_into().unwrap();
            let _ = DepositMemo::decode(&m);
            let _ = classify(&m);
        }
    }
}
