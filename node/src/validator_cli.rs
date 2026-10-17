//! `blockula validator …`: set up a validator node, register it on-chain, stake with validators
//! (delegate, undelegate, withdraw), unjail and set commission, admit a newcomer into a set of
//! one or two, and (on dev and test chains, for admins) pin or remove validators. Talks to any
//! node's HTTP API.

use crate::chain::Genesis;
use crate::cli_net::{ChainPin, Remote};
use crate::crypto::Keys;
use crate::node::NodeAddrs;
use crate::types::*;
use anyhow::{anyhow, bail, Context, Result};
use clap::Subcommand;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Subcommand)]
pub enum ValidatorCmd {
    /// Create a validator home: fetch genesis, generate the validator key, record the addresses
    /// peers should use to reach it
    Init {
        #[arg(long)]
        home: PathBuf,
        /// genesis.json path, or any node's HTTP URL
        #[arg(long)]
        genesis: String,
        /// Refuse a genesis that doesn't hash to this (printed by `summon`, in /api/status).
        /// Required with a URL unless this release knows the chain id
        #[arg(long)]
        genesis_hash: Option<String>,
        /// host:port other validators dial for p2p (default: 127.0.0.1 with a free port)
        #[arg(long)]
        public_p2p_addr: Option<String>,
        /// host:port of this node's HTTP API (default: 127.0.0.1 with a free port)
        #[arg(long)]
        public_http_addr: Option<String>,
    },
    /// Bond FANG and register this validator (signed by the validator's own account, which
    /// must already hold the bond plus fees — on testnets, use the faucet). It joins the active
    /// set at an epoch boundary once its stake ranks (others can delegate to it)
    Register {
        #[arg(long)]
        home: PathBuf,
        /// Any node's HTTP URL
        #[arg(long)]
        rpc: String,
        /// Self-bond in FANG (default: the chain's minimum)
        #[arg(long)]
        bond: Option<String>,
    },
    /// Bring a jailed validator back (once its jail time is over; signed by its owner)
    Unjail(AdminArgs),
    /// Set your validator's commission, in basis points (moves at most `commission_max_change_bps` per epoch)
    Commission {
        /// e.g. 500 for 5%
        bps: u64,
        /// File holding the signing key's 32-byte hex seed: the owner's, or the validator's
        /// consensus key, which signs for its owner (once an epoch; never for a multisig owner)
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        rpc: String,
        #[arg(long)]
        from: Option<String>,
        #[command(flatten)]
        pin: ChainPin,
    },
    /// Admit a candidate into a set of one or two members (signed by this member's consensus key).
    /// Such a set needs every member, the newcomer too, for each block: admit only a validator
    /// whose operator you trust to keep it online. It joins at the end of this epoch once a quorum
    /// of the members admitted it and its node showed up (it does that itself, in the epoch's last
    /// tenth); an admission counts for this epoch only, and each member admits one an epoch
    Admit {
        /// The candidate's address or .boo name
        candidate: String,
        /// This member's consensus key (its home's validator.key)
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        rpc: String,
        /// Account that pays the fee (default: the member's owner, as for its other system txs;
        /// or the validator's own address)
        #[arg(long)]
        from: Option<String>,
        #[command(flatten)]
        pin: ChainPin,
    },
    /// Dev and test chain admins: pin a validator (ranked ahead of stake from the next epoch)
    Approve(AdminArgs),
    /// Dev and test chain admins: remove a validator (it leaves at the next epoch, for good)
    Remove(AdminArgs),
    /// Print the proof of possession another account needs to register this validator
    /// (for registering from a wallet/SDK instead of the validator's own account)
    Pop {
        #[arg(long)]
        home: PathBuf,
        /// The account that will send `register_validator`
        #[arg(long)]
        owner: String,
    },
    /// Show the validator set
    List {
        #[arg(long)]
        rpc: String,
    },
}

#[derive(clap::Args)]
pub struct AdminArgs {
    /// Validator address (boo1…)
    pub validator: String,
    /// File holding the signing key's 32-byte hex seed (e.g. an admin's or owner's key file,
    /// or a validator home's validator.key)
    #[arg(long)]
    pub key: PathBuf,
    #[arg(long)]
    pub rpc: String,
    /// Account to act for, if the key is an added key on another account (default: the key's own address)
    #[arg(long)]
    pub from: Option<String>,
    #[command(flatten)]
    pub pin: ChainPin,
}

