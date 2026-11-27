//! `blockula ghost …` (docs/GHOST-MODE.md section 8, "The CLI"): the first Ghost Mode client. It
//! shields FANG into the pool, sends it privately, unshields it, and scans the chain for the
//! wallet's notes. **Ghost Mode is not audited and ships disabled**: these commands work on a chain
//! that has the `ghost` feature on (`blockula dev --ghost`), and say so in a sentence on any other.
//!
//! | Command | Does |
//! |---|---|
//! | `address [--publish]` | the wallet's Ghost Mode address fingerprint; `--publish` signs `SetGhostAddress` (tag 53), so others can send to the account's name or address |
//! | `scan`, `balance` | pages `/api/ghost/commitments`, rebuilds the commitment tree, finds the wallet's notes by trial decryption, marks the spent ones by their nullifiers, and keeps all of it in the note store |
//! | `shield <amount>` | a signed `Shield` (tag 50) from the wallet's account into a note of its own |
//! | `send <name\|address> <amount> [--memo]` | a `PrivateTransfer` (tag 51) to the recipient's published address |
//! | `unshield <to> <amount>` | an `Unshield` (tag 52) paying a public account |
//! | `export-viewing-key --full\|--incoming\|--outgoing` | a viewing key, as hex |
//!
//! **Keys.** The ghost key comes from the wallet's own seed ([`ghost_key`]); nothing else is stored.
//!
//! **The note store** (`<blockula dir>/ghost/<instance hex>.json`, mode 600, [`Notes`]) holds every
//! leaf of the commitment tree and the wallet's notes with their secrets. It is a cache: deleting it
//! loses nothing, the next `scan` rebuilds it from the chain.
//!
//! **What a node is trusted for.** Its leaves must hash to its own anchor, a note counts only if
//! its plaintext opens the commitment on chain (the crate checks), and a proof made against a wrong
//! anchor or instance is refused by every honest node, so a lying node can waste a fee's worth of
//! work but can't make the wallet spend what it doesn't mean to. Two things are the node's word and
//! are printed for the user: the recipient's address (compare its fingerprint with them, or pass
//! `--fingerprint`) and the canonical fee (refused above `--max-fee`).
//!
//! **One action spends two notes.** A payment that needs more is made as several actions: private
//! transfers to the wallet itself that merge notes two at a time, each paying the fee, then the
//! payment. The CLI says so before it starts.
//!
//! Nothing here prints or logs a seed, a spending key or a note's secrets: key and note types print
//! as `Type(..)`, and the types of this module that hold them have no `Debug` at all.

use crate::cli_net::{account_ref, ChainPin, Remote};
use crate::contrib_cli::confirm;
use crate::crypto::Keys;
use crate::types::{fmt_fang, Action, GhostBundle, HexBytes, Tx, MIN_NEW_ACCOUNT};
use crate::validator_cli::{fee_ceiling, get, load_seed, parse_fang, rt, send_memo, unsigned_tx, wait_final, wallet_keys};
use anyhow::{anyhow, bail, ensure, Context, Result};
use blockula_ghost::encryption::MEMO_BYTES;
use blockula_ghost::field::{digest_from_bytes, digest_to_bytes, Digest};
use blockula_ghost::{dummy_output, encrypt_note, nullifier, ActionWitness, Address, EncryptedNote, FullTree, Note, SpendWitness, SpendingKey, TrialDecryptor, CIRCUIT_ID, MAX_PROOF_BYTES};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Which wallet, and which node.
#[derive(Args, Clone)]
pub struct Who {
    /// File holding the wallet's 32-byte hex seed. Default: the CLI wallet
    /// (~/.blockula/wallet.json, see `blockula wallet new`)
    #[arg(long)]
    pub key: Option<PathBuf>,
    #[arg(long, default_value = "http://localhost:7070")]
    pub rpc: String,
    /// The chain the node at --rpc must be on
    #[command(flatten)]
    pub pin: ChainPin,
}

/// Which viewing key to export: exactly one.
#[derive(Args, Clone)]
#[group(required = true, multiple = false)]
pub struct WhichKey {
    /// Everything the wallet received and sent, and which notes are spent (161 bytes). It can't spend
    #[arg(long)]
    pub full: bool,
    /// Incoming notes only (97 bytes)
    #[arg(long)]
    pub incoming: bool,
    /// Notes this wallet sent (32 bytes)
    #[arg(long)]
    pub outgoing: bool,
}

#[derive(Subcommand)]
pub enum GhostCmd {
    /// Show the wallet's Ghost Mode address fingerprint (what people compare), and whether it's
    /// published. --publish files the address under your account (tag 53), so others can send to
    /// your name
    Address {
        /// Sign and send SetGhostAddress from the wallet's account
        #[arg(long)]
        publish: bool,
        /// With --publish: act for this account (name or address) if the key was added to another one
        #[arg(long, requires = "publish")]
        from: Option<String>,
        #[command(flatten)]
        who: Who,
    },
    /// Read the chain's new shielded outputs, find this wallet's notes and mark the spent ones
    Scan {
        #[command(flatten)]
        who: Who,
    },
    /// Scan, then show the wallet's shielded balance and its unspent notes
    Balance {
        #[command(flatten)]
        who: Who,
    },
    /// Move FANG from the wallet's account into Ghost Mode (tag 50): a note of your own
    Shield {
        /// Amount in FANG, e.g. 1.5
        amount: String,
        /// Shield from this account (name or address) if the key was added to another one
        #[arg(long)]
        from: Option<String>,
        #[command(flatten)]
        who: Who,
    },
    /// Send shielded FANG privately (tag 51) to an account's published Ghost Mode address
    Send {
        /// The recipient's .boo name or address (they publish with `blockula ghost address --publish`)
        to: String,
        /// Amount in FANG, e.g. 1.5
        amount: String,
        /// A note for the recipient (up to 256 bytes, encrypted with the payment)
        #[arg(long, default_value = "")]
        memo: String,
        /// Refuse unless the recipient's address has this fingerprint (as they read it to you)
        #[arg(long)]
        fingerprint: Option<String>,
        /// The most one action may pay in fees, in FANG (the node quotes the fee)
        #[arg(long, default_value = "1")]
        max_fee: String,
        /// Don't ask for confirmation of the recipient's fingerprint
        #[arg(long)]
        yes: bool,
        #[command(flatten)]
        who: Who,
    },
    /// Move shielded FANG out of Ghost Mode (tag 52) to a public account: the amount and the
    /// recipient are public
    Unshield {
        /// The account to pay: a .boo name or an address
        to: String,
        /// Amount in FANG, e.g. 1.5
        amount: String,
        /// The most one action may pay in fees, in FANG (the node quotes the fee)
        #[arg(long, default_value = "1")]
        max_fee: String,
        #[command(flatten)]
        who: Who,
    },
    /// Print one of the wallet's viewing keys as hex. Whoever holds it sees what it shows, for good
    ExportViewingKey {
        #[command(flatten)]
        which: WhichKey,
        /// File holding the wallet's 32-byte hex seed. Default: the CLI wallet
        #[arg(long)]
        key: Option<PathBuf>,
    },
}

