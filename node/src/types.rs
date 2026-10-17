use crate::crypto;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

/// 1 FANG = 10^9 drops 🩸
pub const DECIMALS: u32 = 9;
pub const DROPS_PER_FANG: u64 = 1_000_000_000;
pub const NAME_SUFFIX: &str = ".boo";

/// u64 amounts travel as JSON strings so JavaScript never loses precision.
pub mod u64_str {
    use serde::{de::Error, Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum N {
            S(String),
            N(u64),
        }
        match N::deserialize(d)? {
            N::S(s) => s.parse().map_err(D::Error::custom),
            N::N(n) => Ok(n),
        }
    }
}

/// JSON (the API format) rejects unknown fields, so nothing unsigned can ride along.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    /// `to` is an address (of this chain's prefix) or a `.boo` name.
    Transfer {
        to: String,
        #[serde(with = "u64_str")]
        amount: u64,
    },
    RegisterName { name: String },
    /// Authorize another key on this account (owner keys only). The added key consents: `pop` is
    /// its signature over `key_pop_message(chain_id, genesis_hash, account, key_id)` (context
    /// `KEY_POP_CONTEXT`), so nobody can put a key they don't hold on their own accounts (and
    /// pad its `/api/key` listing: audit gap-7-2).
    AddKey {
        key_id: String,
        label: String,
        role: KeyRole,
        /// The added key's hex ML-DSA-65 public key (`key_id` is its BLAKE3).
        public_key: String,
        /// Its hex ML-DSA-65 proof of possession for this account (see `key_pop`).
        pop: String,
    },
    /// Revoke a key (owner keys only). The last owner key can't be removed.
    RemoveKey { key_id: String },
    /// Offer to validate: bonds `bond` drops from the sender and creates a *pending* validator
    /// owned by the sender. `consensus_key` is the validator's hex ML-DSA-65 public key.
    RegisterValidator {
        consensus_key: String,
        p2p_addr: String,
        http_addr: String,
        #[serde(with = "u64_str")]
        bond: u64,
        /// Proof of possession: hex ML-DSA-65 signature by `consensus_key` over
        /// `pop_message(chain_id, from, p2p_addr, http_addr)` with context `POP_CONTEXT`.
        /// Stops anyone registering (squatting) a key they don't hold.
        #[serde(default)]
        pop: String,
    },
    /// Season 0 allowlist: an admin approves a pending validator (active from the next epoch).
    ApproveValidator { validator: String },
    /// Admin removes a validator (it leaves the set at the next epoch boundary).
    RemoveValidator { validator: String },
    /// Owner reclaims the bond of a removed validator, one full epoch after it left.
    Unbond { validator: String },
    /// Claim a GitHub handle for this account. Verified off-chain: a public gist owned by the
    /// handle must contain the account's address (see `/api/github/{handle}`).
    LinkGithub { handle: String, gist_id: String },
    /// Drop a GitHub link (the holder, or a Season 0 admin to clear squatters).
    UnlinkGithub { handle: String },
    /// Put a bounty on a GitHub issue. `amount` is escrowed from the sender.
    CreateHunt {
        repo: String,
        issue: u64,
        #[serde(with = "u64_str")]
        amount: u64,
        expires_at: u64,
        title: String,
    },
    /// Add to an open hunt's bounty (refunded pro-rata if the hunt is cancelled).
    FundHunt {
        hunt_id: u64,
        #[serde(with = "u64_str")]
        amount: u64,
    },
    /// Pay a hunt's escrow to `to` (address or name) for `pr`. Creator or Season 0 admin only.
    AwardHunt {
        hunt_id: u64,
        to: String,
        pr: u64,
        /// Optional: the recipient's registered Familiar (agent session key) that did the work.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        familiar_key: Option<String>,
    },
    /// Refund a hunt's funders. Creator after `expires_at`, or a Season 0 admin any time.
    CancelHunt { hunt_id: u64 },
    /// Mark one of this account's session keys as an agent Familiar (shown on profiles/awards).
    RegisterFamiliar { key_id: String, label: String },

    // ---- 🪦 Proof of Contribution (see poc.rs) ----
    /// Season 0 admins: make a GitHub repo eligible for the contribution pool, with a weight
    /// (1–100) for splitting it; weight 0 removes the repo.
    SetRepo { repo: String, weight: u64 },
    /// Season 0 admins: bootstrap Soul (milli-Soul, at most 100 Soul per grant). Governance
    /// replaces this in Phase 3.
    GrantSoul { to: String, amount: u64, reason: String },
    /// Stake `VOUCH_BOND` on `account` (a newcomer: no Soul or judged work) being a real,
    /// distinct person. Needs Soul. Lost (with Soul) if the account is flagged as a sybil.
    Vouch { account: String },
    /// The voucher withdraws a vouch (and its bond) once it has been locked for
    /// `vouch_lock_blocks`; the account vouched for may decline it any time.
    Unvouch { account: String },
    /// Season 0 admins: mark an account as a sybil. Zeroes its Soul, rejects its open claims,
    /// claws back its unvested rewards, and slashes whoever vouched for it.
    FlagSybil { account: String, reason: String },
    /// Claim a merged PR for the contribution pool. Needs a linked GitHub handle (the PR's
    /// author) and Soul or a vouch. Bonded; validators check GitHub and attest. A registered
    /// Familiar (agent session key) may sign this for its sponsor.
    SubmitClaim {
        repo: String,
        pr: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        familiar_key: Option<String>,
    },
    /// Validators only: what GitHub says about a pending claim. `merge_sha` is the 40-hex merge
    /// commit when `ok`; `note` says why when not.
    AttestClaim {
        claim_id: u64,
        ok: bool,
        merge_sha: String,
        #[serde(default)]
        note: String,
    },
    /// Season 0 admins: reject a claim (e.g. the PR was reverted). Slashes the bond; a scored
    /// claim's unvested reward is clawed back.
    RejectClaim { claim_id: u64, reason: String },
    /// Join the Séance juror pool by bonding at least `JUROR_BOND`. Needs Soul.
    JoinJury {
        #[serde(with = "u64_str")]
        bond: u64,
    },
    /// Leave the juror pool and get the bond back (not while serving on an open Séance).
    LeaveJury {},
    /// A juror's sealed judgment: hex BLAKE3 of `seance_commitment(...)`. `pairs_digest` is
    /// `seance_pairs_digest` of the pairs (and their claims' PRs) the juror judged, so a node that
    /// showed other pairs makes the commitment fail instead of misdirecting it (audit gap-7-6).
    CommitVotes { seance_id: u64, commitment: String, pairs_digest: String },
    /// Open the sealed judgment: one of `a`, `b`, `=` per assigned pair, claims flagged as spam,
    /// and the salt.
    RevealVotes { seance_id: u64, votes: String, spam: Vec<u64>, salt: String },
    /// Add FANG to the contribution pool.
    FundPool {
        #[serde(with = "u64_str")]
        amount: u64,
    },
    /// Withdraw every reward that has vested so far.
    WithdrawVested {},
    /// Season 0 admins: return an account's unvested rewards to the pool.
    Clawback { account: String, reason: String },

    // ---- protocol v1 (MAINNET-DESIGN 3.1, tags 30–49: staking.rs, governance.rs, beacon.rs,
    // econ.rs) ----
    /// Stake `amount` in a validator's pool (staking.rs).
    Delegate {
        validator: String,
        #[serde(with = "u64_str")]
        amount: u64,
    },
    /// Start unbonding stake in a pool: exactly `shares` of it, or (with `shares` 0) the shares
    /// worth `amount` drops at the pool's price when the tx runs (`staking::shares_worth`: rounded
    /// up, so at least `amount` unbonds). One of the two is set. Naming FANG, a signer never takes
    /// a share price from a node's report (audit gap-7-5). `WithdrawUnbonded` pays them out after
    /// `unbonding_blocks`.
    Undelegate {
        validator: String,
        #[serde(with = "u64_str")]
        shares: u64,
        #[serde(default, with = "u64_str")]
        amount: u64,
    },
    /// Move `shares` from one pool to another without unbonding.
    Redelegate {
        from_validator: String,
        to_validator: String,
        #[serde(with = "u64_str")]
        shares: u64,
    },
    /// Withdraw every unbonding entry that has finished.
    WithdrawUnbonded {},
    /// A validator's owner sets its commission and, when non-empty, its addresses.
    UpdateValidator { commission_bps: u64, p2p_addr: String, http_addr: String },
    /// Return a jailed validator to the candidates once `jail_blocks` have passed.
    Unjail { validator: String },
    /// Two conflicting signed votes by one validator: hex, at most `MAX_EVIDENCE_BYTES` bytes.
    ReportEquivocation { evidence: String },
    /// An active validator's commitment to its secret for the beacon of `epoch` (hex
    /// `beacon::commitment`, which binds the chain, epoch and validator too; beacon.rs).
    BeaconCommit { epoch: u64, commitment: String },
    /// The 32-byte hex secret behind an earlier commitment.
    BeaconReveal { epoch: u64, secret: String },
    /// Put `kind` to a vote, bonding `deposit` (governance.rs). `url` links the full text.
    Propose {
        kind: ProposalKind,
        #[serde(with = "u64_str")]
        deposit: u64,
        title: String,
        #[serde(default)]
        url: String,
    },
    /// Vote with `amount` of the voter's own unlocked holdings, locked for `conviction` (0–6)
    /// lock periods: weight is amount × 0.1 with no lock, then 1×–6× for 1–32 periods.
    /// `proposal_digest` is `proposal_digest(kind, title, url)` of the proposal the voter was
    /// shown: a vote for different content is refused (audit gap-7-6).
    Vote {
        proposal_id: u64,
        aye: bool,
        #[serde(with = "u64_str")]
        amount: u64,
        conviction: u8,
        proposal_digest: String,
    },
    /// Release the vote locks that have expired.
    Unlock {},
    /// Make this account an M-of-N multisig of its owner keys (`owners` are key ids).
    MultisigCreate { owners: Vec<String>, threshold: u8 },
    /// A multisig owner proposes an action for the account (never another multisig proposal);
    /// it runs once `threshold` owners approve.
    MultisigPropose { action: Box<Action> },
    /// Approve proposal `id`; `action_digest` is `action_digest` of the action the approver was
    /// shown, so approving anything else fails (audit gap-7-6).
    MultisigApprove { id: u64, action_digest: String },
    /// Extend a name you own by `years` (econ.rs).
    RenewName { name: String, years: u8 },
    /// Give a name you own to another account (address or name).
    TransferName { name: String, to: String },
    /// Give up a name you own; anyone may register it again.
    ReleaseName { name: String },
    /// Claim `amount` from a community distribution: `proof` is the Merkle path (hex 32-byte
    /// hashes) from the leaf `(distribution_id, address, amount)` to the distribution's root.
    ClaimDistribution {
        distribution_id: u64,
        #[serde(with = "u64_str")]
        amount: u64,
        proof: Vec<String>,
    },
    /// Validators only: a claim's PR was reverted during its revert window, by the 40-hex commit
    /// `revert_sha`.
    AttestRevert { claim_id: u64, revert_sha: String },

    // ---- 👻 Ghost Mode (tags 50–54, ghost.rs). Disabled until governance activates the `ghost`
    // feature: until then every one of them is refused ("Ghost Mode is off"). ----
    /// Move `amount` from the sender into the shielded pool, as the bundle's output notes (its
    /// spends are dummies). Signed like any tx; also burns `ghost_verify_fee`.
    Shield {
        #[serde(with = "u64_str")]
        amount: u64,
        bundle: Box<GhostBundle>,
    },
    /// Unsigned (the proof authorizes it): spend notes, create notes. The tx's `fee` leaves the
    /// pool and is burned (its `tip` goes to the proposer).
    PrivateTransfer { bundle: Box<GhostBundle> },
    /// Unsigned: spend notes and pay `amount` out of the pool to `to` (address or name), plus
    /// the fee as for a private transfer.
    Unshield {
        to: String,
        #[serde(with = "u64_str")]
        amount: u64,
        bundle: Box<GhostBundle>,
    },
    /// Publish this account's Ghost Mode address (1,216 bytes, hex in JSON), so wallets find it
    /// under the account's `.boo` names. Owner keys only.
    SetGhostAddress { address: HexBytes },
    /// An active validator (its consensus key or owner) votes to pause Ghost Mode; it pauses
    /// once f + 1 of the epoch's validators have.
    GhostPause {},

    // ---- a set of one or two (tags 60–61, staking.rs; audit round 3, review-fix-staking-2):
    // the next newcomer would be needed for every quorum, so it joins only with both ----
    /// A candidate's consensus key (from its own account or its owner's, SF-13): its node is
    /// running in `epoch`'s last tenth (the epoch it lands in). Changes nothing but the
    /// candidate's readiness record; accepted only once the set's members admitted it.
    ValidatorReady { epoch: u64 },
    /// An active member's consensus key admits candidate `validator` (an address) into the set
    /// at the end of `epoch` (the epoch it lands in), on its operator's decision (`blockula
    /// validator admit`; nodes never send it). A newcomer into a set of one or two needs a
    /// quorum of the members it joins. Once per member and epoch.
    AdmitValidator { validator: String, epoch: u64 },
}

/// What a `Propose` asks for: every former admin lever, plus parameters, spending, upgrades and
/// features (MAINNET-DESIGN 3.5). Encoded after its own tag byte, in this order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProposalKind {
    /// Set a `ChainParams` key (params.rs), within its bounds.
    ParamChange {
        key: String,
        #[serde(with = "u64_str")]
        value: u64,
    },
    /// Pay `amount` from the treasury to `to` (address or name).
    TreasurySpend {
        to: String,
        #[serde(with = "u64_str")]
        amount: u64,
    },
    /// Open a community distribution of `total` from the community account, claimable with
    /// Merkle proofs against `merkle_root` until `claim_until`. `distribution_id` is the id its
    /// tree's leaves were built for, and the one it opens under: the proposer picks one no
    /// distribution has (`/api/proposals`' `next_distribution_id`), checked when proposed and
    /// when it runs, so a passed distribution is always claimable and doesn't depend on which
    /// proposals land first (audit F45, review-fix-gov-2).
    Distribution {
        distribution_id: u64,
        merkle_root: String,
        #[serde(with = "u64_str")]
        total: u64,
        claim_until: u64,
    },
    SetRepo { repo: String, weight: u64 },
    /// Bounded like the admin grant (milli-Soul, at most `MAX_SOUL_GRANT`).
    GrantSoul { to: String, amount: u64, reason: String },
    FlagSybil { account: String, reason: String },
    RejectClaim { claim_id: u64, reason: String },
    Clawback { account: String, reason: String },
    UnlinkGithub { handle: String },
    /// Refund an open hunt's funders (nobody can redirect its escrow).
    RefundHunt { hunt_id: u64 },
    ScheduleUpgrade { version: u32, activation_height: u64 },
    ActivateFeature { feature: String },
    DeactivateFeature { feature: String },
    /// No on-chain effect: a signal.
    Text {},
    /// Lift a sybil flag: `FlagSybil`'s appeal path. The account can vouch, be vouched for,
    /// claim, earn Soul and join juries again; nothing the flag took comes back.
    UnflagSybil { account: String, reason: String },
    /// Withdraw the scheduled upgrade to protocol `version` at `activation_height` (and any
    /// scheduled after it) before it activates: it must run at least `upgrade::MIN_UPGRADE_LEAD`
    /// blocks before that height. A supermajority kind, like `ScheduleUpgrade`, but with no
    /// timelock: it keeps the rules in force. Tag 15.
    CancelUpgrade { version: u32, activation_height: u64 },
}

/// What a key is allowed to do on an account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", from = "KeyRoleJson")]
pub enum KeyRole {
    /// Full control: another device's passkey, or a cold recovery key.
    Owner,
    /// Agent/app key: transfers only, at most `limit` drops (amount + charged fee) per `period_blocks`,
    /// and nothing at all after block `expires_at`.
    Session {
        #[serde(with = "u64_str")]
        limit: u64,
        period_blocks: u64,
        expires_at: u64,
    },
}

/// `KeyRole`'s JSON, read strictly: serde ignores extra fields on an internally tagged unit
/// variant, so `owner` is a struct variant here.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum KeyRoleJson {
    Owner {},
    Session {
        #[serde(with = "u64_str")]
        limit: u64,
        period_blocks: u64,
        expires_at: u64,
    },
}

impl From<KeyRoleJson> for KeyRole {
    fn from(r: KeyRoleJson) -> Self {
        match r {
            KeyRoleJson::Owner {} => KeyRole::Owner,
            KeyRoleJson::Session { limit, period_blocks, expires_at } => KeyRole::Session { limit, period_blocks, expires_at },
        }
    }
}

/// A key's id is BLAKE3(public key) as hex, the same 32 bytes inside a `boo1…` address.
pub fn key_id(public_key: &[u8]) -> String {
    hex::encode(crypto::hash(public_key))
}

/// Domain separation for a key's proof of possession when it's added to an account (FIPS 204
/// context string; validators' consensus keys use `POP_CONTEXT`).
pub const KEY_POP_CONTEXT: &[u8] = b"blockula/key-pop/v1";

/// What a key signs to consent to being added to `account` (`AddKey.pop`): the chain instance,
/// the account and the key's own id. Not the role or label, which the account's owners choose.
pub fn key_pop_message(chain_id: &str, genesis_hash: &str, account: &str, key_id: &str) -> Vec<u8> {
    let mut out = b"BLOCKULA_KEY_POP_V1".to_vec();
    for s in [chain_id, genesis_hash, account, key_id] {
        put_str(&mut out, s);
    }
    out
}

#[cfg_attr(not(test), allow(dead_code))]
/// `keys`' proof of possession for being added to `account` on the chain `(chain_id, genesis_hash)`.
pub fn key_pop(keys: &crypto::Keys, chain_id: &str, genesis_hash: &str, account: &str) -> String {
    hex::encode(keys.sign_with_context(KEY_POP_CONTEXT, &key_pop_message(chain_id, genesis_hash, account, &key_id(&keys.public_key()))))
}

/// An `AddKey` for `keys`, with its proof of possession for `account`.
#[cfg_attr(not(test), allow(dead_code))]
pub fn add_key(keys: &crypto::Keys, chain_id: &str, genesis_hash: &str, account: &str, label: &str, role: KeyRole) -> Action {
    Action::AddKey { key_id: key_id(&keys.public_key()), label: label.into(), role, public_key: hex::encode(keys.public_key()), pop: key_pop(keys, chain_id, genesis_hash, account) }
}

/// What a `MultisigApprove` binds: BLAKE3 (derive-key mode) of the action's canonical encoding
/// (as inside the signing bytes), so an approval is for that action and nothing else (audit gap-7-6).
pub fn action_digest(action: &Action) -> String {
    let mut out = vec![];
    action.encode(&mut out);
    hex::encode(blake3::derive_key("blockula multisig action v1", &out))
}

/// What a `Vote` binds: the proposal's kind (accounts resolved, as stored), title and link.
pub fn proposal_digest(kind: &ProposalKind, title: &str, url: &str) -> String {
    let mut out = vec![];
    kind.encode(&mut out);
    put_str(&mut out, title);
    put_str(&mut out, url);
    hex::encode(blake3::derive_key("blockula proposal v1", &out))
}

/// What a `CommitVotes` binds: the Séance, the juror and, per assigned pair in order, both claims'
/// ids, repos and PR numbers (what the juror compared).
pub fn seance_pairs_digest(seance_id: u64, juror: &str, pairs: &[[(u64, &str, u64); 2]]) -> String {
    let mut out = vec![];
    put_u64(&mut out, seance_id);
    put_str(&mut out, juror);
    out.extend_from_slice(&(pairs.len() as u16).to_le_bytes());
    for pair in pairs {
        for (id, repo, pr) in pair {
            put_u64(&mut out, *id);
            put_str(&mut out, repo);
            put_u64(&mut out, *pr);
        }
    }
    hex::encode(blake3::derive_key("blockula seance pairs v1", &out))
}

