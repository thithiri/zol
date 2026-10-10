# Phase 3: `crates/zol-zcash`

Date: 2026-10-10. Status: **built and tested.** 15 tests pass offline. 4 regtest tests (`--ignored`) passed against Zebra 6.4.2 when this phase was built:

- the full flow: deposit, scan, genesis head, refund signed 2-of-3 with FROST, then the spend detected;
- a two-node reorg that the scanner rewinds;
- a fixture generator;
- the `zol-zcashd` daemon end to end (see [zol-zcashd](#zol-zcashd)).

**Update (2026-10-10, later):** ZOL now runs on Zakura 1.6.0 (`infra/zakura`, [docs/ops/zcash-node.md](ops/zcash-node.md)) and Zebra was removed. The node module and client were renamed `zebra` → `node` and `ZebraClient` → `NodeClient` (`Scanner::from_zebra` → `from_node`), and the daemon's config section `[zebra]` → `[node]`. Regtest funding uses `generate` to the node's `miner_address`, because Zakura has no `generatetoaddress`. The API, config and run instructions below are current. The findings marked as Zebra 6.4.2 are the record of what was measured then.

Clippy is clean.

The crate is its own Cargo workspace (`[workspace]` in `crates/zol-zcash/Cargo.toml`, and `"crates/zol-zcash"` in the root `exclude`). It uses the root toolchain, Rust 1.97.1. Pinned stack:

- orchard 0.16.0, pczt 0.10.0-pre.1, frost-core and frost-rerandomized 3.0.0, with our own RedPallas ciphersuite (`redpallas`), proven byte-identical to reddsa 0.5.2's FROST, which it replaces
- zcash_primitives 0.31.0-pre.1, zcash_protocol 0.11.0-pre.0, zcash_address 0.14.0-pre.1
- zcash_note_encryption 0.5.1, incrementalmerkletree 0.9.0, shardtree 0.8.0, pasta_curves 0.6.1
- These are the NU7 (ZIP 259) pre-releases. Before the NU7 port (2026-10-10) this phase was built on orchard 0.15.5, reddsa 0.5.2, pczt 0.9.3, zcash_primitives 0.30.1, zcash_protocol 0.10.6 and zcash_address 0.13.0. See [docs/ops/nu7.md](ops/nu7.md).
- blake3 1.8, reqwest 0.12 (rustls)

It depends on `zol-wire` and `zol-types` by path.

**Why a separate process** (as built; with the pre-release stack it is also kept apart so its pins stay independent). pczt 0.9.3 pulled in `zcash_transparent`, then `bip32 =0.6.0-pre.1`, then `sha2 =0.11.0-pre.4`. anchor-lang 1.2.1 needs `sha2 ^0.11` (final), and Cargo allows only one 0.11. So `zol-zcash` cannot share a build with the gateway, and the operator's Zcash side runs as `zol-zcashd`. The JSON contract is `crates/zol-types`.

## API summary

All 32-byte hashes (txids, block hashes) are in **internal byte order**. Use `display_hex()` and `from_display_hex()` for the node's and explorers' form. Receivers are raw 43-byte Orchard-protocol receivers; Ironwood uses the same ones.

### `keys`

```rust
pub type Receiver = [u8; 43];
pub type ReceiverIndex = [u8; 11];           // ZIP 32 diversifier index, LE
pub struct AppKeys {
    pub app_id: [u8; 32], pub network: Network,
    pub counter: u32,        // accepted derivation counter c
    pub tweak: [u8; 32],     // t_app, LE scalar; every signer adds it to its base share
    pub ak: [u8; 32],        // ak_app, ỹ = 0
    /* private: sk_c, qsk, fvk */
}
impl AppKeys {
    pub fn derive(ak_group: [u8; 32], master_view: &[u8; 32], app_id: &[u8; 32], network: Network) -> Result<AppKeys, Error>;
    pub fn fvk(&self) -> &FullViewingKey;
    pub fn vault_address(&self) -> Receiver;                    // external, index 0
    pub fn vault_internal_address(&self) -> Receiver;           // internal, index 0 (head)
    pub fn session_receiver(&self, session_key: &[u8; 32]) -> (Receiver, ReceiverIndex);
    pub fn receiver_index(&self, addr: &Receiver) -> Option<(ReceiverIndex, Scope)>;
    pub fn ivk(&self, Scope) -> IncomingViewingKey;  pub fn ovk(&self, Scope) -> OutgoingViewingKey;
    pub fn padding_ovk(&self) -> OutgoingViewingKey;
    pub fn tweak_scalar(&self) -> pallas::Scalar;
    pub fn qsk(&self) -> [u8; 32];  pub fn qk(&self) -> [u8; 32];  pub fn view_secret(&self) -> [u8; 32];
}
pub fn encode_ua(params: &ChainParams, r: &Receiver) -> String;          // Orchard-only UA for ZIP 321 links
pub fn decode_ua_orchard(params: &ChainParams, ua: &str) -> Option<Receiver>;
```

`derive` returns `Result` only because `ak_group` must decode to a non-identity Pallas point.

### `params`

```rust
pub use zol_wire::Network;
pub struct ChainParams { pub network: Network, pub regtest: RegtestHeights }  // impl zcash_protocol Parameters
ChainParams::{MAINNET, TESTNET, REGTEST}; ChainParams::new(Network);
fn branch_id(&self, h: u32) -> BranchId;  fn ironwood_active(&self, h: u32) -> bool;
```

Regtest defaults to every upgrade through NU7 at height 1 (`infra/zakura/regtest-nu7.toml`); `RegtestHeights::PRE_NU7` stops at NU6.3, and `ChainParams::regtest_from_upgrades` reads the heights from the node (see [docs/ops/nu7.md](ops/nu7.md)).

### `node`

```rust
pub struct NodeClient;  // Clone + Send + Sync
impl NodeClient {
    pub fn new(url: impl Into<String>) -> Self;
    pub fn with_basic_auth(self, user, password) -> Self;        // cookie auth: "__cookie__"
    pub async fn call<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T, Error>;
    pub async fn get_blockchain_info(&self) -> Result<BlockchainInfo, Error>;  // .chaintip_branch(), .pool_zat("ironwood")
    pub async fn get_best_block_hash(&self) -> Result<String, Error>;
    pub async fn get_block_count(&self) -> Result<u32, Error>;
    pub async fn get_block_hash(&self, height: u32) -> Result<String, Error>;
    pub async fn get_block_raw_by_height(&self, height: u32) -> Result<Vec<u8>, Error>;
    pub async fn get_block_raw_by_hash(&self, hash: &str) -> Result<Vec<u8>, Error>;
    pub async fn get_raw_transaction(&self, txid: &str) -> Result<Vec<u8>, Error>;
    pub async fn get_raw_transaction_verbose(&self, txid: &str) -> Result<Value, Error>;
    pub async fn send_raw_transaction(&self, raw: &[u8]) -> Result<String, Error>;   // txid
    pub async fn generate(&self, n: u32) -> Result<Vec<String>, Error>;              // regtest: pays [mining] miner_address
    pub async fn z_get_treestate(&self, hash_or_height: &str) -> Result<Value, Error>;
    pub async fn get_raw_mempool(&self) -> Result<Vec<String>, Error>;
}
```

### `scan`

```rust
pub enum Pool { Orchard, Ironwood }
pub struct Account { pub id: [u8; 32], pub fvk: FullViewingKey }   // From<&AppKeys> (id = app_id)
pub struct ScannedNote {                                            // Serialize + Deserialize
    pub app_id: [u8; 32], pub pool: Pool, pub cmx: [u8; 32], pub nullifier: [u8; 32], pub value: u64,
    pub memo: [u8; 512], pub receiver: [u8; 43], pub receiver_index: [u8; 11], pub scope: Scope,
    pub height: u32, pub block_hash: [u8; 32], pub txid: [u8; 32], pub action_index: u32,
    pub position: Option<u64>,   // Ironwood tree position; None for Orchard-pool notes
}
impl ScannedNote { pub fn note(&self) -> orchard::Note; pub fn deposit_memo(&self) -> Result<DepositMemo, MemoError> /* decode_any */ }
pub struct SpentNote { pub app_id, pub pool, pub cmx, pub nullifier, pub spending_txid: [u8; 32], pub height: u32, pub block_hash: [u8; 32] }
pub struct BlockEvents { pub height, pub hash, pub received: Vec<ScannedNote>, pub spent: Vec<SpentNote> }
pub struct Spendable { pub note: Note, pub scanned: ScannedNote, pub path: MerklePath, pub anchor: Anchor, pub anchor_height: u32 }
pub struct ScannerConfig { pub params: ChainParams, pub scan_orchard_pool: bool /* false */, pub max_reorg_depth: u32 /* 1000 */ }
pub struct SyncReport { pub blocks: u32, pub rewound_to: Option<u32>, pub events: Vec<BlockEvents> }
impl Scanner {   // Send + Sync; sync() future is Send
    pub fn new(cfg, accounts: Vec<Account>, start_height: u32, prev_hash: Option<[u8; 32]>,
               ironwood_frontier: Option<Frontier<MerkleHashOrchard, 32>>) -> Result<Self, Error>;
    pub async fn from_node(cfg, accounts, client: &NodeClient, start_height: u32) -> Result<Self, Error>; // z_gettreestate(start-1)
    pub fn add_account(&mut self, a: Account);            // effective from the next block
    pub fn tip(&self) -> (u32, [u8; 32]);  pub fn next_height(&self) -> u32;
    pub fn ingest_block(&mut self, raw: &[u8]) -> Result<BlockEvents, Error>;  // Err(Error::Reorg{height}) if not linked
    pub fn ingest_parsed(&mut self, b: &ParsedBlock) -> Result<BlockEvents, Error>;
    pub fn rewind(&mut self, to_height: u32) -> Result<(), Error>;
    pub async fn sync(&mut self, client: &NodeClient, until: Option<u32>) -> Result<SyncReport, Error>; // finds the fork itself
    pub fn spendable(&self, cmx: &[u8; 32], anchor_height: Option<u32>) -> Result<Spendable, Error>;
    pub fn note_by_cmx(&self, &[u8; 32]) -> Option<&ScannedNote>;  pub fn spend_of(&self, cmx) -> Option<&SpentNote>;
    pub fn unspent(&self, app_id) -> impl Iterator<Item=&ScannedNote>;  pub fn all_notes(&self); pub fn spent_notes(&self);
    pub fn anchor_at(&self, height: u32) -> Result<Option<Anchor>, Error>;
    pub fn snapshot(&self) -> Result<Vec<u8>, Error>;                          // bincode, keys excluded
    pub fn restore(bytes: &[u8], accounts: Vec<Account>) -> Result<Self, Error>; // same account ids required
}
pub fn parse_legacy_tree(bytes: &[u8]) -> Result<Frontier<MerkleHashOrchard, 32>, Error>;  // z_gettreestate finalState
```

### `refund`

```rust
pub struct RefundPczt { pub pczt: Pczt, pub app_id, pub deposit_cmx, pub deposit_nullifier, pub deposit_value: u64,
    pub to: Receiver, pub refund_value: u64, pub fee: u64, pub target_height: u32, pub expiry_height: u32,
    pub spend_index: usize, pub refund_output_index: usize, pub padding_output_index: usize }
pub fn build(params: &ChainParams, keys: &AppKeys, deposit: &Spendable, to: &Receiver,
             target_height: u32, expiry_delta: u32) -> Result<RefundPczt, Error>;        // IO-finalized and proven
pub fn check(r: &RefundPczt, keys: &AppKeys, deposit: &Note, to: &Receiver, current_height: u32) -> Result<[u8; 32], Error>; // -> sighash
pub fn sign_with_frost(r: RefundPczt, keys: &AppKeys, deposit: &Note,
                       base_shares: &BTreeMap<Identifier, KeyPackage>, base_pubkeys: &PublicKeyPackage,
                       current_height: u32, rng) -> Result<RefundPczt, Error>;
pub fn sign_single(r: RefundPczt, ask_app: &[u8; 32]) -> Result<RefundPczt, Error>;          // tests
pub fn extract(r: RefundPczt, params: &ChainParams) -> Result<(Vec<u8> /*raw*/, [u8; 32] /*txid*/), Error>;
```

### `frost` (for distributed signing)

```rust
pub use crate::redpallas::{Identifier, KeyPackage, PublicKeyPackage, SigningPackage, SigningNonces,
    SigningCommitments, SignatureShare, ...};   // FROST(Pallas, BLAKE2b-512), byte-identical to reddsa 0.5.2
pub fn group_key(&PublicKeyPackage) -> [u8; 32];
pub fn app_packages(app: &AppKeys, base: &KeyPackage, base_pk: &PublicKeyPackage) -> Result<(KeyPackage, PublicKeyPackage), Error>;
pub fn commit(app_kp: &KeyPackage, rng) -> (SigningNonces, SigningCommitments);                       // round 1
pub fn sign_share(sp: &SigningPackage, nonces: &SigningNonces, app_kp: &KeyPackage, alpha: &[u8; 32]) -> Result<SignatureShare, Error>;
pub fn aggregate(sp: &SigningPackage, shares: &BTreeMap<Identifier, SignatureShare>, app_pk: &PublicKeyPackage, alpha: &[u8; 32]) -> Result<[u8; 64], Error>;
pub fn sign_locally(...) -> Result<[u8; 64], Error>;   pub fn dealer(..); pub fn dkg_local(..);   // tests/tooling
pub fn tweak_key_package(..); pub fn tweak_public_package(..);
```

### `genesis`

```rust
pub struct GenesisTx { pub raw_tx: Vec<u8>, pub txid: [u8; 32], pub head_cmx: [u8; 32] /* activate_app */,
                       pub head_value: u64, pub fee: u64, pub change_value: u64 }
pub fn build(params: &ChainParams, funding_sk: &SpendingKey, funding: &Spendable, app: &AppKeys,
             float: u64, target_height: u32, expiry_delta: u32) -> Result<GenesisTx, Error>;
pub fn build_multi(params, funding_sk, funding: &[Spendable] /* same anchor */, app, float,
                   target_height, expiry_delta) -> Result<GenesisTx, Error>;
// testing::wallet_send_multi(params, sk, inputs: &[(Note, MerklePath)], anchor, to, value, memo, target_height)
// testing::zip317_fee(inputs, outputs) -> u64
```

### `verify`

```rust
pub struct VaultOutput { pub pool: Pool, pub action_index: u32, pub scope: Scope, pub receiver: [u8; 43],
    pub receiver_index: [u8; 11], pub value: u64, pub memo: [u8; 512], pub cmx: [u8; 32], pub note: Note }
pub fn vault_outputs(raw_tx: &[u8], keys: &AppKeys) -> Result<Vec<VaultOutput>, Error>;  // both scopes, any diversifier
pub fn nullifiers(raw_tx: &[u8]) -> Result<Vec<(Pool, [u8; 32])>, Error>;
pub fn verify_raw(raw: &[u8], params: &ChainParams) -> Result<[u8; 32], Error>;          // proof + all sigs + sighash
pub fn shielded_sighash_raw(raw: &[u8]) -> Result<[u8; 32], Error>;
```

### Also exposed

- `tx` (the pczt pipeline), `checks` (spike 0.4–0.6 recompute checks) and `block::parse_block`.
- `testing`: `single_key_group`, `app_ask`, `random_spending_key`, `offline_note`, `wallet_send`.
- `Error`, with variants `Key`, `Frost`, `Rpc`, `Parse`, `Build`, `Pczt`, `Verify`, `Scan`, `Reorg { height }` and `Tree`.

## Decisions

### Key derivation v1

This is spike 0.8's spec, plus a network and version byte, plus ZIP 2005. It is written out in `src/keys.rs`:

```text
D = [1] || [network]                       enc(app) = I2LEOSP_16(32) || app_id
t_c  = ToScalar(BLAKE2b-512("ZOL_app_ak_tweak", D || ak_group || enc(app) || I2LEOSP_32(c)))
A_c  = ak_group + [t_c]G                   (reject 𝒪, ỹ = 1)
sk_c = BLAKE2b-256("ZOL_app_view_key", D || K || enc(app) || I2LEOSP_32(c))
ZIP 2005 § 4.2.3, use_qsk = true, with ak^P = A_c:
  nk = ToBase(PRF^expand_sk([0x07]));  qsk = PRF^expand_sk([0x0C])[..32]
  qk = BLAKE3.derive_key("Zcash ZIP 2005 qk-derivation v1", qsk)
  rivk = ToScalar(PRF^expand_qk([0x0D] || ak || nk))      (reject c if the FVK is invalid)
```

- Internal-scope keys are unchanged ZIP 32 (`0x83`), which ZIP 2005 keeps. orchard computes them.
- **This supersedes spike 0.8's spec and vectors** (`docs/spikes/0.8-per-app-key-derivation.md`).
  - That spec was effectively "version 0": no `D` prefix, and `nk`/`rivk` derived ad hoc from `K`.
  - v1 keeps 0.8's tweak and counter construction and its security notes (the signing-isolation correction still applies), but derives the viewing keys per ZIP 2005 with `use_qsk = true`.
  - The 0.8 vectors are not valid for v1, so review the v1 construction above instead.
  - The v1 vectors are printed by `keys::tests::vectors`, for example:

  ```text
  Mainnet, app=a1…, ak_group = [0x5a5a5a5a]G (even), K = 0x11…:
    c = 0, t = 5eb2d7f7…6727, ak = 12a65d89…ab11, qk = 76aedc6a…f6c8
  ```

### ZIP 326 and `use_qsk`

- The vault keys are `use_qsk = true`.
  - ZIP 2005, "Usage with FROST", says FROST participants MUST agree on an `sk` and derive `nk`, `qsk`, `qk` and `rivk_ext` with `use_qsk = true`, taking `ak` from the DKG.
  - The per-app `sk_c` plays that role per app.
  - ZIP 326 forbids generating such keys before NU6.3 Mainnet activation. That activation happened on 2026-07-28.
- **Senders are unaffected.** A payment needs only `(d, pk_d)`. The sender path, `NoteEncryption` with `esk·pk_d`, never touches `rivk`. ZIP 326, "Receiving funds into the Ironwood pool", says the same.
- **Evidence:**
  - On regtest, an ordinary `use_qsk = false` wallet key paid a `use_qsk = true` session receiver.
  - Zebra 6.4.2 accepted and mined the transaction.
  - The scanner decrypted it, and FROST refunded it.
  - The offline test does the same.
- **Scanning.** `use_qsk` is fixed per vault, so the scanner uses exactly one `ivk` per scope. ZIP 326's four-way first-funds search is never needed.
- **Orchard pool.** Following ZIP 326, it is not scanned by default (`scan_orchard_pool = false`).
- **Retention.** Operators must retain `K`, from which every `sk_c`, `qsk` and `qk` is rederived, and `ak_group`, for the future Recovery Protocol.
- **Cross-check (update):** no official implementation or vectors exist yet. The derivation reproduces the third-party vectors proposed to ZcashFoundation/frost-tools#610 byte for byte, and two independent re-derivations agree; see `docs/review/key-derivation.md` § 4.

### Session receivers

- `index = BLAKE2b-256("ZOL_SessionRecv1", app_id || session_key)[..11]`, external scope. Every 11-byte index is a valid Orchard diversifier index.
- The index is public-derivable. Linking it to the receiver still needs `dk`.
- `receiver_index()` inverts it for any diversifier, in both scopes.
- `encode_ua()` produces the Orchard-only UA for the ZIP 321 link.

### Refund shape

- **Outputs:** 1 spend and 2 outputs. One is the refund (`value − 10,000`, vault external OVK, memo `zol_wire::receipt::refund_memo(app, deposit cmx)`, required by `refund::check`). The other is a zero-valued padding output to a fresh throwaway receiver (vault padding OVK, empty memo).
- **Padding:** the builder runs `UNPADDED`, so it adds exactly one dummy *spend* (signed and cleared by IoFinalizer) and never a dummy output.
- **Why 2 actions:**
  - ZIP 317 charges for at least 2 actions anyway.
  - A 1-action refund would reveal the refund shape.
  - All ciphertexts stay predictable.
- **Padding OVK:** `BLAKE2b-256("ZOL_padding_ovk1", fvk)`, which any FVK holder can recompute.
- **Signer check:** `refund::check` recomputes everything, using `checks`:
  - the real spend's `rk` and nullifier for *this* app;
  - zero-valued, signed dummies;
  - every `cmx`;
  - no vault-addressed output;
  - exactly one paid output, to the memo's address;
  - all three ciphertexts per output;
  - fee = ZIP 317 = value balance;
  - expiry within `MAX_EXPIRY_DELTA = 100`.

  It also rejects dummy keys, redacted fields and `user_address`. Tests confirm it rejects a wrong destination and another app's keys.

### Genesis

- One head output to the vault internal address with `HeadMemo::genesis`, using the internal OVK.
- Change goes back to the funding key's internal address.
- After extraction, `verify::vault_outputs` must find exactly that one head.

### Scanner

- A shardtree with one checkpoint per block (id = height) and our leaves marked. That is what lets witnesses survive rewinds: truncating to a checkpoint keeps earlier marks.
- Reorg window: `max_reorg_depth` (default 1,000, the node's window). The tree keeps 1,010 checkpoints. A rewind deeper than the window is an error: global halt.
- Marks of notes spent beyond the window are released.
- `snapshot()`/`restore()` serialize everything except keys: the tree shards and cap, checkpoints, notes, spends and block hashes. They use bincode with a version tag. The snapshot was 6.6 kB after the regtest flow.

### Regtest funding (measured on Zebra 6.4.2; Zakura has no `generatetoaddress` and funds through `generate` to `miner_address`)

- Zebra 6.4.2's `generatetoaddress` **is implemented**. The trait default at `methods.rs` L770-801 is a stub, which is what spike 0.2 read, but `RpcImpl` overrides it at L3362.
- With a UA holding an Orchard receiver, it pays the coinbase **into the Ironwood pool** from NU6.3 (zebra-rpc `TransactionTemplate::new_coinbase`, OVK all-zero): 6.25 ZEC per block, as one Ironwood note.
- The shielded coinbase note was **spendable in the very next block**. Zebra checks maturity only for transparent coinbase (`MIN_TRANSPARENT_COINBASE_MATURITY`).
- `mining.miner_address` would also work, but needs a config change. `generatetoaddress` funds any test wallet.

### Reorg observation (Zebra 6.4.2)

After the reorg, Zebra did **not** return the orphaned deposit transaction to its mempool. A deposit reorged out disappears until the depositor's wallet rebroadcasts it. The early-credit rollback path in the design must assume that.

## Tests

| Suite | What | Result |
|---|---|---|
| `lib` unit (9) | params/branches; key derivation (network binding, ỹ, determinism, ZIP 2005 components, counter retry about ½); receivers and UA round trip; bad group key; FROST 2-of-3 under the app tweak for every pair, 1-of-3 fails; shardtree rewind, old witness and snapshot round trip; `Send`/`Sync` | pass |
| `offline` (3) | Real proven v6 transactions in in-memory blocks: `ZOL1:` deposit to a session receiver (memo signature verified, app B sees nothing); FROST 2-of-3 refund (`check` rejects wrong destination and wrong app); spend detected; genesis head (cmx, internal scope, genesis memo); snapshot/restore; reorg error; rewind drops spend and head and keeps the deposit witness at the old anchor. Single-key refund; refund to a vault receiver refused. Orchard pool scanned only when opted in | pass, about 11 s |
| `fixtures_and_rpc` (3) | A real regtest block (now a Zakura NU7 block with an Ironwood shielded coinbase to the `zol-dev` dev wallet; originally Zebra 6.4.2 and `SpendingKey([7;32])`): parse, hash and prev hash, scan from the `z_gettreestate` frontier, witness at tip; unlinked block is a reorg; JSON-RPC envelope, params and error mapping against a local responder | pass |
| `regtest` (`--ignored`) | Amounts are relative to the funded coinbase: deposit = coinbase/4, float = deposit/2. The test fails with a "restart the chain" message if a coinbase is under 0.01 ZEC. `regtest_deposit_genesis_refund`: fund via `generatetoaddress`, then a `ZOL1:` deposit to a session receiver is broadcast and mined at height 217; scanned; genesis head broadcast and scanned; refund signed 2-of-3 FROST, broadcast, mined at 219; the deposit spend is detected and the player receives 99,990,000 at the memo address. `regtest_reorg_drops_deposit`: b partitioned; deposit mined on a at 323; b mines 3; heal; a reorgs; `sync` → `rewound_to = Some(322)`, deposit dropped, funding note spendable again | pass (about 14 s with the pair up) |
| `keygen` | Runs `zol-zcashd keygen` (regtest, 3 of 2): files and `--out` mode 0600/0700; `group.toml` has no shares or `K`; each operator file loads through `keyfile::load` (the daemon's loader) with one share (operator mode), same public package and `K`, distinct identifiers; ỹ(`ak_group`) = 0; printed `ak_group` and vault UA match. A second run into the same directory fails and leaves the files unchanged; another run gives a different group and `K`. A real vault refund (proven) is signed by each pair of operator files, and one share alone fails. `[funding]` refuses a non-loopback `listen` before touching the node (unless `--allow-remote-admin`) and a wallet file of another network | pass, about 6 s |
| `zcashd` (`--ignored`) | Amounts scale with the current block subsidy: float = subsidy/2, deposit = 2.5 × subsidy, so the deposit must combine about 3 dev notes, and the test asserts at least 2 inputs. Spawns `zol-zcashd --dev`. `/health`, `/vault`, `/session_receiver`, `/valid_receiver`. `/dev/genesis` (the dev wallet mines its own coinbase); then `/dev/send` of a signed `ZOL1:` deposit to a session receiver. `/events`: seqs contiguous from 1, the head and deposit `Note` events, each `Note` before its `Block`. `/refund`: a head is rejected (400); the deposit refund is broadcast; a repeat returns the same txid with `existing: true`; a different `to` while pending gets 409. After mining, a `Spent` event with the refund txid. Restart on the same state dir: identical events and seqs, refund still `existing`, new events continue after the old cursor | pass (about 9 s) |
| `zcashd::zcashd_funding_genesis_without_dev` (`--ignored`) | A funding wallet (zol-wallet key file) distinct from the dev wallet gets two notes of `float` from a `--dev` daemon, which then stops; 3 more blocks are mined. Daemon B runs **without `--dev`**, in `operator` mode on `keygen` keys (`keys_file`), with `[funding] start_height` = the block after the pre-funding tip, below its own start (tip + 1). Its first `Block` event is at the funding birthday; `/admin/funding` shows both notes and the balance. `/admin/genesis` with a too-large float gets 422 naming the free balance, and the node's height is unchanged (never mines). The real genesis combines both notes; a second call while pending gets 409, and `spendable` is 0. After mining, `/head` shows the genesis link (cmx, float, position 0, txid) with no fault, and a third call gets 409 "already has a genesis". `/admin/send` pays a text-memo test deposit to the vault UA from the change; the vault `Note` event appears and the wallet balance drops by value plus fee. After a restart, the funding wallet restores from the snapshot | pass (about 15 s) |

How to run:

```sh
cd crates/zol-zcash && cargo test                      # offline
../../infra/zakura/scripts/up.sh pair                  # ZOL_NODE_CONFIG=pre-nu7 for an NU6.3 chain
cargo test --test regtest -- --ignored --nocapture --test-threads=1
cargo test --test zcashd -- --ignored --nocapture
ZOL_REGEN_FIXTURE=1 cargo test --test regtest regtest_make_block_fixture -- --ignored   # refresh fixture (nu7 config)
../../infra/zakura/scripts/down.sh                     # state is ephemeral
```

Env: `ZOL_NODE_URL`, `ZOL_NODE_B_URL` and `ZOL_DEV_SEED` (default `zol-dev`, the node's miner wallet).

## Gaps and what the gateway should know

1. **ZIP 2005 derivation: no official reference yet.** It matches third-party vectors and two independent re-derivations. Frozen vectors are in `crates/zol-zcash/tests/vectors/`, and the review package is `docs/review/key-derivation.md`.
2. **The spike 0.8 spec and vectors are superseded** by the v1 / ZIP 2005 derivation above. Use v1 for external review.
3. **FROST uses the deprecated `frost_rerandomized::sign`.** The pczt fixes `alpha` before round 1. This is a review item, unchanged from 0.7.
4. **Scanner performance.**
   - Trial decryption is per-output and per-key, not batched.
   - `sync` fetches one block per RPC call, in sequence.
   - The tree store is in memory.
   - It is fine for regtest and a handful of apps. For mainnet, batch decryption (`zcash_note_encryption::batch`) and an incremental store are needed.
   - `snapshot()` writes the whole state each time.
5. **Orchard-pool notes are not witnessed.** They are reported with `position: None`, and these builders cannot spend them. Nothing new can reach that pool after NU6.3.
6. **Not built here:**
   - the head-chain walker;
   - the payout builder and verifier (Phase 4).

   `verify::vault_outputs`, `verify::nullifiers`, `checks` and `tx::build_ironwood` are the pieces for them.
7. **The refund memo is the receipt memo** `zol_wire::receipt::refund_memo(app, deposit cmx)`, plain text "ZOL refund v1 app=… note=…". `refund::build` sets it and `check` requires it. It replaced the ZIP 302 empty memo on 2026-10-10.
8. **Testnet and mainnet are not exercised yet.** There are no funds and no synced node. `Scanner::from_node` (birthday from `z_gettreestate`) was exercised only on regtest.
9. **Vault keys come from a trusted dealer.** `zol-zcashd keygen` (and the dev seed) generate every Pallas spend share on one machine. That is acceptable for the one-host testnet trial. Independent operators need a Pallas FROST DKG across hosts: `redpallas::keys::dkg` exists and `frost::dkg_local` runs it in one process, but there is no networked ceremony for it yet (the gateway's `zol-gateway dkg` covers only the Ed25519 attestor key). Distributing `K` (the master viewing secret) needs its own protocol too.
10. **Wallet pending is in memory.** Dev and funding wallet notes used by an unmined transaction are held in memory until mined or expired; a restart forgets them, so a retry right after a restart can pick the same notes (the node rejects the double spend).
11. **Funding blocks include the mempool.** The regtest test funds by mining (then `generatetoaddress`, now `generate`), and the coinbase includes mempool fees (one run got 625,010,000). Tests take values from the scan, not constants.

## zol-zcashd

The Zcash half of a gateway operator. It is a binary in this crate, `src/bin/zol-zcashd.rs`, built on axum and tokio.

### Run

```sh
cd crates/zol-zcash
cargo run --bin zol-zcashd -- --config zcashd.toml               # keys from the config
cargo run --bin zol-zcashd -- --config zcashd.toml --dev [--dev-seed s]   # regtest only
cargo run --bin zol-zcashd -- --print-dev-keys [--dev-seed s]   # prints a [keys] section
cargo run --bin zol-zcashd -- keygen --network testnet --operators 3 --threshold 2 --out keys/ [--app <hex>]...
cargo run --bin zol-zcashd -- --config zcashd.toml --allow-remote-admin   # [funding] on a non-loopback listen
```

`keygen` and `[funding]` are described in [docs/ops/testnet-deploy.md](ops/testnet-deploy.md).

### Config (TOML)

```toml
listen = "127.0.0.1:18300"
network = "regtest"              # "mainnet" | "testnet" | "regtest"
state_dir = "./zcashd-state"
start_height = 1                 # optional: first block on a fresh state dir; default node tip + 1
poll_ms = 500                    # optional, default 500
expiry_delta = 40                # optional, default 40 (max 100)
apps = ["a1a1…a1"]               # app program IDs, 32-byte hex

[node]
url = "http://127.0.0.1:18232/"
# user = "__cookie__"            # optional basic auth (node cookie auth)
# password = "<cookie>"

# keys_file = "keys/operator-1.keys.toml"   # a file with the [keys] section (zol-zcashd keygen); not with [keys]

[keys]                           # required unless --dev or keys_file
master_view = "<32-byte hex>"    # K, secret
public_key_package = "<hex>"     # frost-core PublicKeyPackage serialization; ak_group is its key
shares = ["<hex>", …]            # frost-core KeyPackage serializations

[funding]                        # optional, any mode: the wallet /admin/genesis spends from
key_file = "keys/testnet-wallet.json"   # zol-wallet format; network must match
start_height = 4474000           # optional: scan the wallet from here (birthday = min with start_height)
```

Each share must belong to `public_key_package`: same group key, and its verifying share listed there.

**Modes:**

| Mode | When | What it can do |
|---|---|---|
| `operator` | Fewer shares than the threshold (production: this operator's own share) | Scans and serves `/events`. `/refund` returns **501**, because cross-operator FROST signing is not implemented yet |
| `cluster` | At least the threshold of shares in one process | Everything |
| `dev` | `--dev`, regtest only | Everything, plus `/dev/*` |

**`--dev` keys.** Without a `[keys]` section, `--dev` derives everything from a seed (default `zol-dev`):

- `rng = ChaCha20(BLAKE2b-256("ZOL_dev_seed_v1_", seed))`;
- a 2-of-3 trusted dealer, then `K`, then the dev wallet's spending key.

With the same seed, the same keys and the same addresses come out every time.

Use `--print-dev-keys` to see the `[keys]` format. With the default seed, `ak_group` = `570b7f5a…7009`.

### Event log and persistence

- **Sync loop.** Every `poll_ms` the daemon runs `Scanner::sync`.
- **Each step becomes events:**
  - a reorg becomes `Rewind { to_height }`, emitted before the replacement blocks;
  - each connected block becomes its `Note` and `Spent` events in chain order, then `Block { height, hash }`, which acts as the block's commit marker.
- **What is included.** Only app vault notes appear. The dev and funding wallets are scanned but never logged.
- **Birthday.** On a fresh state dir the scanner starts at `start_height` (default node tip + 1), or at `[funding] start_height` if that is lower. Apps scanned from before their genesis only see extra `Block` events.
- **Field mapping.**
  - `receiver_index`: the 11 bytes, little-endian, read as a `u128`.
  - `memo` and `receiver`: full 512- and 43-byte vectors.
  - Every hash is in internal byte order.
- **Sequence numbers.** They start at 1, are contiguous, and are never reused.
- **Files in `state_dir`:**
  - `events.jsonl`, one `[seq, event]` per line;
  - `state.bin` (bincode): the scanner snapshot, `last_seq`, the log tip and refund records.
- **Durability order:**
  1. Append events to `events.jsonl` (fsync).
  2. Atomically rewrite `state.bin` (temp file plus rename).
  3. Only then serve the events.

  On restart, log lines past `last_seq` are dropped. They were never served, so a cursor held by the gateway stays valid across restarts. The test checks this.
- **Changing the app list.** New apps added to the config are scanned from the current tip on. Removing an app is refused.
- **The funding wallet** is a scanner account whose id is derived from its viewing key. Removing `[funding]` or changing its key on an existing state dir is refused, like removing an app. Adding it to an existing state dir is refused when `[funding] start_height` is below the scanner's next block, since those notes would be missed.

### Endpoints (`zol_types::api`)

Hex query parameters are lowercase hex of internal byte order. Errors are `{"error": "..."}` with a 4xx or 5xx status.

**`GET /events?after=&limit=`**
- Returns events with `seq > after`, at most `limit` (default 1,000, maximum 10,000).
- `next` is the last returned seq, or `after` if nothing new.
- `tip` is the height of the last block logged.

**`GET /vault?app=`**
- Returns `{app, address, ua, internal_address}`.
- `address` is external index 0; `ua` is the Orchard-only UA.

**`GET /session_receiver?app=&session=`**
- Returns `{receiver, index, ua}`.

**`GET /valid_receiver?raw=<86 hex>`**
- Returns `ReceiverCheck {valid, vault}`.
  - `valid`: the bytes parse as an Orchard-protocol receiver.
  - `vault`: it belongs to a configured app's vault, meaning it decrypts under that app's viewing key in either scope, at any diversifier.
- A refund to a vault receiver is refused, so the gateway holds such deposits.

**`POST /refund {app, cmx, to}`**
- **The note must be a known, unspent Ironwood note of `app`** (404 if not), and a deposit: not internal scope, and its memo is not a head or top-up memo (400 if not).
  - Whether it should be refunded (`Voided`) is the gateway's decision.
- **The transaction:** the daemon builds the refund (1 spend, refund plus padding, fee 10,000), runs the signer recompute (`refund::check`), signs 2-of-3 FROST under the app's tweaked key, verifies offline, and broadcasts.
- **Idempotency per cmx:**
  - **Pending** (not mined and not expired): returns the same `txid` with `existing: true`. Asking for a different `to` while pending gets 409.
  - **Mined** (the scanner shows the note spent by that txid): `existing: true`.
  - **Spent by any other transaction:** 409.
  - **Expired unmined** (`tip ≥ expiry_height`): rebuilt with a fresh anchor and expiry, broadcast, `existing: false`.
- **Errors:**
  - 422: build or check failure.
  - 502: the node rejected the broadcast.
  - 501: not enough shares in this process.
- Refund records persist in `state.bin`.

**`GET /health`** (free-form JSON)
- `ok`: false after a failed sync.
- `mode`, `network`, `scanner_tip`, `log_tip`, `last_seq`, `apps`, `ak_group`, `master_view_fingerprint`, `signers_held`, `min_signers`, `last_error`, `dev_wallet_ua`, `funding_wallet_ua`.

**Dev only:**

`POST /dev/genesis {app, float}` and `POST /dev/send {to, value, memo}`
- **Funding.** Both fund from the dev wallet by **combining notes**:
  - They pick free dev notes (unspent, and not used by an unmined dev transaction), largest first, until they cover the amount plus the ZIP 317 fee for that many inputs: 5,000 × max(inputs, 2).
  - If the free notes fall short, the daemon reads `getblocksubsidy`, mines `ceil(deficit / subsidy) + 1` coinbase blocks with `generate`, syncs, and retries. The node's `[mining] miner_address` must be the dev wallet's UA (`/health` `dev_wallet_ua`, `--print-dev-keys`); `infra/zakura` sets the one for dev seed `zol-dev`.
  - `generate` **also mines whatever is in the mempool**.
  - Bounds: at most 40 inputs per transaction and 200 mined blocks per request.
  - Past either bound, or when the regtest subsidy is 0, the request fails with 400 and an error naming the amount, the free notes and the current subsidy.
  - Why this matters: regtest halves the subsidy every few hundred blocks. At height 900 it is 0.78125 ZEC.
- **Broadcast only.** The resulting transaction is not mined; the caller mines it with `generate`.
- **`/dev/genesis`** uses `genesis::build_multi` and returns `{head_cmx, txid, head_value}`.
- **`/dev/send`** uses `testing::wallet_send_multi` and returns `{txid}`. `memo` is up to 512 bytes, zero-padded; empty means the ZIP 302 empty memo.

Dev operations and refunds are serialized. Dev wallet notes spent by unmined dev transactions are not reused until they are mined or their transaction expires.

**With `[funding]`** (any mode; loopback peers only unless `--allow-remote-admin`):

`GET /admin/funding`, `POST /admin/genesis {app, float}` and `POST /admin/send {to: UA, value, memo?: text}`
- The same input selection and builders as `/dev/genesis` and `/dev/send` (`genesis::select_inputs`, `genesis::build_multi`, `testing::wallet_send_multi`), from the funding wallet, but they **never mine**: short funds are a 422 naming what is needed, free and pending.
- `/admin/genesis` refuses (409) an app whose genesis head the scanner already holds, or whose genesis this daemon broadcast and has not seen mined before its expiry.
- Details and an operator walkthrough: [docs/ops/testnet-deploy.md](ops/testnet-deploy.md).

### Known limits

- Distributed FROST signing across operator processes is not implemented. Production `operator` mode can scan but not refund.
- The scanner holds its lock during a sync round, so `/refund` waits for the round to finish (sub-second on regtest). `/events` is never blocked.
- `state.bin` is rewritten whole on every change.
