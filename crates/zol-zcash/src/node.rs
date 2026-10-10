//! Minimal async JSON-RPC client for the Zcash node: Zakura 1.6.0 (docs/ops/zcash-node.md).
//!
//! Hashes and txids are passed and returned in the node's display form (big-endian hex).
//! `generate` only works on Regtest (PoW disabled). It pays the node's `[mining]
//! miner_address`; with a Unified Address holding an Orchard receiver the coinbase lands in
//! the Ironwood pool from NU6.3 on. infra/zakura sets it to the dev wallet of dev seed
//! "zol-dev", which is how the dev mode and the integration tests are funded. Zakura has no
//! `generatetoaddress`.

use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};

use crate::Error;

#[derive(Clone)]
pub struct NodeClient {
    url: String,
    http: reqwest::Client,
    auth: Option<(String, String)>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BlockchainInfo {
    pub chain: String,
    pub blocks: u32,
    pub bestblockhash: String,
    #[serde(default)]
    pub estimatedheight: Option<u64>,
    #[serde(default)]
    pub consensus: Option<Value>,
    #[serde(default, rename = "valuePools")]
    pub value_pools: Option<Value>,
    #[serde(flatten)]
    pub rest: serde_json::Map<String, Value>,
}

impl BlockchainInfo {
    /// `consensus.chaintip`, the current consensus branch ID (hex).
    pub fn chaintip_branch(&self) -> Option<String> {
        self.consensus
            .as_ref()?
            .get("chaintip")?
            .as_str()
            .map(str::to_owned)
    }

    /// Lowest height whose full block the node still serves, when it prunes (Zakura
    /// `storage_mode = "pruned"` reports `pruned: true` and `pruneheight`). `getblock` and
    /// `z_gettreestate` fail below it. `None` for an archive node
    /// (`pruned: false`).
    pub fn prune_height(&self) -> Option<u32> {
        if self.rest.get("pruned")?.as_bool()? {
            Some(self.rest.get("pruneheight")?.as_u64()? as u32)
        } else {
            None
        }
    }

    /// Value of a named pool in zatoshis (`transparent`, `orchard`, `ironwood`, ...).
    pub fn pool_zat(&self, id: &str) -> Option<i64> {
        self.value_pools
            .as_ref()?
            .as_array()?
            .iter()
            .find(|p| p.get("id").and_then(Value::as_str) == Some(id))?
            .get("chainValueZat")?
            .as_i64()
    }
}

impl NodeClient {
    /// `url` like `http://127.0.0.1:18232/`.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            http: reqwest::Client::new(),
            auth: None,
        }
    }

    /// Basic auth (cookie auth uses user `__cookie__` and the cookie file's contents as password).
    pub fn with_basic_auth(mut self, user: impl Into<String>, password: impl Into<String>) -> Self {
        self.auth = Some((user.into(), password.into()));
        self
    }

    pub async fn call<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T, Error> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let mut req = self.http.post(&self.url).json(&body);
        if let Some((u, p)) = &self.auth {
            req = req.basic_auth(u, Some(p));
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Rpc(format!("{method}: {e}")))?;
        let v: Value = resp
            .json()
            .await
            .map_err(|e| Error::Rpc(format!("{method}: bad body: {e}")))?;
        if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
            return Err(Error::Rpc(format!("{method}: {err}")));
        }
        serde_json::from_value(v.get("result").cloned().unwrap_or(Value::Null))
            .map_err(|e| Error::Rpc(format!("{method}: result: {e}")))
    }

    pub async fn get_blockchain_info(&self) -> Result<BlockchainInfo, Error> {
        self.call("getblockchaininfo", json!([])).await
    }

    pub async fn get_best_block_hash(&self) -> Result<String, Error> {
        self.call("getbestblockhash", json!([])).await
    }

    pub async fn get_block_count(&self) -> Result<u32, Error> {
        self.call("getblockcount", json!([])).await
    }

    pub async fn get_block_hash(&self, height: u32) -> Result<String, Error> {
        self.call("getblockhash", json!([height])).await
    }

    /// Raw serialized block (verbosity 0) by height.
    pub async fn get_block_raw_by_height(&self, height: u32) -> Result<Vec<u8>, Error> {
        let h: String = self
            .call("getblock", json!([height.to_string(), 0]))
            .await?;
        hex::decode(h).map_err(|e| Error::Rpc(format!("getblock hex: {e}")))
    }

    /// Raw serialized block (verbosity 0) by display hash.
    pub async fn get_block_raw_by_hash(&self, hash: &str) -> Result<Vec<u8>, Error> {
        let h: String = self.call("getblock", json!([hash, 0])).await?;
        hex::decode(h).map_err(|e| Error::Rpc(format!("getblock hex: {e}")))
    }

    /// Raw transaction bytes (verbose = 0).
    pub async fn get_raw_transaction(&self, txid: &str) -> Result<Vec<u8>, Error> {
        let h: String = self.call("getrawtransaction", json!([txid, 0])).await?;
        hex::decode(h).map_err(|e| Error::Rpc(format!("getrawtransaction hex: {e}")))
    }

    /// Verbose transaction JSON (includes `height` and `confirmations` when mined).
    pub async fn get_raw_transaction_verbose(&self, txid: &str) -> Result<Value, Error> {
        self.call("getrawtransaction", json!([txid, 1])).await
    }

    /// Broadcasts a raw transaction; returns its txid (display hex).
    pub async fn send_raw_transaction(&self, raw: &[u8]) -> Result<String, Error> {
        self.call("sendrawtransaction", json!([hex::encode(raw)]))
            .await
    }

    /// Regtest only: mine `n` blocks to the configured miner address.
    pub async fn generate(&self, n: u32) -> Result<Vec<String>, Error> {
        self.call("generate", json!([n])).await
    }

    /// `z_gettreestate`: raw JSON (contains `ironwood.commitments.finalState` from NU6.3).
    pub async fn z_get_treestate(&self, hash_or_height: &str) -> Result<Value, Error> {
        self.call("z_gettreestate", json!([hash_or_height])).await
    }

    /// Mempool txids.
    pub async fn get_raw_mempool(&self) -> Result<Vec<String>, Error> {
        self.call("getrawmempool", json!([])).await
    }
}
