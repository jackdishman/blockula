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
    /// Authorize another key on this account (owner keys only).
    AddKey { key_id: String, label: String, role: KeyRole },
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
    /// A juror's sealed judgment: hex BLAKE3 of `seance_commitment(...)`.
    CommitVotes { seance_id: u64, commitment: String },
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

    // ---- protocol v1 (MAINNET-DESIGN 3.1, tags 30–49; until a module lands they fail with
    // "not active yet") ----
    /// Stake `amount` in a validator's pool (staking.rs).
    Delegate {
        validator: String,
        #[serde(with = "u64_str")]
        amount: u64,
    },
    /// Start unbonding `shares` of a pool; `WithdrawUnbonded` pays them out after `unbonding_blocks`.
    Undelegate {
        validator: String,
        #[serde(with = "u64_str")]
        shares: u64,
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
    /// An active validator's commitment (hex BLAKE3 of its secret) to the beacon for `epoch` (beacon.rs).
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
    Vote {
        proposal_id: u64,
        aye: bool,
        #[serde(with = "u64_str")]
        amount: u64,
        conviction: u8,
    },
    /// Release the vote locks that have expired.
    Unlock {},
    /// Make this account an M-of-N multisig of its owner keys (`owners` are key ids).
    MultisigCreate { owners: Vec<String>, threshold: u8 },
    /// A multisig owner proposes an action for the account (never another multisig proposal);
    /// it runs once `threshold` owners approve.
    MultisigPropose { action: Box<Action> },
    MultisigApprove { id: u64 },
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
    /// Validators only: a matured claim's PR was reverted, by the 40-hex commit `revert_sha`.
    AttestRevert { claim_id: u64, revert_sha: String },
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
    /// Merkle proofs against `merkle_root` until `claim_until`.
    Distribution {
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
    /// The most the sender will pay. The tx is charged `min_fee(size) + tip` (see [`Tx::charge`]),
    /// which must fit; nothing above the charge is debited.
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
            Action::AddKey { key_id, label, role } => {
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
            Action::CommitVotes { seance_id, commitment } => {
                out.push(25);
                out.extend_from_slice(&seance_id.to_le_bytes());
                put_str(out, commitment);
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
            Action::Undelegate { validator, shares } => {
                out.push(31);
                put_str(out, validator);
                put_u64(out, *shares);
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
            Action::Vote { proposal_id, aye, amount, conviction } => {
                out.push(40);
                put_u64(out, *proposal_id);
                out.push(*aye as u8);
                put_u64(out, *amount);
                out.push(*conviction);
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
            Action::MultisigApprove { id } => {
                out.push(44);
                put_u64(out, *id);
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
            25 => Action::CommitVotes { seance_id: r.u64()?, commitment: r.str("commitment", HEX_32)? },
            26 => Action::RevealVotes { seance_id: r.u64()?, votes: r.str("votes", PAIRS_PER_JUROR)?, spam: r.ids("spam", MAX_SEANCE_CLAIMS)?, salt: r.str("salt", 64)? },
            27 => Action::FundPool { amount: r.u64()? },
            28 => Action::WithdrawVested {},
            29 => Action::Clawback { account: r.str("account", MAX_NAME_FIELD)?, reason: r.str("reason", MAX_REASON)? },
            30 => Action::Delegate { validator: r.str("validator", MAX_ADDRESS)?, amount: r.u64()? },
            31 => Action::Undelegate { validator: r.str("validator", MAX_ADDRESS)?, shares: r.u64()? },
            32 => Action::Redelegate { from_validator: r.str("from_validator", MAX_ADDRESS)?, to_validator: r.str("to_validator", MAX_ADDRESS)?, shares: r.u64()? },
            33 => Action::WithdrawUnbonded {},
            34 => Action::UpdateValidator { commission_bps: r.u64()?, p2p_addr: r.str("p2p_addr", MAX_ADDR)?, http_addr: r.str("http_addr", MAX_ADDR)? },
            35 => Action::Unjail { validator: r.str("validator", MAX_ADDRESS)? },
            36 => Action::ReportEquivocation { evidence: r.str("evidence", 2 * MAX_EVIDENCE_BYTES)? },
            37 => Action::BeaconCommit { epoch: r.u64()?, commitment: r.str("commitment", HEX_32)? },
            38 => Action::BeaconReveal { epoch: r.u64()?, secret: r.str("secret", HEX_32)? },
            39 => Action::Propose { kind: ProposalKind::decode(r)?, deposit: r.u64()?, title: r.str("title", MAX_PROPOSAL_TITLE)?, url: r.str("url", MAX_URL)? },
            40 => Action::Vote { proposal_id: r.u64()?, aye: r.bool("aye")?, amount: r.u64()?, conviction: r.u8()? },
            41 => Action::Unlock {},
            42 => Action::MultisigCreate { owners: r.strs("owners", crate::chain::MAX_KEYS, HEX_32)?, threshold: r.u8()? },
            43 if nested => bail!("a multisig proposal can't contain another"),
            43 => Action::MultisigPropose { action: Box::new(Action::decode_at(r, true)?) },
            44 => Action::MultisigApprove { id: r.u64()? },
            45 => Action::RenewName { name: r.str("name", MAX_NAME_FIELD)?, years: r.u8()? },
            46 => Action::TransferName { name: r.str("name", MAX_NAME_FIELD)?, to: r.str("to", MAX_NAME_FIELD)? },
            47 => Action::ReleaseName { name: r.str("name", MAX_NAME_FIELD)? },
            48 => Action::ClaimDistribution { distribution_id: r.u64()?, amount: r.u64()?, proof: r.strs("proof", MAX_PROOF, HEX_32)? },
            49 => Action::AttestRevert { claim_id: r.u64()?, revert_sha: r.str("revert_sha", 40)? },
            t => bail!("unknown action tag {t}"),
        })
    }

    /// The action's own format rules (`Tx::check_format`). `nested`: inside a `MultisigPropose`.
    fn check_format(&self, nested: bool) -> Result<()> {
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
            Action::AddKey { key_id, label, role } => {
                if !key_id_ok(key_id) {
                    bail!("key_id must be 32 bytes of lowercase hex (BLAKE3 of the public key)");
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
            Action::CommitVotes { commitment, .. } => {
                if commitment.len() != 64 || !crypto::is_lower_hex(commitment) {
                    bail!("commitment must be 32 bytes of lowercase hex");
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
            Action::Undelegate { validator, shares } => {
                check_address("validator", validator)?;
                if *shares == 0 {
                    bail!("undelegate more than 0 shares");
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
            Action::Vote { amount, conviction, .. } => {
                if *amount == 0 {
                    bail!("vote with more than 0");
                }
                if *conviction > crate::params::MAX_CONVICTION {
                    bail!("conviction must be 0–{}", crate::params::MAX_CONVICTION);
                }
            }
            Action::WithdrawUnbonded {} | Action::Unlock {} | Action::MultisigApprove { .. } => {}
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
        }
        Ok(())
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
        }
    }

    /// Fields that name an account and may hold a raw address (names resolve on this chain),
    /// including those inside a proposal or a multisig proposal.
    pub fn addresses(&self) -> Vec<(&'static str, &str)> {
        match self {
            Action::Transfer { to, .. } | Action::AwardHunt { to, .. } | Action::GrantSoul { to, .. } | Action::TransferName { to, .. } => vec![("to", to.as_str())],
            Action::ApproveValidator { validator } | Action::RemoveValidator { validator } | Action::Unbond { validator } | Action::Delegate { validator, .. } | Action::Undelegate { validator, .. } | Action::Unjail { validator } => vec![("validator", validator.as_str())],
            Action::Redelegate { from_validator, to_validator, .. } => vec![("from_validator", from_validator.as_str()), ("to_validator", to_validator.as_str())],
            Action::Vouch { account } | Action::Unvouch { account } | Action::FlagSybil { account, .. } | Action::Clawback { account, .. } => vec![("account", account.as_str())],
            Action::Propose { kind, .. } => match kind {
                ProposalKind::TreasurySpend { to, .. } | ProposalKind::GrantSoul { to, .. } => vec![("kind.to", to.as_str())],
                ProposalKind::FlagSybil { account, .. } | ProposalKind::Clawback { account, .. } => vec![("kind.account", account.as_str())],
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
            ProposalKind::Distribution { merkle_root, total, claim_until } => {
                out.push(2);
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
        }
    }

    fn decode(r: &mut Reader) -> Result<ProposalKind> {
        Ok(match r.u8()? {
            0 => ProposalKind::ParamChange { key: r.str("key", MAX_PARAM_KEY)?, value: r.u64()? },
            1 => ProposalKind::TreasurySpend { to: r.str("to", MAX_NAME_FIELD)?, amount: r.u64()? },
            2 => ProposalKind::Distribution { merkle_root: r.str("merkle_root", HEX_32)?, total: r.u64()?, claim_until: r.u64()? },
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

    /// Stateless rules (stateful ones, like a parameter's bounds, apply when it executes).
    pub fn check(&self) -> Result<()> {
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
            ProposalKind::Distribution { merkle_root, total, claim_until } => {
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
            ProposalKind::FlagSybil { account, reason } | ProposalKind::Clawback { account, reason } => {
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
            ProposalKind::ActivateFeature { feature } | ProposalKind::DeactivateFeature { feature } => crate::upgrade::check_feature_name(feature)?,
        }
        Ok(())
    }
}

impl Tx {
    /// Canonical encoding that gets signed. Mirrored exactly in sdk-ts/src/tx.ts. The genesis hash
    /// is its 32 raw bytes (malformed hex, which `check_format` refuses, as zeros).
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut out = TX_DOMAIN.to_vec();
        put_str(&mut out, &self.chain_id);
        let genesis: [u8; 32] = hex::decode(&self.genesis_hash).ok().and_then(|b| b.try_into().ok()).unwrap_or_default();
        out.extend_from_slice(&genesis);
        put_str(&mut out, &self.from);
        put_u64(&mut out, self.nonce);
        put_u64(&mut out, self.fee);
        put_u64(&mut out, self.tip);
        put_u64(&mut out, self.valid_until);
        self.action.encode(&mut out);
        put_str(&mut out, &self.memo);
        out
    }

    /// Wire form: `signing_bytes ‖ public_key (1,952 B) ‖ signature (3,309 B)`. Blocks and
    /// mempool gossip carry this; [`Tx::from_bytes`] is its strict inverse. Only txs that pass
    /// [`Tx::check_format`] round-trip (malformed hex is written as zeros).
    pub fn to_bytes(&self) -> Vec<u8> {
        let raw = |h: &str| hex::decode(h).unwrap_or_else(|_| vec![0; h.len() / 2]);
        let mut out = self.signing_bytes();
        out.extend_from_slice(&raw(&self.public_key));
        out.extend_from_slice(&raw(&self.signature));
        out
    }

    /// Parses the wire form. Strict: no trailing bytes, valid UTF-8, known tags, canonical
    /// bools and options, fields within their bounds, and the bytes must be exactly what
    /// `to_bytes` produces for the result.
    pub fn from_bytes(bytes: &[u8]) -> Result<Tx> {
        if bytes.len() > MAX_TX_BYTES {
            bail!("transaction too large ({} bytes, max {MAX_TX_BYTES})", bytes.len());
        }
        let mut r = Reader { buf: bytes };
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
        if !r.buf.is_empty() {
            bail!("{} trailing bytes after the transaction", r.buf.len());
        }
        if tx.to_bytes() != bytes {
            bail!("non-canonical transaction encoding");
        }
        Ok(tx)
    }

    /// What this tx is charged: the burned base fee for its size plus the proposer's tip
    /// (`None` on overflow). It must be at most `fee`. Until the fee market (MAINNET-DESIGN
    /// 3.6) lands, the base fee is `min_fee(size)`.
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
        let size = self.size() as usize;
        if size > MAX_TX_BYTES {
            bail!("transaction too large ({size} bytes, max {MAX_TX_BYTES})");
        }
        if self.chain_id.len() > MAX_CHAIN_ID {
            bail!("chain_id too long");
        }
        if self.genesis_hash.len() != 64 || !crypto::is_lower_hex(&self.genesis_hash) {
            bail!("genesis_hash must be the chain's genesis hash (32 bytes of lowercase hex, from /api/status)");
        }
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
        Ok(key_id(&hex::decode(&self.public_key).context("public_key hex")?))
    }

    /// Wire size in bytes (`to_bytes().len()`), which fees are priced on.
    pub fn size(&self) -> u64 {
        (self.signing_bytes().len() + self.public_key.len() / 2 + self.signature.len() / 2) as u64
    }

    pub fn verify_signature(&self) -> Result<()> {
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

/// Base fee is priced by bytes because post-quantum signatures are big. It's burned 🔥.
pub fn min_fee(tx_size: u64) -> u64 {
    10_000 + tx_size * 10
}

/// Short names are scarce, so they cost more. Paid to the treasury.
pub fn name_price(name: &str) -> u64 {
    match name.len() {
        3 => 100 * DROPS_PER_FANG,
        4 => 25 * DROPS_PER_FANG,
        _ => 5 * DROPS_PER_FANG,
    }
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
            Action::AddKey { key_id: "ab".repeat(32), label: "phone".into(), role: KeyRole::Owner },
            Action::AddKey { key_id: "cd".repeat(32), label: "claude".into(), role: KeyRole::Session { limit: 10, period_blocks: 100, expires_at: 1000 } },
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
            Action::CommitVotes { seance_id: 2, commitment: "ef".repeat(32) },
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
            Action::Undelegate { validator: "boo1v".into(), shares: 7 },
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
            propose(ProposalKind::Distribution { merkle_root: "ef".repeat(32), total: 100, claim_until: 5_000 }),
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
            Action::Vote { proposal_id: 1, aye: true, amount: 10, conviction: 6 },
            Action::Vote { proposal_id: 1, aye: false, amount: 10, conviction: 0 },
            Action::Unlock {},
            Action::MultisigCreate { owners: vec!["ab".repeat(32), "cd".repeat(32)], threshold: 2 },
            Action::MultisigPropose { action: Box::new(Action::Transfer { to: "x.boo".into(), amount: 5 }) },
            Action::MultisigApprove { id: 3 },
            Action::RenewName { name: "vlad".into(), years: 2 },
            Action::TransferName { name: "vlad".into(), to: "x.boo".into() },
            Action::ReleaseName { name: "vlad".into() },
            Action::ClaimDistribution { distribution_id: 1, amount: 50, proof: vec!["ab".repeat(32), "cd".repeat(32)] },
            Action::ClaimDistribution { distribution_id: 1, amount: 50, proof: vec![] },
            Action::AttestRevert { claim_id: 4, revert_sha: "ef".repeat(20) },
        ]
    }

    /// The vector tx around an action: fixed fields, and recognizable key and signature bytes.
    fn vector_tx(action: Action) -> Tx {
        Tx { chain_id: "vec".into(), genesis_hash: "03".repeat(32), from: "boo1from".into(), nonce: 7, fee: 12345, tip: 3, valid_until: 99, action, memo: "m".into(), public_key: "01".repeat(crypto::PUBLIC_KEY_LEN), signature: "02".repeat(crypto::SIGNATURE_LEN) }
    }

    /// The same vectors are computed by sdk-ts (test/e2e.ts): the encodings must match byte for byte.
    #[test]
    fn encodings_match_the_sdk() {
        assert_eq!(seance_commitment(3, "boo1juror", "ab=", &[1, 2], "pepper"), "1f3071e1123a713836e2c9c27798dc74ed1fd1f97de6b5fd985c0dba8f9f7eae");
        let mut h = blake3::Hasher::new();
        for action in vector_actions() {
            h.update(&vector_tx(action).signing_bytes());
        }
        assert_eq!(h.finalize().to_hex().to_string(), "3633490a583afa9a7aec72593fd29e9d5461b3a037abec251ca90c762e3b1b65");
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
        assert_eq!(h.finalize().to_hex().to_string(), "0ffbf0b5bbb1a26f7abe720bae98d7f97581d5268bfbe75fe290605f16172398");
        let p: Action = serde_json::from_str(r#"{"type":"propose","kind":{"type":"param_change","key":"max_validators","value":"40"},"deposit":"1000","title":"t"}"#).unwrap();
        assert_eq!(p, Action::Propose { kind: ProposalKind::ParamChange { key: "max_validators".into(), value: 40 }, deposit: 1000, title: "t".into(), url: String::new() });
        let m: Action = serde_json::from_str(r#"{"type":"multisig_propose","action":{"type":"vote","proposal_id":1,"aye":true,"amount":"5","conviction":2}}"#).unwrap();
        assert_eq!(m, Action::MultisigPropose { action: Box::new(Action::Vote { proposal_id: 1, aye: true, amount: 5, conviction: 2 }) });
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
    }

    #[test]
    fn every_tag_round_trips_through_the_wire_form() {
        let mut tags = std::collections::BTreeSet::new();
        let mut kinds = std::collections::BTreeSet::new();
        for action in vector_actions().into_iter().chain(v1_vector_actions()) {
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
        assert_eq!(tags, (0..=49).collect(), "the vectors cover every action tag");
        assert_eq!(kinds, (0..=13).collect(), "and every proposal kind");
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
        assert!(bad(Action::Delegate { validator: addr.clone(), amount: 0 }).contains("more than 0"));
        assert!(bad(Action::Redelegate { from_validator: addr.clone(), to_validator: addr.clone(), shares: 1 }).contains("different"));
        assert!(bad(Action::UpdateValidator { commission_bps: 10_001, p2p_addr: String::new(), http_addr: String::new() }).contains("0–10000"));
        // Evidence: at most 16 KiB decoded, as lowercase hex.
        ok(Action::ReportEquivocation { evidence: "ab".repeat(MAX_EVIDENCE_BYTES) });
        assert!(bad(Action::ReportEquivocation { evidence: "ab".repeat(MAX_EVIDENCE_BYTES + 1) }).contains("16384"));
        assert!(bad(Action::ReportEquivocation { evidence: "AB".into() }).contains("lowercase hex"));
        assert!(bad(Action::ReportEquivocation { evidence: String::new() }).contains("1–"));
        assert!(bad(Action::BeaconCommit { epoch: 1, commitment: "ab".into() }).contains("32 bytes"));
        // Votes: conviction 0–6.
        ok(Action::Vote { proposal_id: 0, aye: true, amount: 1, conviction: 6 });
        assert!(bad(Action::Vote { proposal_id: 0, aye: true, amount: 1, conviction: 7 }).contains("0–6"));
        assert!(bad(Action::Vote { proposal_id: 0, aye: true, amount: 0, conviction: 1 }).contains("more than 0"));
        // Proposals: the kind's own rules, a deposit, a title, an https link or nothing.
        ok(propose(ProposalKind::ParamChange { key: "quorum_bps".into(), value: 1 }));
        assert!(bad(propose(ProposalKind::ParamChange { key: "nope".into(), value: 1 })).contains("unknown parameter"));
        assert!(bad(propose(ProposalKind::GrantSoul { to: addr.clone(), amount: crate::poc::MAX_SOUL_GRANT + 1, reason: "r".into() })).contains("milli-Soul"));
        assert!(bad(propose(ProposalKind::Distribution { merkle_root: hex32.clone(), total: 0, claim_until: 1 })).contains("total"));
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
        assert!(bad(Action::MultisigPropose { action: Box::new(Action::MultisigApprove { id: 1 }) }).contains("can't hold another"));
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
        let role = |r: &str| serde_json::from_str::<Action>(&format!(r#"{{"type":"add_key","key_id":"ab","label":"l","role":{r}}}"#));
        assert!(role(r#"{"type":"owner","limit":"1"}"#).is_err(), "and inside the key role");
        assert!(role(r#"{"type":"session","limit":"1","period_blocks":2,"expires_at":3,"x":0}"#).is_err());
        assert_eq!(role(r#"{"type":"owner"}"#).unwrap(), Action::AddKey { key_id: "ab".into(), label: "l".into(), role: KeyRole::Owner });
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
        unknown[tag_at] = 50;
        assert!(err(&unknown).contains("unknown action tag 50"));
        // A proposal kind past the last one.
        let text = vector_tx(Action::Propose { kind: ProposalKind::Text {}, deposit: 1, title: "t".into(), url: String::new() }).to_bytes();
        let mut bad_kind = text.clone();
        assert_eq!((bad_kind[tag_at], bad_kind[tag_at + 1]), (39, 13));
        bad_kind[tag_at + 1] = 14;
        assert!(err(&bad_kind).contains("unknown proposal kind 14"));
        let mut bad_aye = vector_tx(Action::Vote { proposal_id: 1, aye: true, amount: 1, conviction: 1 }).to_bytes();
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
        let role = vector_tx(Action::AddKey { key_id: "ab".repeat(32), label: "l".into(), role: KeyRole::Owner });
        let mut bad_role = role.to_bytes();
        bad_role[role.signing_bytes().len() - 2 - 1 - 1] = 2;
        assert!(err(&bad_role).contains("unknown key role"));
        assert!(Tx::from_bytes(&vec![0u8; MAX_TX_BYTES + 1]).unwrap_err().to_string().contains("too large"));
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
            2 => ProposalKind::Distribution { merkle_root: a, total: w, claim_until: x },
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
        (0u8..50, strs, nums, any::<bool>(), opt, ids, list, 0u8..14, any::<u8>()).prop_map(|(tag, [a, b, c, d], [w, x, y], ok, familiar_key, spam, list, kind, small)| match tag {
            0 => Action::Transfer { to: a, amount: w },
            1 => Action::RegisterName { name: a },
            2 => Action::AddKey { key_id: a, label: b, role: if ok { KeyRole::Owner } else { KeyRole::Session { limit: w, period_blocks: x, expires_at: y } } },
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
            25 => Action::CommitVotes { seance_id: w, commitment: a },
            26 => Action::RevealVotes { seance_id: w, votes: a, spam, salt: b },
            27 => Action::FundPool { amount: w },
            28 => Action::WithdrawVested {},
            29 => Action::Clawback { account: a, reason: b },
            30 => Action::Delegate { validator: a, amount: w },
            31 => Action::Undelegate { validator: a, shares: w },
            32 => Action::Redelegate { from_validator: a, to_validator: b, shares: w },
            33 => Action::WithdrawUnbonded {},
            34 => Action::UpdateValidator { commission_bps: w, p2p_addr: a, http_addr: b },
            35 => Action::Unjail { validator: a },
            36 => Action::ReportEquivocation { evidence: a },
            37 => Action::BeaconCommit { epoch: w, commitment: a },
            38 => Action::BeaconReveal { epoch: w, secret: a },
            39 => Action::Propose { kind: proposal_kind(kind, [c, d], [x, y]), deposit: w, title: a, url: b },
            40 => Action::Vote { proposal_id: w, aye: ok, amount: x, conviction: small },
            41 => Action::Unlock {},
            42 => Action::MultisigCreate { owners: list, threshold: small },
            43 => Action::MultisigPropose { action: Box::new(if ok { Action::Transfer { to: a, amount: w } } else { Action::Vote { proposal_id: w, aye: small % 2 == 0, amount: x, conviction: small } }) },
            44 => Action::MultisigApprove { id: w },
            45 => Action::RenewName { name: a, years: small },
            46 => Action::TransferName { name: a, to: b },
            47 => Action::ReleaseName { name: a },
            48 => Action::ClaimDistribution { distribution_id: w, amount: x, proof: list },
            49 => Action::AttestRevert { claim_id: w, revert_sha: a },
            _ => unreachable!(),
        })
    }

    fn arb_tx() -> impl Strategy<Value = Tx> {
        (arb_str(16), arb_hex(32), arb_str(16), proptest::array::uniform4(any::<u64>()), arb_action(), arb_str(64), arb_hex(crypto::PUBLIC_KEY_LEN), arb_hex(crypto::SIGNATURE_LEN)).prop_map(
            |(chain_id, genesis_hash, from, [nonce, fee, tip, valid_until], action, memo, public_key, signature)| Tx { chain_id, genesis_hash, from, nonce, fee, tip, valid_until, action, memo, public_key, signature },
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