/// Checks an `AddKey`'s proof of possession: `public_key` is the key `key_id` names and it signed
/// its consent to `account` on this chain instance.
pub fn verify_key_pop(chain_id: &str, genesis_hash: &str, account: &str, key_id: &str, public_key: &str, pop: &str) -> Result<()> {
    let pk = hex::decode(public_key).map_err(|_| anyhow!("public_key must be hex"))?;
    if self::key_id(&pk) != key_id {
        bail!("public_key isn't the key key_id names (key_id is BLAKE3 of the public key)");
    }
    let sig = hex::decode(pop).map_err(|_| anyhow!("pop must be a hex ML-DSA-65 signature"))?;
    if !crypto::verify_with_context(&pk, KEY_POP_CONTEXT, &key_pop_message(chain_id, genesis_hash, account, key_id), &sig) {
        bail!("the added key's proof of possession doesn't verify: the key must sign its consent to this account on this chain (see Wallet.keyGrant)");
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tx {
    pub chain_id: String,
    /// The chain instance: hex BLAKE3 digest of its genesis block (`/api/status` `genesis_hash`).
    /// Signed right after `chain_id`, so a tx can't replay on another chain that reuses the id
    /// (a relaunched testnet, audit #7). Checked statelessly at admission and in block verification.
    pub genesis_hash: String,
    /// The account acting. The signing key must be authorized on it.
    pub from: String,
    pub nonce: u64,
    /// The most the sender will pay. The tx is charged the block's base fee for its size plus
    /// `tip` (`econ::charge`), which must fit; nothing above the charge is debited.
    #[serde(with = "u64_str")]
    pub fee: u64,
    /// Paid to the block's proposer on top of the burned base fee. Proposals take the highest
    /// tip per byte first.
    #[serde(default, with = "u64_str")]
    pub tip: u64,
    /// Last height at which the tx may execute. Admission and execution require
    /// `height <= valid_until <= height + max_tx_validity_blocks`.
    pub valid_until: u64,
    pub action: Action,
    #[serde(default)]
    pub memo: String,
    /// hex ML-DSA-65 public key (1952 bytes)
    pub public_key: String,
    /// hex ML-DSA-65 signature over `signing_bytes()` (3309 bytes)
    pub signature: String,
}

/// Domain separator that starts every tx's signing bytes (and so its wire form).
pub const TX_DOMAIN: &[u8] = b"BLOCKULA_TX_V3";

/// Domain separator of the unsigned ghost wire form (`PrivateTransfer`, `Unshield`): no key
/// signs those, their Action proof authorizes them (see [`Tx::signing_bytes`] and ghost.rs).
pub const GHOST_TX_DOMAIN: &[u8] = b"BLOCKULA_GHOST_V1";
/// Codec bound on a tx that carries a ghost bundle (tags 50–52; a proof alone is up to 192 KiB).
/// Admission and blocks hold them to the governance parameter `ghost_max_tx_bytes` (≤ this).
pub const MAX_GHOST_TX_BYTES: usize = 1 << 20;
/// A note commitment, nullifier, tree root or chain instance in a ghost bundle.
pub const GHOST_DIGEST_BYTES: usize = 32;
/// One output's note ciphertext (ML-KEM-768 ciphertext ‖ AEAD of the note and memo) …
pub const GHOST_ENC_BYTES: usize = blockula_ghost::encryption::ENC_CIPHERTEXT_BYTES;
/// … and its outgoing ciphertext (for the sender's outgoing viewing key).
pub const GHOST_OUT_BYTES: usize = blockula_ghost::encryption::OUT_CIPHERTEXT_BYTES;
/// A Ghost Mode address (`owner ‖ ML-KEM-768 encapsulation key`).
pub const GHOST_ADDRESS_BYTES: usize = blockula_ghost::keys::ADDRESS_BYTES;

/// Bytes that travel raw in the wire form and as lowercase hex in JSON (ghost bundles and
/// addresses, which are too large to keep as hex strings). `Debug` shows the length and the
/// first bytes only.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct HexBytes(pub Vec<u8>);

impl HexBytes {
    /// The bytes as a 32-byte array, if that's their length.
    pub fn array32(&self) -> Option<[u8; 32]> {
        self.0.as_slice().try_into().ok()
    }
}

impl std::fmt::Debug for HexBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let head = hex::encode(&self.0[..self.0.len().min(8)]);
        write!(f, "HexBytes({} bytes: {head}{})", self.0.len(), if self.0.len() > 8 { "…" } else { "" })
    }
}

impl Serialize for HexBytes {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for HexBytes {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        if !crypto::is_lower_hex(&s) {
            return Err(serde::de::Error::custom("expected lowercase hex"));
        }
        hex::decode(&s).map(HexBytes).map_err(serde::de::Error::custom)
    }
}

/// What a ghost action (tags 50–52) carries (docs/GHOST-MODE.md section 8). Every field but the
/// proof is bound to it through the sighash ([`Tx::ghost_sighash`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GhostBundle {
    /// The chain instance the proof is for (`instance` in `/api/ghost`): carried, so proofs
    /// verify without state, and checked against the chain's at admission and execution.
    pub instance: HexBytes,
    /// The commitment-tree root the real spends are in (a recent one).
    pub anchor: HexBytes,
    /// The two spends' nullifiers (dummies included) and the two outputs' note commitments:
    /// canonical digests (8 little-endian u32s below the KoalaBear modulus).
    pub nullifiers: [HexBytes; 2],
    pub commitments: [HexBytes; 2],
    /// Each output's note ciphertext (`GHOST_ENC_BYTES`) and outgoing ciphertext
    /// (`GHOST_OUT_BYTES`).
    pub enc: [HexBytes; 2],
    pub out: [HexBytes; 2],
    /// The Action proof (postcard-encoded Plonky3 proof, at most 192 KiB).
    pub proof: HexBytes,
}

impl GhostBundle {
    /// Everything but the proof, in wire order: what the sighash covers.
    fn encode_public(&self, out: &mut Vec<u8>) {
        let [n0, n1] = &self.nullifiers;
        let [c0, c1] = &self.commitments;
        let [e0, e1] = &self.enc;
        let [o0, o1] = &self.out;
        for b in [&self.instance, &self.anchor, n0, n1, c0, c1, e0, e1, o0, o1] {
            out.extend_from_slice(&b.0);
        }
    }

    /// Wire form: the fixed-size fields, then the proof with a u32 length.
    fn encode(&self, out: &mut Vec<u8>) {
        self.encode_public(out);
        out.extend_from_slice(&(self.proof.0.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.proof.0);
    }

    fn decode(r: &mut Reader) -> Result<GhostBundle> {
        let mut take = |n: usize| -> Result<HexBytes> { Ok(HexBytes(r.take(n)?.to_vec())) };
        let (instance, anchor) = (take(GHOST_DIGEST_BYTES)?, take(GHOST_DIGEST_BYTES)?);
        let nullifiers = [take(GHOST_DIGEST_BYTES)?, take(GHOST_DIGEST_BYTES)?];
        let commitments = [take(GHOST_DIGEST_BYTES)?, take(GHOST_DIGEST_BYTES)?];
        let enc = [take(GHOST_ENC_BYTES)?, take(GHOST_ENC_BYTES)?];
        let out = [take(GHOST_OUT_BYTES)?, take(GHOST_OUT_BYTES)?];
        let len = r.u32()? as usize;
        if len > blockula_ghost::MAX_PROOF_BYTES {
            bail!("`proof` too long ({len} bytes, max {})", blockula_ghost::MAX_PROOF_BYTES);
        }
        let proof = HexBytes(r.take(len)?.to_vec());
        Ok(GhostBundle { instance, anchor, nullifiers, commitments, enc, out, proof })
    }

    /// Sizes, canonical digests, distinct nullifiers, a proof within its bound.
    fn check(&self) -> Result<()> {
        let digest = |field: &str, b: &HexBytes| -> Result<()> {
            let ok = b.array32().is_some_and(|a| blockula_ghost::field::digest_from_bytes(&a).is_some());
            if !ok {
                bail!("`{field}` must be a canonical 32-byte digest");
            }
            Ok(())
        };
        if self.instance.0.len() != GHOST_DIGEST_BYTES {
            bail!("`instance` must be 32 bytes");
        }
        digest("anchor", &self.anchor)?;
        for b in &self.nullifiers {
            digest("nullifiers", b)?;
        }
        for b in &self.commitments {
            digest("commitments", b)?;
        }
        if self.nullifiers[0] == self.nullifiers[1] {
            bail!("the two spends have the same nullifier");
        }
        if self.enc.iter().any(|e| e.0.len() != GHOST_ENC_BYTES) || self.out.iter().any(|o| o.0.len() != GHOST_OUT_BYTES) {
            bail!("each output needs a {GHOST_ENC_BYTES}-byte note ciphertext (`enc`) and an {GHOST_OUT_BYTES}-byte outgoing ciphertext (`out`)");
        }
        if self.proof.0.is_empty() || self.proof.0.len() > blockula_ghost::MAX_PROOF_BYTES {
            bail!("`proof` must be 1–{} bytes", blockula_ghost::MAX_PROOF_BYTES);
        }
        Ok(())
    }
}

/// Strings carry a u16 length. Longer ones can't occur in a valid tx: `check_format` caps the
/// whole tx at `MAX_TX_BYTES` (counted on the real lengths) before anything relies on them.
fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u16).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn put_ids(out: &mut Vec<u8>, ids: &[u64]) {
    out.extend_from_slice(&(ids.len() as u16).to_le_bytes());
    for id in ids {
        out.extend_from_slice(&id.to_le_bytes());
    }
}

fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_strs(out: &mut Vec<u8>, xs: &[String]) {
    out.extend_from_slice(&(xs.len() as u16).to_le_bytes());
    for x in xs {
        put_str(out, x);
    }
}

/// Bounds-checked cursor over a tx's wire bytes (the inverse of `put_*`).
struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.buf.len() < n {
            bail!("truncated transaction");
        }
        let (head, rest) = self.buf.split_at(n);
        self.buf = rest;
        Ok(head)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn bool(&mut self, field: &str) -> Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            b => bail!("`{field}` must be 0 or 1, got {b}"),
        }
    }

    fn str(&mut self, field: &str, max: usize) -> Result<String> {
        let len = self.u16()? as usize;
        if len > max {
            bail!("`{field}` too long ({len} bytes, max {max})");
        }
        String::from_utf8(self.take(len)?.to_vec()).map_err(|_| anyhow!("`{field}` isn't valid UTF-8"))
    }

    /// Optional strings travel as `""` for `None`, so `Some("")` has no encoding.
    fn opt_str(&mut self, field: &str, max: usize) -> Result<Option<String>> {
        let s = self.str(field, max)?;
        Ok((!s.is_empty()).then_some(s))
    }

    fn ids(&mut self, field: &str, max: usize) -> Result<Vec<u64>> {
        let n = self.u16()? as usize;
        if n > max {
            bail!("`{field}` has too many entries ({n}, max {max})");
        }
        (0..n).map(|_| self.u64()).collect()
    }

    fn strs(&mut self, field: &str, max: usize, max_len: usize) -> Result<Vec<String>> {
        let n = self.u16()? as usize;
        if n > max {
            bail!("`{field}` has too many entries ({n}, max {max})");
        }
        (0..n).map(|_| self.str(field, max_len)).collect()
    }
}

/// Longest `chain_id`.
pub const MAX_CHAIN_ID: usize = 64;
/// Longest address string (bech32's limit).
pub const MAX_ADDRESS: usize = 90;
/// Longest GitHub handle.
pub const MAX_GITHUB_HANDLE: usize = 39;
/// Longest `owner/name` repo (39 + 1 + 100).
pub const MAX_REPO: usize = 140;
/// Hex key ids, commitments and the like (32 bytes).
const HEX_32: usize = 64;
/// Largest equivocation evidence (decoded bytes): two signed votes fit with room to spare.
pub const MAX_EVIDENCE_BYTES: usize = 16 << 10;
pub const MAX_PROPOSAL_TITLE: usize = 96;
pub const MAX_URL: usize = 200;
/// Longest Merkle proof a distribution claim may carry (a tree of up to 2^64 leaves).
pub const MAX_PROOF: usize = 64;
/// Longest feature name (`ActivateFeature`).
pub const MAX_FEATURE: usize = 32;
/// Longest `ChainParams` key.
pub const MAX_PARAM_KEY: usize = 64;
/// `RenewName` renews for 1 to this many years at once.
pub const MAX_RENEW_YEARS: u8 = 10;
/// Largest action a `MultisigPropose` may carry, encoded: the proposal is stored until it's
/// approved or expires, so it's bounded well below `MAX_TX_BYTES` (audit F17). A nested
/// validator registration (~10.6 KiB: key and proof of possession) fits.
pub const MAX_MULTISIG_ACTION_BYTES: usize = 12 << 10;
/// Largest value of an integer field a tx carries as a JSON number rather than a string (ids,
/// heights, PR numbers, nonces): 2^53 − 1, the largest a JavaScript number holds exactly
/// (review-r2-clients-1). Clients compute `proposal_digest` and `action_digest` from the node's
/// JSON of a stored proposal kind or multisig action, so a larger number, which would arrive
/// rounded, would make it impossible to vote on or approve from JavaScript. Amounts travel as
/// strings (`u64_str`) and aren't bounded by this.
pub const MAX_JSON_INT: u64 = (1 << 53) - 1;

/// [`MAX_JSON_INT`] for each named JSON-number field.
fn check_json_ints(ints: &[(&str, u64)]) -> Result<()> {
    if let Some((field, v)) = ints.iter().find(|(_, v)| *v > MAX_JSON_INT) {
        bail!("{field} must be at most {MAX_JSON_INT} (2^53 − 1: it travels as a JSON number), not {v}");
    }
    Ok(())
}

/// What a juror commits to before revealing (hashed with BLAKE3). Mirrored in sdk-ts/src/tx.ts
/// `seanceCommitment`.
pub fn seance_commitment(seance_id: u64, juror: &str, votes: &str, spam: &[u64], salt: &str) -> String {
    let mut out = b"BLOCKULA_SEANCE_V1".to_vec();
    out.extend_from_slice(&seance_id.to_le_bytes());
    put_str(&mut out, juror);
    put_str(&mut out, votes);
    put_ids(&mut out, spam);
    put_str(&mut out, salt);
    hex::encode(crypto::hash(&out))
}