pub fn ghost(cmd: GhostCmd, dir: &Path) -> Result<()> {
    match cmd {
        GhostCmd::Address { publish, from, who } => address(dir, &who, publish, from.as_deref()),
        GhostCmd::Scan { who } => {
            let s = scan(dir, &who)?;
            println!("👻 scanned {} new output{} through block #{} ({} in the tree)", s.new_leaves, plural(s.new_leaves), s.height, s.leaves);
            println!("   found {} new note{} of yours, and {} of your notes spent since the last scan", s.found, plural(s.found), s.spent);
            println!("   shielded balance: {} in {} unspent note{}", fmt_fang(s.balance), s.unspent.len(), plural(s.unspent.len() as u64));
            Ok(())
        }
        GhostCmd::Balance { who } => {
            let s = scan(dir, &who)?;
            println!("👻 {} shielded, in {} unspent note{} (as of block #{})", fmt_fang(s.balance), s.unspent.len(), plural(s.unspent.len() as u64), s.height);
            for n in &s.unspent {
                // A memo is the sender's text: shown escaped, as data.
                let memo = if n.memo.is_empty() { String::new() } else { format!("  memo (written by the sender) {:?}", n.memo) };
                println!("   {:>18}  leaf {} · block #{}{memo}", fmt_fang(n.value), n.position, n.height);
            }
            Ok(())
        }
        GhostCmd::Shield { amount, from, who } => {
            let sent = shield(dir, &who, parse_fang(&amount)?, from.as_deref())?;
            print_sent(&who.rpc, std::slice::from_ref(&sent));
            Ok(())
        }
        GhostCmd::Send { to, amount, memo, fingerprint, max_fee, yes, who } => {
            let sent = send(dir, &who, &to, parse_fang(&amount)?, &memo, fingerprint.as_deref(), parse_fang(&max_fee).context("--max-fee")?, yes)?;
            print_sent(&who.rpc, &sent);
            Ok(())
        }
        GhostCmd::Unshield { to, amount, max_fee, who } => {
            let sent = unshield(dir, &who, &to, parse_fang(&amount)?, parse_fang(&max_fee).context("--max-fee")?)?;
            print_sent(&who.rpc, &sent);
            Ok(())
        }
        GhostCmd::ExportViewingKey { which, key } => {
            let fvk = ghost_key(&keys_of(dir, key.as_deref())?).full_viewing_key();
            let (name, sees, hex) = if which.full {
                ("full", "every note this wallet received and sent, and which are spent (it can't spend them)", hex::encode(fvk.to_bytes()))
            } else if which.incoming {
                ("incoming", "every note this wallet receives", hex::encode(fvk.incoming().to_bytes()))
            } else {
                ("outgoing", "every note this wallet sent", hex::encode(fvk.outgoing().0))
            };
            eprintln!("👻 The {name} viewing key follows. Whoever holds it sees {sees}, for as long as this wallet is used: it can't be revoked. Give it only to someone who should.");
            println!("{hex}");
            Ok(())
        }
    }
}

fn plural(n: u64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn print_sent(rpc: &str, sent: &[Sent]) {
    for s in sent {
        let block = s.height.map(|h| format!(", final in block #{h}")).unwrap_or_default();
        println!("✓ {} (tx {}{block})", s.what, s.id);
        println!("   proved in {} ms · {} bytes on the wire · fee {}", s.prove_ms, s.bytes, fmt_fang(s.fee));
        println!("   {}/#/tx/{}", rpc.trim_end_matches('/'), s.id);
    }
}

// --------------------------------------------------------------------------------------- keys

fn keys_of(dir: &Path, key: Option<&Path>) -> Result<Keys> {
    match key {
        Some(p) => load_seed(p),
        None => wallet_keys(dir),
    }
}

/// The wallet's Ghost Mode spending key, derived from the CLI wallet's seed: no second secret to
/// back up.
///
/// A CLI wallet is one 32-byte seed (`wallet.json`'s `seed`, or a `--key` file; `validator_cli`'s
/// `wallet_keys` and `load_seed`). The account signs with the ML-DSA-65 key that seed expands to
/// (`crypto::Keys::from_seed`, FIPS 204 key generation, which runs the seed through SHAKE-256).
/// The same seed is the *master seed* of the wallet's ghost keys:
///
/// ```text
/// ghost seed = SpendingKey::from_master(seed, 0)
///            = BLAKE3-KDF("blockula ghost v1 account", seed ‖ 0u32 little-endian)
/// ```
///
/// and from the ghost seed the crate derives the spending key `sk`, the ML-KEM-768 seed and the
/// outgoing viewing key, each under its own BLAKE3 context string (docs/GHOST-MODE.md section 1).
/// BLAKE3 in key-derivation mode under a context nothing else uses is domain-separated from ML-DSA's
/// SHAKE expansion of the same seed, so neither key tells anything about the other, and a viewing key
/// (which holds no part of the seed) can't lead back to the signing key.
///
/// Account 0 is the only one the CLI uses. ML-KEM keys can't be diversified, so a second,
/// unlinkable address would be account 1 of the same seed, not another address of this one.
///
/// The seed is read where `Keys` holds it (`Keys::with_seed`): no copy of it, as bytes or as a hex
/// string, is left on the stack or the heap for this.
pub(crate) fn ghost_key(keys: &Keys) -> SpendingKey {
    keys.with_seed(|seed| SpendingKey::from_master(seed, 0))
}

/// OS randomness for note seeds, encapsulation and dummies (the prover draws its own masks).
struct OsRng;

impl rand_core::TryRng for OsRng {
    type Error = std::convert::Infallible;
    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        let mut b = [0; 4];
        self.try_fill_bytes(&mut b)?;
        Ok(u32::from_le_bytes(b))
    }
    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        let mut b = [0; 8];
        self.try_fill_bytes(&mut b)?;
        Ok(u64::from_le_bytes(b))
    }
    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
        getrandom::fill(dst).expect("os randomness");
        Ok(())
    }
}

impl rand_core::TryCryptoRng for OsRng {}

fn random32() -> [u8; 32] {
    let mut b = [0; 32];
    getrandom::fill(&mut b).expect("os randomness");
    b
}

// ----------------------------------------------------------------------------- the note store

const STORE_VERSION: u32 = 1;

