//! Read-only access to the parsed Orchard-protocol bundle of a PCZT (pczt `Verifier` role).
//!
//! The pczt crate's own wire structs (`pczt::orchard::{Spend, Output, Action}`) keep many
//! fields `pub(crate)` without getters (e.g. `rcv`, `alpha`, spend `value`, `ock`).
//! The `Verifier` role hands out the parsed `orchard::pczt::Bundle`, whose getters expose
//! every field. A signer receives the serialized PCZT, so everything in the encoding is
//! available to it either way.

use std::convert::Infallible;

use pczt::{roles::verifier::Verifier, Pczt};

use crate::pool::Pool;

pub fn with_bundle<T>(pczt: &Pczt, pool: Pool, f: impl FnOnce(&orchard::pczt::Bundle) -> T) -> T {
    let mut out = None;
    let v = Verifier::new(pczt.clone());
    let cb = |b: &orchard::pczt::Bundle| {
        out = Some(f(b));
        Ok(())
    };
    match pool {
        Pool::OrchardNu62 => v.with_orchard::<Infallible, _>(cb).map(|_| ()),
        Pool::IronwoodNu63 => v.with_ironwood::<Infallible, _>(cb).map(|_| ()),
    }
    .expect("Verifier parse");
    out.unwrap()
}

/// Which optional fields are present for each action. Used by spike 0.4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldPresence {
    pub index: usize,
    pub spend_value: Option<u64>,
    pub spend_recipient: bool,
    pub spend_rho: bool,
    pub spend_rseed: bool,
    pub spend_fvk: bool,
    pub spend_witness: bool,
    pub spend_alpha: bool,
    pub spend_dummy_sk: bool,
    pub spend_auth_sig: bool,
    pub output_value: Option<u64>,
    pub output_recipient: bool,
    pub output_rseed: bool,
    pub output_ock: bool,
    pub output_user_address: bool,
    pub rcv: bool,
}

impl FieldPresence {
    pub fn row(&self) -> String {
        let b = |x: bool| if x { "Y" } else { "-" };
        format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            self.index,
            self.spend_value.map(|v| v.to_string()).unwrap_or("-".into()),
            b(self.spend_recipient),
            b(self.spend_rho),
            b(self.spend_rseed),
            b(self.spend_fvk),
            b(self.spend_witness),
            b(self.spend_alpha),
            b(self.spend_dummy_sk),
            b(self.spend_auth_sig),
            self.output_value.map(|v| v.to_string()).unwrap_or("-".into()),
            b(self.output_recipient),
            b(self.output_rseed),
            b(self.output_ock),
            b(self.output_user_address),
            b(self.rcv),
        )
    }

    pub const HEADER: &'static str = "| action | spend.value | spend.recipient | spend.rho | spend.rseed | spend.fvk | spend.witness | spend.alpha | spend.dummy_sk | spend_auth_sig | output.value | output.recipient | output.rseed | output.ock | output.user_address | rcv |\n|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|";
}

pub fn presence(pczt: &Pczt, pool: Pool) -> Vec<FieldPresence> {
    with_bundle(pczt, pool, |b| {
        b.actions()
            .iter()
            .enumerate()
            .map(|(i, a)| FieldPresence {
                index: i,
                spend_value: a.spend().value().map(|v| v.inner()),
                spend_recipient: a.spend().recipient().is_some(),
                spend_rho: a.spend().rho().is_some(),
                spend_rseed: a.spend().rseed().is_some(),
                spend_fvk: a.spend().fvk().is_some(),
                spend_witness: a.spend().witness().is_some(),
                spend_alpha: a.spend().alpha().is_some(),
                spend_dummy_sk: a.spend().dummy_sk().is_some(),
                spend_auth_sig: a.spend().spend_auth_sig().is_some(),
                output_value: a.output().value().map(|v| v.inner()),
                output_recipient: a.output().recipient().is_some(),
                output_rseed: a.output().rseed().is_some(),
                output_ock: a.output().ock().is_some(),
                output_user_address: a.output().user_address().is_some(),
                rcv: a.rcv().is_some(),
            })
            .collect()
    })
}