impl Action {
    /// The action's tag and fields, as they appear in the signing bytes (layouts of tags 0–29
    /// are unchanged since v2).
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Action::Transfer { to, amount } => {
                out.push(0);
                put_str(out, to);
                out.extend_from_slice(&amount.to_le_bytes());
            }
            Action::RegisterName { name } => {
                out.push(1);
                put_str(out, name);
            }
            Action::AddKey { key_id, label, role, public_key, pop } => {
                out.push(2);
                put_str(out, key_id);
                put_str(out, label);
                match role {
                    KeyRole::Owner => out.push(0),
                    KeyRole::Session { limit, period_blocks, expires_at } => {
                        out.push(1);
                        out.extend_from_slice(&limit.to_le_bytes());
                        out.extend_from_slice(&period_blocks.to_le_bytes());
                        out.extend_from_slice(&expires_at.to_le_bytes());
                    }
                }
                put_str(out, public_key);
                put_str(out, pop);
            }
            Action::RemoveKey { key_id } => {
                out.push(3);
                put_str(out, key_id);
            }
            Action::RegisterValidator { consensus_key, p2p_addr, http_addr, bond, pop } => {
                out.push(4);
                put_str(out, consensus_key);
                put_str(out, p2p_addr);
                put_str(out, http_addr);
                out.extend_from_slice(&bond.to_le_bytes());
                put_str(out, pop);
            }
            Action::ApproveValidator { validator } => {
                out.push(5);
                put_str(out, validator);
            }
            Action::RemoveValidator { validator } => {
                out.push(6);
                put_str(out, validator);
            }
            Action::Unbond { validator } => {
                out.push(7);
                put_str(out, validator);
            }
            Action::LinkGithub { handle, gist_id } => {
                out.push(8);
                put_str(out, handle);
                put_str(out, gist_id);
            }
            Action::UnlinkGithub { handle } => {
                out.push(9);
                put_str(out, handle);
            }
            Action::CreateHunt { repo, issue, amount, expires_at, title } => {
                out.push(10);
                put_str(out, repo);
                out.extend_from_slice(&issue.to_le_bytes());
                out.extend_from_slice(&amount.to_le_bytes());
                out.extend_from_slice(&expires_at.to_le_bytes());
                put_str(out, title);
            }
            Action::FundHunt { hunt_id, amount } => {
                out.push(11);
                out.extend_from_slice(&hunt_id.to_le_bytes());
                out.extend_from_slice(&amount.to_le_bytes());
            }
            Action::AwardHunt { hunt_id, to, pr, familiar_key } => {
                out.push(12);
                out.extend_from_slice(&hunt_id.to_le_bytes());
                put_str(out, to);
                out.extend_from_slice(&pr.to_le_bytes());
                put_str(out, familiar_key.as_deref().unwrap_or(""));
            }
            Action::CancelHunt { hunt_id } => {
                out.push(13);
                out.extend_from_slice(&hunt_id.to_le_bytes());
            }
            Action::RegisterFamiliar { key_id, label } => {
                out.push(14);
                put_str(out, key_id);
                put_str(out, label);
            }
            Action::SetRepo { repo, weight } => {
                out.push(15);
                put_str(out, repo);
                out.extend_from_slice(&weight.to_le_bytes());
            }
            Action::GrantSoul { to, amount, reason } => {
                out.push(16);
                put_str(out, to);
                out.extend_from_slice(&amount.to_le_bytes());
                put_str(out, reason);
            }
            Action::Vouch { account } => {
                out.push(17);
                put_str(out, account);
            }
            Action::Unvouch { account } => {
                out.push(18);
                put_str(out, account);
            }
            Action::FlagSybil { account, reason } => {
                out.push(19);
                put_str(out, account);
                put_str(out, reason);
            }
            Action::SubmitClaim { repo, pr, familiar_key } => {
                out.push(20);
                put_str(out, repo);
                out.extend_from_slice(&pr.to_le_bytes());
                put_str(out, familiar_key.as_deref().unwrap_or(""));
            }
            Action::AttestClaim { claim_id, ok, merge_sha, note } => {
                out.push(21);
                out.extend_from_slice(&claim_id.to_le_bytes());
                out.push(*ok as u8);
                put_str(out, merge_sha);
                put_str(out, note);
            }
            Action::RejectClaim { claim_id, reason } => {
                out.push(22);
                out.extend_from_slice(&claim_id.to_le_bytes());
                put_str(out, reason);
            }
            Action::JoinJury { bond } => {
                out.push(23);
                out.extend_from_slice(&bond.to_le_bytes());
            }
            Action::LeaveJury {} => out.push(24),
            Action::CommitVotes { seance_id, commitment, pairs_digest } => {
                out.push(25);
                out.extend_from_slice(&seance_id.to_le_bytes());
                put_str(out, commitment);
                put_str(out, pairs_digest);
            }
            Action::RevealVotes { seance_id, votes, spam, salt } => {
                out.push(26);
                out.extend_from_slice(&seance_id.to_le_bytes());
                put_str(out, votes);
                put_ids(out, spam);
                put_str(out, salt);
            }
            Action::FundPool { amount } => {
                out.push(27);
                out.extend_from_slice(&amount.to_le_bytes());
            }
            Action::WithdrawVested {} => out.push(28),
            Action::Clawback { account, reason } => {
                out.push(29);
                put_str(out, account);
                put_str(out, reason);
            }
            Action::Delegate { validator, amount } => {
                out.push(30);
                put_str(out, validator);
                put_u64(out, *amount);
            }
            Action::Undelegate { validator, shares, amount } => {
                out.push(31);
                put_str(out, validator);
                put_u64(out, *shares);
                put_u64(out, *amount);
            }
            Action::Redelegate { from_validator, to_validator, shares } => {
                out.push(32);
                put_str(out, from_validator);
                put_str(out, to_validator);
                put_u64(out, *shares);
            }
            Action::WithdrawUnbonded {} => out.push(33),
            Action::UpdateValidator { commission_bps, p2p_addr, http_addr } => {
                out.push(34);
                put_u64(out, *commission_bps);
                put_str(out, p2p_addr);
                put_str(out, http_addr);
            }
            Action::Unjail { validator } => {
                out.push(35);
                put_str(out, validator);
            }
            Action::ReportEquivocation { evidence } => {
                out.push(36);
                put_str(out, evidence);
            }
            Action::BeaconCommit { epoch, commitment } => {
                out.push(37);
                put_u64(out, *epoch);
                put_str(out, commitment);
            }
            Action::BeaconReveal { epoch, secret } => {
                out.push(38);
                put_u64(out, *epoch);
                put_str(out, secret);
            }
            Action::Propose { kind, deposit, title, url } => {
                out.push(39);
                kind.encode(out);
                put_u64(out, *deposit);
                put_str(out, title);
                put_str(out, url);
            }
            Action::Vote { proposal_id, aye, amount, conviction, proposal_digest } => {
                out.push(40);
                put_u64(out, *proposal_id);
                out.push(*aye as u8);
                put_u64(out, *amount);
                out.push(*conviction);
                put_str(out, proposal_digest);
            }
            Action::Unlock {} => out.push(41),
            Action::MultisigCreate { owners, threshold } => {
                out.push(42);
                put_strs(out, owners);
                out.push(*threshold);
            }
            Action::MultisigPropose { action } => {
                out.push(43);
                action.encode(out);
            }
            Action::MultisigApprove { id, action_digest } => {
                out.push(44);
                put_u64(out, *id);
                put_str(out, action_digest);
            }
            Action::RenewName { name, years } => {
                out.push(45);
                put_str(out, name);
                out.push(*years);
            }
            Action::TransferName { name, to } => {
                out.push(46);
                put_str(out, name);
                put_str(out, to);
            }
            Action::ReleaseName { name } => {
                out.push(47);
                put_str(out, name);
            }
            Action::ClaimDistribution { distribution_id, amount, proof } => {
                out.push(48);
                put_u64(out, *distribution_id);
                put_u64(out, *amount);
                put_strs(out, proof);
            }
            Action::AttestRevert { claim_id, revert_sha } => {
                out.push(49);
                put_u64(out, *claim_id);
                put_str(out, revert_sha);
            }
            Action::Shield { amount, bundle } => {
                out.push(50);
                put_u64(out, *amount);
                bundle.encode(out);
            }
            Action::PrivateTransfer { bundle } => {
                out.push(51);
                bundle.encode(out);
            }
            Action::Unshield { to, amount, bundle } => {
                out.push(52);
                put_str(out, to);
                put_u64(out, *amount);
                bundle.encode(out);
            }
            // Raw and fixed-size (check_format enforces the size).
            Action::SetGhostAddress { address } => {
                out.push(53);
                out.extend_from_slice(&address.0);
            }
            Action::GhostPause {} => out.push(54),
            Action::ValidatorReady { epoch } => {
                out.push(60);
                put_u64(out, *epoch);
            }
            Action::AdmitValidator { validator, epoch } => {
                out.push(61);
                put_str(out, validator);
                put_u64(out, *epoch);
            }
        }
    }

    /// The action's encoding on its own (tag and fields, as inside the signing bytes): how state
    /// records store actions, e.g. pending multisig proposals.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = vec![];
        self.encode(&mut out);
        out
    }

    /// Strict inverse of [`Action::to_bytes`].
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn from_bytes(bytes: &[u8]) -> Result<Action> {
        let mut r = Reader { buf: bytes };
        let a = Action::decode(&mut r)?;
        if !r.buf.is_empty() {
            bail!("{} trailing bytes after the action", r.buf.len());
        }
        Ok(a)
    }

    /// Strict inverse of [`Action::encode`]: unknown tags, non-0/1 bools and over-long fields
    /// are errors.
    fn decode(r: &mut Reader) -> Result<Action> {
        Self::decode_at(r, false)
    }

    /// `nested`: inside a `MultisigPropose`, which can't hold another one.
    fn decode_at(r: &mut Reader, nested: bool) -> Result<Action> {
        use crate::poc::{MAX_SEANCE_CLAIMS, PAIRS_PER_JUROR};
        let hex_pk = crypto::PUBLIC_KEY_LEN * 2;
        let hex_sig = crypto::SIGNATURE_LEN * 2;
        Ok(match r.u8()? {
            0 => Action::Transfer { to: r.str("to", MAX_NAME_FIELD)?, amount: r.u64()? },
            1 => Action::RegisterName { name: r.str("name", MAX_NAME_FIELD)? },
            2 => Action::AddKey {
                key_id: r.str("key_id", HEX_32)?,
                label: r.str("label", MAX_LABEL)?,
                role: match r.u8()? {
                    0 => KeyRole::Owner,
                    1 => KeyRole::Session { limit: r.u64()?, period_blocks: r.u64()?, expires_at: r.u64()? },
                    t => bail!("unknown key role {t}"),
                },
                public_key: r.str("public_key", hex_pk)?,
                pop: r.str("pop", hex_sig)?,
            },
            3 => Action::RemoveKey { key_id: r.str("key_id", HEX_32)? },
            4 => Action::RegisterValidator {
                consensus_key: r.str("consensus_key", hex_pk)?,
                p2p_addr: r.str("p2p_addr", MAX_ADDR)?,
                http_addr: r.str("http_addr", MAX_ADDR)?,
                bond: r.u64()?,
                pop: r.str("pop", hex_sig)?,
            },
            5 => Action::ApproveValidator { validator: r.str("validator", MAX_ADDRESS)? },
            6 => Action::RemoveValidator { validator: r.str("validator", MAX_ADDRESS)? },
            7 => Action::Unbond { validator: r.str("validator", MAX_ADDRESS)? },
            8 => Action::LinkGithub { handle: r.str("handle", MAX_GITHUB_HANDLE)?, gist_id: r.str("gist_id", HEX_32)? },
            9 => Action::UnlinkGithub { handle: r.str("handle", MAX_GITHUB_HANDLE)? },
            10 => Action::CreateHunt { repo: r.str("repo", MAX_REPO)?, issue: r.u64()?, amount: r.u64()?, expires_at: r.u64()?, title: r.str("title", MAX_HUNT_TITLE)? },
            11 => Action::FundHunt { hunt_id: r.u64()?, amount: r.u64()? },
            12 => Action::AwardHunt { hunt_id: r.u64()?, to: r.str("to", MAX_NAME_FIELD)?, pr: r.u64()?, familiar_key: r.opt_str("familiar_key", HEX_32)? },
            13 => Action::CancelHunt { hunt_id: r.u64()? },
            14 => Action::RegisterFamiliar { key_id: r.str("key_id", HEX_32)?, label: r.str("label", MAX_LABEL)? },
            15 => Action::SetRepo { repo: r.str("repo", MAX_REPO)?, weight: r.u64()? },
            16 => Action::GrantSoul { to: r.str("to", MAX_NAME_FIELD)?, amount: r.u64()?, reason: r.str("reason", MAX_REASON)? },
            17 => Action::Vouch { account: r.str("account", MAX_NAME_FIELD)? },
            18 => Action::Unvouch { account: r.str("account", MAX_NAME_FIELD)? },
            19 => Action::FlagSybil { account: r.str("account", MAX_NAME_FIELD)?, reason: r.str("reason", MAX_REASON)? },
            20 => Action::SubmitClaim { repo: r.str("repo", MAX_REPO)?, pr: r.u64()?, familiar_key: r.opt_str("familiar_key", HEX_32)? },
            21 => Action::AttestClaim { claim_id: r.u64()?, ok: r.bool("ok")?, merge_sha: r.str("merge_sha", 40)?, note: r.str("note", MAX_REASON)? },
            22 => Action::RejectClaim { claim_id: r.u64()?, reason: r.str("reason", MAX_REASON)? },
            23 => Action::JoinJury { bond: r.u64()? },
            24 => Action::LeaveJury {},
            25 => Action::CommitVotes { seance_id: r.u64()?, commitment: r.str("commitment", HEX_32)?, pairs_digest: r.str("pairs_digest", HEX_32)? },
            26 => Action::RevealVotes { seance_id: r.u64()?, votes: r.str("votes", PAIRS_PER_JUROR)?, spam: r.ids("spam", MAX_SEANCE_CLAIMS)?, salt: r.str("salt", 64)? },
            27 => Action::FundPool { amount: r.u64()? },
            28 => Action::WithdrawVested {},
            29 => Action::Clawback { account: r.str("account", MAX_NAME_FIELD)?, reason: r.str("reason", MAX_REASON)? },
            30 => Action::Delegate { validator: r.str("validator", MAX_ADDRESS)?, amount: r.u64()? },
            31 => Action::Undelegate { validator: r.str("validator", MAX_ADDRESS)?, shares: r.u64()?, amount: r.u64()? },
            32 => Action::Redelegate { from_validator: r.str("from_validator", MAX_ADDRESS)?, to_validator: r.str("to_validator", MAX_ADDRESS)?, shares: r.u64()? },
            33 => Action::WithdrawUnbonded {},
            34 => Action::UpdateValidator { commission_bps: r.u64()?, p2p_addr: r.str("p2p_addr", MAX_ADDR)?, http_addr: r.str("http_addr", MAX_ADDR)? },
            35 => Action::Unjail { validator: r.str("validator", MAX_ADDRESS)? },
            36 => Action::ReportEquivocation { evidence: r.str("evidence", 2 * MAX_EVIDENCE_BYTES)? },
            37 => Action::BeaconCommit { epoch: r.u64()?, commitment: r.str("commitment", HEX_32)? },
            38 => Action::BeaconReveal { epoch: r.u64()?, secret: r.str("secret", HEX_32)? },
            39 => Action::Propose { kind: ProposalKind::decode(r)?, deposit: r.u64()?, title: r.str("title", MAX_PROPOSAL_TITLE)?, url: r.str("url", MAX_URL)? },
            40 => Action::Vote { proposal_id: r.u64()?, aye: r.bool("aye")?, amount: r.u64()?, conviction: r.u8()?, proposal_digest: r.str("proposal_digest", HEX_32)? },
            41 => Action::Unlock {},
            42 => Action::MultisigCreate { owners: r.strs("owners", crate::chain::MAX_KEYS, HEX_32)?, threshold: r.u8()? },
            43 if nested => bail!("a multisig proposal can't contain another"),
            43 => Action::MultisigPropose { action: Box::new(Action::decode_at(r, true)?) },
            44 => Action::MultisigApprove { id: r.u64()?, action_digest: r.str("action_digest", HEX_32)? },
            45 => Action::RenewName { name: r.str("name", MAX_NAME_FIELD)?, years: r.u8()? },
            46 => Action::TransferName { name: r.str("name", MAX_NAME_FIELD)?, to: r.str("to", MAX_NAME_FIELD)? },
            47 => Action::ReleaseName { name: r.str("name", MAX_NAME_FIELD)? },
            48 => Action::ClaimDistribution { distribution_id: r.u64()?, amount: r.u64()?, proof: r.strs("proof", MAX_PROOF, HEX_32)? },
            49 => Action::AttestRevert { claim_id: r.u64()?, revert_sha: r.str("revert_sha", 40)? },
            50..=54 if nested => bail!("a multisig proposal can't hold a ghost action"),
            50 => Action::Shield { amount: r.u64()?, bundle: Box::new(GhostBundle::decode(r)?) },
            51 => Action::PrivateTransfer { bundle: Box::new(GhostBundle::decode(r)?) },
            52 => Action::Unshield { to: r.str("to", MAX_NAME_FIELD)?, amount: r.u64()?, bundle: Box::new(GhostBundle::decode(r)?) },
            53 => Action::SetGhostAddress { address: HexBytes(r.take(GHOST_ADDRESS_BYTES)?.to_vec()) },
            54 => Action::GhostPause {},
            60 => Action::ValidatorReady { epoch: r.u64()? },
            61 => Action::AdmitValidator { validator: r.str("validator", MAX_ADDRESS)?, epoch: r.u64()? },
            t => bail!("unknown action tag {t}"),
        })
    }

    /// The action's own format rules (`Tx::check_format`). `nested`: inside a `MultisigPropose`.
    fn check_format(&self, nested: bool) -> Result<()> {
        // First, so it's the rule a too-large number meets whatever else is wrong.
        check_json_ints(&self.json_ints())?;
        let key_id_ok = |k: &str| k.len() == 64 && crypto::is_lower_hex(k);
        let addr = |field: &str, v: &str| -> Result<()> {
            if v.len() > MAX_ADDR {
                bail!("{field} too long (max {MAX_ADDR} bytes)");
            }
            crate::chain::check_host_port(v).with_context(|| field.to_string())
        };
        match self {
            Action::Transfer { to, .. } => {
                if to.len() > MAX_NAME_FIELD {
                    bail!("`to` too long (max {MAX_NAME_FIELD} bytes)");
                }
                if !crypto::is_address(to) {
                    normalize_name(to).context("`to` must be a lowercase boo1… address or a .boo name")?;
                }
            }
            Action::RegisterName { name } => {
                if name.len() > MAX_NAME_FIELD {
                    bail!("name too long");
                }
                normalize_name(name)?;
            }
            Action::AddKey { key_id, label, role, public_key, pop } => {
                if !key_id_ok(key_id) {
                    bail!("key_id must be 32 bytes of lowercase hex (BLAKE3 of the public key)");
                }
                if !crypto::is_lower_hex(public_key) || public_key.len() != crypto::PUBLIC_KEY_LEN * 2 {
                    bail!("public_key must be the added key's ML-DSA-65 public key ({} bytes of lowercase hex)", crypto::PUBLIC_KEY_LEN);
                }
                if hex::decode(public_key).map(|pk| self::key_id(&pk) != *key_id).unwrap_or(true) {
                    bail!("public_key isn't the key key_id names (key_id is BLAKE3 of the public key)");
                }
                if !crypto::is_lower_hex(pop) || pop.len() != crypto::SIGNATURE_LEN * 2 {
                    bail!("pop must be the added key's ML-DSA-65 signature ({} bytes of lowercase hex) over its consent to this account (see Wallet.keyGrant)", crypto::SIGNATURE_LEN);
                }
                if label.len() > MAX_LABEL {
                    bail!("key label too long (max {MAX_LABEL} bytes)");
                }
                if let KeyRole::Session { period_blocks, .. } = role {
                    if *period_blocks == 0 {
                        bail!("session period_blocks must be at least 1");
                    }
                }
            }
            Action::RemoveKey { key_id } => {
                if !key_id_ok(key_id) {
                    bail!("key_id must be 32 bytes of lowercase hex");
                }
            }
            Action::RegisterValidator { consensus_key, p2p_addr, http_addr, pop, .. } => {
                if !crypto::is_lower_hex(consensus_key) || consensus_key.len() != crypto::PUBLIC_KEY_LEN * 2 {
                    bail!("consensus_key must be an ML-DSA-65 public key ({} bytes of lowercase hex)", crypto::PUBLIC_KEY_LEN);
                }
                if !crypto::is_lower_hex(pop) || pop.len() != crypto::SIGNATURE_LEN * 2 {
                    bail!("pop must be an ML-DSA-65 signature by the consensus key (see `blockula validator register`)");
                }
                addr("p2p_addr", p2p_addr)?;
                if !(http_addr.starts_with("http://") || http_addr.starts_with("https://")) {
                    addr("http_addr", http_addr)?;
                } else if http_addr.len() > MAX_ADDR {
                    bail!("http_addr too long (max {MAX_ADDR} bytes)");
                }
            }
            Action::ApproveValidator { validator } | Action::RemoveValidator { validator } | Action::Unbond { validator } => {
                if !crypto::is_address(validator) {
                    bail!("validator must be a lowercase boo1… address");
                }
            }
            Action::LinkGithub { handle, gist_id } => {
                check_github_handle(handle)?;
                if gist_id.is_empty() || gist_id.len() > 64 || !gist_id.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase()) {
                    bail!("gist_id must be the gist's id (lowercase letters and digits, up to 64)");
                }
            }
            Action::UnlinkGithub { handle } => check_github_handle(handle)?,
            Action::CreateHunt { repo, issue, title, .. } => {
                check_repo(repo)?;
                if *issue == 0 {
                    bail!("issue must be a GitHub issue number");
                }
                check_title(title)?;
            }
            Action::FundHunt { .. } | Action::CancelHunt { .. } => {}
            Action::AwardHunt { to, pr, familiar_key, .. } => {
                if to.len() > MAX_NAME_FIELD {
                    bail!("`to` too long");
                }
                if !crypto::is_address(to) {
                    normalize_name(to).context("`to` must be a lowercase boo1… address or a .boo name")?;
                }
                if *pr == 0 {
                    bail!("pr must be a pull request number");
                }
                if let Some(k) = familiar_key {
                    if !key_id_ok(k) {
                        bail!("familiar_key must be a key id (32 bytes of lowercase hex)");
                    }
                }
            }
            Action::RegisterFamiliar { key_id, label } => {
                if !key_id_ok(key_id) {
                    bail!("key_id must be 32 bytes of lowercase hex");
                }
                if label.is_empty() || label.len() > MAX_LABEL {
                    bail!("familiar label must be 1–{MAX_LABEL} bytes");
                }
            }
            Action::SetRepo { repo, weight } => {
                check_repo(repo)?;
                if *weight > crate::poc::MAX_REPO_WEIGHT {
                    bail!("repo weight must be 0–{} (0 removes the repo)", crate::poc::MAX_REPO_WEIGHT);
                }
            }
            Action::GrantSoul { to, amount, reason } => {
                check_account_ref("to", to)?;
                if *amount == 0 || *amount > crate::poc::MAX_SOUL_GRANT {
                    bail!("a Soul grant is 1–{} milli-Soul", crate::poc::MAX_SOUL_GRANT);
                }
                check_reason(reason)?;
            }
            Action::Vouch { account } | Action::Unvouch { account } => check_account_ref("account", account)?,
            Action::FlagSybil { account, reason } | Action::Clawback { account, reason } => {
                check_account_ref("account", account)?;
                check_reason(reason)?;
            }
            Action::SubmitClaim { repo, pr, familiar_key } => {
                check_repo(repo)?;
                if *pr == 0 {
                    bail!("pr must be a pull request number");
                }
                if let Some(k) = familiar_key {
                    if !key_id_ok(k) {
                        bail!("familiar_key must be a key id (32 bytes of lowercase hex)");
                    }
                }
            }
            Action::AttestClaim { ok, merge_sha, note, .. } => {
                if *ok && (merge_sha.len() != 40 || !crypto::is_lower_hex(merge_sha)) {
                    bail!("merge_sha must be the 40-hex merge commit");
                }
                if !*ok && !merge_sha.is_empty() {
                    bail!("merge_sha must be empty when ok is false");
                }
                if note.len() > MAX_REASON || note.chars().any(|c| c.is_control()) {
                    bail!("note must be at most {MAX_REASON} bytes with no control characters");
                }
            }
            Action::RejectClaim { reason, .. } => check_reason(reason)?,
            Action::CommitVotes { commitment, pairs_digest, .. } => {
                if commitment.len() != 64 || !crypto::is_lower_hex(commitment) {
                    bail!("commitment must be 32 bytes of lowercase hex");
                }
                if !key_id_ok(pairs_digest) {
                    bail!("pairs_digest must be 32 bytes of lowercase hex (seance_pairs_digest of the pairs you judged)");
                }
            }
            Action::RevealVotes { votes, spam, salt, .. } => {
                if votes.len() > crate::poc::PAIRS_PER_JUROR || !votes.bytes().all(|b| b"ab=".contains(&b)) {
                    bail!("votes must be up to {} of 'a', 'b' or '='", crate::poc::PAIRS_PER_JUROR);
                }
                if spam.len() > crate::poc::MAX_SEANCE_CLAIMS {
                    bail!("too many spam flags");
                }
                if salt.is_empty() || salt.len() > 64 || salt.chars().any(|c| c.is_control()) {
                    bail!("salt must be 1–64 bytes with no control characters");
                }
            }
            Action::JoinJury { .. } | Action::LeaveJury {} | Action::FundPool { .. } | Action::WithdrawVested {} => {}
            Action::Delegate { validator, amount } => {
                check_address("validator", validator)?;
                if *amount == 0 {
                    bail!("delegate more than 0");
                }
            }
            Action::Undelegate { validator, shares, amount } => {
                check_address("validator", validator)?;
                match (*shares, *amount) {
                    (0, 0) => bail!("undelegate more than 0 shares, or an amount above 0"),
                    (s, a) if s > 0 && a > 0 => bail!("undelegate a number of shares or an amount, not both"),
                    _ => {}
                }
            }
            Action::Redelegate { from_validator, to_validator, shares } => {
                check_address("from_validator", from_validator)?;
                check_address("to_validator", to_validator)?;
                if from_validator == to_validator {
                    bail!("redelegate to a different validator");
                }
                if *shares == 0 {
                    bail!("redelegate more than 0 shares");
                }
            }
            Action::Unjail { validator } => check_address("validator", validator)?,
            Action::UpdateValidator { commission_bps, p2p_addr, http_addr } => {
                if *commission_bps > 10_000 {
                    bail!("commission_bps must be 0–10000");
                }
                if !p2p_addr.is_empty() {
                    addr("p2p_addr", p2p_addr)?;
                }
                if !http_addr.is_empty() {
                    crate::chain::check_http_addr(http_addr).context("http_addr")?;
                }
            }
            Action::ReportEquivocation { evidence } => {
                if evidence.is_empty() || evidence.len() > 2 * MAX_EVIDENCE_BYTES || !crypto::is_lower_hex(evidence) {
                    bail!("evidence must be 1–{MAX_EVIDENCE_BYTES} bytes of lowercase hex");
                }
            }
            Action::BeaconCommit { commitment, .. } => check_hex_32("commitment", commitment)?,
            Action::BeaconReveal { secret, .. } => check_hex_32("secret", secret)?,
            Action::Propose { kind, deposit, title, url } => {
                kind.check()?;
                if *deposit == 0 {
                    bail!("a proposal bonds a deposit");
                }
                if title.trim().is_empty() || title.len() > MAX_PROPOSAL_TITLE || title.chars().any(|c| c.is_control()) {
                    bail!("title must be 1–{MAX_PROPOSAL_TITLE} bytes with no control characters");
                }
                if !url.is_empty() && (url.len() > MAX_URL || !url.starts_with("https://") || url.chars().any(|c| c.is_control() || c.is_whitespace())) {
                    bail!("url must be empty or an https:// link of at most {MAX_URL} bytes");
                }
            }
            Action::Vote { amount, conviction, proposal_digest, .. } => {
                if *amount == 0 {
                    bail!("vote with more than 0");
                }
                if !key_id_ok(proposal_digest) {
                    bail!("proposal_digest must be 32 bytes of lowercase hex (proposal_digest of the proposal you vote on)");
                }
                if *conviction > crate::params::MAX_CONVICTION {
                    bail!("conviction must be 0–{}", crate::params::MAX_CONVICTION);
                }
            }
            Action::WithdrawUnbonded {} | Action::Unlock {} => {}
            Action::MultisigApprove { action_digest, .. } => {
                if !key_id_ok(action_digest) {
                    bail!("action_digest must be 32 bytes of lowercase hex (action_digest of the action you approve)");
                }
            }
            Action::MultisigCreate { owners, threshold } => {
                if owners.is_empty() || owners.len() > crate::chain::MAX_KEYS || !owners.iter().all(|k| key_id_ok(k)) {
                    bail!("owners must be 1–{} key ids (32 bytes of lowercase hex)", crate::chain::MAX_KEYS);
                }
                if owners.iter().collect::<std::collections::BTreeSet<_>>().len() != owners.len() {
                    bail!("owners must be distinct");
                }
                if *threshold == 0 || *threshold as usize > owners.len() {
                    bail!("threshold must be 1–{} (the number of owners)", owners.len());
                }
            }
            Action::MultisigPropose { action } => {
                if nested || matches!(**action, Action::MultisigPropose { .. } | Action::MultisigApprove { .. }) {
                    bail!("a multisig proposal can't hold another multisig proposal or approval");
                }
                if action.is_ghost() {
                    bail!("a multisig proposal can't hold a ghost action");
                }
                let size = action.to_bytes().len();
                if size > MAX_MULTISIG_ACTION_BYTES {
                    bail!("a multisig proposal's action is at most {MAX_MULTISIG_ACTION_BYTES} bytes encoded, not {size}");
                }
                action.check_format(true).context("the proposed action")?;
            }
            Action::RenewName { name, years } => {
                check_name_field(name)?;
                if *years == 0 || *years > MAX_RENEW_YEARS {
                    bail!("renew for 1–{MAX_RENEW_YEARS} years");
                }
            }
            Action::TransferName { name, to } => {
                check_name_field(name)?;
                check_account_ref("to", to)?;
            }
            Action::ReleaseName { name } => check_name_field(name)?,
            Action::ClaimDistribution { amount, proof, .. } => {
                if *amount == 0 {
                    bail!("claim more than 0");
                }
                if proof.len() > MAX_PROOF {
                    bail!("proof too long (max {MAX_PROOF} hashes)");
                }
                for h in proof {
                    check_hex_32("proof", h)?;
                }
            }
            Action::AttestRevert { revert_sha, .. } => {
                if revert_sha.len() != 40 || !crypto::is_lower_hex(revert_sha) {
                    bail!("revert_sha must be the 40-hex revert commit");
                }
            }
            Action::Shield { amount, bundle } => {
                if *amount == 0 {
                    bail!("shield more than 0");
                }
                bundle.check()?;
            }
            Action::PrivateTransfer { bundle } => bundle.check()?,
            Action::Unshield { to, amount, bundle } => {
                check_account_ref("to", to)?;
                if *amount == 0 {
                    bail!("unshield more than 0");
                }
                bundle.check()?;
            }
            Action::SetGhostAddress { address } => {
                // Its owner digest must be canonical and its ML-KEM key pass FIPS 203's checks.
                if address.0.len() != GHOST_ADDRESS_BYTES || blockula_ghost::Address::from_bytes(&address.0).is_none() {
                    bail!("address must be a valid {GHOST_ADDRESS_BYTES}-byte Ghost Mode address");
                }
            }
            Action::GhostPause {} | Action::ValidatorReady { .. } => {}
            Action::AdmitValidator { validator, .. } => check_address("validator", validator)?,
        }
        Ok(())
    }

    /// A Ghost Mode action (tags 50–54).
    pub fn is_ghost(&self) -> bool {
        matches!(self, Action::Shield { .. } | Action::PrivateTransfer { .. } | Action::Unshield { .. } | Action::SetGhostAddress { .. } | Action::GhostPause {})
    }

    /// Travels unsigned: `PrivateTransfer` and `Unshield`, whose proof authorizes them.
    pub fn is_unsigned(&self) -> bool {
        matches!(self, Action::PrivateTransfer { .. } | Action::Unshield { .. })
    }

    /// The ghost bundle, for the actions that carry one (tags 50–52).
    pub fn ghost_bundle(&self) -> Option<&GhostBundle> {
        match self {
            Action::Shield { bundle, .. } | Action::PrivateTransfer { bundle } | Action::Unshield { bundle, .. } => Some(bundle),
            _ => None,
        }
    }

    /// The integer fields this action carries as JSON numbers (`u64` fields without `u64_str`;
    /// a `Propose`'s include its kind's). [`MAX_JSON_INT`] bounds them all; a nested multisig
    /// action is checked by its own `check_format`. `types::tests::json_number_fields_are_bounded`
    /// pushes every vector action's JSON numbers past the bound, so a field missing here fails it.
    fn json_ints(&self) -> Vec<(&'static str, u64)> {
        match self {
            Action::AddKey { role: KeyRole::Session { period_blocks, expires_at, .. }, .. } => vec![("period_blocks", *period_blocks), ("expires_at", *expires_at)],
            Action::CreateHunt { issue, expires_at, .. } => vec![("issue", *issue), ("expires_at", *expires_at)],
            Action::FundHunt { hunt_id, .. } | Action::CancelHunt { hunt_id } => vec![("hunt_id", *hunt_id)],
            Action::AwardHunt { hunt_id, pr, .. } => vec![("hunt_id", *hunt_id), ("pr", *pr)],
            Action::SetRepo { weight, .. } => vec![("weight", *weight)],
            Action::GrantSoul { amount, .. } => vec![("amount", *amount)],
            Action::SubmitClaim { pr, .. } => vec![("pr", *pr)],
            Action::AttestClaim { claim_id, .. } | Action::RejectClaim { claim_id, .. } | Action::AttestRevert { claim_id, .. } => vec![("claim_id", *claim_id)],
            Action::CommitVotes { seance_id, .. } => vec![("seance_id", *seance_id)],
            Action::RevealVotes { seance_id, spam, .. } => std::iter::once(("seance_id", *seance_id)).chain(spam.iter().map(|id| ("spam", *id))).collect(),
            Action::UpdateValidator { commission_bps, .. } => vec![("commission_bps", *commission_bps)],
            Action::BeaconCommit { epoch, .. } | Action::BeaconReveal { epoch, .. } | Action::ValidatorReady { epoch } | Action::AdmitValidator { epoch, .. } => vec![("epoch", *epoch)],
            Action::Propose { kind, .. } => kind.json_ints(),
            Action::Vote { proposal_id, .. } => vec![("proposal_id", *proposal_id)],
            Action::MultisigApprove { id, .. } => vec![("id", *id)],
            Action::ClaimDistribution { distribution_id, .. } => vec![("distribution_id", *distribution_id)],
            _ => vec![],
        }
    }

    /// The action's JSON `type` (snake_case), e.g. `"delegate"`.
    pub fn kind(&self) -> &'static str {
        match self {
            Action::Transfer { .. } => "transfer",
            Action::RegisterName { .. } => "register_name",
            Action::AddKey { .. } => "add_key",
            Action::RemoveKey { .. } => "remove_key",
            Action::RegisterValidator { .. } => "register_validator",
            Action::ApproveValidator { .. } => "approve_validator",
            Action::RemoveValidator { .. } => "remove_validator",
            Action::Unbond { .. } => "unbond",
            Action::LinkGithub { .. } => "link_github",
            Action::UnlinkGithub { .. } => "unlink_github",
            Action::CreateHunt { .. } => "create_hunt",
            Action::FundHunt { .. } => "fund_hunt",
            Action::AwardHunt { .. } => "award_hunt",
            Action::CancelHunt { .. } => "cancel_hunt",
            Action::RegisterFamiliar { .. } => "register_familiar",
            Action::SetRepo { .. } => "set_repo",
            Action::GrantSoul { .. } => "grant_soul",
            Action::Vouch { .. } => "vouch",
            Action::Unvouch { .. } => "unvouch",
            Action::FlagSybil { .. } => "flag_sybil",
            Action::SubmitClaim { .. } => "submit_claim",
            Action::AttestClaim { .. } => "attest_claim",
            Action::RejectClaim { .. } => "reject_claim",
            Action::JoinJury { .. } => "join_jury",
            Action::LeaveJury {} => "leave_jury",
            Action::CommitVotes { .. } => "commit_votes",
            Action::RevealVotes { .. } => "reveal_votes",
            Action::FundPool { .. } => "fund_pool",
            Action::WithdrawVested {} => "withdraw_vested",
            Action::Clawback { .. } => "clawback",
            Action::Delegate { .. } => "delegate",
            Action::Undelegate { .. } => "undelegate",
            Action::Redelegate { .. } => "redelegate",
            Action::WithdrawUnbonded {} => "withdraw_unbonded",
            Action::UpdateValidator { .. } => "update_validator",
            Action::Unjail { .. } => "unjail",
            Action::ReportEquivocation { .. } => "report_equivocation",
            Action::BeaconCommit { .. } => "beacon_commit",
            Action::BeaconReveal { .. } => "beacon_reveal",
            Action::Propose { .. } => "propose",
            Action::Vote { .. } => "vote",
            Action::Unlock {} => "unlock",
            Action::MultisigCreate { .. } => "multisig_create",
            Action::MultisigPropose { .. } => "multisig_propose",
            Action::MultisigApprove { .. } => "multisig_approve",
            Action::RenewName { .. } => "renew_name",
            Action::TransferName { .. } => "transfer_name",
            Action::ReleaseName { .. } => "release_name",
            Action::ClaimDistribution { .. } => "claim_distribution",
            Action::AttestRevert { .. } => "attest_revert",
            Action::Shield { .. } => "shield",
            Action::PrivateTransfer { .. } => "private_transfer",
            Action::Unshield { .. } => "unshield",
            Action::SetGhostAddress { .. } => "set_ghost_address",
            Action::GhostPause {} => "ghost_pause",
            Action::ValidatorReady { .. } => "validator_ready",
            Action::AdmitValidator { .. } => "admit_validator",
        }
    }

    /// Fields that name an account and may hold a raw address (names resolve on this chain),
    /// including those inside a proposal or a multisig proposal.
    pub fn addresses(&self) -> Vec<(&'static str, &str)> {
        match self {
            Action::Transfer { to, .. } | Action::AwardHunt { to, .. } | Action::GrantSoul { to, .. } | Action::TransferName { to, .. } | Action::Unshield { to, .. } => vec![("to", to.as_str())],
            Action::ApproveValidator { validator } | Action::RemoveValidator { validator } | Action::Unbond { validator } | Action::Delegate { validator, .. } | Action::Undelegate { validator, .. } | Action::Unjail { validator } | Action::AdmitValidator { validator, .. } => vec![("validator", validator.as_str())],
            Action::Redelegate { from_validator, to_validator, .. } => vec![("from_validator", from_validator.as_str()), ("to_validator", to_validator.as_str())],
            Action::Vouch { account } | Action::Unvouch { account } | Action::FlagSybil { account, .. } | Action::Clawback { account, .. } => vec![("account", account.as_str())],
            Action::Propose { kind, .. } => match kind {
                ProposalKind::TreasurySpend { to, .. } | ProposalKind::GrantSoul { to, .. } => vec![("kind.to", to.as_str())],
                ProposalKind::FlagSybil { account, .. } | ProposalKind::Clawback { account, .. } | ProposalKind::UnflagSybil { account, .. } => vec![("kind.account", account.as_str())],
                _ => vec![],
            },
            Action::MultisigPropose { action } => action.addresses(),
            _ => vec![],
        }
    }
}

