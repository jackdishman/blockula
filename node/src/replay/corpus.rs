//! The recorded v1 history `blockula replay` checks every PR against (`node/testdata/replay/`,
//! docs/REPLAY.md), and the recorder that writes it.
//!
//! The recorder plays the network: it builds each block as a leader would (the leader of its view,
//! the state root it last executed, views that restart every epoch and skip where a leader was
//! away), has the epoch's whole set sign a finalization certificate at the end of each batch, and
//! executes every block with `Chain::execute`, as a node executes finalized blocks. The archive it
//! writes is what `scripts/archive-chain.mjs` would have copied from that node's `/api/sync`, plus
//! the node's own block log, which is what each block did.
//!
//! Every input is fixed (seeds, times; ML-DSA signs deterministically, and Ghost Mode proofs take
//! their masks from fixed seeds), so recording again gives the same bytes for as long as v1's rules
//! are the same. Each tx says how it must end (applied, failed with a reason, or skipped): a
//! recording that would quietly miss what it means to cover stops instead.

use super::Fate;
use crate::chain::{Chain, Genesis};
use crate::consensus::{self, CBlock};
use crate::crypto::Keys;
use crate::pq::{MlDsaScheme, PrivateKey};
use crate::types::{Action, Tx};
use commonware_codec::Encode as _;
use commonware_cryptography::Digestible as _;
use commonware_consensus::simplex::types::{Finalization, Finalize, Proposal};
use commonware_consensus::types::{Epoch, Round, View};
use std::collections::{BTreeMap, HashMap};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Blocks per batch at most (each ends at a certified block, and so does every epoch).
const BATCH: u64 = 50;
/// How long a recorded tx stays valid.
const VALIDITY: u64 = 50;
/// The upgrade signal in every recorded block: a v1 binary's (`SUPPORTED_PROTOCOL_VERSION` until
/// the release that ships v2), whatever binary records, so recording again gives the same bytes.
const SIGNAL: u32 = 1;

/// How a tx must end in its block.
#[derive(Clone, Debug)]
pub(super) enum Want {
    Applied,
    /// Included and charged, with a failure reason that contains this.
    Failed(&'static str),
    /// Not admitted: the block carries it, execution skips it (the proposer pays for it).
    Skipped,
}

pub(super) struct Recorder {
    pub c: Chain,
    home: PathBuf,
    /// Account keys by name.
    keys: BTreeMap<&'static str, Keys>,
    /// Consensus seeds by hex public key: every validator that may sign a certificate.
    seeds: HashMap<String, [u8; 32]>,
    parent: CBlock,
    /// The view of the last block (0 before an epoch's first).
    view: u64,
    /// Views the next block skips (leaders that were away).
    gap: u64,
    timestamp: u64,
    /// Blocks since the last certified one, and the batches so far (`/api/sync` lines).
    batch: Vec<CBlock>,
    lines: Vec<String>,
    /// Nonces the block being built has used per account (on top of the state's).
    used: HashMap<String, u64>,
}

impl Recorder {
    /// A fresh node home on `g`; `keys` are the accounts txs come from, `validators` every
    /// consensus seed a set may hold.
    pub fn new(tag: &str, g: &Genesis, keys: BTreeMap<&'static str, Keys>, validators: &[[u8; 32]]) -> Recorder {
        let home = std::env::temp_dir().join(format!("blockula-corpus-{tag}-{}-{}", std::process::id(), crate::chain::now_ms()));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("genesis.json"), serde_json::to_string_pretty(g).unwrap()).unwrap();
        let mut c = Chain::open(&home).unwrap();
        c.set_scratch();
        let seeds = validators.iter().map(|s| (hex::encode(Keys::from_seed(*s).public_key()), *s)).collect();
        Recorder { parent: consensus::genesis_block(g), timestamp: g.created_at_ms, c, home, keys, seeds, view: 0, gap: 0, batch: vec![], lines: vec![], used: HashMap::new() }
    }

    pub fn prefix(&self) -> String {
        self.c.state.params.prefix().to_string()
    }

    pub fn key(&self, who: &str) -> &Keys {
        self.keys.get(who).unwrap_or_else(|| panic!("no key named {who}"))
    }

    /// The account of the key named `who`.
    pub fn addr(&self, who: &str) -> String {
        self.key(who).address_with(&self.prefix())
    }

    pub fn height(&self) -> u64 {
        self.c.height()
    }

    /// The next block's height.
    pub fn next(&self) -> u64 {
        self.c.height() + 1
    }

    /// A tx from account `from`, signed by the key named `signer`, with the next nonce `from` has
    /// in the block being built, valid for [`VALIDITY`] blocks and paying what the next 17
    /// blocks' base fee may reach (as wallets do; only the block's base fee is charged).
    pub fn tx_from(&mut self, signer: &str, from: &str, action: Action) -> Tx {
        self.tx_with(signer, from, action, |_| {})
    }

    /// [`Recorder::tx_from`] from the signer's own account.
    pub fn tx(&mut self, who: &str, action: Action) -> Tx {
        let from = self.addr(who);
        self.tx_from(who, &from, action)
    }

    /// [`Recorder::tx_from`], adjusted by `f` before it's priced and signed. A tx whose nonce `f`
    /// sets doesn't use one of the block's.
    pub fn tx_with(&mut self, signer: &str, from: &str, action: Action, f: impl FnOnce(&mut Tx)) -> Tx {
        let keys = Keys::from_seed(self.key(signer).seed());
        let next = self.next();
        let nonce = self.c.state.account(from).nonce + self.used.get(from).copied().unwrap_or(0);
        let mut tx = Tx { chain_id: self.c.genesis.chain_id.clone(), genesis_hash: self.c.genesis_hash.clone(), from: from.to_string(), nonce, fee: 0, tip: 0, valid_until: next + VALIDITY, action, memo: String::new(), public_key: hex::encode(keys.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
        f(&mut tx);
        if tx.nonce == nonce {
            *self.used.entry(from.to_string()).or_default() += 1;
        }
        if tx.fee == 0 {
            tx.fee = crate::econ::max_fee(&self.c.state, tx.size()) + tx.tip;
        }
        tx.sign(&keys)
    }

    /// The leader skips `views` views before the next block (their leaders were away).
    pub fn skip(&mut self, views: u64) {
        self.gap += views;
    }

    /// The next block comes `ms` sooner than the chain allows (a block `verify` refuses: the
    /// negative tests).
    pub fn rush(&mut self, ms: u64) {
        self.timestamp -= ms;
    }

    /// Executes the next block, carrying `txs`, and checks each ended as it must.
    pub fn block(&mut self, txs: Vec<(Tx, Want)>) {
        let h = self.next();
        let p = self.c.state.params.clone();
        let epoch = p.epoch_of(h);
        if h == 1 || p.epoch_of(h - 1) != epoch {
            self.view = 0;
        }
        let parent_view = self.view;
        let skipped = std::mem::take(&mut self.gap);
        self.view = parent_view + 1 + skipped;
        self.timestamp += p.block_time_ms * (1 + 3 * skipped);
        let proposer = consensus::leader(&self.c.state, epoch, self.view).expect("a set for the epoch");
        let exec_height = crate::snapshot::certifies(h).unwrap_or(h - 1);
        let exec_root = consensus::hex_32(&self.c.root_at(exec_height).expect("the cited root"));
        let version = crate::upgrade::version_at(&self.c.state, h);
        let raw = txs.iter().map(|(t, _)| t.to_bytes()).collect();
        let b = CBlock::new(self.parent.digest(), h, self.view, parent_view, self.timestamp, proposer, exec_height, exec_root, raw).with_versions(version, SIGNAL);
        let cert = (p.is_boundary(h) || h.is_multiple_of(BATCH)).then(|| self.certify(&b, epoch));
        self.c.execute(consensus::finalized_from(&b, cert.clone())).unwrap_or_else(|e| panic!("executing block {h}: {e:#}"));
        self.used.clear();
        self.check(h, &txs);
        self.batch.push(b.clone());
        if let Some(cert) = cert {
            self.close_batch(&cert);
        }
        self.parent = b;
    }

    /// Empty blocks up to and including `height`.
    pub fn until(&mut self, height: u64) {
        assert!(height >= self.height(), "already at block {}", self.height());
        while self.height() < height {
            self.block(vec![]);
        }
    }

    /// The finalization certificate of `b`, signed by `epoch`'s whole set.
    fn certify(&self, b: &CBlock, epoch: u64) -> Vec<u8> {
        let keys: Vec<String> = self.c.state.active_set(epoch).expect("the epoch's set").iter().map(|v| v.consensus_key.clone()).collect();
        let set = consensus::keys_of(keys.iter().map(String::as_str)).unwrap();
        let namespace = consensus::namespace(&self.c.genesis.chain_id, &self.c.genesis_hash);
        let proposal = Proposal::new(Round::new(Epoch::new(epoch), View::new(b.view)), View::new(b.parent_view), b.digest());
        let votes: Vec<_> = keys
            .iter()
            .map(|k| {
                let key = self.seeds.get(k).unwrap_or_else(|| panic!("no consensus key for validator {}…", &k[..16]));
                let s = MlDsaScheme::signer(&namespace, set.clone(), PrivateKey::from_seed_bytes(*key)).unwrap();
                Finalize::sign(&s, proposal.clone()).unwrap()
            })
            .collect();
        let verifier = MlDsaScheme::verifier(&namespace, set);
        Finalization::from_finalizes(&verifier, commonware_utils::iter::NonEmpty::try_new(votes.iter()).unwrap(), &commonware_parallel::Sequential).unwrap().encode().to_vec()
    }

    /// Checks that each of block `h`'s txs ended as it must (the replay's [`super::fates`]).
    fn check(&self, h: u64, txs: &[(Tx, Want)]) {
        let b = self.c.last_block().unwrap();
        let all: Vec<Tx> = txs.iter().map(|(t, _)| t.clone()).collect();
        for ((tx, want), fate) in txs.iter().zip(super::fates(b, &all).unwrap()) {
            let ok = match (want, &fate) {
                (Want::Applied, Fate::Applied) | (Want::Skipped, Fate::Skipped) => true,
                (Want::Failed(why), Fate::Failed(got)) => got.contains(why),
                _ => false,
            };
            let why_skipped = || self.c.failure(&tx.id()).map(|f| format!(": {}", f.0)).unwrap_or_default();
            assert!(ok, "block {h}: a {} tx (tag {}) must end {want:?}, but it {}{}", tx.action.kind(), tx.action.to_bytes()[0], super::told(&fate, "was"), if fate == Fate::Skipped { why_skipped() } else { String::new() });
        }
    }

    /// Closes the batch since the last certified block with `cert`, the certificate of its last:
    /// one `/api/sync` line.
    fn close_batch(&mut self, cert: &[u8]) {
        let blocks: Vec<String> = self.batch.iter().map(|b| hex::encode(b.encode())).collect();
        let line = serde_json::json!({ "from": self.batch[0].height, "to": self.batch.last().unwrap().height, "blocks": blocks, "finalization": hex::encode(cert) });
        self.lines.push(line.to_string());
        self.batch.clear();
    }

    /// Certifies the blocks since the last batch (an archive ends at a certified block) and writes
    /// the archive to `dir`: `meta.json`, `genesis.json`, the batches and the node's block log,
    /// gzipped as `scripts/archive-chain.mjs` closes its files, or (the committed corpus) as text.
    pub fn write(mut self, dir: &Path, source: &str, gzipped: bool) {
        if let Some(last) = self.batch.last().cloned() {
            // The recorder executes nothing more, so the last block's certificate is all it adds.
            let cert = self.certify(&last, self.c.state.params.epoch_of(last.height));
            self.close_batch(&cert);
        }
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
        std::fs::copy(self.home.join("genesis.json"), dir.join("genesis.json")).unwrap();
        let mut log = vec![];
        let mut segments: Vec<PathBuf> = std::fs::read_dir(self.home.join("blocks")).unwrap().map(|e| e.unwrap().path()).collect();
        segments.sort();
        for s in segments {
            log.extend(std::fs::read(s).unwrap());
        }
        let to = self.height();
        let sync = self.lines.join("\n") + "\n";
        let ext = if gzipped { "jsonl.gz" } else { "jsonl" };
        let files = [(format!("sync-000000000001.{ext}"), sync.into_bytes()), (format!("blocks-000000000001.{ext}"), log)];
        let mut meta = serde_json::json!({ "chain_id": self.c.genesis.chain_id, "genesis_hash": self.c.genesis_hash, "source": source, "segments": [], "open": null, "blocks": [] });
        for (i, (name, bytes)) in files.into_iter().enumerate() {
            let bytes = if gzipped { gzip(&bytes) } else { bytes };
            let sha = crate::release_cli::sha256_hex(&bytes);
            std::fs::write(dir.join(&name), &bytes).unwrap();
            meta[if i == 0 { "segments" } else { "blocks" }] = serde_json::json!([{ "file": name, "from": 1, "to": to, "sha256": sha }]);
        }
        std::fs::write(dir.join("meta.json"), serde_json::to_string_pretty(&meta).unwrap() + "\n").unwrap();
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.home).ok();
    }
}

/// `bytes` gzipped, the same bytes every time (no name, no time).
fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut e = flate2::GzBuilder::new().mtime(0).write(vec![], flate2::Compression::best());
    e.write_all(bytes).unwrap();
    e.finish().unwrap()
}