/// The note store as it's written: `<blockula dir>/ghost/<instance hex>.json`, mode 600.
#[derive(Serialize, Deserialize)]
struct StoreFile {
    version: u32,
    chain_id: String,
    /// The chain instance (`/api/ghost`), hex: the store is for this chain only.
    instance: String,
    /// The owner digest of the ghost key the notes belong to (public: it's in the address).
    owner: String,
    /// Every leaf of the commitment tree scanned so far, in order: 32 bytes each, as one hex string.
    leaves: String,
    notes: Vec<StoredNote>,
}

/// One of the wallet's notes on disk. `rseed` is the note's secret.
#[derive(Serialize, Deserialize)]
struct StoredNote {
    position: u32,
    /// Drops, as a string.
    value: String,
    rseed: String,
    /// The memo's bytes without their zero padding, hex.
    memo: String,
    height: u64,
    tx: String,
    /// The block its nullifier appeared in, once spent.
    spent_at: Option<u64>,
}

/// One of the wallet's notes. No `Debug`: it holds the note's secret.
struct OwnNote {
    position: u32,
    note: Note,
    memo: Vec<u8>,
    height: u64,
    tx: String,
    spent_at: Option<u64>,
}

/// The wallet's view of the pool on one chain: the commitment tree (a `FullTree`, for
/// authentication paths) and its notes. No `Debug`: it holds note secrets.
struct Notes {
    path: PathBuf,
    chain_id: String,
    instance: [u8; 32],
    owner: Digest,
    tree: FullTree,
    /// The tree's leaves as bytes, as the store keeps them.
    leaves: Vec<u8>,
    notes: Vec<OwnNote>,
}

/// Where a wallet's notes for the chain `instance` live. The CLI wallet's are at
/// `ghost/<instance hex>.json`; a `--key` file's get the first bytes of its (public) owner digest
/// in the name too, so two keys used from one directory don't share a store.
fn store_path(dir: &Path, instance: &[u8; 32], own_wallet: bool, owner: &Digest) -> PathBuf {
    let name = if own_wallet { format!("{}.json", hex::encode(instance)) } else { format!("{}.{}.json", hex::encode(instance), hex::encode(&digest_to_bytes(owner)[..8])) };
    dir.join("ghost").join(name)
}

fn hex32(s: &str) -> Option<[u8; 32]> {
    hex::decode(s).ok()?.try_into().ok()
}

fn digest_hex(s: &str) -> Option<Digest> {
    digest_from_bytes(&hex32(s)?)
}

impl Notes {
    /// The store at `path`, or an empty one. A store that isn't this chain's and this key's, or
    /// whose notes don't open the leaves it holds, is refused in a sentence: it's a cache, so the
    /// fix is always to delete it and scan again.
    fn load(path: PathBuf, chain_id: &str, instance: [u8; 32], key: &SpendingKey) -> Result<Notes> {
        let owner = key.address().owner();
        let mut notes = Notes { path, chain_id: chain_id.to_string(), instance, owner, tree: FullTree::new(), leaves: vec![], notes: vec![] };
        let text = match std::fs::read_to_string(&notes.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(notes),
            Err(e) => return Err(e).with_context(|| format!("reading the note store {}", notes.path.display())),
        };
        let redo = format!("Delete {} and run `blockula ghost scan` again: the store is rebuilt from the chain.", notes.path.display());
        let f: StoreFile = serde_json::from_str(&text).map_err(|_| anyhow!("The note store isn't readable. {redo}"))?;
        ensure!(f.version == STORE_VERSION, "The note store was written by another version of blockula (format {}). {redo}", f.version);
        ensure!(hex32(&f.instance) == Some(instance), "The note store is for another chain. {redo}");
        ensure!(digest_hex(&f.owner) == Some(owner), "The note store belongs to another wallet key. {redo}");
        let leaves = hex::decode(&f.leaves).ok().filter(|l| l.len().is_multiple_of(32)).ok_or_else(|| anyhow!("The note store's leaves are damaged. {redo}"))?;
        for leaf in leaves.chunks(32) {
            let cm = digest_from_bytes(leaf.try_into().expect("32 bytes")).ok_or_else(|| anyhow!("The note store's leaves are damaged. {redo}"))?;
            notes.tree.append(cm).map_err(|_| anyhow!("The note store holds more leaves than the tree can. {redo}"))?;
        }
        notes.leaves = leaves;
        for n in f.notes {
            let note = (|| Some(Note { value: n.value.parse().ok()?, owner, rseed: hex32(&n.rseed)? }))().ok_or_else(|| anyhow!("A note in the note store is damaged. {redo}"))?;
            // A note counts only if it opens the leaf at its position.
            ensure!(notes.tree.leaf(n.position) == Some(note.commitment()), "A note in the note store doesn't match the tree. {redo}");
            let memo = hex::decode(&n.memo).map_err(|_| anyhow!("A note in the note store is damaged. {redo}"))?;
            notes.notes.push(OwnNote { position: n.position, note, memo, height: n.height, tx: n.tx, spent_at: n.spent_at });
        }
        Ok(notes)
    }

    /// Writes the store, mode 600, replacing the old one in one rename.
    fn save(&self) -> Result<()> {
        let dir = self.path.parent().ok_or_else(|| anyhow!("the note store has no directory"))?;
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).ok();
        }
        let f = StoreFile {
            version: STORE_VERSION,
            chain_id: self.chain_id.clone(),
            instance: hex::encode(self.instance),
            owner: hex::encode(digest_to_bytes(&self.owner)),
            leaves: hex::encode(&self.leaves),
            notes: self.notes.iter().map(|n| StoredNote { position: n.position, value: n.note.value.to_string(), rseed: hex::encode(n.note.rseed), memo: hex::encode(&n.memo), height: n.height, tx: n.tx.clone(), spent_at: n.spent_at }).collect(),
        };
        let tmp = self.path.with_extension("json.tmp");
        std::fs::remove_file(&tmp).ok();
        // `write_secret` creates the file itself, mode 600, so the secrets are never readable by others.
        crate::chain::write_secret(&tmp, &serde_json::to_string(&f)?)?;
        std::fs::rename(&tmp, &self.path).with_context(|| format!("writing the note store {}", self.path.display()))
    }

    fn unspent(&self) -> impl Iterator<Item = (usize, &OwnNote)> {
        self.notes.iter().enumerate().filter(|(_, n)| n.spent_at.is_none())
    }

    fn balance(&self) -> u64 {
        self.unspent().fold(0u64, |sum, (_, n)| sum.saturating_add(n.note.value))
    }

    /// The unspent notes worth spending, largest first.
    fn spendable(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self.unspent().filter(|(_, n)| n.note.value > 0).map(|(i, _)| i).collect();
        v.sort_by(|a, b| self.notes[*b].note.value.cmp(&self.notes[*a].note.value).then(self.notes[*a].position.cmp(&self.notes[*b].position)));
        v
    }
}