impl ProposalKind {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            ProposalKind::ParamChange { key, value } => {
                out.push(0);
                put_str(out, key);
                put_u64(out, *value);
            }
            ProposalKind::TreasurySpend { to, amount } => {
                out.push(1);
                put_str(out, to);
                put_u64(out, *amount);
            }
            ProposalKind::Distribution { distribution_id, merkle_root, total, claim_until } => {
                out.push(2);
                put_u64(out, *distribution_id);
                put_str(out, merkle_root);
                put_u64(out, *total);
                put_u64(out, *claim_until);
            }
            ProposalKind::SetRepo { repo, weight } => {
                out.push(3);
                put_str(out, repo);
                put_u64(out, *weight);
            }
            ProposalKind::GrantSoul { to, amount, reason } => {
                out.push(4);
                put_str(out, to);
                put_u64(out, *amount);
                put_str(out, reason);
            }
            ProposalKind::FlagSybil { account, reason } => {
                out.push(5);
                put_str(out, account);
                put_str(out, reason);
            }
            ProposalKind::RejectClaim { claim_id, reason } => {
                out.push(6);
                put_u64(out, *claim_id);
                put_str(out, reason);
            }
            ProposalKind::Clawback { account, reason } => {
                out.push(7);
                put_str(out, account);
                put_str(out, reason);
            }
            ProposalKind::UnlinkGithub { handle } => {
                out.push(8);
                put_str(out, handle);
            }
            ProposalKind::RefundHunt { hunt_id } => {
                out.push(9);
                put_u64(out, *hunt_id);
            }
            ProposalKind::ScheduleUpgrade { version, activation_height } => {
                out.push(10);
                out.extend_from_slice(&version.to_le_bytes());
                put_u64(out, *activation_height);
            }
            ProposalKind::ActivateFeature { feature } => {
                out.push(11);
                put_str(out, feature);
            }
            ProposalKind::DeactivateFeature { feature } => {
                out.push(12);
                put_str(out, feature);
            }
            ProposalKind::Text {} => out.push(13),
            ProposalKind::UnflagSybil { account, reason } => {
                out.push(14);
                put_str(out, account);
                put_str(out, reason);
            }
            ProposalKind::CancelUpgrade { version, activation_height } => {
                out.push(15);
                out.extend_from_slice(&version.to_le_bytes());
                put_u64(out, *activation_height);
            }
        }
    }

    fn decode(r: &mut Reader) -> Result<ProposalKind> {
        Ok(match r.u8()? {
            0 => ProposalKind::ParamChange { key: r.str("key", MAX_PARAM_KEY)?, value: r.u64()? },
            1 => ProposalKind::TreasurySpend { to: r.str("to", MAX_NAME_FIELD)?, amount: r.u64()? },
            2 => ProposalKind::Distribution { distribution_id: r.u64()?, merkle_root: r.str("merkle_root", HEX_32)?, total: r.u64()?, claim_until: r.u64()? },
            3 => ProposalKind::SetRepo { repo: r.str("repo", MAX_REPO)?, weight: r.u64()? },
            4 => ProposalKind::GrantSoul { to: r.str("to", MAX_NAME_FIELD)?, amount: r.u64()?, reason: r.str("reason", MAX_REASON)? },
            5 => ProposalKind::FlagSybil { account: r.str("account", MAX_NAME_FIELD)?, reason: r.str("reason", MAX_REASON)? },
            6 => ProposalKind::RejectClaim { claim_id: r.u64()?, reason: r.str("reason", MAX_REASON)? },
            7 => ProposalKind::Clawback { account: r.str("account", MAX_NAME_FIELD)?, reason: r.str("reason", MAX_REASON)? },
            8 => ProposalKind::UnlinkGithub { handle: r.str("handle", MAX_GITHUB_HANDLE)? },
            9 => ProposalKind::RefundHunt { hunt_id: r.u64()? },
            10 => ProposalKind::ScheduleUpgrade { version: r.u32()?, activation_height: r.u64()? },
            11 => ProposalKind::ActivateFeature { feature: r.str("feature", MAX_FEATURE)? },
            12 => ProposalKind::DeactivateFeature { feature: r.str("feature", MAX_FEATURE)? },
            13 => ProposalKind::Text {},
            14 => ProposalKind::UnflagSybil { account: r.str("account", MAX_NAME_FIELD)?, reason: r.str("reason", MAX_REASON)? },
            15 => ProposalKind::CancelUpgrade { version: r.u32()?, activation_height: r.u64()? },
            t => bail!("unknown proposal kind {t}"),
        })
    }

    /// The kind's encoding on its own (tag and fields): how proposal records store it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = vec![];
        self.encode(&mut out);
        out
    }

    /// Strict inverse of [`ProposalKind::to_bytes`].
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn from_bytes(bytes: &[u8]) -> Result<ProposalKind> {
        let mut r = Reader { buf: bytes };
        let k = ProposalKind::decode(&mut r)?;
        if !r.buf.is_empty() {
            bail!("{} trailing bytes after the proposal kind", r.buf.len());
        }
        Ok(k)
    }

    /// The integer fields this kind carries as JSON numbers: at most [`MAX_JSON_INT`], so every
    /// client computes its `proposal_digest` exactly (review-r2-clients-1).
    fn json_ints(&self) -> Vec<(&'static str, u64)> {
        match self {
            ProposalKind::Distribution { distribution_id, claim_until, .. } => vec![("distribution_id", *distribution_id), ("claim_until", *claim_until)],
            ProposalKind::SetRepo { weight, .. } => vec![("weight", *weight)],
            ProposalKind::GrantSoul { amount, .. } => vec![("amount", *amount)],
            ProposalKind::RejectClaim { claim_id, .. } => vec![("claim_id", *claim_id)],
            ProposalKind::RefundHunt { hunt_id } => vec![("hunt_id", *hunt_id)],
            ProposalKind::ScheduleUpgrade { activation_height, .. } | ProposalKind::CancelUpgrade { activation_height, .. } => vec![("activation_height", *activation_height)],
            ProposalKind::ParamChange { .. } | ProposalKind::TreasurySpend { .. } | ProposalKind::FlagSybil { .. } | ProposalKind::Clawback { .. } | ProposalKind::UnlinkGithub { .. } | ProposalKind::ActivateFeature { .. } | ProposalKind::DeactivateFeature { .. } | ProposalKind::Text {} | ProposalKind::UnflagSybil { .. } => vec![],
        }
    }

    /// Stateless rules (stateful ones, like a parameter's bounds, apply when it executes).
    pub fn check(&self) -> Result<()> {
        check_json_ints(&self.json_ints())?;
        match self {
            ProposalKind::ParamChange { key, .. } => {
                if !crate::params::ChainParams::KEYS.contains(&key.as_str()) {
                    bail!("unknown parameter `{key}`");
                }
            }
            ProposalKind::TreasurySpend { to, amount } => {
                check_account_ref("to", to)?;
                if *amount == 0 {
                    bail!("a treasury spend must move more than 0");
                }
            }
            ProposalKind::Distribution { merkle_root, total, claim_until, .. } => {
                check_hex_32("merkle_root", merkle_root)?;
                if *total == 0 || *claim_until == 0 {
                    bail!("a distribution needs a total and a claim deadline");
                }
            }
            ProposalKind::SetRepo { repo, weight } => {
                check_repo(repo)?;
                if *weight > crate::poc::MAX_REPO_WEIGHT {
                    bail!("repo weight must be 0–{} (0 removes the repo)", crate::poc::MAX_REPO_WEIGHT);
                }
            }
            ProposalKind::GrantSoul { to, amount, reason } => {
                check_account_ref("to", to)?;
                if *amount == 0 || *amount > crate::poc::MAX_SOUL_GRANT {
                    bail!("a Soul grant is 1–{} milli-Soul", crate::poc::MAX_SOUL_GRANT);
                }
                check_reason(reason)?;
            }
            ProposalKind::FlagSybil { account, reason } | ProposalKind::Clawback { account, reason } | ProposalKind::UnflagSybil { account, reason } => {
                check_account_ref("account", account)?;
                check_reason(reason)?;
            }
            ProposalKind::RejectClaim { reason, .. } => check_reason(reason)?,
            ProposalKind::UnlinkGithub { handle } => check_github_handle(handle)?,
            ProposalKind::RefundHunt { .. } | ProposalKind::Text {} => {}
            ProposalKind::ScheduleUpgrade { version, activation_height } => {
                if *version == 0 || *activation_height == 0 {
                    bail!("an upgrade needs a protocol version and an activation height");
                }
            }
            ProposalKind::CancelUpgrade { version, activation_height } => {
                if *version == 0 || *activation_height == 0 {
                    bail!("a cancellation names the scheduled upgrade it withdraws: its protocol version and activation height");
                }
            }
            ProposalKind::ActivateFeature { feature } | ProposalKind::DeactivateFeature { feature } => crate::upgrade::check_feature_name(feature)?,
        }
        Ok(())
    }
}

impl Tx {
    /// Canonical encoding that gets signed. Mirrored exactly in sdk-ts/src/tx.ts. The genesis hash
    /// is its 32 raw bytes (malformed hex, which `check_format` refuses, as zeros).
    ///
    /// Unsigned ghost txs (`PrivateTransfer`, `Unshield`) have a form of their own, which is
    /// their whole wire form: `GHOST_TX_DOMAIN ‖ chain_id ‖ genesis hash ‖ fee ‖ tip ‖
    /// valid_until ‖ action` (no `from`, nonce, memo, key or signature: those stay empty and 0).
    pub fn signing_bytes(&self) -> Vec<u8> {
        if self.is_unsigned() {
            let mut out = GHOST_TX_DOMAIN.to_vec();
            put_str(&mut out, &self.chain_id);
            out.extend_from_slice(&self.genesis_bytes());
            put_u64(&mut out, self.fee);
            put_u64(&mut out, self.tip);
            put_u64(&mut out, self.valid_until);
            self.action.encode(&mut out);
            return out;
        }
        let mut out = TX_DOMAIN.to_vec();
        put_str(&mut out, &self.chain_id);
        out.extend_from_slice(&self.genesis_bytes());
        put_str(&mut out, &self.from);
        put_u64(&mut out, self.nonce);
        put_u64(&mut out, self.fee);
        put_u64(&mut out, self.tip);
        put_u64(&mut out, self.valid_until);
        self.action.encode(&mut out);
        put_str(&mut out, &self.memo);
        out
    }

    /// Wire form: `signing_bytes ‖ public_key (1,952 B) ‖ signature (3,309 B)`, or just the
    /// signing bytes for an unsigned ghost tx. Blocks and mempool gossip carry this;
    /// [`Tx::from_bytes`] is its strict inverse. Only txs that pass [`Tx::check_format`]
    /// round-trip (malformed hex is written as zeros).
    pub fn to_bytes(&self) -> Vec<u8> {
        let raw = |h: &str| hex::decode(h).unwrap_or_else(|_| vec![0; h.len() / 2]);
        let mut out = self.signing_bytes();
        out.extend_from_slice(&raw(&self.public_key));
        out.extend_from_slice(&raw(&self.signature));
        out
    }

