//! Gateway-side recompute checks (spikes 0.4, 0.5, 0.6): computed only from the fields a
//! PCZT exposes to a Signer, never trusting any derived value in the proposal.

use orchard::{
    keys::{FullViewingKey, OutgoingViewingKey, SpendValidatingKey},
    note::{ExtractedNoteCommitment, NoteVersion, Rho},
    note_encryption::{IronwoodDomain, OrchardDomain, OrchardNoteEncryption},
    pczt::Action,
    value::{NoteValue, ValueCommitment},
    Note,
};
use zcash_note_encryption::{try_output_recovery_with_pkd_esk, Domain, EphemeralKeyBytes, ShieldedOutput, ENC_CIPHERTEXT_SIZE};

/// The output note as described by the PCZT plaintext fields (recipient, value, rseed) and
/// rho = the action's spend nullifier.
pub fn output_note(a: &Action, nv: NoteVersion) -> Result<Note, String> {
    let o = a.output();
    let rho = Rho::from_bytes(&a.spend().nullifier().to_bytes())
        .into_option()
        .ok_or("rho")?;
    Note::from_parts(
        o.recipient().ok_or("output.recipient missing")?,
        o.value().ok_or("output.value missing")?,
        rho,
        o.rseed().ok_or("output.rseed missing")?,
        nv,
    )
    .into_option()
    .ok_or_else(|| "invalid output note".to_string())
}

/// Commitments: cmx follows from the supplied plaintext.
pub fn check_cmx(a: &Action, nv: NoteVersion) -> Result<(), String> {
    let n = output_note(a, nv)?;
    if ExtractedNoteCommitment::from(n.commitment()) == *a.output().cmx() {
        Ok(())
    } else {
        Err("cmx mismatch".into())
    }
}

/// Real spend: rk == ak_vault + [alpha] G.
pub fn is_vault_spend(a: &Action, vault_fvk: &FullViewingKey) -> bool {
    let ak = SpendValidatingKey::from(vault_fvk.clone());
    match a.spend().alpha() {
        Some(alpha) => ak.randomize(alpha) == *a.spend().rk(),
        None => false,
    }
}

/// Real spend of a specific known note: rk check plus nullifier == note.nullifier(fvk).
/// The nullifier check is computed from the signer's own copy of the note, not the PCZT's.
pub fn check_real_spend(a: &Action, vault_fvk: &FullViewingKey, note: &Note) -> Result<(), String> {
    if !is_vault_spend(a, vault_fvk) {
        return Err("rk does not follow from vault ak and alpha".into());
    }
    if note.nullifier(vault_fvk) != *a.spend().nullifier() {
        return Err("nullifier is not the expected vault note's nullifier".into());
    }
    Ok(())
}

/// cv_net for a real action, from the signer's own spend value and the output value.
pub fn check_cv_net(a: &Action, spend_value: u64) -> Result<(), String> {
    let rcv = a.rcv().clone().ok_or("rcv missing")?;
    let out_v = a.output().value().ok_or("output.value missing")?;
    let cv = ValueCommitment::derive(NoteValue::from_raw(spend_value) - out_v, rcv);
    if cv.to_bytes() == a.cv_net().to_bytes() {
        Ok(())
    } else {
        Err("cv_net mismatch".into())
    }
}

/// Spike 0.4: dummy spend is worth zero. Recompute cv_net with spend value fixed to 0
/// (NOT the PCZT's spend.value field) and the paired output's value; compare.
/// Sound only together with `check_cmx` on the same action (which pins output.value).
pub fn check_dummy_zero_value(a: &Action, nv: NoteVersion) -> Result<(), String> {
    check_cmx(a, nv)?;
    check_cv_net(a, 0)
}

/// Spike 0.4: the dummy's attached spend-auth signature verifies under its rk.
pub fn check_dummy_sig(a: &Action, sighash: &[u8; 32]) -> Result<(), String> {
    let sig = a.spend().spend_auth_sig().as_ref().ok_or("dummy spend_auth_sig missing")?;
    a.spend()
        .rk()
        .verify(sighash, sig)
        .map_err(|e| format!("dummy sig invalid: {e:?}"))
}