// ----------------------------------------------------------------------------- what the node says

/// `/api/ghost`, as far as a wallet needs it. Every field is checked before it's used.
struct Pool {
    active: bool,
    paused: bool,
    instance: [u8; 32],
    root: [u8; 32],
    size: u64,
    /// The canonical fee of an unsigned ghost tx, and of an unshield that opens its recipient's account.
    fee: u64,
    fee_new_account: u64,
    verify_fee: u64,
}

async fn pool(remote: &Remote) -> Result<Pool> {
    let rpc = &remote.rpc;
    let g = get(rpc, "/ghost").await.with_context(|| format!("asking {rpc} about Ghost Mode"))?;
    let drops = |k: &str| g[k].as_str().and_then(|v| v.parse::<u64>().ok()).ok_or_else(|| anyhow!("{rpc}/api/ghost has no {k}"));
    let bytes = |k: &str| g[k].as_str().and_then(hex32).ok_or_else(|| anyhow!("{rpc}/api/ghost has no {k}"));
    let circuit = g["circuit_id"].as_u64().ok_or_else(|| anyhow!("{rpc}/api/ghost has no circuit_id"))?;
    ensure!(circuit == u64::from(CIRCUIT_ID), "{rpc} verifies Ghost Mode circuit {circuit}, and this release proves circuit {CIRCUIT_ID}: update blockula");
    let (fee, fee_new_account, grid) = (drops("canonical_fee")?, drops("canonical_fee_new_account")?, drops("fee_grid")?);
    // The fee rule (section 8, "Fees"): whole multiples of the grid, or every node refuses the tx.
    ensure!(grid > 0 && fee.is_multiple_of(grid) && fee_new_account.is_multiple_of(grid) && fee_new_account >= fee, "{rpc} quotes ghost fees that aren't on its own fee grid. Nothing was signed: use another node");
    Ok(Pool {
        active: g["active"] == true,
        paused: g["paused"] == true,
        instance: bytes("instance")?,
        root: bytes("root")?,
        size: g["frontier_size"].as_u64().ok_or_else(|| anyhow!("{rpc}/api/ghost has no frontier_size"))?,
        fee,
        fee_new_account,
        verify_fee: drops("verify_fee")?,
    })
}

impl Pool {
    /// Ghost Mode is on (for reading).
    fn on(&self, rpc: &str) -> Result<()> {
        ensure!(self.active, "Ghost Mode is off on the chain at {rpc}. It ships disabled and isn't audited: a local test chain made with `blockula dev --ghost` has it on.");
        Ok(())
    }

    /// Ghost Mode takes transactions.
    fn open(&self, rpc: &str) -> Result<()> {
        self.on(rpc)?;
        ensure!(!self.paused, "Ghost Mode is paused on the chain at {rpc}: its validators stopped the pool, and only governance resumes it. Nothing was sent.");
        Ok(())
    }
}

/// A wallet on a chain: the node, the account's keys, the ghost key and where its notes live.
struct Session {
    remote: Remote,
    keys: Keys,
    key: SpendingKey,
    dir: PathBuf,
    own_wallet: bool,
}

impl Session {
    async fn connect(dir: &Path, who: &Who) -> Result<Session> {
        let keys = keys_of(dir, who.key.as_deref())?;
        let key = ghost_key(&keys);
        let remote = Remote::connect(dir, &who.rpc, &who.pin).await?;
        Ok(Session { remote, keys, key, dir: dir.to_path_buf(), own_wallet: who.key.is_none() })
    }

    fn notes(&self, pool: &Pool) -> Result<Notes> {
        let path = store_path(&self.dir, &pool.instance, self.own_wallet, &self.key.address().owner());
        Notes::load(path, &self.remote.chain_id, pool.instance, &self.key)
    }
}

// --------------------------------------------------------------------------------------- scan

/// What a scan found, for `scan` and `balance` to print (no secrets).
#[derive(Debug)]
pub(crate) struct Scanned {
    pub new_leaves: u64,
    pub found: u64,
    pub spent: u64,
    pub leaves: u64,
    pub height: u64,
    pub balance: u64,
    pub unspent: Vec<NoteLine>,
}

/// An unspent note as `balance` lists it. The memo is the sender's text: data, shown escaped.
#[derive(Debug)]
pub(crate) struct NoteLine {
    pub value: u64,
    pub position: u32,
    pub height: u64,
    pub memo: String,
}

/// One page of `/api/ghost/commitments`, with the refusals a wallet can act on in plain words.
async fn commitments_page(rpc: &str, from: u64) -> Result<Value> {
    let url = format!("{}/api/ghost/commitments?from={from}&limit=256", rpc.trim_end_matches('/'));
    let res = reqwest::get(&url).await.with_context(|| format!("asking {rpc} for the pool's outputs"))?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or(Value::Null);
    if status == reqwest::StatusCode::NOT_FOUND && body["error"].is_null() {
        bail!("{rpc} doesn't serve /api/ghost/commitments, which wallets sync from: it runs an older blockula. Use a node on this release.");
    }
    if status == reqwest::StatusCode::GONE {
        bail!("{rpc} no longer holds the blocks this scan needs ({}). Scan from an archive node (`blockula run --archive`), or a node that still holds them.", body["error"].as_str().unwrap_or("pruned"));
    }
    ensure!(status.is_success(), "{rpc} couldn't list the pool's outputs: {}", body["error"].as_str().unwrap_or("request failed"));
    Ok(body)
}

