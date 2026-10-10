//! Signer-side recompute checks on a parsed Ironwood PCZT bundle (spikes 0.4-0.6), computed
//! only from fields the PCZT carries, never trusting a derived value in the proposal.
//! Phase 4's full payout verifier builds on these.

use orchard::{
    keys::{FullViewingKey, OutgoingViewingKey, SpendValidatingKey},
    note::{ExtractedNoteCommitment, NoteVersion, Rho},
    note_encryption::{IronwoodDomain, OrchardNoteEncryption},
    pczt::{Action, Bundle},
    value::{NoteValue, ValueCommitment},
    Note,
};
use zcash_note_encryption::{
    try_output_recovery_with_pkd_esk, Domain, EphemeralKeyBytes, ShieldedOutput,
    ENC_CIPHERTEXT_SIZE,
};

use crate::Error;

const NV: NoteVersion = NoteVersion::V3;

fn fail(s: impl Into<String>) -> Error {
    Error::Verify(s.into())
}

/// The output note described by the plaintext fields, with rho = the spend's nullifier.
pub fn output_note(a: &Action) -> Result<Note, Error> {
    let o = a.output();
    let rho = Option::from(Rho::from_bytes(&a.spend().nullifier().to_bytes()))
        .ok_or_else(|| fail("rho"))?;
    Option::from(Note::from_parts(
        o.recipient()
            .ok_or_else(|| fail("output.recipient redacted"))?,
        o.value().ok_or_else(|| fail("output.value redacted"))?,
        rho,
        o.rseed().ok_or_else(|| fail("output.rseed redacted"))?,
        NV,
    ))
    .ok_or_else(|| fail("invalid output note"))
}

/// Rejects leftover dummy keys and redacted `rcv` / output plaintext (design v0.3.1).
pub fn check_complete(b: &Bundle) -> Result<(), Error> {
    for (i, a) in b.actions().iter().enumerate() {
        if a.spend().dummy_sk().is_some() {
            return Err(fail(format!("action {i}: dummy spending key present")));
        }
        if a.rcv().is_none() {
            return Err(fail(format!("action {i}: rcv redacted")));
        }
        output_note(a)?;
        if a.spend().alpha().is_none() {
            return Err(fail(format!("action {i}: alpha missing")));
        }
        if a.output().user_address().is_some() {
            return Err(fail(format!("action {i}: unexpected user_address")));
        }
    }
    Ok(())
}

pub fn check_cmx(a: &Action) -> Result<(), Error> {
    if ExtractedNoteCommitment::from(output_note(a)?.commitment()) == *a.output().cmx() {
        Ok(())
    } else {
        Err(fail("cmx mismatch"))
    }
}

/// `rk == ak + [alpha]G` for the given FVK.
pub fn rk_matches(a: &Action, fvk: &FullViewingKey) -> bool {
    match a.spend().alpha() {
        Some(alpha) => SpendValidatingKey::from(fvk.clone()).randomize(alpha) == *a.spend().rk(),
        None => false,
    }
}

/// `cv_net == ValueCommit(spend_value - output.value, rcv)`.
pub fn check_cv_net(a: &Action, spend_value: u64) -> Result<(), Error> {
    let rcv = a.rcv().clone().ok_or_else(|| fail("rcv redacted"))?;
    let out = a
        .output()
        .value()
        .ok_or_else(|| fail("output.value redacted"))?;
    if ValueCommitment::derive(NoteValue::from_raw(spend_value) - out, rcv).to_bytes()
        == a.cv_net().to_bytes()
    {
        Ok(())
    } else {
        Err(fail("cv_net mismatch"))
    }
}

/// A real spend of `note` under `fvk`: rk test **and** nullifier == note's nullifier.
pub fn check_real_spend(a: &Action, fvk: &FullViewingKey, note: &Note) -> Result<(), Error> {
    if !rk_matches(a, fvk) {
        return Err(fail("rk does not follow from the vault key and alpha"));
    }
    if note.nullifier(fvk) != *a.spend().nullifier() {
        return Err(fail("nullifier is not the expected note's"));
    }
    check_cv_net(a, note.value().inner())
}

/// A dummy spend: zero-valued (with the paired cmx pinned) and validly self-signed.
pub fn check_dummy_spend(a: &Action, sighash: &[u8; 32]) -> Result<(), Error> {
    check_cmx(a)?;
    check_cv_net(a, 0)?;
    let sig = a
        .spend()
        .spend_auth_sig()
        .as_ref()
        .ok_or_else(|| fail("dummy spend not signed"))?;
    a.spend()
        .rk()
        .verify(sighash, sig)
        .map_err(|_| fail("dummy signature invalid"))
}

/// Re-encrypts the output (`epk`, `enc_ciphertext`) from its plaintext and `memo`, and the
/// outgoing ciphertext with `ovk`, and byte-compares all three.
pub fn check_ciphertexts(
    a: &Action,
    memo: &[u8; 512],
    ovk: &OutgoingViewingKey,
) -> Result<(), Error> {
    let note = output_note(a)?;
    let enc = OrchardNoteEncryption::new(Some(ovk.clone()), note, *memo);
    let en = a.output().encrypted_note();
    if IronwoodDomain::epk_bytes(enc.epk()).0 != en.epk_bytes {
        return Err(fail("epk mismatch"));
    }
    if enc.encrypt_note_plaintext() != en.enc_ciphertext {
        return Err(fail("enc_ciphertext mismatch"));
    }
    if enc.encrypt_outgoing_plaintext(a.cv_net(), a.output().cmx(), &mut NoRng) != en.out_ciphertext
    {
        return Err(fail("out_ciphertext mismatch (wrong OVK policy?)"));
    }
    Ok(())
}

/// Recovers the memo with pk_d + esk(rseed): succeeds only if the ciphertext decrypts to
/// exactly the committed note.
pub fn recover_memo(a: &Action) -> Option<[u8; 512]> {
    struct Out<'a>(&'a Action);
    impl<D: Domain<ExtractedCommitmentBytes = [u8; 32]>> ShieldedOutput<D, ENC_CIPHERTEXT_SIZE>
        for Out<'_>
    {
        fn ephemeral_key(&self) -> EphemeralKeyBytes {
            EphemeralKeyBytes(self.0.output().encrypted_note().epk_bytes)
        }
        fn cmstar_bytes(&self) -> [u8; 32] {
            self.0.output().cmx().to_bytes()
        }
        fn enc_ciphertext(&self) -> &[u8; ENC_CIPHERTEXT_SIZE] {
            &self.0.output().encrypted_note().enc_ciphertext
        }
    }
    let note = output_note(a).ok()?;
    let esk = IronwoodDomain::derive_esk(&note)?;
    try_output_recovery_with_pkd_esk(
        &IronwoodDomain::for_pczt_action(a),
        IronwoodDomain::get_pk_d(&note),
        esk,
        &Out(a),
    )
    .map(|(_, _, m)| m)
}

struct NoRng;
impl rand_core::RngCore for NoRng {
    fn next_u32(&mut self) -> u32 {
        unreachable!("deterministic")
    }
    fn next_u64(&mut self) -> u64 {
        unreachable!("deterministic")
    }
    fn fill_bytes(&mut self, _: &mut [u8]) {
        unreachable!("deterministic")
    }
    fn try_fill_bytes(&mut self, _: &mut [u8]) -> Result<(), rand_core::Error> {
        unreachable!("deterministic")
    }
}
