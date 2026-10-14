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
        }
    }

    /// Strict inverse of [`Action::encode`]: unknown tags, non-0/1 bools and over-long fields
    /// are errors.
    fn decode(r: &mut Reader) -> Result<Action> {
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
            t => bail!("unknown action tag {t}"),
        })
    }

    /// Fields that name an account and may hold a raw address (names resolve on this chain).
    pub fn addresses(&self) -> Vec<(&'static str, &str)> {
        match self {
            Action::Transfer { to, .. } | Action::AwardHunt { to, .. } | Action::GrantSoul { to, .. } => vec![("to", to.as_str())],
            Action::ApproveValidator { validator } | Action::RemoveValidator { validator } | Action::Unbond { validator } => vec![("validator", validator.as_str())],
            Action::Vouch { account } | Action::Unvouch { account } | Action::FlagSybil { account, .. } | Action::Clawback { account, .. } => vec![("account", account.as_str())],
            _ => vec![],
        }
    }
}

impl Tx {
    /// Canonical encoding that gets signed. Mirrored exactly in sdk-ts/src/tx.ts.
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut out = TX_DOMAIN.to_vec();
        put_str(&mut out, &self.chain_id);
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
        let key_id_ok = |k: &str| k.len() == 64 && crypto::is_lower_hex(k);
        let addr = |field: &str, v: &str| -> Result<()> {
            if v.len() > MAX_ADDR {
                bail!("{field} too long (max {MAX_ADDR} bytes)");
            }
            crate::chain::check_host_port(v).with_context(|| field.to_string())
        };
        match &self.action {
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
        }
        // Whatever the wire form can't carry exactly (e.g. `familiar_key: Some("")`) is invalid.
        match Tx::from_bytes(&self.to_bytes()) {
            Ok(back) if back == *self => Ok(()),
            Ok(_) => bail!("transaction doesn't survive the wire encoding (an optional field is empty?)"),
            Err(e) => Err(e.context("transaction doesn't survive the wire encoding")),
        }
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
    /// Protocol version the block was made under (`consensus::PROTOCOL_VERSION`).
    #[serde(default)]
    pub version: u32,
    pub proposer: String,
    pub state_root: String,
    pub txs: Vec<Tx>,
    pub hash: String,
    /// Kept for compatibility; certificates live in `certs.bin` now (see chain.rs).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub signature: String,
    /// Txs (by index into `txs`) that were included and charged their fee, but whose action
    /// failed at execution (e.g. the balance was spent by an earlier tx in the block).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub failed: std::collections::BTreeMap<usize, String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// One action per tag (0–29), in tag order. Mirrored by `vectorActions` in sdk-ts/test/e2e.ts.
    fn vector_actions() -> Vec<Action> {
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

    /// The vector tx around an action: fixed fields, and recognizable key and signature bytes.
    fn vector_tx(action: Action) -> Tx {
        Tx { chain_id: "vec".into(), from: "boo1from".into(), nonce: 7, fee: 12345, tip: 3, valid_until: 99, action, memo: "m".into(), public_key: "01".repeat(crypto::PUBLIC_KEY_LEN), signature: "02".repeat(crypto::SIGNATURE_LEN) }
    }

    /// The same vectors are computed by sdk-ts (test/e2e.ts): the encodings must match byte for byte.
    #[test]
    fn encodings_match_the_sdk() {
        assert_eq!(seance_commitment(3, "boo1juror", "ab=", &[1, 2], "pepper"), "1f3071e1123a713836e2c9c27798dc74ed1fd1f97de6b5fd985c0dba8f9f7eae");
        let mut h = blake3::Hasher::new();
        for action in vector_actions() {
            h.update(&vector_tx(action).signing_bytes());
        }
        assert_eq!(h.finalize().to_hex().to_string(), "67416fa0f99281aa382895815bea59d2a43b40f6865c500e4cb80611d8754875");
        // Wire form, size and id of one tx.
        let tx = vector_tx(Action::Transfer { to: "x.boo".into(), amount: 5 });
        assert_eq!(hex::encode(crypto::hash(&tx.to_bytes())), "f32398f98b16d95d4f68af7ee29524a274d5465476390d91b357633627712e39");
        assert_eq!(tx.size(), tx.to_bytes().len() as u64);
        assert_eq!(tx.id(), "0a787dbaf66a3f5a3df1609272c68f5229397c418010ecd8453b1e7c7ff6617d");
        // And the JSON shapes the SDK sends parse.
        let a: Action = serde_json::from_str(r#"{"type":"leave_jury"}"#).unwrap();
        assert!(matches!(a, Action::LeaveJury {}));
        let a: Action = serde_json::from_str(r#"{"type":"join_jury","bond":"10"}"#).unwrap();
        assert!(matches!(a, Action::JoinJury { bond: 10 }));
        let a: Action = serde_json::from_str(r#"{"type":"attest_claim","claim_id":1,"ok":false,"merge_sha":"","note":"x"}"#).unwrap();
        assert!(matches!(a, Action::AttestClaim { ok: false, .. }));
        let t: Tx = serde_json::from_value(serde_json::json!({ "chain_id": "c", "from": "boo1from", "nonce": 1, "fee": "100", "tip": "2", "valid_until": 9, "action": { "type": "transfer", "to": "x.boo", "amount": "5" }, "memo": "", "public_key": "", "signature": "" })).unwrap();
        assert_eq!((t.fee, t.tip, t.valid_until), (100, 2, 9));
    }

    #[test]
    fn every_tag_round_trips_through_the_wire_form() {
        let mut tags = std::collections::BTreeSet::new();
        for action in vector_actions() {
            let mut enc = vec![];
            action.encode(&mut enc);
            tags.insert(enc[0]);
            let tx = vector_tx(action);
            assert_eq!(Tx::from_bytes(&tx.to_bytes()).unwrap(), tx);
        }
        assert_eq!(tags, (0..=29).collect(), "the vectors cover every action tag");
    }

    #[test]
    fn json_rejects_unknown_fields() {
        let ok = serde_json::json!({ "chain_id": "c", "from": "boo1from", "nonce": 1, "fee": "100", "valid_until": 9, "action": { "type": "transfer", "to": "x.boo", "amount": "5" }, "public_key": "", "signature": "" });
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
        // Field offsets: domain, chain_id, from, nonce, fee, tip, valid_until, then the tag.
        let tag_at = TX_DOMAIN.len() + 2 + 3 + 2 + 8 + 4 * 8;
        assert_eq!(good[tag_at], 21);
        let mut unknown = good.clone();
        unknown[tag_at] = 30;
        assert!(err(&unknown).contains("unknown action tag 30"));
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
        let base = Tx { chain_id: "c".into(), from: keys.address(), nonce: 0, fee: 1, tip: 0, valid_until: 9, action: Action::SubmitClaim { repo: "a/b".into(), pr: 1, familiar_key: None }, memo: String::new(), public_key: String::new(), signature: String::new() }.sign(&keys);
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

    #[test]
    fn prefixes_are_checked_against_the_chain() {
        let k = crypto::Keys::from_seed([4; 32]);
        let tx = |from: String, to: String| Tx { chain_id: "c".into(), from, nonce: 0, fee: 0, tip: 0, valid_until: 0, action: Action::Transfer { to, amount: 1 }, memo: String::new(), public_key: String::new(), signature: String::new() };
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

    /// Any action, by tag, from a bag of random fields (strings of up to 6 chars fit every bound).
    fn arb_action() -> impl Strategy<Value = Action> {
        let strs = proptest::array::uniform4(arb_str(6));
        let nums = proptest::array::uniform3(any::<u64>());
        let opt = proptest::option::of(arb_str(6).prop_filter("Some is never empty", |s| !s.is_empty()));
        let ids = proptest::collection::vec(any::<u64>(), 0..4);
        (0u8..30, strs, nums, any::<bool>(), opt, ids).prop_map(|(tag, [a, b, c, d], [w, x, y], ok, familiar_key, spam)| match tag {
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
            _ => unreachable!(),
        })
    }

    fn arb_tx() -> impl Strategy<Value = Tx> {
        (arb_str(16), arb_str(16), proptest::array::uniform4(any::<u64>()), arb_action(), arb_str(64), arb_hex(crypto::PUBLIC_KEY_LEN), arb_hex(crypto::SIGNATURE_LEN)).prop_map(
            |(chain_id, from, [nonce, fee, tip, valid_until], action, memo, public_key, signature)| Tx { chain_id, from, nonce, fee, tip, valid_until, action, memo, public_key, signature },
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
