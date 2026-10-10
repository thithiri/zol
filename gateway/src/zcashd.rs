//! Client for an operator's `zol-zcashd` (the Zcash half of the operator), and
//! the receiver validity check the gateway runs locally.

use std::time::Duration;

use group::GroupEncoding;
use zol_types::api;

use crate::chain::{AddressCheck, AppId, ChainEvent};

#[derive(Debug, thiserror::Error)]
pub enum ZcashdError {
    #[error("http: {0}")]
    Http(String),
    #[error("zcashd: {0}")]
    Api(String),
}

pub type Result<T> = std::result::Result<T, ZcashdError>;

pub struct ZcashdClient {
    base: String,
    http: reqwest::blocking::Client,
}

fn http(e: impl std::fmt::Display) -> ZcashdError {
    ZcashdError::Http(e.to_string())
}

impl ZcashdClient {
    pub fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            http: reqwest::blocking::Client::builder().timeout(Duration::from_secs(300)).build().expect("client"),
        }
    }

    fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let r = self.http.get(format!("{}{path}", self.base)).send().map_err(http)?;
        Self::decode(r)
    }

    fn post<B: serde::Serialize, T: serde::de::DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        let r = self.http.post(format!("{}{path}", self.base)).json(body).send().map_err(http)?;
        Self::decode(r)
    }

    fn decode<T: serde::de::DeserializeOwned>(r: reqwest::blocking::Response) -> Result<T> {
        if r.status().is_success() {
            r.json().map_err(http)
        } else {
            let status = r.status();
            let e: api::Error = r.json().unwrap_or(api::Error { error: status.to_string() });
            Err(ZcashdError::Api(e.error))
        }
    }

    pub fn valid_receiver(&self, raw: &[u8; 43]) -> Result<api::ReceiverCheck> {
        self.get(&format!("/valid_receiver?raw={}", hex::encode(raw)))
    }

    pub fn health(&self) -> bool {
        self.http.get(format!("{}/health", self.base)).send().is_ok_and(|r| r.status().is_success())
    }

    pub fn events(&self, after: u64, limit: u32) -> Result<api::Events> {
        self.get(&format!("/events?after={after}&limit={limit}"))
    }

    pub fn vault(&self, app: &AppId) -> Result<api::Vault> {
        self.get(&format!("/vault?app={}", hex::encode(app)))
    }

    pub fn session_receiver(&self, app: &AppId, session: &[u8; 32]) -> Result<api::SessionReceiver> {
        self.get(&format!("/session_receiver?app={}&session={}", hex::encode(app), hex::encode(session)))
    }

    pub fn refund(&self, app: &AppId, cmx: &[u8; 32], to: &[u8; 43]) -> Result<api::RefundResponse> {
        self.post("/refund", &api::RefundRequest { app: *app, cmx: *cmx, to: to.to_vec() })
    }

    pub fn dev_genesis(&self, app: &AppId, float: u64) -> Result<api::GenesisResponse> {
        self.post("/dev/genesis", &api::GenesisRequest { app: *app, float })
    }

    pub fn dev_send(&self, to: &[u8], value: u64, memo: &[u8]) -> Result<api::SendResponse> {
        self.post("/dev/send", &api::SendRequest { to: to.to_vec(), value, memo: memo.to_vec() })
    }

    pub fn head(&self, app: &AppId, genesis: &[u8; 32]) -> Result<api::HeadChain> {
        self.get(&format!("/head?app={}&genesis={}", hex::encode(app), hex::encode(genesis)))
    }

    pub fn payout_propose(&self, plan: &api::PayoutPlan) -> Result<api::Proposal> {
        self.post("/payout/propose", plan)
    }

    pub fn payout_verify(&self, plan: &api::PayoutPlan, pczt: &[u8]) -> Result<api::VerifyResponse> {
        self.post("/payout/verify", &api::VerifyRequest { plan: plan.clone(), pczt: pczt.to_vec() })
    }

    pub fn refund_propose(&self, app: &AppId, cmx: &[u8; 32], to: &[u8; 43]) -> Result<api::RefundProposal> {
        self.post("/refund/propose", &api::RefundRequest { app: *app, cmx: *cmx, to: to.to_vec() })
    }

    pub fn sign_commit(&self, req: &api::CommitRequest) -> Result<api::CommitResponse> {
        self.post("/sign/commit", req)
    }

    pub fn sign_share(&self, req: &api::ShareRequest) -> Result<api::ShareResponse> {
        self.post("/sign/share", req)
    }

    pub fn sign_aggregate(&self, req: &api::AggregateRequest) -> Result<api::Broadcast> {
        self.post("/sign/aggregate", req)
    }

    /// Drains every event after `cursor`, returning them and the new cursor.
    pub fn drain(&self, mut cursor: u64) -> Result<(Vec<ChainEvent>, u64)> {
        let mut out = Vec::new();
        loop {
            let page = self.events(cursor, 1_000)?;
            let n = page.events.len();
            out.extend(page.events.into_iter().map(|(_, e)| e));
            cursor = page.next;
            if n < 1_000 {
                return Ok((out, cursor));
            }
        }
    }
}