/// Brings `notes` up to the node's tree: appends every new leaf, keeps the outputs that decrypt
/// to this wallet's key, and marks its notes spent where their nullifier shows up. Returns (new
/// leaves, notes found, notes spent, the height it's as of). Nothing is saved unless the leaves
/// hash to the node's anchor.
async fn sync(remote: &Remote, key: &SpendingKey, notes: &mut Notes) -> Result<(u64, u64, u64, u64)> {
    let rpc = &remote.rpc;
    let fvk = key.full_viewing_key();
    let decryptor = TrialDecryptor::new(&fvk.incoming());
    let nk = fvk.nk();
    let nullifier_of = |n: &OwnNote| digest_to_bytes(&nullifier(&nk, &n.note.commitment(), n.position));
    // The wallet's unspent notes by the nullifier that spends them.
    let mut mine: HashMap<[u8; 32], usize> = notes.unspent().map(|(i, n)| (nullifier_of(n), i)).collect();
    let lied = |what: &str| anyhow!("{rpc} lists the pool's outputs wrongly ({what}). Nothing was stored: scan from another node");
    let (mut new_leaves, mut found, mut spent) = (0u64, 0u64, 0u64);
    loop {
        let from = notes.tree.size();
        let page = commitments_page(rpc, from).await?;
        let size = page["frontier_size"].as_u64().ok_or_else(|| lied("no frontier_size"))?;
        ensure!(size >= from, "The chain at {rpc} has {size} shielded outputs and this wallet's note store has {from}: the node is behind, or the store is from another chain. Wait for the node to sync, or delete {} and scan again.", notes.path.display());
        let list = page["commitments"].as_array().ok_or_else(|| lied("no commitments"))?;
        for e in list {
            let position = notes.tree.size();
            ensure!(e["position"].as_u64() == Some(position), lied("a leaf out of order"));
            let cm = e["cm"].as_str().and_then(digest_hex).ok_or_else(|| lied("a commitment that isn't a digest"))?;
            let enc = e["enc"].as_str().and_then(|h| hex::decode(h).ok()).ok_or_else(|| lied("a ciphertext that isn't hex"))?;
            let height = e["height"].as_u64().ok_or_else(|| lied("a leaf without its block"))?;
            let position = notes.tree.append(cm).map_err(|_| lied("more leaves than the tree holds"))?;
            notes.leaves.extend_from_slice(&digest_to_bytes(&cm));
            new_leaves += 1;
            // A spend is listed with the action it's in, which comes after the note it spends.
            if let Some(i) = e["nullifier"].as_str().and_then(hex32).and_then(|nf| mine.remove(&nf)) {
                notes.notes[i].spent_at = Some(height);
                spent += 1;
            }
            if let Some(d) = decryptor.try_decrypt(&cm, &enc) {
                let end = d.memo.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
                let note = OwnNote { position, note: d.note, memo: d.memo[..end].to_vec(), height, tx: e["tx"].as_str().unwrap_or_default().to_string(), spent_at: None };
                mine.insert(nullifier_of(&note), notes.notes.len());
                notes.notes.push(note);
                found += 1;
            }
        }
        if page["next"].is_null() {
            // The node's own anchor is the root of the leaves it listed, or it listed them wrongly.
            let root = page["root"].as_str().and_then(hex32).ok_or_else(|| lied("no root"))?;
            ensure!(notes.tree.size() == size && digest_to_bytes(&notes.tree.root()) == root, lied("they don't hash to its anchor"));
            notes.save()?;
            return Ok((new_leaves, found, spent, page["height"].as_u64().unwrap_or(0)));
        }
        ensure!(!list.is_empty(), lied("an empty page that isn't the last"));
    }
}

pub(crate) fn scan(dir: &Path, who: &Who) -> Result<Scanned> {
    rt()?.block_on(async {
        let s = Session::connect(dir, who).await?;
        let pool = pool(&s.remote).await?;
        pool.on(&s.remote.rpc)?;
        let mut notes = s.notes(&pool)?;
        let (new_leaves, found, spent, height) = sync(&s.remote, &s.key, &mut notes).await?;
        let unspent = notes.unspent().map(|(_, n)| NoteLine { value: n.note.value, position: n.position, height: n.height, memo: String::from_utf8_lossy(&n.memo).into_owned() }).collect();
        Ok(Scanned { new_leaves, found, spent, leaves: notes.tree.size(), height, balance: notes.balance(), unspent })
    })
}

// ------------------------------------------------------------------------------------ address

fn address(dir: &Path, who: &Who, publish: bool, from: Option<&str>) -> Result<()> {
    let keys = keys_of(dir, who.key.as_deref())?;
    let mine = ghost_key(&keys).address();
    println!("👻 Ghost Mode address fingerprint: {}", mine.fingerprint());
    println!("   Read it to whoever pays you, so they can check the address their wallet found is yours ({} bytes; nobody types it).", mine.to_bytes().len());
    rt()?.block_on(async {
        if !publish {
            // Whether it's published is the node's to say; the fingerprint above needs no node.
            let remote = match Remote::connect(dir, &who.rpc, &who.pin).await {
                Ok(r) => r,
                Err(e) => {
                    println!("   (Couldn't ask {} whether it's published: {e})", who.rpc);
                    return Ok(());
                }
            };
            let me = remote.address(&keys);
            match published(&remote.rpc, &me).await? {
                Some(a) if a == mine => println!("   Published under {me} on `{}`: others can send to that account's name or address.", remote.chain_id),
                Some(a) => println!("   {me} has published another Ghost Mode address (fingerprint {}): `blockula ghost address --publish` replaces it with this one.", a.fingerprint()),
                None => println!("   Not published on `{}` yet: `blockula ghost address --publish` files it under {me}, so others can send to it.", remote.chain_id),
            }
            return Ok(());
        }
        let remote = Remote::connect(dir, &who.rpc, &who.pin).await?;
        pool(&remote).await?.open(&remote.rpc)?;
        let account = match from {
            Some(a) => remote.from_account(a).await?,
            None => remote.address(&keys),
        };
        let tx = send_memo(&remote, &keys, &account, Action::SetGhostAddress { address: HexBytes(mine.to_bytes().to_vec()) }, String::new()).await?;
        println!("✓ {}", tx["summary"].as_str().unwrap_or_default());
        println!("   {}/#/tx/{}", remote.rpc.trim_end_matches('/'), tx["id"].as_str().unwrap_or_default());
        Ok(())
    })
}

/// The Ghost Mode address `account` (a name or an address) published, as `rpc` reports it: the
/// node's word, so callers show its fingerprint. `None`: it has published none.
async fn published(rpc: &str, account: &str) -> Result<Option<Address>> {
    let res = reqwest::get(format!("{}/api/ghost/address/{account}", rpc.trim_end_matches('/'))).await?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or(Value::Null);
    if status == reqwest::StatusCode::NOT_FOUND {
        ensure!(body["error"].as_str() != Some("account not found"), "{account} isn't an account on this chain (a .boo name nobody holds, or a mistyped address)");
        return Ok(None);
    }
    ensure!(status.is_success(), "{rpc} couldn't look up {account}'s Ghost Mode address: {}", body["error"].as_str().unwrap_or("request failed"));
    let address = body["address"].as_str().and_then(|h| hex::decode(h).ok()).and_then(|b| Address::from_bytes(&b));
    Ok(Some(address.ok_or_else(|| anyhow!("{rpc} reports a Ghost Mode address for {account} that isn't one. Nothing was sent: use another node"))?))
}

// ------------------------------------------------------------------------- building an action

/// One Action as the wallet assembles it before proving: two spends and two outputs, dummies
/// where it has nothing real.
struct Act {
    spends: [SpendWitness; 2],
    outputs: [(Note, EncryptedNote); 2],
    anchor: Digest,
}