    /// Parses the wire form (signed, or the unsigned ghost form: the domain says which). Strict:
    /// no trailing bytes, valid UTF-8, known tags, canonical bools and options, fields within
    /// their bounds, the size bound of the tx's kind, and the bytes must be exactly what
    /// `to_bytes` produces for the result.
    pub fn from_bytes(bytes: &[u8]) -> Result<Tx> {
        if bytes.len() > MAX_GHOST_TX_BYTES {
            bail!("transaction too large ({} bytes, max {MAX_GHOST_TX_BYTES})", bytes.len());
        }
        let mut r = Reader { buf: bytes };
        let tx = if bytes.starts_with(GHOST_TX_DOMAIN) {
            r.take(GHOST_TX_DOMAIN.len())?;
            let (chain_id, genesis_hash) = (r.str("chain_id", MAX_CHAIN_ID)?, hex::encode(r.take(32)?));
            let (fee, tip, valid_until) = (r.u64()?, r.u64()?, r.u64()?);
            let action = Action::decode(&mut r)?;
            if !action.is_unsigned() {
                bail!("only private transfers and unshields travel unsigned");
            }
            Tx { chain_id, genesis_hash, from: String::new(), nonce: 0, fee, tip, valid_until, action, memo: String::new(), public_key: String::new(), signature: String::new() }
        } else {
            if r.take(TX_DOMAIN.len())? != TX_DOMAIN {
                bail!("not a v3 transaction");
            }
            let tx = Tx {
                chain_id: r.str("chain_id", MAX_CHAIN_ID)?,
                genesis_hash: hex::encode(r.take(32)?),
                from: r.str("from", MAX_ADDRESS)?,
                nonce: r.u64()?,
                fee: r.u64()?,
                tip: r.u64()?,
                valid_until: r.u64()?,
                action: Action::decode(&mut r)?,
                memo: r.str("memo", MAX_MEMO)?,
                public_key: hex::encode(r.take(crypto::PUBLIC_KEY_LEN)?),
                signature: hex::encode(r.take(crypto::SIGNATURE_LEN)?),
            };
            if tx.action.is_unsigned() {
                bail!("private transfers and unshields are unsigned: they travel in the BLOCKULA_GHOST_V1 form");
            }
            tx
        };
        if !r.buf.is_empty() {
            bail!("{} trailing bytes after the transaction", r.buf.len());
        }
        let max = tx.max_bytes();
        if bytes.len() > max {
            bail!("transaction too large ({} bytes, max {max})", bytes.len());
        }
        if tx.to_bytes() != bytes {
            bail!("non-canonical transaction encoding");
        }
        Ok(tx)
    }

    /// The codec's size bound for this tx: [`MAX_GHOST_TX_BYTES`] for one carrying a ghost
    /// bundle, [`MAX_TX_BYTES`] for the rest.
    pub fn max_bytes(&self) -> usize {
        if self.action.ghost_bundle().is_some() { MAX_GHOST_TX_BYTES } else { MAX_TX_BYTES }
    }

    /// An unsigned ghost tx (`PrivateTransfer`, `Unshield`): no sender, nonce, key or signature;
    /// its proof authorizes it (ghost.rs).
    pub fn is_unsigned(&self) -> bool {
        self.action.is_unsigned()
    }

    /// The 32 bytes a ghost tx's Action proof is bound to (tags 50–52; `None` for other txs):
    /// `BLAKE3-KDF("blockula ghost v1 tx sighash", chain_id ‖ genesis hash ‖ tag ‖ valid_until ‖
    /// fee ‖ tip ‖
    /// the tag's own fields (Shield: from, nonce, amount; Unshield: to, amount) ‖ the bundle
    /// without its proof (instance, anchor, nullifiers, commitments, ciphertexts) ‖ CIRCUIT_ID)`.
    /// So a proof can't be lifted into another tx, chain (or another instance of it: the genesis
    /// hash, as for signed txs, audit #7) or fee, or have its ciphertexts swapped.
    /// Mirrored in sdk-ts/src/tx.ts `ghostSighash`.
    pub fn ghost_sighash(&self) -> Option<[u8; 32]> {
        let bundle = self.action.ghost_bundle()?;
        let mut fields = vec![];
        let tag = match &self.action {
            Action::Shield { amount, .. } => {
                put_str(&mut fields, &self.from);
                put_u64(&mut fields, self.nonce);
                put_u64(&mut fields, *amount);
                50
            }
            Action::Unshield { to, amount, .. } => {
                put_str(&mut fields, to);
                put_u64(&mut fields, *amount);
                52
            }
            _ => 51,
        };
        let mut out = vec![];
        put_str(&mut out, &self.chain_id);
        out.extend_from_slice(&self.genesis_bytes());
        out.push(tag);
        put_u64(&mut out, self.valid_until);
        put_u64(&mut out, self.fee);
        put_u64(&mut out, self.tip);
        out.extend_from_slice(&fields);
        bundle.encode_public(&mut out);
        out.extend_from_slice(&blockula_ghost::CIRCUIT_ID.to_le_bytes());
        Some(blake3::derive_key("blockula ghost v1 tx sighash", &out))
    }

    /// The genesis hash's 32 raw bytes, as signed (malformed hex, which `check_format` refuses,
    /// as zeros).
    fn genesis_bytes(&self) -> [u8; 32] {
        hex::decode(&self.genesis_hash).ok().and_then(|b| b.try_into().ok()).unwrap_or_default()
    }

    /// What this tx is charged while the base fee is at its genesis floor: `min_fee(size)` plus
    /// the proposer's tip (`None` on overflow). The base fee moves with block fullness, so the
    /// exact charge depends on the block (`econ::charge`); this is what wallets and tests pay
    /// on an idle chain.
    #[cfg_attr(not(test), allow(dead_code))] // tests and the fuzz harness
    pub fn charge(&self) -> Option<u64> {
        min_fee(self.size()).checked_add(self.tip)
    }

    /// `from` and every account the action names by address must use this chain's prefix.
    pub fn check_prefix(&self, prefix: &str) -> Result<()> {
        for (field, addr) in std::iter::once(("from", self.from.as_str())).chain(self.action.addresses()) {
            if let Some((p, _)) = crypto::parse_address(addr) {
                if p != prefix {
                    bail!("`{field}` is a {p} ({}) address, but this chain's addresses start with {prefix}1", crypto::network_of(p));
                }
            }
        }
        Ok(())
    }

    /// BLAKE3 over the signed bytes and the *decoded* signature, so re-encoding the hex
    /// (e.g. uppercase) can't produce a different id for the same tx.
    pub fn id(&self) -> String {
        let mut b = self.signing_bytes();
        match hex::decode(&self.signature) {
            Ok(sig) => b.extend_from_slice(&sig),
            Err(_) => b.extend_from_slice(self.signature.as_bytes()),
        }
        hex::encode(crypto::hash(&b))
    }

    /// Size/format rules every tx must meet before anything else looks at it: canonical
    /// lowercase hex and addresses, capped string fields, the wire size limit, and an exact
    /// round trip through the wire form.
    pub fn check_format(&self) -> Result<()> {
        let (size, max) = (self.size() as usize, self.max_bytes());
        if size > max {
            bail!("transaction too large ({size} bytes, max {max})");
        }
        if self.chain_id.len() > MAX_CHAIN_ID {
            bail!("chain_id too long");
        }
        check_json_ints(&[("nonce", self.nonce), ("valid_until", self.valid_until)])?;
        if self.genesis_hash.len() != 64 || !crypto::is_lower_hex(&self.genesis_hash) {
            bail!("genesis_hash must be the chain's genesis hash (32 bytes of lowercase hex, from /api/status)");
        }
        if self.is_unsigned() {
            if !self.from.is_empty() || self.nonce != 0 || !self.memo.is_empty() || !self.public_key.is_empty() || !self.signature.is_empty() {
                bail!("private transfers and unshields are unsigned (their proof authorizes them): `from`, `memo`, `public_key` and `signature` must be empty and `nonce` 0");
            }
        } else {
            if !crypto::is_address(&self.from) {
                bail!("`from` must be a lowercase address (boo1… or tboo1…)");
            }
            if self.memo.len() > MAX_MEMO {
                bail!("memo too long (max {MAX_MEMO} bytes)");
            }
            if !crypto::is_lower_hex(&self.public_key) || self.public_key.len() != crypto::PUBLIC_KEY_LEN * 2 {
                bail!("public_key must be {} bytes of lowercase hex", crypto::PUBLIC_KEY_LEN);
            }
            if !crypto::is_lower_hex(&self.signature) || self.signature.len() != crypto::SIGNATURE_LEN * 2 {
                bail!("signature must be {} bytes of lowercase hex", crypto::SIGNATURE_LEN);
            }
        }
        self.action.check_format(false)?;
        // Whatever the wire form can't carry exactly (e.g. `familiar_key: Some("")`) is invalid.
        match Tx::from_bytes(&self.to_bytes()) {
            Ok(back) if back == *self => Ok(()),
            Ok(_) => bail!("transaction doesn't survive the wire encoding (an optional field is empty?)"),
            Err(e) => Err(e.context("transaction doesn't survive the wire encoding")),
        }
    }

    /// The tx was signed for this chain instance: its id and its genesis hash.
    pub fn check_chain(&self, chain_id: &str, genesis_hash: &str) -> Result<()> {
        if self.chain_id != chain_id {
            bail!("wrong chain id: this is {chain_id}");
        }
        if self.genesis_hash != genesis_hash {
            bail!("signed for another instance of {chain_id}: this chain's genesis hash is {genesis_hash}");
        }
        Ok(())
    }

    pub fn sender(&self) -> Result<String> {
        if !crypto::is_address(&self.from) {
            bail!("`from` must be an address (boo1… or tboo1…)");
        }
        Ok(self.from.clone())
    }

    pub fn signer_key_id(&self) -> Result<String> {
        if self.is_unsigned() {
            bail!("an unsigned ghost transaction has no signing key");
        }
        Ok(key_id(&hex::decode(&self.public_key).context("public_key hex")?))
    }

    /// Wire size in bytes (`to_bytes().len()`), which fees are priced on.
    pub fn size(&self) -> u64 {
        (self.signing_bytes().len() + self.public_key.len() / 2 + self.signature.len() / 2) as u64
    }

    /// Checks the ML-DSA signature. Unsigned ghost txs have none, so they fail here: their
    /// Action proof authorizes them instead (`ghost::verify_proof`).
    pub fn verify_signature(&self) -> Result<()> {
        if self.is_unsigned() {
            bail!("private transfers and unshields carry no signature: their Action proof authorizes them");
        }
        if self.memo.len() > MAX_MEMO {
            bail!("memo too long (max {MAX_MEMO} bytes)");
        }
        let pk = hex::decode(&self.public_key).context("public_key hex")?;
        let sig = hex::decode(&self.signature).context("signature hex")?;
        crypto::verify(&pk, &self.signing_bytes(), &sig)
    }

    pub fn sign(mut self, keys: &crypto::Keys) -> Self {
        self.public_key = hex::encode(keys.public_key());
        self.signature = hex::encode(keys.sign(&self.signing_bytes()));
        self
    }
}

/// The base fee at its genesis floor (`base_fee_floor_flat + base_fee_floor_per_byte × size`
/// with the default parameters): priced by bytes because post-quantum signatures are big, and
/// burned 🔥. The base fee a block charges moves with fullness (`econ::base_fee`).
pub fn min_fee(tx_size: u64) -> u64 {
    10_000 + tx_size * 10
}

pub const MAX_TX_BYTES: usize = 64 * 1024;
pub const MAX_HUNT_TITLE: usize = 96;
pub const MIN_HUNT: u64 = 10 * DROPS_PER_FANG;
pub const MIN_HUNT_FUNDING: u64 = DROPS_PER_FANG;

/// GitHub login rules: 1–39 of a-z, A-Z, 0-9 and single hyphens, not starting/ending with '-'.
pub fn check_github_handle(h: &str) -> Result<()> {
    let ok = (1..=39).contains(&h.len()) && h.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') && !h.starts_with('-') && !h.ends_with('-') && !h.contains("--");
    if !ok {
        bail!("not a valid GitHub handle");
    }
    Ok(())
}

/// `owner/name` on GitHub.
pub fn check_repo(repo: &str) -> Result<()> {
    let Some((owner, name)) = repo.split_once('/') else { bail!("repo must be owner/name") };
    check_github_handle(owner).context("repo owner")?;
    if name.is_empty() || name.len() > 100 || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)) || name == "." || name == ".." {
        bail!("bad repo name");
    }
    Ok(())
}

pub fn check_title(t: &str) -> Result<()> {
    if t.trim().is_empty() || t.len() > MAX_HUNT_TITLE || t.chars().any(|c| c.is_control()) {
        bail!("title must be 1–{MAX_HUNT_TITLE} bytes with no control characters");
    }
    Ok(())
}

pub const MAX_REASON: usize = 96;

/// Why an admin did something (shown in the explorer).
pub fn check_reason(r: &str) -> Result<()> {
    if r.trim().is_empty() || r.len() > MAX_REASON || r.chars().any(|c| c.is_control()) {
        bail!("reason must be 1–{MAX_REASON} bytes with no control characters");
    }
    Ok(())
}

/// A raw address (validators are always named by address).
fn check_address(field: &str, v: &str) -> Result<()> {
    if !crypto::is_address(v) {
        bail!("`{field}` must be a lowercase boo1… address");
    }
    Ok(())
}

/// 32 bytes of lowercase hex.
fn check_hex_32(field: &str, v: &str) -> Result<()> {
    if v.len() != 64 || !crypto::is_lower_hex(v) {
        bail!("`{field}` must be 32 bytes of lowercase hex");
    }
    Ok(())
}

/// A name ("jack" or "jack.boo") within the field cap.
fn check_name_field(v: &str) -> Result<()> {
    if v.len() > MAX_NAME_FIELD {
        bail!("name too long");
    }
    normalize_name(v)?;
    Ok(())
}

/// A `boo1…` address or a `.boo` name.
fn check_account_ref(field: &str, v: &str) -> Result<()> {
    if v.len() > MAX_NAME_FIELD {
        bail!("`{field}` too long (max {MAX_NAME_FIELD} bytes)");
    }
    if !crypto::is_address(v) {
        normalize_name(v).with_context(|| format!("`{field}` must be a lowercase boo1… address or a .boo name"))?;
    }
    Ok(())
}
pub const MAX_MEMO: usize = 256;
pub const MAX_LABEL: usize = 64;
pub const MAX_NAME_FIELD: usize = 64;
pub const MAX_ADDR: usize = 100;
/// Transfers that create an account must carry at least this much (makes account spam costly).
pub const MIN_NEW_ACCOUNT: u64 = DROPS_PER_FANG / 100;

/// Domain separation for validator proofs of possession (FIPS 204 context string).
pub const POP_CONTEXT: &[u8] = b"blockula/validator-pop/v1";

/// What a validator's consensus key signs to prove possession when registering.
pub fn pop_message(chain_id: &str, owner: &str, p2p_addr: &str, http_addr: &str) -> Vec<u8> {
    let mut out = b"BLOCKULA_VALIDATOR_POP_V1".to_vec();
    for s in [chain_id, owner, p2p_addr, http_addr] {
        put_str(&mut out, s);
    }
    out
}

/// Accepts "jack" or "jack.boo" (ASCII letters in any case), returns the bare label "jack".
/// Whitespace and non-ASCII are rejected, not trimmed or folded (e.g. the Kelvin sign 'K'
/// lowercases to 'k', which would alias names).
pub fn normalize_name(input: &str) -> Result<String> {
    if !input.is_ascii() {
        bail!("names may only use a-z, 0-9 and '-'");
    }
    if input.len() > MAX_NAME_FIELD || input.trim() != input {
        bail!("names can't contain spaces");
    }
    let n = input.to_ascii_lowercase();
    let n = n.strip_suffix(NAME_SUFFIX).unwrap_or(&n).to_string();
    if !(3..=32).contains(&n.len()) {
        bail!("names must be 3–32 characters");
    }
    if !n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        bail!("names may only use a-z, 0-9 and '-'");
    }
    if n.starts_with('-') || n.ends_with('-') {
        bail!("names can't start or end with '-'");
    }
    Ok(n)
}