// ------------------------------------------------------------------------------------ chains

pub(super) const FANG: u64 = crate::types::DROPS_PER_FANG;

/// The fixed key named `name` (every account and validator of the recordings).
pub(super) fn seed(name: &str) -> [u8; 32] {
    *blake3::hash(format!("blockula replay corpus/{name}").as_bytes()).as_bytes()
}

pub(super) fn keys_of(names: &[&'static str]) -> BTreeMap<&'static str, Keys> {
    names.iter().map(|n| (*n, Keys::from_seed(seed(n)))).collect()
}

/// A genesis allocation of `fang` to `address`, vesting from `start` (cliff, end) if given.
pub(super) fn alloc(address: String, fang: u64, vesting: Option<(u64, u64, u64)>, label: &str) -> crate::chain::Allocation {
    let vesting = vesting.map(|(start, cliff, end)| crate::chain::Vesting { start, cliff, end });
    crate::chain::Allocation { address, amount: fang * FANG, vesting, label: Some(label.into()) }
}

/// A genesis validator: its consensus seed's address and key, its operator `owner`.
pub(super) fn validator(chain_id: &str, name: &str, owner: &str, self_bond: u64) -> (crate::chain::ValidatorInfo, crate::chain::Operator) {
    let k = Keys::from_seed(seed(name));
    let prefix = crate::crypto::parse_address(owner).map(|(p, _)| p.to_string()).unwrap();
    let (p2p, http) = (format!("{name}.replay.invalid:7071"), format!("https://{name}.replay.invalid"));
    let info = crate::chain::ValidatorInfo { address: k.address_with(&prefix), public_key: hex::encode(k.public_key()), p2p_addr: p2p.clone(), http_addr: http.clone() };
    let pop = crate::chain::make_pop(seed(name), chain_id, owner, &p2p, &http);
    let op = crate::chain::Operator { owner: owner.into(), consensus_key: info.public_key.clone(), pop, p2p_addr: p2p, http_addr: http, self_bond: self_bond * FANG, commission_bps: 500, provider: None, asn: None, country: None, entity: None };
    (info, op)
}

impl Recorder {
    /// The id of the proposal titled `title`.
    pub fn proposal(&self, title: &str) -> u64 {
        self.c.state.governance.proposals.iter().find(|(_, p)| p.title == title).map(|(id, _)| *id).unwrap_or_else(|| panic!("no proposal titled {title:?}"))
    }

    /// Aye votes on proposal `id` (1 FANG, no conviction) from each account in `voters`: the
    /// owners of every validator that may sit on the council when it closes.
    pub fn ayes(&mut self, id: u64, voters: &[&str]) -> Vec<(Tx, Want)> {
        let digest = crate::governance::digest_of(&self.c.state.governance.proposals[&id]);
        voters.iter().map(|v| (self.tx(v, Action::Vote { proposal_id: id, aye: true, amount: FANG, conviction: 0, proposal_digest: digest.clone() }), Want::Applied)).collect()
    }

    /// `delegator`'s pool shares with `validator`.
    pub fn shares(&self, delegator: &str, validator: &str) -> u64 {
        self.c.state.staking.delegations.get(&format!("{delegator}/{validator}")).map_or(0, |d| d.shares as u64)
    }

    /// The beacon duties validators have now, as `beacon::duties` sends them (signed by their
    /// consensus keys for their owners): each member of the epoch's set commits to the epoch after
    /// next (while `commit`), and in the epoch's second half whoever committed to the next one
    /// reveals. Except `skip`'s reveal: the validator that misses its round.
    pub fn beacon(&mut self, secrets: &mut BTreeMap<(String, u64), [u8; 32]>, skip: Option<&str>, commit: bool) -> Vec<(Tx, Want)> {
        let h = self.next();
        let p = self.c.state.params.clone();
        let epoch = p.epoch_of(h);
        let prefix = self.prefix();
        let name_of = |r: &Recorder, v: &str| r.keys.iter().find(|(_, k)| k.address_with(&prefix) == v).map(|(n, _)| *n).unwrap();
        let owner_of = |r: &Recorder, v: &str| r.c.state.validators.get(v).map(|x| x.owner.clone()).unwrap();
        let mut txs = vec![];
        let set: Vec<String> = self.c.state.active_set(epoch).unwrap().iter().map(|v| v.address.clone()).collect();
        for v in set {
            let target = epoch + 2;
            if commit && !secrets.contains_key(&(v.clone(), target)) && !self.c.state.staking.pools.get(&v).is_some_and(|p| p.jail.is_some()) {
                let secret = *blake3::hash(format!("{v}/{target}").as_bytes()).as_bytes();
                secrets.insert((v.clone(), target), secret);
                let commitment = hex::encode(crate::beacon::commitment(&self.c.genesis.chain_id, target, &v, &secret));
                let (name, owner) = (name_of(self, &v), owner_of(self, &v));
                txs.push((self.tx_from(name, &owner, Action::BeaconCommit { epoch: target, commitment }), Want::Applied));
            }
        }
        if h % p.epoch_blocks >= p.epoch_blocks / 2 {
            let due: Vec<(String, [u8; 32])> = secrets.iter().filter(|((v, t), _)| *t == epoch + 1 && skip.is_none_or(|s| name_of(self, v) != s)).map(|((v, _), s)| (v.clone(), *s)).collect();
            for (v, secret) in due {
                secrets.remove(&(v.clone(), epoch + 1));
                let (name, owner) = (name_of(self, &v), owner_of(self, &v));
                txs.push((self.tx_from(name, &owner, Action::BeaconReveal { epoch: epoch + 1, secret: hex::encode(secret) }), Want::Applied));
            }
        }
        txs
    }
}

// -------------------------------------------------------------------------------------- open

pub(super) const OPEN: &str = "replay-v1-open";
/// The open chain's epochs.
const E: u64 = 30;
const OPEN_KEYS: &[&str] = &[
    "v0", "v1", "v2", "v3", "v4", "o0", "o1", "o2", "o3", "alice", "alice2", "bob", "carol", "dave", "erin", "nina", "sam", "peggy", "agent", "oscar", "oscar2", "trent", "vic", "j1", "j2", "j3", "c1", "c2", "ivan", "mallory", "walter", "ulysses", "founder", "foundation",
];
const OPEN_VALIDATORS: &[&str] = &["v0", "v1", "v2", "v3", "v4"];

/// A testnet like blockula-testnet-2: no Season 0 admins, genesis validators run by operators
/// that aren't their consensus keys, vesting buckets (the bootstrap delegation's and the
/// founder's among them, the latter passed over by it), a contribution pool from genesis with
/// founding jurors in the Soul ledger, and Ghost Mode off. Its set starts at two, so it grows by
/// admission (tags 60 and 61) before stake alone can grow it.
pub(super) fn open_genesis() -> Genesis {
    let k = keys_of(OPEN_KEYS);
    let p = "tboo";
    let a = |n: &str| k[n].address_with(p);
    let (v0, op0) = validator(OPEN, "v0", &a("o0"), 2_000);
    let (v1, op1) = validator(OPEN, "v1", &a("o1"), 2_000);
    let mut params: BTreeMap<String, u64> = crate::params::ChainParams::dev_overrides(1000, E);
    params.extend([("min_self_bond", 1_000 * FANG), ("quorum_floor", 0), ("max_validators", 5), ("name_registration_blocks", 300), ("name_grace_blocks", 60), ("ghost_pool_cap", 10_000 * FANG)].map(|(k, v)| (k.to_string(), v)));
    let mut excluded = vec![a("founder"), a("foundation")];
    excluded.sort();
    let soul = |n: &str, s: u64, why: &str| crate::chain::SoulEntry { account: a(n), soul: s * crate::poc::SOUL, reason: why.into() };
    let mut allocations = vec![
        alloc(crate::chain::community(p), 1_000_000, None, "community"),
        alloc(crate::chain::treasury(p), 5_000_000, Some((0, 0, 2_000)), "treasury"),
        alloc(crate::chain::endowment(p), 500_000, Some((0, 0, 4_000)), "endowment"),
        alloc(crate::chain::bootstrap_delegation(p), 1_000_000, Some((300, 300, 1_500)), "bootstrap"),
        alloc(a("founder"), 2_000_000, Some((0, 300, 1_500)), "founder"),
        alloc(a("foundation"), 500_000, Some((0, 0, 1_500)), "foundation"),
        alloc(crate::poc::pool_address(p), 50_000, None, "contribution pool"),
        alloc(a("trent"), 3_000_000, None, "trent"),
    ];
    for (n, fang) in [("v0", 10_000), ("v1", 10_000), ("o0", 20_000), ("o1", 20_000), ("o2", 20_000), ("o3", 20_000), ("v4", 20_000), ("alice", 100_000), ("bob", 50_000), ("carol", 20_000), ("dave", 20_000), ("erin", 20_000), ("peggy", 20_000), ("oscar", 20_000), ("vic", 20_000), ("j1", 20_000), ("j2", 20_000), ("j3", 20_000), ("c1", 10_000), ("c2", 10_000), ("ivan", 10_000), ("mallory", 10_000)] {
        allocations.push(alloc(a(n), fang, None, n));
    }
    serde_json::from_value(serde_json::json!({
        "chain_id": OPEN, "token": "FANG", "decimals": 9, "block_time_ms": 1000, "faucet": true, "created_at_ms": 1_790_000_000_000u64,
        "validators": [v0, v1], "operators": [op0, op1], "allocations": allocations, "epoch_blocks": E, "validator_admins": [],
        "contribution": { "seance_blocks": 12, "claim_maturity_blocks": 4, "vesting_blocks": 200, "vouch_lock_blocks": 10, "claim_ttl_blocks": 300, "soul_half_life_blocks": 1_000_000 },
        "address_prefix": p, "params": params.iter().map(|(k, v)| (k.clone(), v.to_string())).collect::<BTreeMap<_, _>>(),
        "reserved_names": ["blockula", "boo", "treasury"],
        "soul_ledger": [soul("j1", 25, "founding juror"), soul("j2", 25, "founding juror"), soul("j3", 25, "founding juror"), soul("vic", 30, "maintainer"), soul("c1", 1, "contributor"), soul("c2", 1, "contributor")],
        "bootstrap_excluded": excluded, "pool_repos": { "blockula/replay": 10 },
    }))
    .unwrap()
}

/// The open chain's history (see the module docs and docs/REPLAY.md for what it covers).
pub(super) fn record_open() -> Recorder {
    use crate::types::KeyRole;
    use Want::*;
    let g = open_genesis();
    let validators: Vec<[u8; 32]> = OPEN_VALIDATORS.iter().map(|n| seed(n)).collect();
    let mut r = Recorder::new("open", &g, keys_of(OPEN_KEYS), &validators);
    let (chain_id, gh) = (g.chain_id.clone(), r.c.genesis_hash.clone());
    let a = |r: &Recorder, n: &str| r.addr(n);
    let key_id = |r: &Recorder, n: &str| crate::types::key_id(&r.key(n).public_key());

    // ---- epoch 0: accounts, names, keys, GitHub links, Hunts, the contribution pool, stake
    // Block 1: transfers (one opens an account), a name, and two that fail.
    let (bob, nina) = (a(&r, "bob"), a(&r, "nina"));
    let txs = vec![
        (r.tx_with("alice", &a(&r, "alice"), Action::Transfer { to: bob.clone(), amount: 10 * FANG }, |t| t.memo = "rent".into()), Applied),
        (r.tx("alice", Action::Transfer { to: nina.clone(), amount: FANG }), Applied),
        (r.tx("alice", Action::RegisterName { name: "alice".into() }), Applied),
        (r.tx("bob", Action::Transfer { to: "nobody.boo".into(), amount: FANG }), Failed("no one has claimed")),
        (r.tx("carol", Action::Transfer { to: bob.clone(), amount: 1_000_000 * FANG }), Failed("insufficient")),
        (r.tx("bob", Action::RegisterName { name: "blockula".into() }), Failed("reserved")),
    ];
    r.block(txs);
    // Keys: a second owner key, an agent's session key (and its Familiar), a key whose proof of
    // possession is for another account.
    let alice = a(&r, "alice");
    let peggy = a(&r, "peggy");
    let add_alice2 = crate::types::add_key(r.key("alice2"), &chain_id, &gh, &alice, "laptop", KeyRole::Owner);
    let add_agent = crate::types::add_key(r.key("agent"), &chain_id, &gh, &peggy, "claude", KeyRole::Session { limit: 3 * FANG, period_blocks: 400, expires_at: 600 });
    let stolen = crate::types::add_key(r.key("erin"), &chain_id, &gh, &alice, "not mine", KeyRole::Owner);
    let txs = vec![
        (r.tx("alice", add_alice2), Applied),
        (r.tx("peggy", add_agent), Applied),
        (r.tx("bob", stolen), Failed("")),
        (r.tx("dave", Action::RemoveKey { key_id: "ab".repeat(32) }), Failed("")),
    ];
    r.block(txs);
    let agent = key_id(&r, "agent");
    let txs = vec![
        (r.tx_from("alice2", &alice, Action::Transfer { to: bob.clone(), amount: FANG }), Applied),
        (r.tx_from("agent", &peggy, Action::Transfer { to: bob.clone(), amount: FANG }), Applied),
        (r.tx("peggy", Action::RegisterFamiliar { key_id: agent.clone(), label: "claude".into() }), Applied),
        (r.tx("alice", Action::RegisterFamiliar { key_id: key_id(&r, "alice2"), label: "not a session key".into() }), Failed("")),
    ];
    r.block(txs);
    // The agent opens an account with what's left of its limit (C21's pre-v2 case: below v2 the
    // opening fee goes on its spending without a check against the limit).
    let sam = a(&r, "sam");
    let txs = vec![
        (r.tx_from("agent", &peggy, Action::Transfer { to: sam, amount: FANG }), Applied),
        (r.tx("alice", Action::RemoveKey { key_id: key_id(&r, "alice2") }), Applied),
        (r.tx("alice", Action::RemoveKey { key_id: key_id(&r, "alice") }), Failed("")),
    ];
    r.block(txs);
    // GitHub links (on-chain only: validators check them against GitHub when they attest).
    let txs = vec![
        (r.tx("c1", Action::LinkGithub { handle: "replay-c1".into(), gist_id: "c1".into() }), Applied),
        (r.tx("c2", Action::LinkGithub { handle: "replay-c2".into(), gist_id: "c2".into() }), Applied),
        (r.tx("ivan", Action::LinkGithub { handle: "replay-ivan".into(), gist_id: "1fa".into() }), Applied),
        (r.tx("mallory", Action::LinkGithub { handle: "replay-mallory".into(), gist_id: "bad".into() }), Applied),
        (r.tx("dave", Action::LinkGithub { handle: "replay-dave".into(), gist_id: "d".into() }), Applied),
        (r.tx("dave", Action::UnlinkGithub { handle: "replay-dave".into() }), Applied),
        (r.tx("bob", Action::UnlinkGithub { handle: "replay-c1".into() }), Failed("")),
        (r.tx("c1", Action::LinkGithub { handle: "replay-c1-again".into(), gist_id: "c1".into() }), Failed("")),
    ];
    r.block(txs);
    // Hunts.
    let h = r.next();
    let txs = vec![
        (r.tx("alice", Action::CreateHunt { repo: "blockula/replay".into(), issue: 1, amount: 20 * FANG, expires_at: h + 300, title: "Replay me".into() }), Applied),
        (r.tx("dave", Action::CreateHunt { repo: "blockula/replay".into(), issue: 2, amount: 10 * FANG, expires_at: h + 10, title: "Short".into() }), Applied),
        (r.tx("erin", Action::CreateHunt { repo: "blockula/replay".into(), issue: 3, amount: 10 * FANG, expires_at: h + 500, title: "Refunded by a vote".into() }), Applied),
        (r.tx("erin", Action::CreateHunt { repo: "blockula/replay".into(), issue: 4, amount: 5 * FANG, expires_at: h + 50, title: "Too small".into() }), Failed("")),
    ];
    let first_hunt = r.c.state.next_hunt_id;
    r.block(txs);
    let (replay_me, short, refunded) = (first_hunt, first_hunt + 1, first_hunt + 2);
    let txs = vec![
        (r.tx("bob", Action::FundHunt { hunt_id: replay_me, amount: 3 * FANG }), Applied),
        (r.tx("bob", Action::FundHunt { hunt_id: 99, amount: 3 * FANG }), Failed("")),
        (r.tx("alice", Action::AwardHunt { hunt_id: replay_me, to: peggy.clone(), pr: 7, familiar_key: Some(agent.clone()) }), Applied),
        (r.tx("bob", Action::AwardHunt { hunt_id: short, to: bob.clone(), pr: 8, familiar_key: None }), Failed("")),
        (r.tx("erin", Action::CancelHunt { hunt_id: short }), Failed("")),
    ];
    r.block(txs);
    // The contribution pool: a vouch, the jury, claims, funding; the Season 0 levers fail (no
    // admins on this chain).
    let (ivan, c1, c2) = (a(&r, "ivan"), a(&r, "c1"), a(&r, "c2"));
    let claim = |repo: &str, pr: u64| Action::SubmitClaim { repo: repo.into(), pr, familiar_key: None };
    let first_claim = r.c.state.poc.next_claim_id;
    let txs = vec![
        (r.tx("vic", Action::Vouch { account: ivan.clone() }), Applied),
        (r.tx("bob", Action::Vouch { account: ivan.clone() }), Failed("")),
        (r.tx("j1", Action::JoinJury { bond: crate::poc::JUROR_BOND }), Applied),
        (r.tx("j2", Action::JoinJury { bond: crate::poc::JUROR_BOND }), Applied),
        (r.tx("j3", Action::JoinJury { bond: crate::poc::JUROR_BOND }), Applied),
        (r.tx("erin", Action::JoinJury { bond: crate::poc::JUROR_BOND }), Failed("")),
        (r.tx("c1", claim("blockula/replay", 101)), Applied),
        (r.tx("c2", claim("blockula/replay", 102)), Applied),
        (r.tx("ivan", claim("blockula/replay", 103)), Applied),
        (r.tx("c1", claim("blockula/replay", 104)), Applied),
        (r.tx("c1", claim("someone/else", 5)), Failed("")),
        (r.tx("c2", claim("blockula/replay", 101)), Failed("")),
        (r.tx("bob", Action::FundPool { amount: 5 * FANG }), Applied),
    ];
    r.block(txs);
    let (claim_c1, claim_c2, claim_ivan, claim_c1b) = (first_claim, first_claim + 1, first_claim + 2, first_claim + 3);
    let admin_only = vec![
        (r.tx("alice", Action::SetRepo { repo: "blockula/other".into(), weight: 5 }), Failed("")),
        (r.tx("alice", Action::GrantSoul { to: bob.clone(), amount: crate::poc::SOUL, reason: "no".into() }), Failed("")),
        (r.tx("alice", Action::FlagSybil { account: a(&r, "mallory"), reason: "no".into() }), Failed("")),
        (r.tx("alice", Action::RejectClaim { claim_id: claim_c1, reason: "no".into() }), Failed("")),
        (r.tx("alice", Action::Clawback { account: c1.clone(), reason: "no".into() }), Failed("")),
    ];
    r.block(admin_only);
    // The validators attest (q = 2 of 2): three claims merged, one not; then two of them see a
    // revert of ivan's. Their consensus keys sign for their owners (SF-13), as nodes do.
    let (o0, o1) = (a(&r, "o0"), a(&r, "o1"));
    let sha = |n: u8| format!("{n:02x}").repeat(20);
    let attest = |r: &mut Recorder, v: &str, o: &str, id: u64, ok: bool, s: String| r.tx_from(v, o, Action::AttestClaim { claim_id: id, ok, merge_sha: if ok { s } else { String::new() }, note: if ok { String::new() } else { "not merged".into() } });
    let mut txs = vec![];
    for (v, o) in [("v0", &o0), ("v1", &o1)] {
        txs.push((attest(&mut r, v, o, claim_c1, true, sha(1)), Applied));
        txs.push((attest(&mut r, v, o, claim_c2, true, sha(2)), Applied));
        txs.push((attest(&mut r, v, o, claim_ivan, true, sha(3)), Applied));
    }
    txs.push((attest(&mut r, "v0", &o0, claim_c1b, false, String::new()), Applied));
    txs.push((attest(&mut r, "o0", &o0, claim_c1, true, sha(1)), Failed("")));
    r.block(txs);
    let txs = vec![
        (r.tx_from("v0", &o0, Action::AttestRevert { claim_id: claim_ivan, revert_sha: sha(9) }), Applied),
        (r.tx_from("v1", &o1, Action::AttestRevert { claim_id: claim_ivan, revert_sha: sha(9) }), Applied),
        (r.tx_from("o0", &o0, Action::AttestRevert { claim_id: claim_c1, revert_sha: sha(1) }), Failed("")),
    ];
    r.block(txs);
    // Stake.
    let (v0, v1) = (a(&r, "v0"), a(&r, "v1"));
    let txs = vec![
        (r.tx("alice", Action::Delegate { validator: v0.clone(), amount: 100 * FANG }), Applied),
        (r.tx("bob", Action::Delegate { validator: v1.clone(), amount: 50 * FANG }), Applied),
        (r.tx("carol", Action::Delegate { validator: v0.clone(), amount: FANG / 2 }), Failed("")),
        (r.tx("carol", Action::Delegate { validator: a(&r, "erin"), amount: 5 * FANG }), Failed("")),
    ];
    r.block(txs);
    let half = r.shares(&alice, &v0) / 2;
    let txs = vec![
        (r.tx("alice", Action::Undelegate { validator: v0.clone(), shares: half / 2, amount: 0 }), Applied),
        (r.tx("bob", Action::Undelegate { validator: v1.clone(), shares: 0, amount: 10 * FANG }), Applied),
        (r.tx("alice", Action::Redelegate { from_validator: v0.clone(), to_validator: v1.clone(), shares: half / 2 }), Applied),
        (r.tx("erin", Action::WithdrawUnbonded {}), Failed("")),
        (r.tx("o0", Action::UpdateValidator { commission_bps: 550, p2p_addr: String::new(), http_addr: String::new() }), Applied),
        (r.tx_from("v1", &o1, Action::UpdateValidator { commission_bps: 450, p2p_addr: String::new(), http_addr: String::new() }), Applied),
        (r.tx("o1", Action::UpdateValidator { commission_bps: 700, p2p_addr: String::new(), http_addr: String::new() }), Failed("")),
        (r.tx("o0", Action::Unbond { validator: v0.clone() }), Failed("")),
        (r.tx("alice", Action::ApproveValidator { validator: v1.clone() }), Failed("")),
        (r.tx("alice", Action::RemoveValidator { validator: v1.clone() }), Failed("")),
    ];
    r.block(txs);
    r.skip(1);

    // ---- validators, a multisig, names, and every kind of proposal (epochs 0 and 1)
    let (o2, v2, v3, v4) = (a(&r, "o2"), a(&r, "v2"), a(&r, "v3"), a(&r, "v4"));
    let register = |r: &mut Recorder, owner: &str, v: &str, bond: u64, pop_for: Option<&str>| {
        let (p2p, http) = (format!("{v}.replay.invalid:7071"), format!("https://{v}.replay.invalid"));
        let pop = crate::chain::make_pop(seed(v), OPEN, &r.addr(pop_for.unwrap_or(owner)), &p2p, &http);
        let consensus_key = hex::encode(r.key(v).public_key());
        r.tx(owner, Action::RegisterValidator { consensus_key, p2p_addr: p2p, http_addr: http, bond: bond * FANG, pop })
    };
    let txs = vec![
        (register(&mut r, "o2", "v2", 2_000, None), Applied),
        (register(&mut r, "o2", "v2", 2_000, None), Failed("")),
        (register(&mut r, "erin", "v3", 2_000, Some("o3")), Failed("")),
        (register(&mut r, "erin", "v4", 10, None), Failed("")),
    ];
    r.block(txs);
    let oscar = a(&r, "oscar");
    let add_oscar2 = crate::types::add_key(r.key("oscar2"), &chain_id, &gh, &oscar, "second signer", KeyRole::Owner);
    let txs = vec![(r.tx("oscar", add_oscar2), Applied)];
    r.block(txs);
    let owners = vec![key_id(&r, "oscar"), key_id(&r, "oscar2")];
    let txs = vec![
        (r.tx("oscar", Action::MultisigCreate { owners, threshold: 2 }), Applied),
        (r.tx("dave", Action::MultisigCreate { owners: vec!["ab".repeat(32)], threshold: 1 }), Failed("")),
    ];
    r.block(txs);
    let m0 = r.c.state.governance.next_multisig_id;
    let pay = Action::Transfer { to: bob.clone(), amount: 5 * FANG };
    let too_much = Action::Transfer { to: bob.clone(), amount: 10_000_000 * FANG };
    let txs = vec![
        (r.tx("oscar", Action::MultisigPropose { action: Box::new(pay.clone()) }), Applied),
        (r.tx("oscar", Action::MultisigPropose { action: Box::new(too_much.clone()) }), Applied),
    ];
    r.block(txs);
    let txs = vec![
        (r.tx_from("oscar2", &oscar, Action::MultisigApprove { id: m0, action_digest: crate::types::action_digest(&pay) }), Applied),
        (r.tx_from("oscar2", &oscar, Action::MultisigApprove { id: m0 + 1, action_digest: "00".repeat(32) }), Failed("")),
        (r.tx_from("oscar2", &oscar, Action::MultisigApprove { id: m0 + 1, action_digest: crate::types::action_digest(&too_much) }), Failed("")),
    ];
    r.block(txs);
    // Names: renew, an offer and its acceptance, a release, and a name that will lapse.
    let txs = vec![
        (r.tx("alice", Action::RenewName { name: "alice".into(), years: 1 }), Applied),
        (r.tx("bob", Action::RenewName { name: "alice".into(), years: 1 }), Failed("")),
        (r.tx("carol", Action::RegisterName { name: "carol".into() }), Applied),
        (r.tx("alice", Action::TransferName { name: "alice".into(), to: bob.clone() }), Applied),
        (r.tx("carol", Action::TransferName { name: "alice".into(), to: a(&r, "carol") }), Failed("")),
    ];
    r.block(txs);
    let txs = vec![(r.tx("bob", Action::TransferName { name: "alice".into(), to: bob.clone() }), Applied)];
    r.block(txs);
    let txs = vec![
        (r.tx("bob", Action::ReleaseName { name: "alice".into() }), Applied),
        (r.tx("carol", Action::ReleaseName { name: "bob".into() }), Failed("")),
    ];
    r.block(txs);
    // Proposals of every kind. The council (⅔ of the closing epoch's set: v0, v1 and v2 by then)
    // passes them; each owner votes for its validator.
    let council = ["o0", "o1", "o2"];
    let h = r.next();
    let dist_id = crate::governance::next_distribution_id(&r.c.state);
    let (walter, ulysses) = (a(&r, "walter"), a(&r, "ulysses"));
    let entries = vec![(walter.clone(), 50 * FANG), (alice.clone(), 20 * FANG)];
    let (root, proofs) = crate::governance::build_distribution(dist_id, &entries).unwrap();
    let claim_until = h + 600;
    let late_claim = r.c.state.poc.next_claim_id;
    let (erin, mallory) = (a(&r, "erin"), a(&r, "mallory"));
    let propose = |r: &mut Recorder, who: &str, title: &str, kind: crate::types::ProposalKind| r.tx(who, Action::Propose { kind, deposit: 10 * FANG, title: title.into(), url: String::new() });
    use crate::types::ProposalKind as K;
    let txs = vec![
        (r.tx("c2", claim("blockula/replay", 105)), Applied),
        (propose(&mut r, "alice", "max validators", K::ParamChange { key: "max_validators".into(), value: 6 }), Applied),
        (propose(&mut r, "alice", "pay bob", K::TreasurySpend { to: bob.clone(), amount: 1_000 * FANG }), Applied),
        (propose(&mut r, "alice", "airdrop", K::Distribution { distribution_id: dist_id, merkle_root: hex::encode(root), total: 70 * FANG, claim_until }), Applied),
        (propose(&mut r, "alice", "docs repo", K::SetRepo { repo: "blockula/replay-docs".into(), weight: 3 }), Applied),
        (propose(&mut r, "alice", "soul for erin", K::GrantSoul { to: erin.clone(), amount: 10 * crate::poc::SOUL, reason: "docs".into() }), Applied),
        (propose(&mut r, "alice", "flag mallory", K::FlagSybil { account: mallory.clone(), reason: "sock puppet".into() }), Applied),
        (propose(&mut r, "alice", "unlink mallory", K::UnlinkGithub { handle: "replay-mallory".into() }), Applied),
        (propose(&mut r, "alice", "refund hunt", K::RefundHunt { hunt_id: refunded }), Applied),
        (propose(&mut r, "alice", "ghost on", K::ActivateFeature { feature: "ghost".into() }), Applied),
        (propose(&mut r, "alice", "hello", K::Text {}), Applied),
        (propose(&mut r, "bob", "nobody votes", K::Text {}), Applied),
        (propose(&mut r, "alice", "too cheap", K::Text {}), Applied),
        (propose(&mut r, "bob", "no such feature", K::ActivateFeature { feature: "warp".into() }), Failed("")),
    ];
    r.block(txs);
    let txs = vec![(propose(&mut r, "alice", "reject a claim", K::RejectClaim { claim_id: late_claim, reason: "duplicate".into() }), Applied)];
    r.block(txs);
    let mut votes = vec![];
    for title in ["max validators", "pay bob", "airdrop", "docs repo", "soul for erin", "flag mallory", "unlink mallory", "refund hunt", "ghost on", "hello", "too cheap", "reject a claim"] {
        let id = r.proposal(title);
        votes.extend(r.ayes(id, &council));
    }
    let spend_id = r.proposal("pay bob");
    let hello = r.proposal("hello");
    let digest = |r: &Recorder, id: u64| crate::governance::digest_of(&r.c.state.governance.proposals[&id]);
    let d = digest(&r, spend_id);
    votes.push((r.tx("trent", Action::Vote { proposal_id: spend_id, aye: true, amount: 100_000 * FANG, conviction: 2, proposal_digest: d }), Applied));
    let d = digest(&r, hello);
    votes.push((r.tx("alice", Action::Vote { proposal_id: hello, aye: false, amount: 10 * FANG, conviction: 1, proposal_digest: d }), Applied));
    votes.push((r.tx("bob", Action::Vote { proposal_id: hello, aye: true, amount: FANG, conviction: 0, proposal_digest: "00".repeat(32) }), Failed("")));
    r.block(votes);
    // The upgrade, and its cancellation in time (a v1 binary halts before a v2 block).
    let h = r.next();
    let activation = h + 300;
    let txs = vec![(propose(&mut r, "alice", "upgrade", K::ScheduleUpgrade { version: 2, activation_height: activation }), Applied)];
    r.block(txs);
    let id = r.proposal("upgrade");
    let txs = r.ayes(id, &council);
    r.block(txs);

    // ---- epoch 1: the set of two admits v2, which shows up in the epoch's last tenth
    r.until(E);
    let txs = vec![
        (r.tx_from("v0", &o0, Action::AdmitValidator { validator: v2.clone(), epoch: 1 }), Applied),
        (r.tx_from("v1", &o1, Action::AdmitValidator { validator: v2.clone(), epoch: 1 }), Applied),
        (r.tx("o0", Action::AdmitValidator { validator: v2.clone(), epoch: 1 }), Failed("")),
        (r.tx("vic", Action::Unvouch { account: ivan.clone() }), Applied),
        (r.tx("erin", Action::Unvouch { account: ivan.clone() }), Failed("")),
    ];
    r.block(txs);
    r.until(2 * E - 6);
    let txs = vec![(r.tx("o2", Action::ValidatorReady { epoch: 1 }), Failed(""))];
    r.block(txs);
    r.until(2 * E - 3);
    let txs = vec![(r.tx_from("v2", &o2, Action::ValidatorReady { epoch: 1 }), Applied)];
    r.block(txs);
    r.until(2 * E - 1);
    assert!(r.c.state.active_set(2).unwrap().iter().any(|v| v.address == v2), "v2 joins the set at epoch 2");

    // ---- epoch 2: v3 registers (it joins by stake), the beacon starts, the Séance opens
    let mut secrets = BTreeMap::new();
    let mut txs = vec![(register(&mut r, "o3", "v3", 2_000, None), Applied)];
    txs.extend(r.beacon(&mut secrets, None, true));
    r.block(txs);
    while r.next() < 3 * E - 1 {
        let txs = r.beacon(&mut secrets, None, true);
        r.block(txs);
    }
    let txs = r.beacon(&mut secrets, None, true);
    r.block(txs);
    let seance = r.c.state.poc.open_seance.expect("a Séance opens at the end of epoch 2");
    assert!(r.c.state.active_set(3).unwrap().iter().any(|v| v.address == v3), "v3 joins by stake at epoch 3");

    // ---- epoch 3: the jury judges, v4 registers, ghost and the rest of governance
    let jurors = ["j1", "j2", "j3"];
    let mut txs = r.beacon(&mut secrets, None, true);
    let mut ballots = BTreeMap::new();
    for j in jurors {
        let juror = r.addr(j);
        let pairs = r.c.state.poc.seances[&seance].jurors[&juror].pairs.clone();
        let votes = "=".repeat(pairs.len());
        let salt = format!("salt-{j}");
        let commitment = crate::types::seance_commitment(seance, &juror, &votes, &[], &salt);
        let pairs_digest = crate::poc::seat_pairs_digest(&r.c.state, seance, &juror, &pairs);
        txs.push((r.tx(j, Action::CommitVotes { seance_id: seance, commitment, pairs_digest }), Applied));
        ballots.insert(j, (votes, salt));
    }
    txs.push((r.tx("j1", Action::LeaveJury {}), Failed("")));
    txs.push((register(&mut r, "v4", "v4", 2_000, None), Applied));
    txs.push((r.tx("carol", Action::Delegate { validator: v3.clone(), amount: 20 * FANG }), Applied));
    r.block(txs);
    let commit_until = r.c.state.poc.seances[&seance].commit_until;
    while r.next() <= commit_until {
        let txs = r.beacon(&mut secrets, None, true);
        r.block(txs);
    }
    let mut txs = r.beacon(&mut secrets, None, true);
    let (votes, _) = ballots["j2"].clone();
    txs.push((r.tx("j2", Action::RevealVotes { seance_id: seance, votes, spam: vec![], salt: "wrong".into() }), Failed("")));
    for j in jurors {
        let (votes, salt) = ballots[j].clone();
        txs.push((r.tx(j, Action::RevealVotes { seance_id: seance, votes, spam: vec![], salt }), Applied));
    }
    r.block(txs);
    let reveal_until = r.c.state.poc.seances[&seance].reveal_until;
    while r.next() <= reveal_until {
        let txs = r.beacon(&mut secrets, None, true);
        r.block(txs);
    }
    assert!(r.c.state.poc.seances[&seance].closed, "the Séance closed");

    // ---- the proposals run (epoch 3), Ghost Mode on, more votes, the paid jury
    let hello = r.proposal("hello");
    let d = digest(&r, hello);
    let mut txs = r.beacon(&mut secrets, None, true);
    txs.extend([
        (r.tx("bob", Action::Vote { proposal_id: hello, aye: true, amount: FANG, conviction: 0, proposal_digest: d }), Failed("")),
        (r.tx("c1", Action::WithdrawVested {}), Applied),
        (r.tx("bob", Action::WithdrawVested {}), Failed("")),
        (r.tx("j3", Action::LeaveJury {}), Applied),
        (propose(&mut r, "alice", "claw back", K::Clawback { account: c2.clone(), reason: "duplicate work".into() }), Applied),
        (propose(&mut r, "trent", "tokens decide", K::ParamChange { key: "jail_blocks".into(), value: 50 }), Applied),
    ]);
    r.block(txs);
    let (claw, tokens) = (r.proposal("claw back"), r.proposal("tokens decide"));
    let d = digest(&r, tokens);
    let mut txs = r.beacon(&mut secrets, None, true);
    txs.extend(r.ayes(claw, &["o0", "o1", "o2", "v4"]));
    txs.push((r.tx("trent", Action::Vote { proposal_id: tokens, aye: true, amount: 1_000_000 * FANG, conviction: 0, proposal_digest: d }), Applied));
    r.block(txs);
    let upgrade = r.proposal("upgrade");
    while !crate::upgrade::is_active(&r.c.state, "ghost") || r.c.state.governance.proposals[&upgrade].status != crate::governance::ProposalStatus::Executed {
        let txs = r.beacon(&mut secrets, None, true);
        r.block(txs);
    }
    let mut txs = r.beacon(&mut secrets, None, true);
    txs.extend([
        (propose(&mut r, "alice", "cancel the upgrade", K::CancelUpgrade { version: 2, activation_height: activation }), Applied),
        (propose(&mut r, "alice", "unflag mallory", K::UnflagSybil { account: mallory.clone(), reason: "appeal".into() }), Applied),
    ]);
    r.block(txs);
    let mut txs = r.beacon(&mut secrets, None, true);
    for title in ["cancel the upgrade", "unflag mallory"] {
        let id = r.proposal(title);
        txs.extend(r.ayes(id, &["o0", "o1", "o2", "v4"]));
    }
    r.block(txs);
    // The airdrop: walter has no account (the claim pays its own fee and opening), alice has one.
    let claim_tx = |r: &mut Recorder, who: &str, amount: u64, proof: &[[u8; 32]]| r.tx(who, Action::ClaimDistribution { distribution_id: dist_id, amount: amount * FANG, proof: proof.iter().map(hex::encode).collect() });
    let mut txs = r.beacon(&mut secrets, None, true);
    txs.extend([
        (claim_tx(&mut r, "walter", 50, &proofs[0]), Applied),
        (claim_tx(&mut r, "alice", 20, &proofs[1]), Applied),
        (claim_tx(&mut r, "bob", 20, &proofs[1]), Failed("")),
    ]);
    r.block(txs);
    let mut txs = r.beacon(&mut secrets, None, true);
    txs.push((claim_tx(&mut r, "alice", 20, &proofs[1]), Failed("")));
    r.block(txs);
    // Ghost Mode: an address, a shield, a private transfer, an unshield that opens an account,
    // then a pause by f + 1 validators.
    let mut ghost = Ghost::new(OPEN);
    let (ga, gb) = (Ghost::key("alice"), Ghost::key("bob"));
    let mut txs = r.beacon(&mut secrets, None, true);
    txs.push((r.tx("alice", Action::SetGhostAddress { address: crate::types::HexBytes(ga.address().to_bytes().to_vec()) }), Applied));
    let (shield, note) = ghost.shield(&mut r, "alice", 50 * FANG, &ga);
    txs.push((shield, Applied));
    r.block(txs);
    let fee = crate::ghost::canonical_fee(&r.c.state, false);
    let outputs = [ghost.output(20 * FANG, &gb, &ga), ghost.output(30 * FANG - fee, &ga, &ga)];
    let to_bob = outputs[0].0;
    let mut txs = r.beacon(&mut secrets, None, true);
    txs.push((ghost.transfer(&r, &ga, note, outputs), Applied));
    r.block(txs);
    let (unshield, _) = ghost.unshield(&r, &gb, to_bob, &ulysses, 10 * FANG);
    let (double, _) = ghost.unshield(&r, &gb, to_bob, &bob, 5 * FANG);
    let mut txs = r.beacon(&mut secrets, None, true);
    txs.push((unshield, Applied));
    r.block(txs);
    let mut txs = r.beacon(&mut secrets, None, true);
    txs.extend([
        (r.tx("v0", Action::GhostPause {}), Applied),
        (r.tx("o2", Action::GhostPause {}), Applied),
        (r.tx("erin", Action::GhostPause {}), Failed("")),
    ]);
    r.block(txs);
    let mut txs = r.beacon(&mut secrets, None, true);
    txs.push((propose(&mut r, "alice", "ghost off", K::DeactivateFeature { feature: "ghost".into() }), Applied));
    r.block(txs);
    let off = r.proposal("ghost off");
    let mut txs = r.beacon(&mut secrets, None, true);
    txs.extend(r.ayes(off, &["o0", "o1", "o2", "v4"]));
    r.block(txs);
    // A Byzantine leader's block: txs that pass every stateless check but can't be admitted
    // (honest validators refuse such a block; execution skips them and charges the proposer).
    let (paused_shield, _) = ghost.shield(&mut r, "bob", 5 * FANG, &gb);
    let stale = r.tx_with("alice", &alice, Action::Transfer { to: bob.clone(), amount: FANG }, |t| t.nonce = 0);
    let session_name = r.tx_from("agent", &peggy, Action::RegisterName { name: "agentname".into() });
    let multisig_owner = r.tx("oscar", Action::Transfer { to: bob.clone(), amount: FANG });
    r.block(vec![(stale, Skipped), (session_name, Skipped), (multisig_owner, Skipped), (paused_shield, Skipped), (double, Skipped)]);
    r.skip(2);

    // ---- epoch 4: v4 joins; v3's double-sign is reported; stake comes back
    r.until(4 * E);
    assert!(r.c.state.active_set(4).unwrap().iter().any(|v| v.address == v4), "v4 joins by stake at epoch 4");
    let evidence = {
        use commonware_consensus::simplex::types::{Activity, ConflictingNotarize, Notarize};
        let keys: Vec<String> = r.c.state.active_set(3).unwrap().iter().map(|v| v.consensus_key.clone()).collect();
        let set = consensus::keys_of(keys.iter().map(String::as_str)).unwrap();
        let s = MlDsaScheme::signer(&consensus::namespace(OPEN, &gh), set, PrivateKey::from_seed_bytes(seed("v3"))).unwrap();
        let p = |d: u8| Proposal::new(Round::new(Epoch::new(3), View::new(5)), View::new(4), commonware_cryptography::blake3::Digest([d; 32]));
        hex::encode(Activity::ConflictingNotarize(ConflictingNotarize::new(Notarize::sign(&s, p(1)).unwrap(), Notarize::sign(&s, p(2)).unwrap())).encode())
    };
    let mut txs = r.beacon(&mut secrets, None, true);
    txs.extend([
        (r.tx("bob", Action::ReportEquivocation { evidence: evidence.clone() }), Applied),
        (r.tx("carol", Action::ReportEquivocation { evidence: "0a0b".into() }), Failed("")),
    ]);
    r.block(txs);
    let mut txs = r.beacon(&mut secrets, None, true);
    txs.extend([
        (r.tx("carol", Action::ReportEquivocation { evidence }), Failed("")),
    ]);
    r.block(txs);
    while r.next() < 5 * E + 5 {
        let txs = r.beacon(&mut secrets, None, true);
        r.block(txs);
    }
    let mut txs = r.beacon(&mut secrets, None, true);
    txs.extend([
        (r.tx("alice", Action::WithdrawUnbonded {}), Applied),
        (r.tx("bob", Action::WithdrawUnbonded {}), Applied),
        (r.tx("trent", Action::Unlock {}), Applied),
        (r.tx("bob", Action::Unlock {}), Failed("")),
    ]);
    r.block(txs);
    assert!(!r.c.state.active_set(5).unwrap().iter().any(|v| v.address == v3), "v3 is out for good");

    // ---- epoch 5: v1 commits but never reveals, and is jailed at its end
    while r.next() < 6 * E {
        let txs = r.beacon(&mut secrets, Some("v1"), true);
        r.block(txs);
    }
    assert!(r.c.state.staking.pools[&v1].jail.is_some(), "v1 missed its beacon round");

    // ---- epochs 6 to 8: the hunt that lapsed, v1 unjails, the last beacon rounds
    let mut txs = r.beacon(&mut secrets, None, true);
    txs.push((r.tx("dave", Action::CancelHunt { hunt_id: short }), Applied));
    r.block(txs);
    while r.next() < 6 * E + 20 {
        let txs = r.beacon(&mut secrets, None, true);
        r.block(txs);
    }
    let mut txs = r.beacon(&mut secrets, None, true);
    txs.push((r.tx("o1", Action::Unjail { validator: v1.clone() }), Failed("")));
    r.block(txs);
    let until = r.c.state.staking.pools[&v1].jail.as_ref().unwrap().until;
    while r.next() < until.max(8 * E) {
        let commit = r.next() < 8 * E;
        let txs = r.beacon(&mut secrets, None, commit);
        r.block(txs);
    }
    let mut txs = r.beacon(&mut secrets, None, false);
    txs.push((r.tx("o1", Action::Unjail { validator: v1.clone() }), Applied));
    r.block(txs);
    while secrets.keys().any(|(_, t)| *t > r.c.state.params.epoch_of(r.next())) {
        let txs = r.beacon(&mut secrets, None, false);
        r.block(txs);
    }
    r.until(10 * E);
    assert!(r.c.state.active_set(10).unwrap().iter().any(|v| v.address == v1), "v1 is back");
    assert!(r.c.state.upgrade.schedule.is_empty(), "the upgrade was cancelled in time");

    // ---- the name lapses and someone else takes it; then quiet epochs, a transfer each
    r.until(390);
    let txs = vec![
        (r.tx("dave", Action::RegisterName { name: "carol".into() }), Applied),
        (r.tx("carol", Action::RenewName { name: "carol".into(), years: 1 }), Failed("")),
    ];
    r.block(txs);
    let mut epoch = r.c.state.params.epoch_of(r.next());
    while r.next() < 2_000 {
        let e = r.c.state.params.epoch_of(r.next());
        if e != epoch {
            epoch = e;
            let txs = vec![(r.tx("alice", Action::Transfer { to: bob.clone(), amount: FANG }), Applied)];
            r.block(txs);
            if e.is_multiple_of(7) {
                r.skip(1);
            }
            continue;
        }
        if r.next() == 1_000 {
            let txs = vec![(propose(&mut r, "alice", "still here", K::Text {}), Applied)];
            r.block(txs);
            let id = r.proposal("still here");
            let txs = r.ayes(id, &["o0", "o1", "o2", "v4"]);
            r.block(txs);
            continue;
        }
        r.block(vec![]);
    }
    use crate::governance::ProposalStatus::{Executed, Rejected};
    let status = |r: &Recorder, title: &str| r.c.state.governance.proposals[&r.proposal(title)].status;
    // "hello": token nays outweigh the council's ayes, so the council can't pass it.
    let want = [("max validators", Executed), ("pay bob", Executed), ("airdrop", Executed), ("docs repo", Executed), ("soul for erin", Executed), ("flag mallory", Executed), ("unlink mallory", Executed), ("refund hunt", Executed), ("ghost on", Executed), ("hello", Rejected), ("too cheap", Executed), ("reject a claim", Executed), ("upgrade", Executed), ("claw back", Executed), ("tokens decide", Executed), ("cancel the upgrade", Executed), ("unflag mallory", Executed), ("ghost off", Executed), ("still here", Executed), ("nobody votes", Rejected)];
    let wrong: Vec<String> = want.iter().filter(|(t, s)| status(&r, t) != *s).map(|(t, s)| format!("{t:?}: {:?}, not {s:?} ({:?})", status(&r, t), r.c.state.governance.proposals[&r.proposal(t)].outcome)).collect();
    assert!(wrong.is_empty(), "proposals that ended otherwise: {wrong:#?}");
    r
}

// -------------------------------------------------------------------------------- Ghost Mode

/// Ghost Mode for the recordings: the node tests' wallet (`ghost::tests::Wallet`) for notes and
/// encryption, with every proof's masks drawn from a fixed seed, so a recording is the same each
/// time (a wallet proves with the OS's randomness).
pub(super) struct Ghost {
    w: crate::ghost::tests::Wallet,
    proofs: u64,
}

type Output = (blockula_ghost::Note, blockula_ghost::EncryptedNote);

impl Ghost {
    pub fn new(tag: &str) -> Ghost {
        Ghost { w: crate::ghost::tests::Wallet::new(tag), proofs: 0 }
    }

    pub fn key(name: &str) -> blockula_ghost::SpendingKey {
        blockula_ghost::SpendingKey::from_seed(seed(&format!("ghost/{name}")))
    }

    pub fn output(&mut self, value: u64, to: &blockula_ghost::SpendingKey, from: &blockula_ghost::SpendingKey) -> Output {
        self.w.output(value, to, from)
    }

    /// A shield of `amount` from `from` to `to`, proved and signed, and its note.
    pub fn shield(&mut self, r: &mut Recorder, from: &str, amount: u64, to: &blockula_ghost::SpendingKey) -> (Tx, blockula_ghost::Note) {
        let out = self.w.output(amount, to, to);
        let note = out.0;
        let spends = [self.w.dummy_spend(), self.w.dummy_spend()];
        let outputs = [out, self.w.dummy_output()];
        let anchor = anchor(&r.c);
        let bundle = Box::new(bundle(&r.c, &spends, &outputs, &anchor));
        let fee = crate::ghost::canonical_fee(&r.c.state, false);
        let addr = r.addr(from);
        let proofs = &mut self.proofs;
        let tx = r.tx_with(from, &addr, Action::Shield { amount, bundle }, |t| {
            t.fee = fee;
            prove(t, spends, [outputs[0].0, outputs[1].0], anchor, proofs);
        });
        (tx, note)
    }

    /// An unsigned private transfer of `note` (of `key`) into `outputs`.
    pub fn transfer(&mut self, r: &Recorder, key: &blockula_ghost::SpendingKey, note: blockula_ghost::Note, outputs: [Output; 2]) -> Tx {
        let spends = [spend(&r.c, key, note), self.w.dummy_spend()];
        let action = Action::PrivateTransfer { bundle: Box::new(bundle(&r.c, &spends, &outputs, &anchor(&r.c))) };
        self.unsigned(r, action, spends, [outputs[0].0, outputs[1].0], false)
    }

    /// An unsigned unshield of `amount` to `to` from `note` (of `key`), the rest back to `key`;
    /// and the change's note.
    pub fn unshield(&mut self, r: &Recorder, key: &blockula_ghost::SpendingKey, note: blockula_ghost::Note, to: &str, amount: u64) -> (Tx, blockula_ghost::Note) {
        let opens = r.c.state.creation_fee_for(to) > 0;
        let fee = crate::ghost::canonical_fee(&r.c.state, opens);
        let change = self.w.output(note.value - amount - fee, key, key);
        let back = change.0;
        let spends = [spend(&r.c, key, note), self.w.dummy_spend()];
        let outputs = [change, self.w.dummy_output()];
        let action = Action::Unshield { to: to.into(), amount, bundle: Box::new(bundle(&r.c, &spends, &outputs, &anchor(&r.c))) };
        (self.unsigned(r, action, spends, [outputs[0].0, outputs[1].0], opens), back)
    }

    fn unsigned(&mut self, r: &Recorder, action: Action, spends: [blockula_ghost::SpendWitness; 2], outputs: [blockula_ghost::Note; 2], opens: bool) -> Tx {
        let fee = crate::ghost::canonical_fee(&r.c.state, opens);
        let mut tx = Tx { chain_id: r.c.genesis.chain_id.clone(), genesis_hash: r.c.genesis_hash.clone(), from: String::new(), nonce: 0, fee, tip: 0, valid_until: r.next() + VALIDITY, action, memo: String::new(), public_key: String::new(), signature: String::new() };
        prove(&mut tx, spends, outputs, anchor(&r.c), &mut self.proofs);
        tx
    }
}

/// The chain's current commitment-tree root (what a wallet proves against).
fn anchor(c: &Chain) -> blockula_ghost::Digest {
    blockula_ghost::field::digest_from_bytes(&c.state.ghost.meta.root).unwrap()
}

/// A spend of `note` (of `key`), at its place in the tree as a wallet rebuilds it.
fn spend(c: &Chain, key: &blockula_ghost::SpendingKey, note: blockula_ghost::Note) -> blockula_ghost::SpendWitness {
    let mut tree = blockula_ghost::FullTree::new();
    for (_, cm) in c.state.ghost.commitments.iter() {
        tree.append(blockula_ghost::field::digest_from_bytes(cm).unwrap()).unwrap();
    }
    let cm = blockula_ghost::field::digest_to_bytes(&note.commitment());
    let pos = c.state.ghost.commitments.iter().find(|(_, x)| **x == cm).map(|(p, _)| *p as u32).expect("the note is in the tree");
    blockula_ghost::SpendWitness::new(key, note, tree.path(pos).unwrap())
}

fn bundle(c: &Chain, spends: &[blockula_ghost::SpendWitness; 2], outputs: &[Output; 2], anchor: &blockula_ghost::Digest) -> crate::types::GhostBundle {
    use crate::types::HexBytes;
    let h = |d: &blockula_ghost::Digest| HexBytes(blockula_ghost::field::digest_to_bytes(d).to_vec());
    crate::types::GhostBundle {
        instance: HexBytes(c.state.ghost.meta.instance.to_vec()),
        anchor: h(anchor),
        nullifiers: [h(&spends[0].nullifier()), h(&spends[1].nullifier())],
        commitments: [h(&outputs[0].0.commitment()), h(&outputs[1].0.commitment())],
        enc: [HexBytes(outputs[0].1.enc.clone()), HexBytes(outputs[1].1.enc.clone())],
        out: [HexBytes(outputs[0].1.out.clone()), HexBytes(outputs[1].1.out.clone())],
        proof: HexBytes::default(),
    }
}

/// Proves `tx`'s action for its sighash (every field but the proof and the signature), with
/// the masks of the next fixed seed.
fn prove(tx: &mut Tx, spends: [blockula_ghost::SpendWitness; 2], outputs: [blockula_ghost::Note; 2], anchor: blockula_ghost::Digest, proofs: &mut u64) {
    let sighash = tx.ghost_sighash().unwrap();
    let w = blockula_ghost::ActionWitness { spends, outputs, anchor, sighash };
    *proofs += 1;
    let (_, proof) = blockula_ghost::circuit::prove_with_seed(&w, seed(&format!("proof/{}/{proofs}", tx.chain_id))).unwrap();
    match &mut tx.action {
        Action::Shield { bundle, .. } | Action::PrivateTransfer { bundle } | Action::Unshield { bundle, .. } => bundle.proof = crate::types::HexBytes(proof),
        _ => unreachable!(),
    }
}

// ----------------------------------------------------------------------------------- season0

pub(super) const SEASON0: &str = "replay-v1-season0";
const SEASON0_KEYS: &[&str] = &["s0", "s1", "o1", "bob", "carol", "erin", "j1", "j2", "j3", "c1", "c2", "c3", "mallory", "newbie"];

/// A dev chain, as `blockula dev` summons one: a validator that owns itself and is the Season 0
/// admin, a faucet, short durations. What only admins do happens here: approving and removing
/// validators (tags 5 and 6) and the contribution pool's levers (tags 15, 16, 19, 22, 29, and the
/// admin's side of 9 and 13).
pub(super) fn season0_genesis() -> Genesis {
    let k = keys_of(SEASON0_KEYS);
    let p = "boo";
    let a = |n: &str| k[n].address_with(p);
    let (s0, op0) = validator(SEASON0, "s0", &a("s0"), 2_000);
    let mut params: BTreeMap<String, u64> = crate::params::ChainParams::dev_overrides(1000, 20);
    params.insert("min_self_bond".into(), 1_000 * FANG);
    let mut allocations = vec![alloc(crate::chain::treasury(p), 1_000_000, None, "treasury")];
    for (n, fang) in [("s0", 1_000_000), ("o1", 20_000), ("bob", 10_000), ("carol", 10_000), ("erin", 10_000), ("j1", 10_000), ("j2", 10_000), ("j3", 10_000), ("c1", 10_000), ("c2", 10_000), ("c3", 10_000), ("mallory", 10_000), ("newbie", 10_000)] {
        allocations.push(alloc(a(n), fang, None, n));
    }
    serde_json::from_value(serde_json::json!({
        "chain_id": SEASON0, "token": "FANG", "decimals": 9, "block_time_ms": 1000, "faucet": true, "created_at_ms": 1_790_500_000_000u64,
        "validators": [s0], "operators": [op0], "allocations": allocations, "epoch_blocks": 20, "validator_admins": [a("s0")],
        "contribution": { "seance_blocks": 9, "claim_maturity_blocks": 2, "vesting_blocks": 100, "vouch_lock_blocks": 10, "claim_ttl_blocks": 100, "soul_half_life_blocks": 1_000_000 },
        "address_prefix": p, "params": params.iter().map(|(k, v)| (k.clone(), v.to_string())).collect::<BTreeMap<_, _>>(),
    }))
    .unwrap()
}

/// The Season 0 chain's history.
pub(super) fn record_season0() -> Recorder {
    use Want::*;
    let g = season0_genesis();
    let mut r = Recorder::new("season0", &g, keys_of(SEASON0_KEYS), &[seed("s0"), seed("s1")]);
    let a = |r: &Recorder, n: &str| r.addr(n);
    let (s0, s1, bob, mallory, newbie) = (a(&r, "s0"), a(&r, "s1"), a(&r, "bob"), a(&r, "mallory"), a(&r, "newbie"));
    let repo = "blockula/season0";
    // The admin opens the pool to a repo and grants Soul.
    let mut txs = vec![
        (r.tx("s0", Action::SetRepo { repo: repo.into(), weight: 10 }), Applied),
        (r.tx("bob", Action::SetRepo { repo: "bob/repo".into(), weight: 10 }), Failed("")),
        (r.tx("s0", Action::FundPool { amount: 1_000 * FANG }), Applied),
    ];
    for (n, soul) in [("j1", 20), ("j2", 20), ("j3", 20), ("c1", 2), ("c2", 2), ("c3", 2)] {
        let to = r.addr(n);
        txs.push((r.tx("s0", Action::GrantSoul { to, amount: soul * crate::poc::SOUL, reason: "season 0".into() }), Applied));
    }
    txs.push((r.tx("bob", Action::GrantSoul { to: bob.clone(), amount: crate::poc::SOUL, reason: "me".into() }), Failed("")));
    r.block(txs);
    let mut txs = vec![];
    for (n, handle) in [("c1", "s0-c1"), ("c2", "s0-c2"), ("c3", "s0-c3"), ("mallory", "squatted")] {
        txs.push((r.tx(n, Action::LinkGithub { handle: handle.into(), gist_id: "s0".into() }), Applied));
    }
    r.block(txs);
    let first = r.c.state.poc.next_claim_id;
    let mut txs = vec![];
    for (n, pr) in [("c1", 1), ("c2", 2), ("c3", 3)] {
        txs.push((r.tx(n, Action::SubmitClaim { repo: repo.into(), pr, familiar_key: None }), Applied));
    }
    for j in ["j1", "j2", "j3"] {
        txs.push((r.tx(j, Action::JoinJury { bond: crate::poc::JUROR_BOND }), Applied));
    }
    txs.push((r.tx("j1", Action::Vouch { account: newbie.clone() }), Applied));
    r.block(txs);
    // One validator, so one attestation verifies a claim; the admin rejects the third.
    let sha = |n: u8| format!("{n:02x}").repeat(20);
    let txs = vec![
        (r.tx("s0", Action::AttestClaim { claim_id: first, ok: true, merge_sha: sha(1), note: String::new() }), Applied),
        (r.tx("s0", Action::AttestClaim { claim_id: first + 1, ok: true, merge_sha: sha(2), note: String::new() }), Applied),
        (r.tx("s0", Action::RejectClaim { claim_id: first + 2, reason: "reverted".into() }), Applied),
        (r.tx("s0", Action::RejectClaim { claim_id: first + 2, reason: "again".into() }), Failed("")),
        (r.tx("newbie", Action::Unvouch { account: newbie.clone() }), Applied),
    ];
    r.block(txs);
    let txs = vec![
        (r.tx("s0", Action::UnlinkGithub { handle: "squatted".into() }), Applied),
        (r.tx("bob", Action::UnlinkGithub { handle: "s0-c1".into() }), Failed("")),
        (r.tx("s0", Action::FlagSybil { account: mallory.clone(), reason: "squatter".into() }), Applied),
        (r.tx("s0", Action::FlagSybil { account: mallory.clone(), reason: "again".into() }), Failed("")),
    ];
    r.block(txs);
    let h = r.next();
    let hunt = r.c.state.next_hunt_id;
    let txs = vec![(r.tx("erin", Action::CreateHunt { repo: repo.into(), issue: 9, amount: 10 * FANG, expires_at: h + 500, title: "Cancelled by the admin".into() }), Applied)];
    r.block(txs);
    let txs = vec![(r.tx("s0", Action::CancelHunt { hunt_id: hunt }), Applied)];
    r.block(txs);
    // The admin pins a validator into the set, then removes it.
    let (p2p, http) = ("s1.replay.invalid:7071".to_string(), "https://s1.replay.invalid".to_string());
    let pop = crate::chain::make_pop(seed("s1"), SEASON0, &a(&r, "o1"), &p2p, &http);
    let consensus_key = hex::encode(r.key("s1").public_key());
    let txs = vec![(r.tx("o1", Action::RegisterValidator { consensus_key, p2p_addr: p2p, http_addr: http, bond: 2_000 * FANG, pop }), Applied)];
    r.block(txs);
    let txs = vec![
        (r.tx("s0", Action::ApproveValidator { validator: s1.clone() }), Applied),
        (r.tx("bob", Action::ApproveValidator { validator: s1.clone() }), Failed("")),
    ];
    r.block(txs);
    r.until(20);
    assert!(r.c.state.active_set(1).unwrap().iter().any(|v| v.address == s1), "the admin's pick joins at epoch 1");
    r.until(45);
    let txs = vec![
        (r.tx("s0", Action::RemoveValidator { validator: s1.clone() }), Applied),
        (r.tx("o1", Action::Unbond { validator: s1.clone() }), Failed("")),
    ];
    r.block(txs);
    // The Séance (jurors from the admin's grants), its pay, and a clawback.
    r.until(59);
    let seance = r.c.state.poc.open_seance.expect("a Séance opens at the end of epoch 2");
    assert!(!r.c.state.active_set(3).unwrap().iter().any(|v| v.address == s1), "the admin removed it");
    let mut txs = vec![(r.tx("s0", Action::RemoveValidator { validator: s0.clone() }), Failed(""))];
    let mut ballots = BTreeMap::new();
    for j in ["j1", "j2", "j3"] {
        let juror = r.addr(j);
        let pairs = r.c.state.poc.seances[&seance].jurors[&juror].pairs.clone();
        let votes = "=".repeat(pairs.len());
        let salt = format!("season0-{j}");
        let commitment = crate::types::seance_commitment(seance, &juror, &votes, &[], &salt);
        let pairs_digest = crate::poc::seat_pairs_digest(&r.c.state, seance, &juror, &pairs);
        txs.push((r.tx(j, Action::CommitVotes { seance_id: seance, commitment, pairs_digest }), Applied));
        ballots.insert(j, (votes, salt));
    }
    r.block(txs);
    let commit_until = r.c.state.poc.seances[&seance].commit_until;
    r.until(commit_until);
    let mut txs = vec![];
    for j in ["j1", "j2", "j3"] {
        let (votes, salt) = ballots[j].clone();
        txs.push((r.tx(j, Action::RevealVotes { seance_id: seance, votes, spam: vec![], salt }), Applied));
    }
    r.block(txs);
    let reveal_until = r.c.state.poc.seances[&seance].reveal_until;
    r.until(reveal_until + 10);
    let c2 = a(&r, "c2");
    let txs = vec![
        (r.tx("c1", Action::WithdrawVested {}), Applied),
        (r.tx("s0", Action::Clawback { account: c2.clone(), reason: "duplicate".into() }), Applied),
        (r.tx("bob", Action::Clawback { account: c2.clone(), reason: "me too".into() }), Failed("")),
    ];
    r.block(txs);
    let txs = vec![(r.tx("s0", Action::Clawback { account: c2, reason: "again".into() }), Failed(""))];
    r.block(txs);
    r.until(200);
    r
}