pub fn check_no_dummy_sk(b: &orchard::pczt::Bundle) -> Result<(), String> {
    if b.actions().iter().any(|a| a.spend().dummy_sk().is_some()) {
        Err("PCZT still carries a dummy spending key".into())
    } else {
        Ok(())
    }
}

/// Spike 0.5: re-encrypt enc_ciphertext from (recipient, value, rseed, rho, memo).
/// Also returns the epk bytes, which are a deterministic function of the note too.
pub fn recompute_enc(a: &Action, nv: NoteVersion, memo: [u8; 512]) -> Result<([u8; 32], [u8; 580]), String> {
    let note = output_note(a, nv)?;
    let enc = OrchardNoteEncryption::new(None, note, memo);
    Ok((OrchardDomain::epk_bytes(enc.epk()).0, enc.encrypt_note_plaintext()))
}

/// Spike 0.5: re-encrypt out_ciphertext. With ovk = Some(..) this is deterministic:
/// ock = PRF^ock(ovk, cv_net, cmx, epk), plaintext = pk_d || esk, ChaCha20Poly1305 nonce 0.
pub fn recompute_out(a: &Action, nv: NoteVersion, ovk: &OutgoingViewingKey) -> Result<[u8; 80], String> {
    let note = output_note(a, nv)?;
    let enc = OrchardNoteEncryption::new(Some(ovk.clone()), note, [0u8; 512]);
    // The rng is not used when ovk is Some.
    Ok(enc.encrypt_outgoing_plaintext(a.cv_net(), a.output().cmx(), &mut NoRng))
}

/// ock as a signer would compute it (the PCZT `ock` field is left `None` by the builder).
pub fn derive_ock(a: &Action, ovk: &OutgoingViewingKey) -> [u8; 32] {
    let ock = OrchardDomain::derive_ock(
        ovk,
        a.cv_net(),
        &a.output().cmx().to_bytes(),
        &EphemeralKeyBytes(a.output().encrypted_note().epk_bytes),
    );
    ock.as_ref().try_into().unwrap()
}

/// Recover the memo from enc_ciphertext using only public PCZT data: pk_d from the
/// recipient and esk derived from rseed (ZIP 212). Fails unless the ciphertext decrypts
/// to exactly the note described by the plaintext fields and matches cmx and epk.
pub fn recover_with_esk(a: &Action, nv: NoteVersion) -> Option<[u8; 512]> {
    struct Out<'a>(&'a Action);
    impl<D: Domain<ExtractedCommitmentBytes = [u8; 32]>> ShieldedOutput<D, ENC_CIPHERTEXT_SIZE> for Out<'_> {
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
    let note = output_note(a, nv).ok()?;
    let pk_d = OrchardDomain::get_pk_d(&note);
    match nv {
        NoteVersion::V2 => {
            let esk = OrchardDomain::derive_esk(&note)?;
            try_output_recovery_with_pkd_esk(&OrchardDomain::for_pczt_action(a), pk_d, esk, &Out(a))
                .map(|(_, _, m)| m)
        }
        NoteVersion::V3 => {
            let esk = IronwoodDomain::derive_esk(&note)?;
            try_output_recovery_with_pkd_esk(&IronwoodDomain::for_pczt_action(a), pk_d, esk, &Out(a))
                .map(|(_, _, m)| m)
        }
    }
}

/// A "random" source that panics if used: proves that recompute_out is deterministic.
pub struct NoRng;
impl rand_core::RngCore for NoRng {
    fn next_u32(&mut self) -> u32 {
        panic!("rng used")
    }
    fn next_u64(&mut self) -> u64 {
        panic!("rng used")
    }
    fn fill_bytes(&mut self, _: &mut [u8]) {
        panic!("rng used")
    }
    fn try_fill_bytes(&mut self, _: &mut [u8]) -> Result<(), rand_core::Error> {
        panic!("rng used")
    }
}