pub fn fmt_fang(drops: u64) -> String {
    let whole = drops / DROPS_PER_FANG;
    let frac = drops % DROPS_PER_FANG;
    if frac == 0 {
        format!("{whole} FANG")
    } else {
        let f = format!("{frac:09}");
        format!("{whole}.{} FANG", f.trim_end_matches('0'))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Block {
    pub height: u64,
    pub prev_hash: String,
    pub timestamp_ms: u64,
    /// Simplex view this block was proposed in, and its parent's view (0 for an epoch's first
    /// block). Views skipped in between had no canonical block (missed leader slots).
    #[serde(default)]
    pub view: u64,
    #[serde(default)]
    pub parent_view: u64,
    /// Protocol version the block was made under (`upgrade::version_at` its height).
    #[serde(default)]
    pub version: u32,
    /// Newest protocol version the proposer's binary supports (upgrade readiness, display only).
    #[serde(default)]
    pub signal: u32,
    pub proposer: String,
    pub state_root: String,
    pub txs: Vec<Tx>,
    pub hash: String,
    /// Kept for compatibility; certificates live in the state database now (see store.rs).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub signature: String,
    /// Txs (by index into `txs`) that were included and charged their fee, but whose action
    /// failed at execution (e.g. the balance was spent by an earlier tx in the block).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub failed: std::collections::BTreeMap<usize, String>,
    /// Wire sizes of the txs the consensus block carried that couldn't be admitted (skipped,
    /// not in `txs`). Replay hands them to the end-of-block steps exactly as execution did
    /// (the proposer pays for junk, MAINNET-DESIGN 3.4).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<u64>,
    /// The base fee its txs paid and its size limit (fixed when the block 17 earlier executed,
    /// `econ::FeeSlot`), so explorers show what a tx was charged. Display only: replay
    /// recomputes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_fee: Option<crate::econ::FeeSlot>,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use proptest::prelude::*;

    /// One action per tag (0–29), in tag order. Mirrored by `vectorActions` in sdk-ts/test/e2e.ts.
    pub(crate) fn vector_actions() -> Vec<Action> {
        vec![
            Action::Transfer { to: "x.boo".into(), amount: 5 },
            Action::RegisterName { name: "vlad".into() },
            Action::AddKey { key_id: "ab".repeat(32), label: "phone".into(), role: KeyRole::Owner, public_key: "0c".into(), pop: "0d".into() },
            Action::AddKey { key_id: "cd".repeat(32), label: "claude".into(), role: KeyRole::Session { limit: 10, period_blocks: 100, expires_at: 1000 }, public_key: "0e".into(), pop: "0f".into() },
            Action::RemoveKey { key_id: "ab".repeat(32) },
            Action::RegisterValidator { consensus_key: "0a".into(), p2p_addr: "1.2.3.4:5".into(), http_addr: "https://v.example".into(), bond: 50, pop: "0b".into() },
            Action::ApproveValidator { validator: "boo1v".into() },
            Action::RemoveValidator { validator: "boo1v".into() },
            Action::Unbond { validator: "boo1v".into() },
            Action::LinkGithub { handle: "vlad".into(), gist_id: "abc123".into() },
            Action::UnlinkGithub { handle: "vlad".into() },
            Action::CreateHunt { repo: "a/b".into(), issue: 3, amount: 10, expires_at: 500, title: "t".into() },
            Action::FundHunt { hunt_id: 1, amount: 2 },
            Action::AwardHunt { hunt_id: 1, to: "x.boo".into(), pr: 4, familiar_key: Some("ef".repeat(32)) },
            Action::AwardHunt { hunt_id: 1, to: "x.boo".into(), pr: 4, familiar_key: None },
            Action::CancelHunt { hunt_id: 1 },
            Action::RegisterFamiliar { key_id: "cd".repeat(32), label: "claude".into() },
            Action::SetRepo { repo: "a/b".into(), weight: 10 },
            Action::GrantSoul { to: "x.boo".into(), amount: 2500, reason: "r".into() },
            Action::Vouch { account: "x.boo".into() },
            Action::Unvouch { account: "x.boo".into() },
            Action::FlagSybil { account: "x.boo".into(), reason: "r".into() },
            Action::SubmitClaim { repo: "a/b".into(), pr: 9, familiar_key: Some("ab".repeat(32)) },
            Action::SubmitClaim { repo: "a/b".into(), pr: 9, familiar_key: None },
            Action::AttestClaim { claim_id: 4, ok: true, merge_sha: "cd".repeat(20), note: String::new() },
            Action::RejectClaim { claim_id: 4, reason: "r".into() },
            Action::JoinJury { bond: 10 * DROPS_PER_FANG },
            Action::LeaveJury {},
            Action::CommitVotes { seance_id: 2, commitment: "ef".repeat(32), pairs_digest: "e0".repeat(32) },
            Action::RevealVotes { seance_id: 2, votes: "ab=".into(), spam: vec![5, 6], salt: "s".into() },
            Action::FundPool { amount: 5 },
            Action::WithdrawVested {},
            Action::Clawback { account: "x.boo".into(), reason: "r".into() },
        ]
    }

    /// Protocol v1 (MAINNET-DESIGN 3.1): one action per tag 30–49 (some twice, for optional or
    /// empty fields), with every `ProposalKind` and a multisig proposal, in tag order. Mirrored by
    /// `v1VectorActions` in sdk-ts/test/e2e.ts.
    pub(crate) fn v1_vector_actions() -> Vec<Action> {
        let propose = |kind: ProposalKind| Action::Propose { kind, deposit: 1_000 * DROPS_PER_FANG, title: "t".into(), url: String::new() };
        vec![
            Action::Delegate { validator: "boo1v".into(), amount: 5 * DROPS_PER_FANG },
            Action::Undelegate { validator: "boo1v".into(), shares: 7, amount: 0 },
            Action::Undelegate { validator: "boo1v".into(), shares: 0, amount: 9 * DROPS_PER_FANG },
            Action::Redelegate { from_validator: "boo1v".into(), to_validator: "boo1w".into(), shares: 8 },
            Action::WithdrawUnbonded {},
            Action::UpdateValidator { commission_bps: 500, p2p_addr: "1.2.3.4:5".into(), http_addr: "https://v.example".into() },
            Action::UpdateValidator { commission_bps: 0, p2p_addr: String::new(), http_addr: String::new() },
            Action::Unjail { validator: "boo1v".into() },
            Action::ReportEquivocation { evidence: "0a0b".into() },
            Action::BeaconCommit { epoch: 3, commitment: "ab".repeat(32) },
            Action::BeaconReveal { epoch: 3, secret: "cd".repeat(32) },
            Action::Propose { kind: ProposalKind::ParamChange { key: "max_validators".into(), value: 40 }, deposit: 1_000 * DROPS_PER_FANG, title: "More validators".into(), url: "https://x.example/p/1".into() },
            propose(ProposalKind::TreasurySpend { to: "x.boo".into(), amount: 9 }),
            propose(ProposalKind::Distribution { distribution_id: 6, merkle_root: "ef".repeat(32), total: 100, claim_until: 5_000 }),
            propose(ProposalKind::SetRepo { repo: "a/b".into(), weight: 10 }),
            propose(ProposalKind::GrantSoul { to: "x.boo".into(), amount: 2500, reason: "r".into() }),
            propose(ProposalKind::FlagSybil { account: "x.boo".into(), reason: "r".into() }),
            propose(ProposalKind::RejectClaim { claim_id: 4, reason: "r".into() }),
            propose(ProposalKind::Clawback { account: "x.boo".into(), reason: "r".into() }),
            propose(ProposalKind::UnlinkGithub { handle: "vlad".into() }),
            propose(ProposalKind::RefundHunt { hunt_id: 1 }),
            propose(ProposalKind::ScheduleUpgrade { version: 2, activation_height: 100_000 }),
            propose(ProposalKind::ActivateFeature { feature: "ghost".into() }),
            propose(ProposalKind::DeactivateFeature { feature: "ghost".into() }),
            propose(ProposalKind::Text {}),
            Action::Vote { proposal_id: 1, aye: true, amount: 10, conviction: 6, proposal_digest: "d1".repeat(32) },
            Action::Vote { proposal_id: 1, aye: false, amount: 10, conviction: 0, proposal_digest: "d1".repeat(32) },
            Action::Unlock {},
            Action::MultisigCreate { owners: vec!["ab".repeat(32), "cd".repeat(32)], threshold: 2 },
            Action::MultisigPropose { action: Box::new(Action::Transfer { to: "x.boo".into(), amount: 5 }) },
            Action::MultisigApprove { id: 3, action_digest: "a3".repeat(32) },
            Action::RenewName { name: "vlad".into(), years: 2 },
            Action::TransferName { name: "vlad".into(), to: "x.boo".into() },
            Action::ReleaseName { name: "vlad".into() },
            Action::ClaimDistribution { distribution_id: 1, amount: 50, proof: vec!["ab".repeat(32), "cd".repeat(32)] },
            Action::ClaimDistribution { distribution_id: 1, amount: 50, proof: vec![] },
            Action::AttestRevert { claim_id: 4, revert_sha: "ef".repeat(20) },
        ]
    }

    /// Ghost Mode (tags 50–54): one action per tag, around a fixed bundle (bytes chosen to be
    /// canonical digests). Mirrored by `ghostVectorActions` in sdk-ts/test/e2e.ts.
    pub(crate) fn ghost_vector_actions() -> Vec<Action> {
        vec![
            Action::Shield { amount: 5 * DROPS_PER_FANG, bundle: vector_bundle(11) },
            Action::PrivateTransfer { bundle: vector_bundle(12) },
            Action::Unshield { to: "x.boo".into(), amount: 7, bundle: vector_bundle(13) },
            Action::SetGhostAddress { address: HexBytes(vec![14; GHOST_ADDRESS_BYTES]) },
            Action::GhostPause {},
        ]
    }

    /// A set of one or two's readiness and admission (tags 60–61, audit round 3): one action per
    /// tag. Mirrored by `smallSetVectorActions` in sdk-ts/test/e2e.ts.
    pub(crate) fn small_set_vector_actions() -> Vec<Action> {
        vec![Action::ValidatorReady { epoch: 3 }, Action::AdmitValidator { validator: "boo1v".into(), epoch: 3 }]
    }

    /// A bundle whose fields are `[1; 32]`, `[2; 32]` … in wire order and whose 300-byte proof is
    /// `[proof; 300]`.
    pub(crate) fn vector_bundle(proof: u8) -> Box<GhostBundle> {
        let b = |n: u8, len: usize| HexBytes(vec![n; len]);
        let d = GHOST_DIGEST_BYTES;
        Box::new(GhostBundle { instance: b(1, d), anchor: b(2, d), nullifiers: [b(3, d), b(4, d)], commitments: [b(5, d), b(6, d)], enc: [b(7, GHOST_ENC_BYTES), b(8, GHOST_ENC_BYTES)], out: [b(9, GHOST_OUT_BYTES), b(10, GHOST_OUT_BYTES)], proof: b(proof, 300) })
    }

    /// The vector tx around an action: fixed fields, and recognizable key and signature bytes.
    /// Unsigned ghost actions get the unsigned form's empty fields.
    pub(crate) fn vector_tx(action: Action) -> Tx {
        let tx = Tx { chain_id: "vec".into(), genesis_hash: "03".repeat(32), from: "boo1from".into(), nonce: 7, fee: 12345, tip: 3, valid_until: 99, action, memo: "m".into(), public_key: "01".repeat(crypto::PUBLIC_KEY_LEN), signature: "02".repeat(crypto::SIGNATURE_LEN) };
        if tx.is_unsigned() { Tx { from: String::new(), nonce: 0, memo: String::new(), public_key: String::new(), signature: String::new(), ..tx } } else { tx }
    }

    /// `sdk-ts/test/tx-vectors.json`, found from the crate's directory (`node/`, or `node/fuzz/`,
    /// which compiles these modules too).
    fn tx_vectors_file() -> std::path::PathBuf {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        dir.ancestors().map(|d| d.join("sdk-ts/test/tx-vectors.json")).find(|p| p.exists()).unwrap_or_else(|| dir.join("../sdk-ts/test/tx-vectors.json"))
    }

    /// Signing-bytes vectors for the SDK's encoder (`signingBytes` in sdk-ts/src/tx.ts, checked
    /// offline by sdk-ts/test/tx-vectors.ts): every vector action, then txs at the bounds of
    /// every integer width (u64 amounts at u64::MAX, JSON-number fields at 2^53 − 1, the largest
    /// a JavaScript number holds exactly, u8 and u32 fields at their maxima, zeros) and non-ASCII
    /// strings. Each case is the tx's JSON (key and signature left empty: they aren't signed) and
    /// its signing bytes; the SDK must produce the same bytes from the JSON, and must refuse the
    /// out-of-range values the node's JSON parser refuses instead of wrapping them.
    fn tx_vectors() -> serde_json::Value {
        const JS_MAX: u64 = MAX_JSON_INT;
        let unsigned = |tx: Tx| Tx { public_key: String::new(), signature: String::new(), ..tx };
        let mut cases: Vec<(String, Tx)> = vector_actions().into_iter().chain(v1_vector_actions()).chain(ghost_vector_actions()).chain(small_set_vector_actions()).map(|a| (format!("{} (tag {})", a.kind(), a.to_bytes()[0]), unsigned(vector_tx(a)))).collect();
        let base = unsigned(vector_tx(Action::Transfer { to: "x.boo".into(), amount: u64::MAX }));
        let at = |label: &str, action: Action| (label.to_string(), Tx { action, ..base.clone() });
        cases.extend([
            ("tx integers at their maxima".to_string(), Tx { nonce: JS_MAX, fee: u64::MAX, tip: u64::MAX, valid_until: JS_MAX, ..base.clone() }),
            ("tx integers at zero".to_string(), Tx { nonce: 0, fee: 0, tip: 0, valid_until: 0, action: Action::Transfer { to: "x.boo".into(), amount: 0 }, memo: String::new(), ..base.clone() }),
            ("non-ASCII strings".to_string(), Tx { chain_id: "vec-🧛".into(), memo: "ünïcödé 🦇 \u{0}\u{7f}".into(), action: Action::AddKey { key_id: "ab".repeat(32), label: "Vlad’s phone 📱".into(), role: KeyRole::Owner, public_key: "0c".into(), pop: "0d".into() }, ..base.clone() }),
            at("session key limit at u64::MAX", Action::AddKey { key_id: "cd".repeat(32), label: "agent".into(), role: KeyRole::Session { limit: u64::MAX, period_blocks: JS_MAX, expires_at: JS_MAX }, public_key: "0e".into(), pop: "0f".into() }),
            at("hunt at the bounds", Action::CreateHunt { repo: "a/b".into(), issue: JS_MAX, amount: u64::MAX, expires_at: JS_MAX, title: "t".into() }),
            at("award at the bounds", Action::AwardHunt { hunt_id: JS_MAX, to: "x.boo".into(), pr: JS_MAX, familiar_key: None }),
            at("vote at the bounds", Action::Vote { proposal_id: JS_MAX, aye: false, amount: u64::MAX, conviction: u8::MAX, proposal_digest: "ff".repeat(32) }),
            at("multisig threshold at u8::MAX", Action::MultisigCreate { owners: vec![], threshold: u8::MAX }),
            at("renewal years at u8::MAX", Action::RenewName { name: "vlad".into(), years: u8::MAX }),
            at("reveal with large ids", Action::RevealVotes { seance_id: JS_MAX, votes: String::new(), spam: vec![0, JS_MAX], salt: String::new() }),
            at("param change at u64::MAX", Action::Propose { kind: ProposalKind::ParamChange { key: "k".into(), value: u64::MAX }, deposit: u64::MAX, title: "t".into(), url: String::new() }),
            at("upgrade at u32::MAX", Action::Propose { kind: ProposalKind::ScheduleUpgrade { version: u32::MAX, activation_height: JS_MAX }, deposit: 0, title: "t".into(), url: String::new() }),
            at("distribution at the bounds", Action::Propose { kind: ProposalKind::Distribution { distribution_id: JS_MAX, merkle_root: "ef".repeat(32), total: u64::MAX, claim_until: JS_MAX }, deposit: 1, title: "t".into(), url: String::new() }),
            at("unflag sybil (proposal kind 14)", Action::Propose { kind: ProposalKind::UnflagSybil { account: "vlad.boo".into(), reason: "appeal ✓".into() }, deposit: 1, title: "t".into(), url: String::new() }),
            at("upgrade cancellation at the bounds (proposal kind 15)", Action::Propose { kind: ProposalKind::CancelUpgrade { version: u32::MAX, activation_height: JS_MAX }, deposit: 1, title: "t".into(), url: String::new() }),
            at("claim at u64::MAX", Action::ClaimDistribution { distribution_id: JS_MAX, amount: u64::MAX, proof: vec![] }),
            at("delegate at u64::MAX", Action::Delegate { validator: "boo1v".into(), amount: u64::MAX }),
            at("commission at the JS bound", Action::UpdateValidator { commission_bps: JS_MAX, p2p_addr: String::new(), http_addr: String::new() }),
            at("shield at u64::MAX", Action::Shield { amount: u64::MAX, bundle: vector_bundle(15) }),
            at("readiness at the JS bound", Action::ValidatorReady { epoch: JS_MAX }),
            at("admission at the JS bound", Action::AdmitValidator { validator: "boo1v".into(), epoch: JS_MAX }),
        ]);
        let cases: Vec<serde_json::Value> = cases
            .into_iter()
            .map(|(label, tx)| {
                let json = serde_json::to_value(&tx).unwrap();
                assert_eq!(serde_json::from_value::<Tx>(json.clone()).unwrap(), tx, "{label}: the JSON parses back");
                serde_json::json!({ "label": label, "tx": json, "signing_bytes": hex::encode(tx.signing_bytes()) })
            })
            .collect();
        // What approvals, votes and sealed judgments bind (audit gap-7-6), and a key's consent
        // message (gap-7-2): the SDK's `actionDigest`, `proposalDigest`, `seancePairsDigest` and
        // `keyPopMessage` must compute the same.
        let digests = [
            Action::Transfer { to: "bob.boo".into(), amount: 5_000_000_000 },
            Action::Propose { kind: ProposalKind::TreasurySpend { to: "boo1to".into(), amount: 9 }, deposit: 1, title: "Pay Bob".into(), url: String::new() },
            Action::AddKey { key_id: "ab".repeat(32), label: "Vlad’s phone 📱".into(), role: KeyRole::Session { limit: u64::MAX, period_blocks: 1, expires_at: JS_MAX }, public_key: "0c".into(), pop: "0d".into() },
            // Every JSON-number field at the largest value the format rules allow
            // (review-r2-clients-1): a JavaScript client computes the same digests.
            Action::CreateHunt { repo: "a/b".into(), issue: JS_MAX, amount: u64::MAX, expires_at: JS_MAX, title: "t".into() },
            Action::Propose { kind: ProposalKind::Distribution { distribution_id: JS_MAX, merkle_root: "ef".repeat(32), total: u64::MAX, claim_until: JS_MAX }, deposit: u64::MAX, title: "t".into(), url: String::new() },
            Action::ClaimDistribution { distribution_id: JS_MAX, amount: u64::MAX, proof: vec!["ab".repeat(32)] },
        ]
        .into_iter()
        .map(|a| serde_json::json!({ "action": a, "digest": action_digest(&a) }))
        .collect::<Vec<_>>();
        let at_bounds = [
            ProposalKind::Distribution { distribution_id: JS_MAX, merkle_root: "ef".repeat(32), total: u64::MAX, claim_until: JS_MAX },
            ProposalKind::ScheduleUpgrade { version: u32::MAX, activation_height: JS_MAX },
            ProposalKind::CancelUpgrade { version: u32::MAX, activation_height: JS_MAX },
            ProposalKind::RejectClaim { claim_id: JS_MAX, reason: "r".into() },
            ProposalKind::RefundHunt { hunt_id: JS_MAX },
        ];
        let proposals = [(ProposalKind::Text {}, "Plant garlic 🧄", "https://forum.example/p/1"), (ProposalKind::ParamChange { key: "max_validators".into(), value: u64::MAX }, "More validators", ""), (ProposalKind::GrantSoul { to: "boo1to".into(), amount: 1500, reason: "r".into() }, "", "")]
            .into_iter()
            .chain(at_bounds.into_iter().map(|k| (k, "At the bounds", "")))
            .map(|(kind, title, url)| serde_json::json!({ "kind": kind, "title": title, "url": url, "digest": proposal_digest(&kind, title, url) }))
            .collect::<Vec<_>>();
        let pairs: [[(u64, &str, u64); 2]; 2] = [[(9, "c/d", 90), (7, "a/b", 70)], [(JS_MAX, "x/ÿ", JS_MAX), (0, "", 0)]];
        let seance = serde_json::json!({ "seance_id": 4, "juror": "boo1juror", "pairs": pairs.iter().map(|p| p.iter().map(|(id, repo, pr)| serde_json::json!({ "id": id, "repo": repo, "pr": pr })).collect::<Vec<_>>()).collect::<Vec<_>>(), "digest": seance_pairs_digest(4, "boo1juror", &pairs) });
        let key_pop = serde_json::json!({ "chain_id": "vec-🧛", "genesis_hash": "03".repeat(32), "account": "boo1from", "key_id": "ab".repeat(32), "message": hex::encode(key_pop_message("vec-🧛", &"03".repeat(32), "boo1from", &"ab".repeat(32))) });
        serde_json::json!({
            "description": "Signing bytes of txs from their JSON (node/src/types.rs `tx_vectors`), for sdk-ts/src/tx.ts `signingBytes`. Regenerate: cargo test --manifest-path node/Cargo.toml types::tests::write_tx_vectors -- --ignored",
            "cases": cases,
            "action_digests": digests,
            "proposal_digests": proposals,
            "seance_pairs_digest": seance,
            "key_pop_message": key_pop,
        })
    }

    /// The signing encoding is a consensus rule: the SDK's vector file must match exactly.
    #[test]
    fn tx_vectors_match_the_sdk_file() {
        let file: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(tx_vectors_file()).expect("sdk-ts/test/tx-vectors.json")).unwrap();
        assert!(file == tx_vectors(), "the tx encoding or the vectors changed: regenerate sdk-ts/test/tx-vectors.json (types::tests::write_tx_vectors) and run `npm run test:vectors -w @blockula/sdk`");
    }

    #[test]
    #[ignore = "generator, run by hand (see tx_vectors)"]
    fn write_tx_vectors() {
        std::fs::write(tx_vectors_file(), serde_json::to_string_pretty(&tx_vectors()).unwrap() + "\n").unwrap();
    }

    /// The same vectors are computed by sdk-ts (test/e2e.ts): the encodings must match byte for byte.
    #[test]
    fn encodings_match_the_sdk() {
        assert_eq!(seance_commitment(3, "boo1juror", "ab=", &[1, 2], "pepper"), "1f3071e1123a713836e2c9c27798dc74ed1fd1f97de6b5fd985c0dba8f9f7eae");
        let mut h = blake3::Hasher::new();
        for action in vector_actions() {
            h.update(&vector_tx(action).signing_bytes());
        }
        assert_eq!(h.finalize().to_hex().to_string(), "1078aa2103641542458acf9a1928da918af6505c8cb3a3a1f7bbf7320bbe3c30");
        // Wire form, size and id of one tx.
        let tx = vector_tx(Action::Transfer { to: "x.boo".into(), amount: 5 });
        assert_eq!(hex::encode(crypto::hash(&tx.to_bytes())), "084ce40c9d406468f7373b0aced2b285ff77d353de986752050877e1953501e0");
        assert_eq!(tx.size(), tx.to_bytes().len() as u64);
        assert_eq!(tx.id(), "250e5fc58d322d13ba15e01991885090daa4e8742cc1f68336a2ca37c8539a22");
        // And the JSON shapes the SDK sends parse.
        let a: Action = serde_json::from_str(r#"{"type":"leave_jury"}"#).unwrap();
        assert!(matches!(a, Action::LeaveJury {}));
        let a: Action = serde_json::from_str(r#"{"type":"join_jury","bond":"10"}"#).unwrap();
        assert!(matches!(a, Action::JoinJury { bond: 10 }));
        let a: Action = serde_json::from_str(r#"{"type":"attest_claim","claim_id":1,"ok":false,"merge_sha":"","note":"x"}"#).unwrap();
        assert!(matches!(a, Action::AttestClaim { ok: false, .. }));
        // Protocol v1 (tags 30–49), hashed separately so the 0–29 vector above stays put.
        let mut h = blake3::Hasher::new();
        for action in v1_vector_actions() {
            h.update(&vector_tx(action).signing_bytes());
        }
        assert_eq!(h.finalize().to_hex().to_string(), "a411ddbbcc010f275e102b3d2b2abec2b9e96c6c0e3f69cb50868a07678d4f6e");
        // Proposal kinds added after that vector froze, each on its own (mirrored in e2e.ts):
        // `UnflagSybil` (tag 14, audit F54), then `CancelUpgrade` (tag 15, audit F50; unit.ts too).
        let unflag = Action::Propose { kind: ProposalKind::UnflagSybil { account: "x.boo".into(), reason: "r".into() }, deposit: 1_000 * DROPS_PER_FANG, title: "t".into(), url: String::new() };
        assert_eq!(blake3::hash(&vector_tx(unflag.clone()).signing_bytes()).to_hex().to_string(), "225aa5b8e4df8e8d27fe796992dfd02e5e402c14a9bbc3873d41d63dd07dc2cf");
        assert_eq!(serde_json::to_value(&unflag).unwrap()["kind"], serde_json::json!({ "type": "unflag_sybil", "account": "x.boo", "reason": "r" }));
        let k = ProposalKind::UnflagSybil { account: "x.boo".into(), reason: "r".into() };
        assert_eq!(ProposalKind::from_bytes(&k.to_bytes()).unwrap(), k);
        // `CancelUpgrade`:
        let k = ProposalKind::CancelUpgrade { version: 2, activation_height: 100_000 };
        let cancel = Action::Propose { kind: k.clone(), deposit: 1_000 * DROPS_PER_FANG, title: "t".into(), url: String::new() };
        assert_eq!(blake3::hash(&vector_tx(cancel.clone()).signing_bytes()).to_hex().to_string(), "199d2455419f738528317187cdbfa72e5430308b096c2c8741f5801290d3d156");
        assert_eq!(serde_json::to_value(&cancel).unwrap()["kind"], serde_json::json!({ "type": "cancel_upgrade", "version": 2, "activation_height": 100_000 }));
        assert_eq!(serde_json::from_value::<Action>(serde_json::to_value(&cancel).unwrap()).unwrap(), cancel);
        assert_eq!(k.to_bytes(), [15, 2, 0, 0, 0, 0xa0, 0x86, 0x01, 0, 0, 0, 0, 0]);
        assert_eq!(ProposalKind::from_bytes(&k.to_bytes()).unwrap(), k);
        assert_eq!(Action::from_bytes(&cancel.to_bytes()).unwrap(), cancel);
        assert!(ProposalKind::from_bytes(&[15, 2, 0, 0, 0]).is_err(), "the activation height is part of it");
        assert!(ProposalKind::CancelUpgrade { version: 0, activation_height: 5 }.check().is_err());
        assert!(ProposalKind::CancelUpgrade { version: 2, activation_height: 0 }.check().is_err());
        assert!(ProposalKind::from_bytes(&[16]).unwrap_err().to_string().contains("unknown proposal kind 16"));
        let p: Action = serde_json::from_str(r#"{"type":"propose","kind":{"type":"param_change","key":"max_validators","value":"40"},"deposit":"1000","title":"t"}"#).unwrap();
        assert_eq!(p, Action::Propose { kind: ProposalKind::ParamChange { key: "max_validators".into(), value: 40 }, deposit: 1000, title: "t".into(), url: String::new() });
        let m: Action = serde_json::from_str(&format!(r#"{{"type":"multisig_propose","action":{{"type":"vote","proposal_id":1,"aye":true,"amount":"5","conviction":2,"proposal_digest":"{}"}}}}"#, "d1".repeat(32))).unwrap();
        assert!(serde_json::from_str::<Action>(r#"{"type":"vote","proposal_id":1,"aye":true,"amount":"5","conviction":2}"#).is_err(), "a vote binds its proposal (gap-7-6)");
        assert_eq!(m, Action::MultisigPropose { action: Box::new(Action::Vote { proposal_id: 1, aye: true, amount: 5, conviction: 2, proposal_digest: "d1".repeat(32) }) });
        for a in [r#"{"type":"withdraw_unbonded"}"#, r#"{"type":"unlock"}"#, r#"{"type":"propose","kind":{"type":"text"},"deposit":"1","title":"t","url":""}"#] {
            serde_json::from_str::<Action>(a).unwrap();
        }
        assert!(serde_json::from_str::<Action>(r#"{"type":"propose","kind":{"type":"text","x":1},"deposit":"1","title":"t"}"#).is_err(), "proposal kinds reject unknown fields too");
        for a in v1_vector_actions() {
            let json = serde_json::to_value(&a).unwrap();
            assert_eq!(json["type"], a.kind());
            assert_eq!(serde_json::from_value::<Action>(json).unwrap(), a);
        }
        let t: Tx = serde_json::from_value(serde_json::json!({ "chain_id": "c", "genesis_hash": "00".repeat(32), "from": "boo1from", "nonce": 1, "fee": "100", "tip": "2", "valid_until": 9, "action": { "type": "transfer", "to": "x.boo", "amount": "5" }, "memo": "", "public_key": "", "signature": "" })).unwrap();
        assert_eq!((t.fee, t.tip, t.valid_until), (100, 2, 9));
        // Ghost Mode (tags 50–54): signing bytes (the unsigned wire form for 51 and 52) and the
        // sighashes of 50–52, hashed on their own.
        let mut h = blake3::Hasher::new();
        for action in ghost_vector_actions() {
            let tx = vector_tx(action);
            h.update(&tx.signing_bytes());
            if let Some(sighash) = tx.ghost_sighash() {
                h.update(&sighash);
            }
        }
        assert_eq!(h.finalize().to_hex().to_string(), "a0673611a2f5de08736a58b4fa66413b17c98255c077dc15432383e68e20414b");
        let pt = vector_tx(ghost_vector_actions().remove(1));
        assert_eq!(hex::encode(pt.ghost_sighash().unwrap()), "c68439d83ad46343419a1645022271f6de9a343108ed610bbccb33e38a423378");
        assert_eq!(pt.id(), hex::encode(crypto::hash(&pt.to_bytes())), "an unsigned tx's id is the hash of its wire form");
        let json = serde_json::to_value(&pt.action).unwrap();
        assert_eq!((json["type"].as_str(), json["bundle"]["anchor"].as_str()), (Some("private_transfer"), Some("02".repeat(32).as_str())));
        assert_eq!(serde_json::from_value::<Action>(json).unwrap(), pt.action);
        for a in ghost_vector_actions() {
            let json = serde_json::to_value(&a).unwrap();
            assert_eq!(json["type"], a.kind());
            assert_eq!(serde_json::from_value::<Action>(json).unwrap(), a);
        }
        // A set of one or two's readiness and admission (tags 60–61, audit round 3), each on its
        // own (mirrored in e2e.ts).
        let [ready, admit]: [Action; 2] = small_set_vector_actions().try_into().unwrap();
        assert_eq!(ready.to_bytes(), [60, 3, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(blake3::hash(&vector_tx(ready.clone()).signing_bytes()).to_hex().to_string(), "1276620e9888e634548d914538007b797a6b81281a89a27d3f88470804d19012");
        assert_eq!(blake3::hash(&vector_tx(admit.clone()).signing_bytes()).to_hex().to_string(), "75a6f69d5103b23658fc22a88a23a4ba1811d8c52d8a8043cd633b59a3dcf3c1");
        assert_eq!(serde_json::to_value(&ready).unwrap(), serde_json::json!({ "type": "validator_ready", "epoch": 3 }));
        assert_eq!(serde_json::to_value(&admit).unwrap(), serde_json::json!({ "type": "admit_validator", "validator": "boo1v", "epoch": 3 }));
        for a in [ready, admit] {
            assert_eq!(serde_json::from_value::<Action>(serde_json::to_value(&a).unwrap()).unwrap(), a);
        }
        assert!(serde_json::from_str::<Action>(r#"{"type":"validator_ready","epoch":3,"commission_bps":0}"#).is_err(), "readiness carries no terms");
    }

    /// Unsigned ghost txs (51, 52) travel in their own wire form; nothing else does, and they
    /// travel in no other. Ghost actions don't nest in multisig proposals; bundles and addresses
    /// are checked for size and canonical digests.
    #[test]
    fn ghost_wire_forms_and_bounds() {
        let pt = vector_tx(ghost_vector_actions().remove(1));
        let wire = pt.to_bytes();
        assert!(wire.starts_with(GHOST_TX_DOMAIN));
        // Like a signed tx, an unsigned one names its chain instance (audit #7): the genesis hash
        // follows the chain id in the wire form and the proof's sighash commits to it.
        assert_eq!(&wire[GHOST_TX_DOMAIN.len() + 2 + 3..][..32], &[3; 32]);
        let elsewhere = Tx { genesis_hash: "04".repeat(32), ..pt.clone() };
        assert_ne!(elsewhere.ghost_sighash(), pt.ghost_sighash());
        assert!(elsewhere.check_chain("vec", &pt.genesis_hash).unwrap_err().to_string().contains("another instance"));
        assert_eq!((Tx::from_bytes(&wire).unwrap(), wire.len() as u64), (pt.clone(), pt.size()));
        pt.check_format().unwrap();
        assert!(pt.verify_signature().unwrap_err().to_string().contains("no signature"));
        assert!(pt.signer_key_id().is_err() && pt.sender().is_err());
        let err = |b: &[u8]| Tx::from_bytes(b).unwrap_err().to_string();
        // Fields an unsigned tx can't have.
        for bad in [Tx { from: "boo1from".into(), ..pt.clone() }, Tx { nonce: 1, ..pt.clone() }, Tx { memo: "m".into(), ..pt.clone() }, Tx { signature: "02".into(), ..pt.clone() }] {
            assert!(bad.check_format().unwrap_err().to_string().contains("unsigned"));
        }
        // An unsigned action in the signed form, and a signed one in the unsigned form.
        let mut signed_form = TX_DOMAIN.to_vec();
        put_str(&mut signed_form, "vec");
        signed_form.extend_from_slice(&[3; 32]);
        put_str(&mut signed_form, "boo1from");
        for v in [7, 12345, 3, 99] {
            put_u64(&mut signed_form, v);
        }
        pt.action.encode(&mut signed_form);
        put_str(&mut signed_form, "");
        signed_form.extend_from_slice(&[1; crypto::PUBLIC_KEY_LEN]);
        signed_form.extend_from_slice(&[2; crypto::SIGNATURE_LEN]);
        assert!(err(&signed_form).contains("BLOCKULA_GHOST_V1 form"));
        let mut unsigned_form = GHOST_TX_DOMAIN.to_vec();
        put_str(&mut unsigned_form, "vec");
        unsigned_form.extend_from_slice(&[3; 32]);
        for v in [12345, 3, 99] {
            put_u64(&mut unsigned_form, v);
        }
        unsigned_form.push(54);
        assert!(err(&unsigned_form).contains("only private transfers and unshields"));
        let mut trailing = wire.clone();
        trailing.push(0);
        assert!(err(&trailing).contains("trailing"));
        // Ghost txs are bounded by MAX_GHOST_TX_BYTES, everything else by MAX_TX_BYTES.
        let shield = vector_tx(ghost_vector_actions().remove(0));
        assert_eq!(shield.max_bytes(), MAX_GHOST_TX_BYTES);
        assert_eq!(vector_tx(Action::GhostPause {}).max_bytes(), MAX_TX_BYTES);
        let mut big = pt.clone();
        if let Action::PrivateTransfer { bundle } = &mut big.action {
            bundle.proof = HexBytes(vec![1; blockula_ghost::MAX_PROOF_BYTES]);
        }
        assert!(big.size() as usize > MAX_TX_BYTES);
        assert_eq!(Tx::from_bytes(&big.to_bytes()).unwrap(), big);
        // No ghost action in a multisig proposal, in either direction.
        for a in ghost_vector_actions() {
            let m = Action::MultisigPropose { action: Box::new(a) };
            assert!(m.check_format(false).unwrap_err().to_string().contains("ghost action"));
            assert!(Action::from_bytes(&m.to_bytes()).unwrap_err().to_string().contains("ghost action"));
        }
        // Bundles: canonical digests, distinct nullifiers, exact ciphertext sizes, a proof.
        let bad = |edit: &dyn Fn(&mut GhostBundle)| {
            let mut b = vector_bundle(1);
            edit(&mut b);
            Action::PrivateTransfer { bundle: b }.check_format(false).unwrap_err().to_string()
        };
        assert!(bad(&|b| b.anchor = HexBytes(vec![0xff; 32])).contains("canonical"));
        assert!(bad(&|b| b.commitments[1] = HexBytes(vec![0; 31])).contains("canonical"));
        assert!(bad(&|b| b.nullifiers[1] = b.nullifiers[0].clone()).contains("same nullifier"));
        assert!(bad(&|b| b.enc[0].0.push(0)).contains("note ciphertext"));
        assert!(bad(&|b| b.out[1].0.pop().map(drop).unwrap_or(())).contains("outgoing ciphertext"));
        assert!(bad(&|b| b.proof = HexBytes::default()).contains("proof"));
        assert!(bad(&|b| b.instance = HexBytes(vec![1; 33])).contains("instance"));
        let amount = |a: Action| a.check_format(false).unwrap_err().to_string();
        assert!(amount(Action::Shield { amount: 0, bundle: vector_bundle(1) }).contains("more than 0"));
        assert!(amount(Action::Unshield { to: "carol.boo".into(), amount: 0, bundle: vector_bundle(1) }).contains("more than 0"));
        assert!(amount(Action::Unshield { to: "x".into(), amount: 1, bundle: vector_bundle(1) }).contains("`to`"));
        // Addresses: a real one passes; the vector's filler doesn't (no valid ML-KEM key).
        let real = blockula_ghost::SpendingKey::from_seed([9; 32]).address().to_bytes().to_vec();
        Action::SetGhostAddress { address: HexBytes(real) }.check_format(false).unwrap();
        assert!(amount(ghost_vector_actions().remove(3)).contains("valid 1216-byte"));
        // JSON: lowercase hex only.
        assert!(serde_json::from_str::<HexBytes>("\"AB\"").is_err());
        assert_eq!(serde_json::from_str::<HexBytes>("\"ab\"").unwrap(), HexBytes(vec![0xab]));
    }

    #[test]
    fn every_tag_round_trips_through_the_wire_form() {
        let mut tags = std::collections::BTreeSet::new();
        let mut kinds = std::collections::BTreeSet::new();
        for action in vector_actions().into_iter().chain(v1_vector_actions()).chain(ghost_vector_actions()).chain(small_set_vector_actions()) {
            let enc = action.to_bytes();
            tags.insert(enc[0]);
            if let Action::Propose { kind, .. } = &action {
                kinds.insert(enc[1]);
                assert_eq!(ProposalKind::from_bytes(&kind.to_bytes()).unwrap(), *kind);
            }
            assert_eq!(Action::from_bytes(&enc).unwrap(), action);
            let tx = vector_tx(action);
            assert_eq!(Tx::from_bytes(&tx.to_bytes()).unwrap(), tx);
        }
        assert_eq!(tags, (0..=54).chain(60..=61).collect(), "the vectors cover every action tag (55–59 are Ghost Mode's, unused)");
        assert_eq!(kinds, (0..=13).collect(), "and every proposal kind");
    }

    /// review-r2-clients-1: every integer field a tx carries as a JSON number (not a `u64_str`
    /// string) is at most `MAX_JSON_INT` by the format rules, so a JavaScript client reads every
    /// stored proposal kind and multisig action exactly and can compute what a vote or an
    /// approval binds. Each vector action's JSON numbers (a proposal kind's and a nested multisig
    /// action's too), pushed one at a time past the bound, make the format check fail with that
    /// rule; at the bound they don't trip it. A field `json_ints` misses fails here.
    #[test]
    fn json_number_fields_are_bounded() {
        fn numbers(v: &serde_json::Value, path: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
            match v {
                serde_json::Value::Number(_) => out.push(path.clone()),
                serde_json::Value::Object(m) => m.iter().for_each(|(k, v)| {
                    path.push(k.clone());
                    numbers(v, path, out);
                    path.pop();
                }),
                serde_json::Value::Array(a) => a.iter().enumerate().for_each(|(i, v)| {
                    path.push(i.to_string());
                    numbers(v, path, out);
                    path.pop();
                }),
                _ => {}
            }
        }
        fn set(v: &mut serde_json::Value, path: &[String], n: u64) {
            let mut at = v;
            for p in path {
                at = if at.is_array() { &mut at[p.parse::<usize>().unwrap()] } else { &mut at[p.as_str()] };
            }
            *at = n.into();
        }
        let propose = |kind: ProposalKind| Action::Propose { kind, deposit: 1, title: "t".into(), url: String::new() };
        let wrap = |a: Action| Action::MultisigPropose { action: Box::new(a) };
        let session = KeyRole::Session { limit: 1, period_blocks: 2, expires_at: 3 };
        let extra = [
            propose(ProposalKind::UnflagSybil { account: "x.boo".into(), reason: "r".into() }),
            propose(ProposalKind::CancelUpgrade { version: 2, activation_height: 9 }),
            wrap(Action::CreateHunt { repo: "a/b".into(), issue: 3, amount: 10, expires_at: 500, title: "t".into() }),
            wrap(Action::AddKey { key_id: "cd".repeat(32), label: "agent".into(), role: session, public_key: "0e".into(), pop: "0f".into() }),
            wrap(propose(ProposalKind::Distribution { distribution_id: 6, merkle_root: "ef".repeat(32), total: 100, claim_until: 5_000 })),
            wrap(Action::RevealVotes { seance_id: 2, votes: "ab".into(), spam: vec![5], salt: "s".into() }),
        ];
        let rule = format!("must be at most {MAX_JSON_INT}");
        let mut checked = 0;
        for action in vector_actions().into_iter().chain(v1_vector_actions()).chain(ghost_vector_actions()).chain(small_set_vector_actions()).chain(extra) {
            let json = serde_json::to_value(&action).unwrap();
            let mut paths = vec![];
            numbers(&json, &mut vec![], &mut paths);
            for path in paths {
                let at = |n: u64| {
                    let mut j = json.clone();
                    set(&mut j, &path, n);
                    serde_json::from_value::<Action>(j).ok()
                };
                // u8 and u32 fields (conviction, threshold, years, version) can't hold it at all.
                let Some(over) = at(MAX_JSON_INT + 1) else { continue };
                let e = over.check_format(false).map_err(|e| format!("{e:#}"));
                assert!(e.as_ref().is_err_and(|e| e.contains(&rule)), "{}: {path:?} past 2^53 − 1: {e:?}", action.kind());
                let fine = at(MAX_JSON_INT).expect("the bound parses").check_format(false).map_err(|e| format!("{e:#}"));
                assert!(fine.as_ref().is_ok() || fine.as_ref().is_err_and(|e| !e.contains(&rule)), "{}: {path:?} at 2^53 − 1: {fine:?}", action.kind());
                checked += 1;
            }
        }
        assert!(checked >= 40, "only {checked} JSON-number fields checked");
        // The tx's own: its nonce and valid_until.
        for field in ["nonce", "valid_until"] {
            let mut j = serde_json::to_value(vector_tx(Action::Transfer { to: "x.boo".into(), amount: 5 })).unwrap();
            j[field] = (MAX_JSON_INT + 1).into();
            let e = serde_json::from_value::<Tx>(j).unwrap().check_format().unwrap_err().to_string();
            assert!(e.contains(&format!("{field} {rule}")), "{e}");
        }
    }

    #[test]
    fn v1_actions_are_bounded() {
        let ok = |a: Action| a.check_format(false).unwrap_or_else(|e| panic!("{a:?}: {e}"));
        let bad = |a: Action| match a.check_format(false) {
            Ok(()) => panic!("{a:?} should be refused"),
            Err(e) => format!("{e:#}"),
        };
        let k = crypto::Keys::from_seed([7; 32]);
        let (addr, hex32) = (k.address(), "ab".repeat(32));
        let propose = |kind: ProposalKind| Action::Propose { kind, deposit: 1, title: "t".into(), url: String::new() };
        ok(Action::Delegate { validator: addr.clone(), amount: 1 });
        assert!(bad(Action::Delegate { validator: "x.boo".into(), amount: 1 }).contains("address"), "validators are named by address");
        ok(Action::AdmitValidator { validator: addr.clone(), epoch: 1 });
        assert!(bad(Action::AdmitValidator { validator: "x.boo".into(), epoch: 1 }).contains("address"), "and admitted by address");
        assert!(bad(Action::Delegate { validator: addr.clone(), amount: 0 }).contains("more than 0"));
        assert!(bad(Action::Redelegate { from_validator: addr.clone(), to_validator: addr.clone(), shares: 1 }).contains("different"));
        // Undelegate: a number of shares or an amount (audit gap-7-5), exactly one of them.
        ok(Action::Undelegate { validator: addr.clone(), shares: 1, amount: 0 });
        ok(Action::Undelegate { validator: addr.clone(), shares: 0, amount: 1 });
        assert!(bad(Action::Undelegate { validator: addr.clone(), shares: 0, amount: 0 }).contains("more than 0"));
        assert!(bad(Action::Undelegate { validator: addr.clone(), shares: 1, amount: 1 }).contains("not both"));
        // The shape clients sent before the amount existed still parses (by shares).
        let a: Action = serde_json::from_str(&format!(r#"{{"type":"undelegate","validator":"{addr}","shares":"5"}}"#)).unwrap();
        assert_eq!(a, Action::Undelegate { validator: addr.clone(), shares: 5, amount: 0 });
        assert!(bad(Action::UpdateValidator { commission_bps: 10_001, p2p_addr: String::new(), http_addr: String::new() }).contains("0–10000"));
        // Evidence: at most 16 KiB decoded, as lowercase hex.
        ok(Action::ReportEquivocation { evidence: "ab".repeat(MAX_EVIDENCE_BYTES) });
        assert!(bad(Action::ReportEquivocation { evidence: "ab".repeat(MAX_EVIDENCE_BYTES + 1) }).contains("16384"));
        assert!(bad(Action::ReportEquivocation { evidence: "AB".into() }).contains("lowercase hex"));
        assert!(bad(Action::ReportEquivocation { evidence: String::new() }).contains("1–"));
        assert!(bad(Action::BeaconCommit { epoch: 1, commitment: "ab".into() }).contains("32 bytes"));
        // Votes: conviction 0–6.
        let d = "d0".repeat(32);
        ok(Action::Vote { proposal_id: 0, aye: true, amount: 1, conviction: 6, proposal_digest: d.clone() });
        assert!(bad(Action::Vote { proposal_id: 0, aye: true, amount: 1, conviction: 7, proposal_digest: d.clone() }).contains("0–6"));
        assert!(bad(Action::Vote { proposal_id: 0, aye: true, amount: 0, conviction: 1, proposal_digest: d.clone() }).contains("more than 0"));
        // gap-7-6: what a vote, an approval and a sealed judgment bind is 32 bytes of lowercase hex.
        assert!(bad(Action::Vote { proposal_id: 0, aye: true, amount: 1, conviction: 1, proposal_digest: "D0".repeat(32) }).contains("proposal_digest"));
        assert!(bad(Action::MultisigApprove { id: 0, action_digest: String::new() }).contains("action_digest"));
        assert!(bad(Action::CommitVotes { seance_id: 0, commitment: d.clone(), pairs_digest: "0".into() }).contains("pairs_digest"));
        // Proposals: the kind's own rules, a deposit, a title, an https link or nothing.
        ok(propose(ProposalKind::ParamChange { key: "quorum_bps".into(), value: 1 }));
        assert!(bad(propose(ProposalKind::ParamChange { key: "nope".into(), value: 1 })).contains("unknown parameter"));
        assert!(bad(propose(ProposalKind::GrantSoul { to: addr.clone(), amount: crate::poc::MAX_SOUL_GRANT + 1, reason: "r".into() })).contains("milli-Soul"));
        assert!(bad(propose(ProposalKind::Distribution { distribution_id: 0, merkle_root: hex32.clone(), total: 0, claim_until: 1 })).contains("total"));
        assert!(bad(propose(ProposalKind::ActivateFeature { feature: "Ghost Mode".into() })).contains("feature"));
        assert!(bad(propose(ProposalKind::ScheduleUpgrade { version: 0, activation_height: 5 })).contains("version"));
        assert!(bad(Action::Propose { kind: ProposalKind::Text {}, deposit: 0, title: "t".into(), url: String::new() }).contains("deposit"));
        assert!(bad(Action::Propose { kind: ProposalKind::Text {}, deposit: 1, title: " ".into(), url: String::new() }).contains("title"));
        assert!(bad(Action::Propose { kind: ProposalKind::Text {}, deposit: 1, title: "t".into(), url: "http://x".into() }).contains("https"));
        // Multisig: distinct key ids, 1 ≤ threshold ≤ owners, and proposals don't nest.
        ok(Action::MultisigCreate { owners: vec![hex32.clone()], threshold: 1 });
        assert!(bad(Action::MultisigCreate { owners: vec![hex32.clone(), hex32.clone()], threshold: 1 }).contains("distinct"));
        assert!(bad(Action::MultisigCreate { owners: vec![hex32.clone()], threshold: 2 }).contains("threshold"));
        assert!(bad(Action::MultisigCreate { owners: vec![], threshold: 0 }).contains("owners"));
        let inner = Action::Transfer { to: addr.clone(), amount: 1 };
        ok(Action::MultisigPropose { action: Box::new(inner.clone()) });
        let nested = Action::MultisigPropose { action: Box::new(Action::MultisigPropose { action: Box::new(inner.clone()) }) };
        assert!(bad(nested.clone()).contains("can't hold another"));
        assert!(bad(Action::MultisigPropose { action: Box::new(Action::MultisigApprove { id: 1, action_digest: "a1".repeat(32) }) }).contains("can't hold another"));
        assert!(bad(Action::MultisigPropose { action: Box::new(Action::Transfer { to: "x".into(), amount: 1 }) }).contains("proposed action"));
        // … nor do they decode nested.
        assert!(Action::from_bytes(&nested.to_bytes()).unwrap_err().to_string().contains("can't contain another"));
        // Names and distributions.
        assert!(bad(Action::RenewName { name: "vlad".into(), years: 0 }).contains("1–10"));
        assert!(bad(Action::RenewName { name: "vlad".into(), years: MAX_RENEW_YEARS + 1 }).contains("1–10"));
        assert!(bad(Action::TransferName { name: "vlad".into(), to: "b".into() }).contains("`to`"));
        assert!(bad(Action::ClaimDistribution { distribution_id: 0, amount: 1, proof: vec![hex32.clone(); MAX_PROOF + 1] }).contains("proof too long"));
        assert!(bad(Action::ClaimDistribution { distribution_id: 0, amount: 1, proof: vec!["ab".into()] }).contains("32 bytes"));
        assert!(bad(Action::AttestRevert { claim_id: 0, revert_sha: "ab".repeat(32) }).contains("40-hex"));
        // Every account an action names is checked against the chain's prefix, nested ones too.
        let testnet = k.address_with(crypto::TESTNET_HRP);
        let tx = |action: Action| Tx { chain_id: "c".into(), genesis_hash: String::new(), from: addr.clone(), nonce: 0, fee: 0, tip: 0, valid_until: 0, action, memo: String::new(), public_key: String::new(), signature: String::new() };
        for a in [
            Action::Delegate { validator: testnet.clone(), amount: 1 },
            Action::Redelegate { from_validator: addr.clone(), to_validator: testnet.clone(), shares: 1 },
            Action::TransferName { name: "vlad".into(), to: testnet.clone() },
            propose(ProposalKind::TreasurySpend { to: testnet.clone(), amount: 1 }),
            propose(ProposalKind::Clawback { account: testnet.clone(), reason: "r".into() }),
            Action::MultisigPropose { action: Box::new(Action::Transfer { to: testnet.clone(), amount: 1 }) },
        ] {
            assert!(tx(a.clone()).check_prefix("boo").unwrap_err().to_string().contains("tboo"), "{a:?}");
        }
    }

    #[test]
    fn json_rejects_unknown_fields() {
        let ok = serde_json::json!({ "chain_id": "c", "genesis_hash": "00".repeat(32), "from": "boo1from", "nonce": 1, "fee": "100", "valid_until": 9, "action": { "type": "transfer", "to": "x.boo", "amount": "5" }, "public_key": "", "signature": "" });
        let t: Tx = serde_json::from_value(ok.clone()).unwrap();
        assert_eq!((t.tip, t.memo.as_str()), (0, ""), "tip and memo default");
        let mut extra = ok.clone();
        extra["padding"] = serde_json::json!("x".repeat(1000));
        assert!(serde_json::from_value::<Tx>(extra).unwrap_err().to_string().contains("unknown field"));
        let mut inner = ok.clone();
        inner["action"]["padding"] = serde_json::json!("x");
        assert!(serde_json::from_value::<Tx>(inner).is_err(), "unknown field inside the (internally tagged) action");
        assert!(serde_json::from_str::<Action>(r#"{"type":"leave_jury","x":1}"#).is_err());
        let role = |r: &str| serde_json::from_str::<Action>(&format!(r#"{{"type":"add_key","key_id":"ab","label":"l","role":{r},"public_key":"0c","pop":"0d"}}"#));
        assert!(role(r#"{"type":"owner","limit":"1"}"#).is_err(), "and inside the key role");
        assert!(role(r#"{"type":"session","limit":"1","period_blocks":2,"expires_at":3,"x":0}"#).is_err());
        assert_eq!(role(r#"{"type":"owner"}"#).unwrap(), Action::AddKey { key_id: "ab".into(), label: "l".into(), role: KeyRole::Owner, public_key: "0c".into(), pop: "0d".into() });
        // gap-7-2: the added key's public key and proof of possession are required.
        assert!(serde_json::from_str::<Action>(r#"{"type":"add_key","key_id":"ab","label":"l","role":{"type":"owner"}}"#).is_err(), "an AddKey without the key's consent");
        let session = role(r#"{"type":"session","limit":"1","period_blocks":2,"expires_at":3}"#).unwrap();
        assert_eq!(serde_json::to_value(&session).unwrap()["role"], serde_json::json!({ "type": "session", "limit": "1", "period_blocks": 2, "expires_at": 3 }));
        let mut missing = ok;
        missing.as_object_mut().unwrap().remove("valid_until");
        assert!(serde_json::from_value::<Tx>(missing).is_err(), "valid_until is required");
    }

    #[test]
    fn from_bytes_rejects_malformed_input() {
        let tx = vector_tx(Action::AttestClaim { claim_id: 4, ok: true, merge_sha: "cd".repeat(20), note: "n".into() });
        let good = tx.to_bytes();
        let sb = tx.signing_bytes().len();
        let err = |b: &[u8]| Tx::from_bytes(b).unwrap_err().to_string();
        let mut trailing = good.clone();
        trailing.push(0);
        assert!(err(&trailing).contains("trailing"));
        assert!(err(&good[..good.len() - 1]).contains("truncated"));
        assert!(err(&good[..10]).contains("truncated"));
        let mut v2 = good.clone();
        v2[..TX_DOMAIN.len()].copy_from_slice(b"BLOCKULA_TX_V2");
        assert!(err(&v2).contains("v3"));
        // Field offsets: domain, chain_id, genesis hash, from, nonce, fee, tip, valid_until, then
        // the tag.
        let tag_at = TX_DOMAIN.len() + 2 + 3 + 32 + 2 + 8 + 4 * 8;
        assert_eq!(good[tag_at], 21);
        let mut unknown = good.clone();
        for t in [55, 59, 62] {
            unknown[tag_at] = t;
            assert!(err(&unknown).contains(&format!("unknown action tag {t}")));
        }
        // A proposal kind past the last one.
        let text = vector_tx(Action::Propose { kind: ProposalKind::Text {}, deposit: 1, title: "t".into(), url: String::new() }).to_bytes();
        let mut bad_kind = text.clone();
        assert_eq!((bad_kind[tag_at], bad_kind[tag_at + 1]), (39, 13));
        bad_kind[tag_at + 1] = 16;
        assert!(err(&bad_kind).contains("unknown proposal kind 16"));
        let mut bad_aye = vector_tx(Action::Vote { proposal_id: 1, aye: true, amount: 1, conviction: 1, proposal_digest: "d1".repeat(32) }).to_bytes();
        bad_aye[tag_at + 1 + 8] = 2;
        assert!(err(&bad_aye).contains("`aye` must be 0 or 1"));
        let mut bad_bool = good.clone();
        bad_bool[tag_at + 1 + 8] = 2;
        assert!(err(&bad_bool).contains("0 or 1"));
        // Invalid UTF-8 in the memo (the last string of the signing bytes).
        let mut bad_utf8 = good.clone();
        bad_utf8[sb - 1] = 0xff;
        assert!(err(&bad_utf8).contains("UTF-8"));
        // Over-long fields are rejected even when the bytes are all there.
        let long_memo = Tx { memo: "m".repeat(MAX_MEMO + 1), ..tx.clone() };
        assert!(err(&long_memo.to_bytes()).contains("`memo` too long"));
        let spam = vector_tx(Action::RevealVotes { seance_id: 1, votes: "a".into(), spam: vec![1; crate::poc::MAX_SEANCE_CLAIMS + 1], salt: "s".into() });
        assert!(err(&spam.to_bytes()).contains("too many entries"));
        let role = vector_tx(Action::AddKey { key_id: "ab".repeat(32), label: "l".into(), role: KeyRole::Owner, public_key: "0c".into(), pop: "0d".into() });
        let mut bad_role = role.to_bytes();
        // Back from the end of the signing bytes: the memo ("m"), the pop and the public key.
        bad_role[role.signing_bytes().len() - (2 + 1) - (2 + 2) - (2 + 2) - 1] = 2;
        assert!(err(&bad_role).contains("unknown key role"));
        assert!(Tx::from_bytes(&vec![0u8; MAX_GHOST_TX_BYTES + 1]).unwrap_err().to_string().contains("too large"));
    }

    #[test]
    fn check_format_requires_an_exact_wire_round_trip() {
        let keys = crypto::Keys::from_seed([9; 32]);
        let base = Tx { chain_id: "c".into(), genesis_hash: "00".repeat(32), from: keys.address(), nonce: 0, fee: 1, tip: 0, valid_until: 9, action: Action::SubmitClaim { repo: "a/b".into(), pr: 1, familiar_key: None }, memo: String::new(), public_key: String::new(), signature: String::new() }.sign(&keys);
        base.check_format().unwrap();
        // `Some("")` encodes like `None`, so it would change meaning on the wire.
        let empty = Tx { action: Action::SubmitClaim { repo: "a/b".into(), pr: 1, familiar_key: Some(String::new()) }, ..base.clone() }.sign(&keys);
        assert!(empty.check_format().is_err());
        assert!(matches!(Tx::from_bytes(&empty.to_bytes()).unwrap().action, Action::SubmitClaim { familiar_key: None, .. }));
        let award = Tx { action: Action::AwardHunt { hunt_id: 1, to: keys.address(), pr: 1, familiar_key: Some(String::new()) }, ..base.clone() }.sign(&keys);
        assert!(award.check_format().is_err());
        // A huge memo is measured on its real length (no u16 wrap) and refused.
        let huge = Tx { memo: "m".repeat(70_000), ..base.clone() };
        assert!(huge.check_format().unwrap_err().to_string().contains("too large"));
    }

    /// Audit #7: the genesis hash is signed (right after the chain id), well-formed, and the chain
    /// a tx names by id and genesis hash is checked.
    #[test]
    fn the_chain_instance_is_signed_and_checked() {
        let keys = crypto::Keys::from_seed([9; 32]);
        let (here, there) = ("aa".repeat(32), "bb".repeat(32));
        let tx = Tx { chain_id: "c".into(), genesis_hash: here.clone(), from: keys.address(), nonce: 0, fee: 1, tip: 0, valid_until: 9, action: Action::Transfer { to: "vlad.boo".into(), amount: 1 }, memo: String::new(), public_key: String::new(), signature: String::new() }.sign(&keys);
        tx.check_format().unwrap();
        tx.verify_signature().unwrap();
        assert_eq!(&tx.signing_bytes()[TX_DOMAIN.len() + 3..][..32], &[0xaa; 32]);
        tx.check_chain("c", &here).unwrap();
        assert!(tx.check_chain("c", &there).unwrap_err().to_string().contains("another instance of c"));
        assert!(tx.check_chain("d", &here).unwrap_err().to_string().contains("wrong chain id"));
        // Re-pointed at another instance, the signature no longer holds.
        assert!(Tx { genesis_hash: there, ..tx.clone() }.verify_signature().is_err());
        for bad in [String::new(), "aa".repeat(31), "AA".repeat(32), "zz".repeat(32), "aa".repeat(33)] {
            let t = Tx { genesis_hash: bad.clone(), ..tx.clone() }.sign(&keys);
            assert!(t.check_format().unwrap_err().to_string().contains("genesis_hash"), "{bad:?}");
        }
    }

    #[test]
    fn prefixes_are_checked_against_the_chain() {
        let k = crypto::Keys::from_seed([4; 32]);
        let tx = |from: String, to: String| Tx { chain_id: "c".into(), genesis_hash: String::new(), from, nonce: 0, fee: 0, tip: 0, valid_until: 0, action: Action::Transfer { to, amount: 1 }, memo: String::new(), public_key: String::new(), signature: String::new() };
        let (main, test) = (k.address(), k.address_with(crypto::TESTNET_HRP));
        tx(main.clone(), "x.boo".into()).check_prefix("boo").unwrap();
        tx(main.clone(), main.clone()).check_prefix("boo").unwrap();
        let err = tx(main.clone(), test.clone()).check_prefix("boo").unwrap_err().to_string();
        assert!(err.contains("`to` is a tboo (testnet) address"), "{err}");
        assert!(tx(main.clone(), main.clone()).check_prefix("tboo").unwrap_err().to_string().contains("`from` is a boo (mainnet) address"));
        tx(test.clone(), test).check_prefix("tboo").unwrap();
    }

    fn arb_str(max_chars: usize) -> impl Strategy<Value = String> {
        proptest::collection::vec(any::<char>(), 0..=max_chars).prop_map(|v| v.into_iter().collect())
    }

    fn arb_hex(bytes: usize) -> impl Strategy<Value = String> {
        proptest::collection::vec(any::<u8>(), bytes).prop_map(hex::encode)
    }

    /// A proposal kind, by its tag, from the same kind of field bag.
    fn proposal_kind(tag: u8, [a, b]: [String; 2], [w, x]: [u64; 2]) -> ProposalKind {
        match tag {
            0 => ProposalKind::ParamChange { key: a, value: w },
            1 => ProposalKind::TreasurySpend { to: a, amount: w },
            2 => ProposalKind::Distribution { distribution_id: x ^ w, merkle_root: a, total: w, claim_until: x },
            3 => ProposalKind::SetRepo { repo: a, weight: w },
            4 => ProposalKind::GrantSoul { to: a, amount: w, reason: b },
            5 => ProposalKind::FlagSybil { account: a, reason: b },
            6 => ProposalKind::RejectClaim { claim_id: w, reason: a },
            7 => ProposalKind::Clawback { account: a, reason: b },
            8 => ProposalKind::UnlinkGithub { handle: a },
            9 => ProposalKind::RefundHunt { hunt_id: w },
            10 => ProposalKind::ScheduleUpgrade { version: w as u32, activation_height: x },
            11 => ProposalKind::ActivateFeature { feature: a },
            12 => ProposalKind::DeactivateFeature { feature: a },
            13 => ProposalKind::Text {},
            14 => ProposalKind::UnflagSybil { account: a, reason: b },
            15 => ProposalKind::CancelUpgrade { version: w as u32, activation_height: x },
            _ => unreachable!(),
        }
    }

    /// Any action, by tag, from a bag of random fields (strings of up to 6 chars fit every bound).
    /// A multisig proposal wraps a transfer or a vote (they don't nest).
    fn arb_action() -> impl Strategy<Value = Action> {
        let strs = proptest::array::uniform4(arb_str(6));
        let nums = proptest::array::uniform3(any::<u64>());
        let opt = proptest::option::of(arb_str(6).prop_filter("Some is never empty", |s| !s.is_empty()));
        let ids = proptest::collection::vec(any::<u64>(), 0..4);
        let list = proptest::collection::vec(arb_str(6), 0..4);
        (0u8..57, strs, nums, any::<bool>(), opt, ids, list, 0u8..16, any::<u8>()).prop_map(|(tag, [a, b, c, d], [w, x, y], ok, familiar_key, spam, list, kind, small)| match tag {
            0 => Action::Transfer { to: a, amount: w },
            1 => Action::RegisterName { name: a },
            2 => Action::AddKey { key_id: a, label: b, role: if ok { KeyRole::Owner } else { KeyRole::Session { limit: w, period_blocks: x, expires_at: y } }, public_key: c, pop: d },
            3 => Action::RemoveKey { key_id: a },
            4 => Action::RegisterValidator { consensus_key: a, p2p_addr: b, http_addr: c, bond: w, pop: d },
            5 => Action::ApproveValidator { validator: a },
            6 => Action::RemoveValidator { validator: a },
            7 => Action::Unbond { validator: a },
            8 => Action::LinkGithub { handle: a, gist_id: b },
            9 => Action::UnlinkGithub { handle: a },
            10 => Action::CreateHunt { repo: a, issue: w, amount: x, expires_at: y, title: b },
            11 => Action::FundHunt { hunt_id: w, amount: x },
            12 => Action::AwardHunt { hunt_id: w, to: a, pr: x, familiar_key },
            13 => Action::CancelHunt { hunt_id: w },
            14 => Action::RegisterFamiliar { key_id: a, label: b },
            15 => Action::SetRepo { repo: a, weight: w },
            16 => Action::GrantSoul { to: a, amount: w, reason: b },
            17 => Action::Vouch { account: a },
            18 => Action::Unvouch { account: a },
            19 => Action::FlagSybil { account: a, reason: b },
            20 => Action::SubmitClaim { repo: a, pr: w, familiar_key },
            21 => Action::AttestClaim { claim_id: w, ok, merge_sha: a, note: b },
            22 => Action::RejectClaim { claim_id: w, reason: a },
            23 => Action::JoinJury { bond: w },
            24 => Action::LeaveJury {},
            25 => Action::CommitVotes { seance_id: w, commitment: a, pairs_digest: c },
            26 => Action::RevealVotes { seance_id: w, votes: a, spam, salt: b },
            27 => Action::FundPool { amount: w },
            28 => Action::WithdrawVested {},
            29 => Action::Clawback { account: a, reason: b },
            30 => Action::Delegate { validator: a, amount: w },
            31 => Action::Undelegate { validator: a, shares: w, amount: x },
            32 => Action::Redelegate { from_validator: a, to_validator: b, shares: w },
            33 => Action::WithdrawUnbonded {},
            34 => Action::UpdateValidator { commission_bps: w, p2p_addr: a, http_addr: b },
            35 => Action::Unjail { validator: a },
            36 => Action::ReportEquivocation { evidence: a },
            37 => Action::BeaconCommit { epoch: w, commitment: a },
            38 => Action::BeaconReveal { epoch: w, secret: a },
            39 => Action::Propose { kind: proposal_kind(kind, [c, d], [x, y]), deposit: w, title: a, url: b },
            40 => Action::Vote { proposal_id: w, aye: ok, amount: x, conviction: small, proposal_digest: a },
            41 => Action::Unlock {},
            42 => Action::MultisigCreate { owners: list, threshold: small },
            43 => Action::MultisigPropose { action: Box::new(if ok { Action::Transfer { to: a, amount: w } } else { Action::Vote { proposal_id: w, aye: small % 2 == 0, amount: x, conviction: small, proposal_digest: b } }) },
            44 => Action::MultisigApprove { id: w, action_digest: a },
            45 => Action::RenewName { name: a, years: small },
            46 => Action::TransferName { name: a, to: b },
            47 => Action::ReleaseName { name: a },
            48 => Action::ClaimDistribution { distribution_id: w, amount: x, proof: list },
            49 => Action::AttestRevert { claim_id: w, revert_sha: a },
            50 => Action::Shield { amount: w, bundle: vector_bundle(small) },
            51 => Action::PrivateTransfer { bundle: vector_bundle(small) },
            52 => Action::Unshield { to: a, amount: w, bundle: vector_bundle(small) },
            53 => Action::SetGhostAddress { address: HexBytes(vec![small; GHOST_ADDRESS_BYTES]) },
            54 => Action::GhostPause {},
            // Tags 60–61 (55–59 are Ghost Mode's, unused).
            55 => Action::ValidatorReady { epoch: w },
            56 => Action::AdmitValidator { validator: a, epoch: w },
            _ => unreachable!(),
        })
    }

    fn arb_tx() -> impl Strategy<Value = Tx> {
        (arb_str(16), arb_hex(32), arb_str(16), proptest::array::uniform4(any::<u64>()), arb_action(), arb_str(64), arb_hex(crypto::PUBLIC_KEY_LEN), arb_hex(crypto::SIGNATURE_LEN)).prop_map(
            |(chain_id, genesis_hash, from, [nonce, fee, tip, valid_until], action, memo, public_key, signature)| {
                // The unsigned form carries no sender, nonce, memo, key or signature.
                let unsigned = action.is_unsigned();
                let (from, nonce, memo, public_key, signature) = if unsigned { (String::new(), 0, String::new(), String::new(), String::new()) } else { (from, nonce, memo, public_key, signature) };
                Tx { chain_id, genesis_hash, from, nonce, fee, tip, valid_until, action, memo, public_key, signature }
            },
        )
    }

    proptest! {
        #[test]
        fn wire_round_trip(tx in arb_tx()) {
            let bytes = tx.to_bytes();
            prop_assert_eq!(bytes.len() as u64, tx.size());
            prop_assert_eq!(Tx::from_bytes(&bytes).unwrap(), tx);
        }

        /// Mangled encodings are either rejected or re-encode to exactly the same bytes.
        #[test]
        fn accepted_bytes_are_canonical(tx in arb_tx(), at in any::<prop::sample::Index>(), byte in any::<u8>(), op in 0u8..3) {
            let mut bytes = tx.to_bytes();
            let i = at.index(bytes.len());
            match op {
                0 => bytes[i] = byte,
                1 => bytes.insert(i, byte),
                _ => { bytes.remove(i); }
            }
            if let Ok(parsed) = Tx::from_bytes(&bytes) {
                prop_assert_eq!(parsed.to_bytes(), bytes);
            }
        }
    }

    #[test]
    fn names_and_amounts() {
        assert_eq!(normalize_name("Jack.boo").unwrap(), "jack");
        assert!(normalize_name("ab").is_err());
        assert!(normalize_name("-ab").is_err());
        assert_eq!(fmt_fang(1_500_000_000), "1.5 FANG");
        assert_eq!(fmt_fang(DROPS_PER_FANG), "1 FANG");
    }
}