/// Orchard-protocol receiver check: 11-byte diversifier (any value is valid in
/// Orchard) and a 32-byte `pk_d` that decodes to a non-identity Pallas point.
/// This is what `orchard::Address::from_raw_address_bytes` checks.
pub struct OrchardReceivers;

impl AddressCheck for OrchardReceivers {
    fn is_valid_receiver(&self, raw: &[u8; zol_wire::ORCHARD_ADDR_LEN]) -> bool {
        let pk_d: [u8; 32] = raw[11..].try_into().unwrap();
        let p = pasta_curves::pallas::Affine::from_bytes(&pk_d);
        bool::from(p.is_some()) && !bool::from(group::prime::PrimeCurveAffine::is_identity(&p.unwrap()))
    }
}

/// The production check: validity locally, vault membership from the
/// operator's own `zol-zcashd`, which holds every app's viewing keys. Answers
/// are cached; a vault's receivers never change, and new apps need a restart.
pub struct ZcashdReceivers {
    client: ZcashdClient,
    cache: std::sync::Mutex<std::collections::HashMap<[u8; 43], bool>>,
}

impl ZcashdReceivers {
    pub fn new(base: &str) -> Self {
        Self { client: ZcashdClient::new(base), cache: Default::default() }
    }
}

impl AddressCheck for ZcashdReceivers {
    fn is_valid_receiver(&self, raw: &[u8; zol_wire::ORCHARD_ADDR_LEN]) -> bool {
        OrchardReceivers.is_valid_receiver(raw)
    }

    fn is_vault_receiver(&self, raw: &[u8; zol_wire::ORCHARD_ADDR_LEN]) -> Option<bool> {
        if let Some(v) = self.cache.lock().unwrap().get(raw) {
            return Some(*v);
        }
        let v = self.client.valid_receiver(raw).ok()?.vault;
        self.cache.lock().unwrap().insert(*raw, v);
        Some(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use group::prime::PrimeCurveAffine;
    use group::Group;

    #[test]
    fn receiver_check() {
        let g = pasta_curves::pallas::Point::generator() * pasta_curves::pallas::Scalar::from(7u64);
        let mut raw = [0u8; 43];
        raw[11..].copy_from_slice(&g.to_bytes());
        assert!(OrchardReceivers.is_valid_receiver(&raw));
        // Identity is refused.
        raw[11..].copy_from_slice(&pasta_curves::pallas::Affine::identity().to_bytes());
        assert!(!OrchardReceivers.is_valid_receiver(&raw));
        // Not a point (x = 2^255 − 1 is out of range).
        raw[11..].copy_from_slice(&[0xFF; 32]);
        assert!(!OrchardReceivers.is_valid_receiver(&raw));
    }
}