impl Act {
    /// The bundle, its proof left empty (the sighash covers everything but the proof).
    fn bundle(&self, instance: [u8; 32]) -> GhostBundle {
        let h = |d: &Digest| HexBytes(digest_to_bytes(d).to_vec());
        GhostBundle {
            instance: HexBytes(instance.to_vec()),
            anchor: h(&self.anchor),
            nullifiers: [h(&self.spends[0].nullifier()), h(&self.spends[1].nullifier())],
            commitments: [h(&self.outputs[0].0.commitment()), h(&self.outputs[1].0.commitment())],
            enc: [HexBytes(self.outputs[0].1.enc.clone()), HexBytes(self.outputs[1].1.enc.clone())],
            out: [HexBytes(self.outputs[0].1.out.clone()), HexBytes(self.outputs[1].1.out.clone())],
            proof: HexBytes::default(),
        }
    }

    /// Proves the action for `tx`'s sighash and puts the proof in. Returns the milliseconds the
    /// prover took.
    fn prove(self, tx: &mut Tx) -> Result<u128> {
        let sighash = tx.ghost_sighash().ok_or_else(|| anyhow!("not a ghost transaction"))?;
        let witness = ActionWitness { spends: self.spends, outputs: [self.outputs[0].0, self.outputs[1].0], anchor: self.anchor, sighash };
        let started = Instant::now();
        // `prove` checks every constraint first: a witness that doesn't add up is an error here,
        // never an invalid proof on the wire. The error names a constraint, no values.
        let (_, proof) = blockula_ghost::prove(&witness).map_err(|e| anyhow!("the Action proof couldn't be made ({e:?}): nothing was sent"))?;
        let ms = started.elapsed().as_millis();
        match &mut tx.action {
            Action::Shield { bundle, .. } | Action::PrivateTransfer { bundle } | Action::Unshield { bundle, .. } => bundle.proof = HexBytes(proof),
            _ => bail!("not a ghost transaction"),
        }
        Ok(ms)
    }
}

/// A note of `value` for `to`, encrypted to them, recoverable with `from`'s outgoing viewing key.
fn output(value: u64, to: &Address, memo: &[u8; MEMO_BYTES], from: &SpendingKey) -> Result<(Note, EncryptedNote)> {
    let note = Note { value, owner: to.owner(), rseed: random32() };
    let enc = encrypt_note(&note, to, memo, Some(&from.full_viewing_key().outgoing()), &mut OsRng).map_err(|_| anyhow!("the note isn't for that address"))?;
    Ok((note, enc))
}

/// What a command sent, for printing (and the measured numbers in the docs).
#[derive(Debug)]
pub(crate) struct Sent {
    pub id: String,
    pub what: String,
    pub height: Option<u64>,
    pub fee: u64,
    pub prove_ms: u128,
    pub bytes: usize,
}

/// The height a tx signed now counts from and the last block it's valid in: the node's height,
/// refused if the chain can't have reached it by the clock, plus `max_tx_validity_blocks`.
async fn validity(remote: &Remote) -> Result<(u64, u64)> {
    let status = remote.status().await?;
    let height = status["height"].as_u64().ok_or_else(|| anyhow!("{} doesn't report its height", remote.rpc))?;
    remote.check_height(height).await?;
    let window = status["max_tx_validity_blocks"].as_u64().ok_or_else(|| anyhow!("{} doesn't report max_tx_validity_blocks", remote.rpc))?;
    Ok((height, height.checked_add(window).ok_or_else(|| anyhow!("{} reports a validity window past the last block", remote.rpc))?))
}

/// Posts `tx`'s wire bytes (`application/octet-stream`, how ghost txs travel) and waits for it to
/// be final. A tx the node took but that isn't final yet is an error that says to check it by id,
/// never to send it again.
async fn submit(remote: &Remote, tx: &Tx) -> Result<(String, Option<u64>, usize)> {
    let bytes = tx.to_bytes();
    let size = bytes.len();
    let res = reqwest::Client::new().post(format!("{}/api/tx", remote.rpc.trim_end_matches('/'))).header("content-type", "application/octet-stream").body(bytes).send().await?;
    let ok = res.status().is_success();
    let v: Value = res.json().await.unwrap_or(Value::Null);
    ensure!(ok, "the node refused it: {}", v["error"].as_str().unwrap_or("no reason given"));
    let id = v["id"].as_str().ok_or_else(|| anyhow!("the node took the transaction but named no id"))?.to_string();
    let done = wait_final(&remote.rpc, &id, tx.valid_until).await?;
    Ok((id, done["height"].as_u64(), size))
}

// ------------------------------------------------------------------------------------- shield

pub(crate) fn shield(dir: &Path, who: &Who, amount: u64, from: Option<&str>) -> Result<Sent> {
    ensure!(amount > 0, "shield an amount above 0");
    rt()?.block_on(async {
        let s = Session::connect(dir, who).await?;
        let (remote, rpc) = (&s.remote, s.remote.rpc.as_str());
        let pool = pool(remote).await?;
        pool.open(rpc)?;
        let account = match from {
            Some(a) => remote.from_account(a).await?,
            None => remote.address(&s.keys),
        };
        let (_, valid_until) = validity(remote).await?;
        let acct = get(rpc, &format!("/account/{account}")).await.ok();
        let nonce = acct.as_ref().and_then(|a| a["next_nonce"].as_u64()).unwrap_or(0);
        // A shield spends nothing shielded: two dummy spends, a note of the amount for this
        // wallet and a dummy output, anchored at the pool's current root.
        let me = s.key.address();
        let act = Act { spends: [SpendWitness::dummy(&mut OsRng), SpendWitness::dummy(&mut OsRng)], outputs: [output(amount, &me, &[0; MEMO_BYTES], &s.key)?, dummy_output(&mut OsRng)], anchor: digest_from_bytes(&pool.root).ok_or_else(|| anyhow!("{rpc} reports an anchor that isn't a digest"))? };
        let mut bundle = act.bundle(pool.instance);
        // The fee is in the sighash, so it's fixed before the proof (and with it the tx's size) is
        // known: priced for the largest proof there is. It's a ceiling; the block charges its own
        // base fee for the real size.
        bundle.proof = HexBytes(vec![0; MAX_PROOF_BYTES]);
        let mut tx = unsigned_tx(remote, &s.keys, &account, nonce, valid_until, Action::Shield { amount, bundle: Box::new(bundle) }, String::new());
        tx.fee = fee_ceiling(rpc, &tx, 0).await?;
        if let Action::Shield { bundle, .. } = &mut tx.action {
            bundle.proof = HexBytes::default();
        }
        let cost = amount.checked_add(pool.verify_fee).and_then(|c| c.checked_add(tx.fee)).ok_or_else(|| anyhow!("that amount is more FANG than exists"))?;
        if let Some(have) = acct.as_ref().and_then(|a| a["transferable"].as_str()).and_then(|t| t.parse::<u64>().ok()) {
            ensure!(have >= cost, "{account} can move {} now, and shielding {} takes up to {} with its fees. Nothing was sent.", fmt_fang(have), fmt_fang(amount), fmt_fang(cost));
        }
        let prove_ms = act.prove(&mut tx)?;
        let tx = tx.sign(&s.keys);
        let (id, height, bytes) = submit(remote, &tx).await?;
        // The new note is found like any other: by scanning.
        let mut notes = s.notes(&pool)?;
        sync(remote, &s.key, &mut notes).await?;
        Ok(Sent { id, what: format!("👻 shielded {} from {account}", fmt_fang(amount)), height, fee: tx.fee, prove_ms, bytes })
    })
}