pub fn parse_fang(s: &str) -> Result<u64> {
    let s = s.trim();
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    if whole.is_empty() || !whole.chars().all(|c| c.is_ascii_digit()) || frac.len() > 9 || !frac.chars().all(|c| c.is_ascii_digit()) {
        bail!("invalid FANG amount: {s}");
    }
    let frac = format!("{frac:0<9}");
    Ok(whole.parse::<u64>()?.checked_mul(DROPS_PER_FANG).ok_or_else(|| anyhow!("amount too large"))? + frac.parse::<u64>()?)
}

/// A key file: a bare 32-byte hex seed (validator.key), or JSON with a `seed` field (the output of
/// `blockula keygen`, or a CLI wallet's wallet.json).
pub(crate) fn load_seed(path: &Path) -> Result<Keys> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let text = text.trim();
    let hex_seed = if text.starts_with('{') {
        let v: Value = serde_json::from_str(text).with_context(|| format!("{} isn't valid JSON", path.display()))?;
        v["seed"].as_str().map(String::from).ok_or_else(|| anyhow!("{} has no \"seed\" field", path.display()))?
    } else {
        text.to_string()
    };
    let seed = hex::decode(&hex_seed).map_err(|_| anyhow!("{} must hold a 32-byte hex seed (or keygen JSON)", path.display()))?;
    Ok(Keys::from_seed(seed.try_into().map_err(|_| anyhow!("{} must hold a 32-byte hex seed (or keygen JSON)", path.display()))?))
}

pub(crate) fn rt() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread().enable_all().build()?)
}

pub(crate) async fn get(rpc: &str, path: &str) -> Result<Value> {
    let res = reqwest::get(format!("{}/api{path}", rpc.trim_end_matches('/'))).await?;
    let ok = res.status().is_success();
    let v: Value = res.json().await?;
    if !ok {
        bail!("{}", v["error"].as_str().unwrap_or("request failed"));
    }
    Ok(v)
}

/// The address prefix of the chain behind `rpc` (`boo` from nodes that predate prefixes).
pub(crate) async fn chain_prefix(rpc: &str) -> Result<String> {
    let prefix = get(rpc, "/status").await?["address_prefix"].as_str().unwrap_or(crate::crypto::ADDRESS_HRP).to_string();
    if !crate::crypto::is_prefix(&prefix) {
        bail!("{rpc} reports an unknown address prefix {prefix:?}");
    }
    Ok(prefix)
}

/// `keys`' own account on the chain behind `rpc`: its address with that chain's prefix.
pub(crate) async fn own_address(rpc: &str, keys: &Keys) -> Result<String> {
    Ok(keys.address_with(&chain_prefix(rpc).await?))
}

/// Builds, signs, submits and waits for a tx; returns the finalized tx JSON.
async fn send(remote: &Remote, keys: &Keys, from: &str, action: Action) -> Result<Value> {
    send_memo(remote, keys, from, action, String::new()).await
}

/// Signs for the chain `remote` was pinned to (`cli_net`), from a status checked against it.
pub(crate) async fn send_memo(remote: &Remote, keys: &Keys, from: &str, action: Action, memo: String) -> Result<Value> {
    send_memo_with(remote, keys, from, |_| Ok(action), memo).await
}