// ---------------------------------------------------------------------------- send and unshield

/// What a spend of shielded notes pays.
enum Pay {
    /// A note of `amount` for `to`, with a memo.
    Send { to: Box<Address>, label: String, amount: u64, memo: Box<[u8; MEMO_BYTES]> },
    /// `amount` to the public account `to` (as signed: an address, or a name the chain resolves).
    Unshield { to: String, amount: u64, opens_account: bool },
}

impl Pay {
    fn amount(&self) -> u64 {
        match self {
            Pay::Send { amount, .. } | Pay::Unshield { amount, .. } => *amount,
        }
    }

    /// The canonical fee of the action that pays it.
    fn fee(&self, pool: &Pool) -> u64 {
        match self {
            Pay::Unshield { opens_account: true, .. } => pool.fee_new_account,
            _ => pool.fee,
        }
    }
}

/// How many merges (private transfers to itself, spending two notes into one) a wallet holding
/// notes of `values` (largest first) makes before one action can pay `need`, when a merge pays
/// `merge_fee`: 0 if its two largest notes cover it, `None` if nothing it holds does.
fn merges_needed(values: &[u64], need: u64, merge_fee: u64) -> Option<usize> {
    let mut have = u128::from(*values.first()?) + u128::from(values.get(1).copied().unwrap_or(0));
    let mut merges = 0;
    for v in values.iter().skip(2) {
        if have >= u128::from(need) {
            break;
        }
        // Merging in a note worth no more than the fee only loses FANG.
        if *v <= merge_fee || have < u128::from(merge_fee) {
            return None;
        }
        have = have - u128::from(merge_fee) + u128::from(*v);
        merges += 1;
    }
    (have >= u128::from(need)).then_some(merges)
}

/// The notes one action spends to cover `need`, from the spendable ones (largest first): the
/// smallest single note that covers it, else the two largest if they do. `None`: merge first.
fn pick(notes: &Notes, spendable: &[usize], need: u64) -> Option<Vec<usize>> {
    let value = |i: usize| notes.notes[i].note.value;
    if let Some(i) = spendable.iter().rev().find(|i| value(**i) >= need) {
        return Some(vec![*i]);
    }
    match spendable {
        [a, b, ..] if u128::from(value(*a)) + u128::from(value(*b)) >= u128::from(need) => Some(vec![*a, *b]),
        _ => None,
    }
}

/// Builds, proves and submits one unsigned action spending `inputs` (one or two of the wallet's
/// notes): it pays `pay` (`None`: a merge), the `fee`, and the rest back to the wallet.
async fn act(s: &Session, pool: &Pool, notes: &Notes, inputs: &[usize], pay: Option<&Pay>, fee: u64) -> Result<Sent> {
    let me = s.key.address();
    let total: u128 = inputs.iter().map(|i| u128::from(notes.notes[*i].note.value)).sum();
    let (to_recipient, leaving_public) = match pay {
        Some(Pay::Send { amount, .. }) => (*amount, 0),
        Some(Pay::Unshield { amount, .. }) => (0, *amount),
        None => (0, 0),
    };
    let change = total.checked_sub(u128::from(to_recipient) + u128::from(leaving_public) + u128::from(fee)).and_then(|c| u64::try_from(c).ok()).ok_or_else(|| anyhow!("those notes don't cover the amount and the fee"))?;
    let change_note = |key: &SpendingKey| if change > 0 { output(change, &me, &[0; MEMO_BYTES], key) } else { Ok(dummy_output(&mut OsRng)) };
    let outputs = match pay {
        Some(Pay::Send { to, amount, memo, .. }) => [output(*amount, to, memo, &s.key)?, change_note(&s.key)?],
        _ => [change_note(&s.key)?, dummy_output(&mut OsRng)],
    };
    let mut spends = Vec::with_capacity(2);
    for i in inputs {
        let n = &notes.notes[*i];
        let path = notes.tree.path(n.position).ok_or_else(|| anyhow!("a note isn't in the tree: delete {} and scan again", notes.path.display()))?;
        spends.push(SpendWitness::new(&s.key, n.note, path));
    }
    while spends.len() < 2 {
        spends.push(SpendWitness::dummy(&mut OsRng));
    }
    let spends: [SpendWitness; 2] = spends.try_into().map_err(|_| anyhow!("one action spends at most two notes"))?;
    let action_of = |bundle: GhostBundle| match pay {
        Some(Pay::Unshield { to, amount, .. }) => Action::Unshield { to: to.clone(), amount: *amount, bundle: Box::new(bundle) },
        _ => Action::PrivateTransfer { bundle: Box::new(bundle) },
    };
    let (_, valid_until) = validity(&s.remote).await?;
    let act = Act { spends, outputs, anchor: notes.tree.root() };
    // An unsigned tx: no sender, nonce, key or signature. The proof authorizes it, the fee is
    // exact (it's in the value balance), and the tip is 0.
    let mut tx = Tx { chain_id: s.remote.chain_id.clone(), genesis_hash: s.remote.genesis_hash.clone(), from: String::new(), nonce: 0, fee, tip: 0, valid_until, action: action_of(act.bundle(pool.instance)), memo: String::new(), public_key: String::new(), signature: String::new() };
    let prove_ms = act.prove(&mut tx)?;
    let (id, height, bytes) = submit(&s.remote, &tx).await?;
    let what = match pay {
        Some(Pay::Send { label, amount, .. }) => format!("👻 sent {} privately to {label}", fmt_fang(*amount)),
        Some(Pay::Unshield { to, amount, .. }) => format!("👻 unshielded {} to {to}", fmt_fang(*amount)),
        None => format!("👻 merged {} notes into one of {}", inputs.len(), fmt_fang(change)),
    };
    Ok(Sent { id, what, height, fee, prove_ms, bytes })
}

/// Pays `pay` from the wallet's notes: scans, merges notes while the two largest don't cover the
/// amount and the fee (saying so first), then the payment. Every action waits for the one before
/// it to be final, and the wallet scans again in between (its change is a new note).
async fn spend(s: &Session, pay: Pay, max_fee: u64) -> Result<Vec<Sent>> {
    let rpc = s.remote.rpc.as_str();
    let mut sent = vec![];
    let mut announced = false;
    // Each merge leaves one note fewer; the bound only stops a loop no honest node causes.
    for _ in 0..256 {
        let pool = pool(&s.remote).await?;
        pool.open(rpc)?;
        let mut notes = s.notes(&pool)?;
        sync(&s.remote, &s.key, &mut notes).await?;
        if notes.tree.size() != pool.size || digest_to_bytes(&notes.tree.root()) != pool.root {
            // A block landed between the two reads: read both again.
            continue;
        }
        let fee = pay.fee(&pool);
        ensure!(fee <= max_fee && pool.fee <= max_fee, "{rpc} quotes a fee of {} for this, above the most this command pays ({}, --max-fee). Nothing was sent.", fmt_fang(fee), fmt_fang(max_fee));
        let need = pay.amount().checked_add(fee).ok_or_else(|| anyhow!("that amount is more FANG than exists"))?;
        let spendable = notes.spendable();
        let values: Vec<u64> = spendable.iter().map(|i| notes.notes[*i].note.value).collect();
        let Some(merges) = merges_needed(&values, need, pool.fee) else {
            let why = if notes.balance() >= need { format!(" (its notes are too small to gather: each merge pays the {} fee)", fmt_fang(pool.fee)) } else { String::new() };
            bail!("This wallet has {} shielded in {} note{}, and this takes {} ({} and the {} fee){why}. Nothing was sent.", fmt_fang(notes.balance()), values.len(), plural(values.len() as u64), fmt_fang(need), fmt_fang(pay.amount()), fmt_fang(fee));
        };
        if merges > 0 && !announced {
            println!("👻 This takes {} of your notes, and one action spends at most two: {merges} private transfer{} to yourself merge{} them first (each pays the {} fee), then the payment. {} actions in all, one after another.", merges + 2, plural(merges as u64), if merges == 1 { "s" } else { "" }, fmt_fang(pool.fee), merges + 1);
            announced = true;
        }
        match pick(&notes, &spendable, need) {
            Some(inputs) => {
                sent.push(act(s, &pool, &notes, &inputs, Some(&pay), fee).await?);
                sync(&s.remote, &s.key, &mut notes).await?;
                return Ok(sent);
            }
            None => sent.push(act(s, &pool, &notes, &spendable[..2], None, pool.fee).await?),
        }
    }
    bail!("The pool kept changing under this payment. {} action{} went through; run the command again for the rest.", sent.len(), plural(sent.len() as u64))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn send(dir: &Path, who: &Who, to: &str, amount: u64, memo: &str, fingerprint: Option<&str>, max_fee: u64, yes: bool) -> Result<Vec<Sent>> {
    ensure!(amount > 0, "send an amount above 0");
    ensure!(memo.len() <= MEMO_BYTES, "--memo is {} bytes, and a note carries at most {MEMO_BYTES}", memo.len());
    let mut memo_bytes = Box::new([0u8; MEMO_BYTES]);
    memo_bytes[..memo.len()].copy_from_slice(memo.as_bytes());
    rt()?.block_on(async {
        let s = Session::connect(dir, who).await?;
        pool(&s.remote).await?.open(&s.remote.rpc)?;
        // Their name or address, as typed; the address filed under it is this node's word.
        let label = account_ref_quiet(to)?;
        let address = published(&s.remote.rpc, &label).await?.ok_or_else(|| anyhow!("{label} hasn't published a Ghost Mode address on this chain. They run `blockula ghost address --publish` first. Nothing was sent."))?;
        let theirs = address.fingerprint();
        if let Some(want) = fingerprint {
            ensure!(want.trim().eq_ignore_ascii_case(&theirs), "{label}'s Ghost Mode address at {} has the fingerprint {theirs}, not {}. Nothing was sent.", s.remote.rpc, want.trim());
        }
        println!("👻 {label}'s Ghost Mode address has the fingerprint {theirs} (as {} reports it{}).", s.remote.rpc, if fingerprint.is_some() { ", and it's the one you gave" } else { ": check it with them" });
        if fingerprint.is_none() {
            confirm(&format!("Send {} privately to the address with fingerprint {theirs}? A private transfer can't be undone.", fmt_fang(amount)), yes)?;
        }
        spend(&s, Pay::Send { to: Box::new(address), label, amount, memo: memo_bytes }, max_fee).await
    })
}

pub(crate) fn unshield(dir: &Path, who: &Who, to: &str, amount: u64, max_fee: u64) -> Result<Vec<Sent>> {
    ensure!(amount > 0, "unshield an amount above 0");
    rt()?.block_on(async {
        let s = Session::connect(dir, who).await?;
        pool(&s.remote).await?.open(&s.remote.rpc)?;
        // A name is signed as the name: the chain resolves it when the tx runs.
        let to = account_ref(to)?;
        let account = get(&s.remote.rpc, &format!("/account/{to}")).await.map_err(|e| anyhow!("{to} isn't an account this chain can pay ({e}). Nothing was sent."))?;
        // Opening an account burns the creation fee out of the tx's fee, so the fee is higher.
        let opens_account = account["exists"] == false;
        if opens_account {
            ensure!(amount >= MIN_NEW_ACCOUNT, "{to} has no account yet, and opening one takes at least {}. Nothing was sent.", fmt_fang(MIN_NEW_ACCOUNT));
            println!("👻 {to} has no account yet: this unshield opens it, which burns the account creation fee on top.");
        }
        spend(&s, Pay::Unshield { to, amount, opens_account }, max_fee).await
    })
}

/// [`account_ref`] for a recipient that's only looked up, never signed: an address as given, or
/// the normalized `.boo` name.
fn account_ref_quiet(who: &str) -> Result<String> {
    let w = who.trim();
    if crate::crypto::is_address(w) {
        return Ok(w.to_string());
    }
    ensure!(!w.starts_with("boo1") && !w.starts_with("tboo1"), "{w} isn't a valid address (check for typos; addresses are lowercase)");
    let n = crate::types::normalize_name(w).with_context(|| format!("{w} is neither an address nor a .boo name"))?;
    Ok(format!("{n}{}", crate::types::NAME_SUFFIX))
}

#[cfg(test)]
mod tests;