/// [`send_memo`] for an action that carries an absolute height (an expiry, a deadline): `make`
/// builds it from the height `valid_until` counts from, one status read, so a node that lies
/// about the height can stretch it by at most the validity window (audit gap-7-3).
pub(crate) async fn send_memo_with(remote: &Remote, keys: &Keys, from: &str, make: impl FnOnce(u64) -> Result<Action>, memo: String) -> Result<Value> {
    let rpc = remote.rpc.as_str();
    let status = remote.status().await?;
    let height = status["height"].as_u64().ok_or_else(|| anyhow!("{rpc} doesn't report its height"))?;
    let action = make(height)?;
    let nonce = match get(rpc, &format!("/account/{from}")).await {
        Ok(a) => a["next_nonce"].as_u64().unwrap_or(0),
        Err(_) => 0,
    };
    // Valid for 600 blocks (or the chain's maximum, if shorter).
    let validity = status["max_tx_validity_blocks"].as_u64().unwrap_or(600).min(600);
    let valid_until = height + validity;
    let mut tx = Tx { chain_id: remote.chain_id.clone(), genesis_hash: remote.genesis_hash.clone(), from: from.into(), nonce, fee: 0, tip: 0, valid_until, action, memo, public_key: hex::encode(keys.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
    // The highest base fee fixed for the next blocks (`/api/fees`); only the actual one is charged.
    let fees = get(rpc, "/fees").await?;
    let part = |k: &str| fees["suggested"][k].as_str().and_then(|v| v.parse().ok()).ok_or_else(|| anyhow!("{rpc}/api/fees has no suggested {k}"));
    tx.fee = crate::econ::FeeSlot::pricing(part("flat")?, part("rate")?).fee(tx.size());
    let tx = tx.sign(keys);
    let res = reqwest::Client::new().post(format!("{}/api/tx", rpc.trim_end_matches('/'))).json(&tx).send().await?;
    let ok = res.status().is_success();
    let v: Value = res.json().await?;
    if !ok {
        bail!("{}", v["error"].as_str().unwrap_or("rejected"));
    }
    let id = v["id"].as_str().unwrap_or_default().to_string();
    for _ in 0..120 {
        if let Some(done) = get(rpc, &format!("/tx/{id}")).await.ok().and_then(|t| tx_outcome(&id, t, valid_until)) {
            return done;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    bail!("tx {id} still pending after 60s: it was submitted and can still execute until block {valid_until}, so check it (GET /api/tx/{id}) before sending it again")
}

/// What a `/api/tx` answer says about a tx this CLI signed (valid until `valid_until`): final
/// (`Ok`), failed in a block (its fee was charged), dropped from that node's mempool without
/// running (nothing charged, but another node may still include it until `valid_until`, so it
/// mustn't be sent again before then: audit review-fix-clients-2), or not yet known (`None`).
/// A drop is answered without the tx itself, and older nodes don't say `dropped`.
fn tx_outcome(id: &str, t: Value, valid_until: u64) -> Option<Result<Value>> {
    let reason = t["error"].as_str().unwrap_or("unknown reason");
    match t["status"].as_str() {
        Some("finalized") => Some(Ok(t)),
        Some("failed") if t["dropped"] == true || (t["action"].is_null() && t["dropped"] != false) => Some(Err(anyhow!("tx {id} was dropped by the node without running ({reason}): nothing was charged, but another node may still include it until block {valid_until}, so check it (GET /api/tx/{id}) before sending it again"))),
        Some("failed") => Some(Err(anyhow!("tx {id} failed{}: {reason} (it ran, so its fee was charged)", t["height"].as_u64().map(|h| format!(" in block {h}")).unwrap_or_default()))),
        _ => None,
    }
}

pub fn run(cmd: ValidatorCmd) -> Result<()> {
    match cmd {
        ValidatorCmd::Init { home, genesis, genesis_hash, public_p2p_addr, public_http_addr } => {
            if home.join("validator.key").exists() {
                bail!("{} already has a validator.key", home.display());
            }
            let remote = genesis.starts_with("http://") || genesis.starts_with("https://");
            let g: Genesis = if remote {
                let g: Genesis = serde_json::from_value(rt()?.block_on(get(&genesis, "/genesis"))?)?;
                g.check_prefix().context("genesis")?;
                g
            } else {
                Genesis::load(Path::new(&genesis))?
            };
            // As `join` does: a known chain's genesis must match this release, a pin must match,
            // and a genesis fetched from a node must be pinned (a local file is the operator's own).
            crate::checkpoints::Trust::new(&g, &crate::checkpoints::Pins { genesis_hash, ..Default::default() }, remote)?;
            std::fs::create_dir_all(&home)?;
            let p2p = match public_p2p_addr { Some(a) => a, None => format!("127.0.0.1:{}", crate::chain::free_port()?) };
            let http = match public_http_addr { Some(a) => a, None => format!("127.0.0.1:{}", crate::chain::free_port()?) };
            crate::chain::check_host_port(&p2p).context("--public-p2p-addr")?;
            crate::chain::check_http_addr(&http).context("--public-http-addr")?;
            let keys = Keys::generate();
            let me = keys.address_with(&g.address_prefix);
            std::fs::write(home.join("genesis.json"), serde_json::to_string_pretty(&g)?)?;
            write_secret(&home.join("validator.key"), &keys.seed_hex())?;
            std::fs::write(home.join("node.json"), serde_json::to_string_pretty(&NodeAddrs { p2p_addr: p2p.clone(), http_addr: http.clone() })?)?;
            println!("🧛 Validator home ready at {}", home.display());
            println!("   address (account + validator id): {me}");
            println!("   consensus key: ML-DSA-65, {}…", &hex::encode(keys.public_key())[..32]);
            println!("   advertised p2p {p2p} · http {http}");
            println!("\nNext:");
            println!("   1. Fund {me} with at least {} plus fees (testnets: POST /api/faucet {{\"to\": \"{me}\"}})", fmt_fang(g.chain_params()?.min_self_bond));
            println!("   2. blockula run --home {} --bind 0.0.0.0   (syncs now, votes once it's in the active set)", home.display());
            println!("   3. blockula validator register --home {} --rpc <node url>   (joins at an epoch boundary once its stake ranks)", home.display());
            Ok(())
        }
        ValidatorCmd::Register { home, rpc, bond } => {
            let keys = load_seed(&home.join("validator.key"))?;
            let g = Genesis::load(&home.join("genesis.json"))?;
            let me = keys.address_with(&g.address_prefix);
            let addrs: NodeAddrs = match g.validators.iter().find(|v| v.address == me) {
                Some(v) => NodeAddrs { p2p_addr: v.p2p_addr.clone(), http_addr: v.http_addr.clone() },
                None => serde_json::from_slice(&std::fs::read(home.join("node.json")).context("node.json missing — run `blockula validator init`")?)?,
            };
            let rt = rt()?;
            let bond = match bond {
                Some(b) => parse_fang(&b)?,
                None => rt.block_on(get(&rpc, "/validators"))?["min_bond"].as_str().and_then(|s| s.parse().ok()).unwrap_or(g.chain_params()?.min_self_bond),
            };
            let seed: [u8; 32] = hex::decode(std::fs::read_to_string(home.join("validator.key"))?.trim())?.try_into().map_err(|_| anyhow!("bad validator.key"))?;
            let pop = crate::chain::make_pop(seed, &g.chain_id, &me, &addrs.p2p_addr, &addrs.http_addr);
            let action = Action::RegisterValidator { consensus_key: hex::encode(keys.public_key()), p2p_addr: addrs.p2p_addr, http_addr: addrs.http_addr, bond, pop };
            // The node must be on the home's chain: its genesis pins it.
            let pin = ChainPin { network: None, chain_id: Some(g.chain_id.clone()), genesis_hash: Some(crate::checkpoints::genesis_hash(&g)) };
            let tx = rt.block_on(async { send(&Remote::connect_in(&crate::cli_net::blockula_dir(), &rpc, &pin).await?, &keys, &me, action).await })?;
            println!("✓ {}", tx["summary"].as_str().unwrap_or_default());
            println!("  It joins the active set at an epoch boundary if its stake ranks in the top max_validators (newcomers are at most a third of a set).");
            println!("  Others can back it: blockula stake delegate {me} <FANG> --key <key file> --rpc {rpc}");
            Ok(())
        }
        ValidatorCmd::Unjail(a) => admin(a, |v| Action::Unjail { validator: v }),
        ValidatorCmd::Commission { bps, key, rpc, from, pin } => {
            let keys = load_seed(&key)?;
            let tx = rt()?.block_on(async {
                let remote = Remote::connect(&crate::cli_net::blockula_dir(), &rpc, &pin).await?;
                let from = match from {
                    Some(f) => remote.resolve(&f).await?,
                    // A validator's consensus key signs its terms for its owner (they're the owner's call).
                    None => validator_owner(&remote, &remote.address(&keys)).await?.unwrap_or_else(|| remote.address(&keys)),
                };
                send(&remote, &keys, &from, Action::UpdateValidator { commission_bps: bps, p2p_addr: String::new(), http_addr: String::new() }).await
            })?;
            println!("✓ {}", tx["summary"].as_str().unwrap_or_default());
            Ok(())
        }
        ValidatorCmd::Admit { candidate, key, rpc, from, pin } => {
            let keys = load_seed(&key)?;
            let tx = rt()?.block_on(async {
                let remote = Remote::connect(&crate::cli_net::blockula_dir(), &rpc, &pin).await?;
                let me = remote.address(&keys);
                let candidate = remote.resolve(&candidate).await?;
                let set = get(&remote.rpc, "/validators").await?;
                if !set["active"].as_array().into_iter().flatten().any(|v| v["validator"]["address"].as_str() == Some(me.as_str())) {
                    bail!("{me} isn't in this epoch's set: only its members admit newcomers");
                }
                let from = match from {
                    Some(f) => remote.resolve(&f).await?,
                    None => validator_owner(&remote, &me).await?.unwrap_or_else(|| me.clone()),
                };
                // The epoch the next block is in: the admission fails in any other.
                let epoch = set["epoch"].as_u64().ok_or_else(|| anyhow!("{rpc} doesn't report the epoch"))?;
                send(&remote, &keys, &from, Action::AdmitValidator { validator: candidate, epoch }).await
            })?;
            println!("✓ {}", tx["summary"].as_str().unwrap_or_default());
            println!("  It joins at the end of the epoch once a quorum of the set's members admitted it and its node showed up (in the epoch's last tenth). Admissions count for this epoch only.");
            Ok(())
        }
        ValidatorCmd::Approve(a) => admin(a, |v| Action::ApproveValidator { validator: v }),
        ValidatorCmd::Remove(a) => admin(a, |v| Action::RemoveValidator { validator: v }),
        ValidatorCmd::Pop { home, owner } => {
            let g = Genesis::load(&home.join("genesis.json"))?;
            let seed: [u8; 32] = hex::decode(std::fs::read_to_string(home.join("validator.key"))?.trim())?.try_into().map_err(|_| anyhow!("bad validator.key"))?;
            let keys = Keys::from_seed(seed);
            let addrs: NodeAddrs = match g.validators.iter().find(|v| v.address == keys.address_with(&g.address_prefix)) {
                Some(v) => NodeAddrs { p2p_addr: v.p2p_addr.clone(), http_addr: v.http_addr.clone() },
                None => serde_json::from_slice(&std::fs::read(home.join("node.json"))?)?,
            };
            println!("{}", serde_json::json!({ "consensus_key": hex::encode(keys.public_key()), "p2p_addr": addrs.p2p_addr, "http_addr": addrs.http_addr, "owner": owner, "pop": crate::chain::make_pop(seed, &g.chain_id, &owner, &addrs.p2p_addr, &addrs.http_addr) }));
            Ok(())
        }
        ValidatorCmd::List { rpc } => {
            let v = rt()?.block_on(get(&rpc, "/validators"))?;
            let churn = match v["churn_limit"].as_u64().unwrap_or(0) {
                0 => "too small a set to take newcomers".to_string(),
                1 => "at most 1 newcomer per epoch".to_string(),
                n => format!("at most {n} newcomers per epoch"),
            };
            println!("epoch {} · {} blocks until the next epoch · {churn}", v["epoch"], v["blocks_until_next_epoch"]);
            if let Some(a) = v["admission"].as_object() {
                let members: Vec<&str> = a["members"].as_array().into_iter().flatten().filter_map(|m| m["address"].as_str()).collect();
                println!("a set of {}: a newcomer joins once {} of its members admitted it this epoch (blockula validator admit) and its node shows up from block {} on", members.len(), a["quorum"], a["ready_from"]);
            }
            for (title, key) in [("active", "active"), ("joining next epoch", "joining"), ("leaving next epoch", "leaving"), ("candidates", "candidates"), ("jailed", "jailed"), ("tombstoned", "tombstoned"), ("removed", "removed")] {
                let list = v[key].as_array().cloned().unwrap_or_default();
                println!("{title} ({}):", list.len());
                for x in list {
                    println!("   {}  {}  stake {}  self-bond {}  commission {}", x["validator"]["address"].as_str().unwrap_or_default(), x["p2p_addr"].as_str().unwrap_or_default(), x["stake_display"].as_str().unwrap_or_default(), x["self_bond_display"].as_str().unwrap_or_default(), x["commission_display"].as_str().unwrap_or_default());
                }
            }
            Ok(())
        }
    }
}

/// The owner of validator `v` per the node's `/api/validators` (`None`: `v` is no validator).
async fn validator_owner(remote: &Remote, v: &str) -> Result<Option<String>> {
    let set = get(&remote.rpc, "/validators").await?;
    let lists = ["active", "candidates", "jailed", "next_epoch"];
    Ok(lists.iter().flat_map(|k| set[*k].as_array().cloned().unwrap_or_default()).find(|r| r["validator"]["address"].as_str() == Some(v)).and_then(|r| r["owner"]["address"].as_str().map(String::from)))
}

fn admin(a: AdminArgs, action: impl FnOnce(String) -> Action) -> Result<()> {
    let keys = load_seed(&a.key)?;
    let tx = rt()?.block_on(async {
        let remote = Remote::connect(&crate::cli_net::blockula_dir(), &a.rpc, &a.pin).await?;
        let from = match a.from {
            Some(f) => remote.resolve(&f).await?,
            None => remote.address(&keys),
        };
        send(&remote, &keys, &from, action(a.validator)).await
    })?;
    println!("✓ {}", tx["summary"].as_str().unwrap_or_default());
    Ok(())
}

fn write_secret(path: &Path, contents: &str) -> Result<()> {
    crate::chain::write_secret(path, contents)
}

// ------------------------------------------------------------------------------- wallet CLI

#[derive(Subcommand)]
pub enum WalletCmd {
    /// Create a wallet key (refuses to overwrite an existing one). The address is printed with
    /// the network's prefix: `--prefix`, else the one the node at `--rpc` reports, else `boo`
    New {
        /// A node of the network the wallet is for (its address prefix is used)
        #[arg(long)]
        rpc: Option<String>,
        /// Address prefix: `boo` (mainnet) or `tboo` (public testnets)
        #[arg(long, conflicts_with = "rpc")]
        prefix: Option<String>,
    },
    /// Show the wallet's balance
    Balance {
        #[arg(long, default_value = "http://localhost:7070")]
        rpc: String,
    },
    /// Send FANG to a boo1… address or a .boo name
    Send {
        to: String,
        /// Amount in FANG, e.g. 1.5
        amount: String,
        #[arg(long, default_value = "")]
        memo: String,
        #[arg(long, default_value = "http://localhost:7070")]
        rpc: String,
        #[command(flatten)]
        pin: ChainPin,
    },
    /// Claim a .boo name
    Name {
        name: String,
        #[arg(long, default_value = "http://localhost:7070")]
        rpc: String,
        #[command(flatten)]
        pin: ChainPin,
    },
    /// Testnets: get 100 FANG from the faucet
    Faucet {
        #[arg(long, default_value = "http://localhost:7070")]
        rpc: String,
    },
}

#[derive(serde::Serialize, serde::Deserialize)]
struct WalletFile {
    address: String,
    seed: String,
}

pub(crate) fn wallet_keys(dir: &Path) -> Result<Keys> {
    let path = dir.join("wallet.json");
    let f: WalletFile = serde_json::from_slice(&std::fs::read(&path).with_context(|| format!("no wallet at {} — run `blockula wallet new`", path.display()))?)?;
    let seed: [u8; 32] = hex::decode(&f.seed)?.try_into().map_err(|_| anyhow!("bad seed in {}", path.display()))?;
    Ok(Keys::from_seed(seed))
}

pub fn wallet(cmd: WalletCmd, dir: &Path) -> Result<()> {
    match cmd {
        WalletCmd::New { rpc, prefix } => {
            // The same key is an account on every network; show the address this network uses.
            let prefix = match (prefix, rpc) {
                (Some(p), _) if crate::crypto::is_prefix(&p) => p,
                (Some(p), _) => bail!("unknown address prefix {p:?} (boo or tboo)"),
                (None, Some(rpc)) => rt()?.block_on(chain_prefix(&rpc))?,
                (None, None) => crate::crypto::ADDRESS_HRP.to_string(),
            };
            std::fs::create_dir_all(dir)?;
            let keys = Keys::generate();
            let f = WalletFile { address: keys.address_with(&prefix), seed: keys.seed_hex() };
            write_secret(&dir.join("wallet.json"), &serde_json::to_string_pretty(&f)?)?;
            println!("🧛 New wallet {}", f.address);
            println!("   key saved to {} (mode 600 — back it up; anyone with it controls the funds)", dir.join("wallet.json").display());
            Ok(())
        }
        WalletCmd::Balance { rpc } => {
            let keys = wallet_keys(dir)?;
            let rt = rt()?;
            let me = rt.block_on(own_address(&rpc, &keys))?;
            let a = rt.block_on(get(&rpc, &format!("/account/{me}")))?;
            println!("{me}  {}", a["balance_display"].as_str().unwrap_or("0 FANG"));
            Ok(())
        }
        WalletCmd::Send { to, amount, memo, rpc, pin } => {
            let keys = wallet_keys(dir)?;
            let amount = parse_fang(&amount)?;
            let tx = rt()?.block_on(async {
                let remote = Remote::connect(dir, &rpc, &pin).await?;
                // A name is signed as the name: the chain resolves it, not this node (audit gap-7-1).
                let to = crate::cli_net::account_ref(&to)?;
                send_memo(&remote, &keys, &remote.address(&keys), Action::Transfer { to, amount }, memo).await
            })?;
            println!("✓ {}", tx["summary"].as_str().unwrap_or_default());
            Ok(())
        }
        WalletCmd::Name { name, rpc, pin } => {
            let keys = wallet_keys(dir)?;
            let tx = rt()?.block_on(async {
                let remote = Remote::connect(dir, &rpc, &pin).await?;
                send(&remote, &keys, &remote.address(&keys), Action::RegisterName { name }).await
            })?;
            println!("✓ {}", tx["summary"].as_str().unwrap_or_default());
            Ok(())
        }
        WalletCmd::Faucet { rpc } => {
            let keys = wallet_keys(dir)?;
            let rt = rt()?;
            let res = rt.block_on(async {
                let me = own_address(&rpc, &keys).await?;
                let res = reqwest::Client::new().post(format!("{}/api/faucet", rpc.trim_end_matches('/'))).json(&serde_json::json!({ "to": me })).send().await?;
                let ok = res.status().is_success();
                let v: Value = res.json().await?;
                if !ok {
                    bail!("{}", v["error"].as_str().unwrap_or("faucet refused"));
                }
                Ok::<_, anyhow::Error>(v)
            })?;
            println!("✓ faucet sent {} (tx {})", res["amount_display"].as_str().unwrap_or("FANG"), res["id"].as_str().unwrap_or_default());
            Ok(())
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// A tx the node dropped unrun isn't reported as a failure that was charged (review-fix-clients-2).
    #[test]
    fn dropped_txs_are_not_failures() {
        use serde_json::json;
        let id = "ab".repeat(32);
        let ran = json!({ "status": "failed", "error": "insufficient balance", "height": 12, "action": { "type": "transfer" } });
        let err = tx_outcome(&id, ran, 90).unwrap().unwrap_err().to_string();
        assert!(err.contains("failed in block 12: insufficient balance (it ran, so its fee was charged)"), "{err}");
        // As node/src/api.rs answers a drop, and as an older node did (no `dropped`, no tx).
        for dropped in [json!({ "status": "failed", "dropped": true, "error": "evicted", "height": 12 }), json!({ "status": "failed", "error": "evicted" })] {
            let err = tx_outcome(&id, dropped, 90).unwrap().unwrap_err().to_string();
            assert!(err.contains("dropped by the node without running (evicted): nothing was charged, but another node may still include it until block 90") && !err.contains("fee was charged"), "{err}");
        }
        assert!(tx_outcome(&id, json!({ "status": "failed", "dropped": false, "error": "x", "height": 3 }), 90).unwrap().unwrap_err().to_string().contains("fee was charged"));
        assert!(tx_outcome(&id, json!({ "status": "finalized", "height": 4 }), 90).unwrap().is_ok());
        assert!(tx_outcome(&id, json!({ "status": "pending" }), 90).is_none());
    }

    /// `--key` takes a bare hex seed, `blockula keygen` JSON (e.g. the testnet admin.json), or a
    /// wallet.json, and they all load the same key.
    #[test]
    fn key_files_accept_hex_and_keygen_json() {
        let k = Keys::generate();
        let dir = std::env::temp_dir().join(format!("blockula-keyfile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bare = dir.join("validator.key");
        std::fs::write(&bare, format!("{}\n", k.seed_hex())).unwrap();
        let json = dir.join("admin.json");
        std::fs::write(&json, serde_json::json!({ "address": k.address(), "seed": k.seed_hex() }).to_string()).unwrap();
        assert_eq!(load_seed(&bare).unwrap().address(), k.address());
        assert_eq!(load_seed(&json).unwrap().address(), k.address());
        std::fs::write(&json, r#"{"address":"boo1x"}"#).unwrap();
        assert!(load_seed(&json).err().unwrap().to_string().contains("seed"));
        std::fs::write(&bare, "not hex").unwrap();
        assert!(load_seed(&bare).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `wallet new` writes and prints the address with the network's prefix (WAL-4).
    #[test]
    fn wallet_new_uses_the_network_prefix() {
        let dir = std::env::temp_dir().join(format!("blockula-wallet-new-{}", std::process::id()));
        for (prefix, want) in [(Some("tboo"), "tboo1"), (None, "boo1")] {
            std::fs::remove_dir_all(&dir).ok();
            wallet(WalletCmd::New { rpc: None, prefix: prefix.map(String::from) }, &dir).unwrap();
            let f: WalletFile = serde_json::from_slice(&std::fs::read(dir.join("wallet.json")).unwrap()).unwrap();
            assert!(f.address.starts_with(want), "{}", f.address);
            assert_eq!(wallet_keys(&dir).unwrap().address_with(&f.address[..want.len() - 1]), f.address);
        }
        assert!(wallet(WalletCmd::New { rpc: None, prefix: None }, &dir).is_err(), "never overwrites a wallet");
        std::fs::remove_dir_all(&dir).ok();
        assert!(wallet(WalletCmd::New { rpc: None, prefix: Some("xboo".into()) }, &dir).unwrap_err().to_string().contains("unknown address prefix"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `validator init` checks the genesis like `join`: a pin must match, and a home is only
    /// created for a genesis that passed.
    #[test]
    fn validator_init_checks_the_genesis_pin() {
        let root = std::env::temp_dir().join(format!("blockula-vinit-{}-{}", crate::chain::now_ms(), std::process::id()));
        let homes = crate::chain::Chain::summon(&root, &crate::chain::SummonOpts::new("vinit", 1)).unwrap();
        let file = homes[0].join("genesis.json").display().to_string();
        let hash = crate::checkpoints::genesis_hash(&Genesis::load(Path::new(&file)).unwrap());
        let init = |home: &str, pin: Option<String>| run(ValidatorCmd::Init { home: root.join(home), genesis: file.clone(), genesis_hash: pin, public_p2p_addr: None, public_http_addr: None });
        assert!(init("wrong", Some("00".repeat(32))).unwrap_err().to_string().contains("genesis hash mismatch"));
        assert!(!root.join("wrong").exists(), "no home for a refused genesis");
        init("pinned", Some(hash)).unwrap();
        assert!(root.join("pinned/validator.key").exists());
        // A local file is the operator's own: no pin needed.
        init("local", None).unwrap();
        std::fs::remove_dir_all(&root).ok();
    }

    /// `--rpc https://…` must work for the CLI (wallet, validator register/approve).
    #[test]
    fn cli_rpc_supports_https() {
        let err = format!("{:#}", rt().unwrap().block_on(get("https://127.0.0.1:9", "/status")).unwrap_err());
        assert!(!err.contains("scheme"), "{err}");
    }
}
