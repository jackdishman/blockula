//! `blockula validator …`: set up a validator node, register it on-chain, stake with validators
//! (delegate, undelegate, withdraw), unjail and set commission, and (on dev and test chains,
//! for admins) pin or remove validators. Talks to any node's HTTP API.

use crate::chain::Genesis;
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
    /// Stake FANG with a validator (buys shares of its pool)
    Delegate {
        #[command(flatten)]
        a: AdminArgs,
        /// Amount in FANG, e.g. 1500
        amount: String,
    },
    /// Start unbonding stake from a validator's pool (withdraw it once the unbonding period is over)
    Undelegate {
        #[command(flatten)]
        a: AdminArgs,
        /// Shares to sell (`blockula validator stake` shows them)
        #[arg(long, conflicts_with = "all")]
        shares: Option<String>,
        /// Every share you hold in that pool
        #[arg(long)]
        all: bool,
    },
    /// Withdraw every unbonding entry that has finished
    Withdraw {
        /// File holding the signing key's 32-byte hex seed
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        rpc: String,
        #[arg(long)]
        from: Option<String>,
    },
    /// Bring a jailed validator back (once its jail time is over; signed by its owner)
    Unjail(AdminArgs),
    /// Set your validator's commission, in basis points (moves at most `commission_max_change_bps` per epoch)
    Commission {
        /// e.g. 500 for 5%
        bps: u64,
        /// File holding the signing key's 32-byte hex seed (the owner's, or the validator's consensus key)
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        rpc: String,
        #[arg(long)]
        from: Option<String>,
    },
    /// Show an account's stake: delegations, unbonding entries, validators it runs
    Stake {
        /// Address or .boo name
        account: String,
        #[arg(long)]
        rpc: String,
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
async fn send(rpc: &str, keys: &Keys, from: &str, action: Action) -> Result<Value> {
    send_memo(rpc, keys, from, action, String::new()).await
}

pub(crate) async fn send_memo(rpc: &str, keys: &Keys, from: &str, action: Action, memo: String) -> Result<Value> {
    let status = get(rpc, "/status").await?;
    let nonce = match get(rpc, &format!("/account/{from}")).await {
        Ok(a) => a["next_nonce"].as_u64().unwrap_or(0),
        Err(_) => 0,
    };
    // Valid for 600 blocks (or the chain's maximum, if shorter).
    let validity = status["max_tx_validity_blocks"].as_u64().unwrap_or(600).min(600);
    let valid_until = status["height"].as_u64().unwrap_or(0) + validity;
    let genesis_hash = status["genesis_hash"].as_str().ok_or_else(|| anyhow!("{rpc} didn't report its genesis_hash"))?;
    let mut tx = Tx { chain_id: status["chain_id"].as_str().unwrap_or_default().into(), genesis_hash: genesis_hash.into(), from: from.into(), nonce, fee: 0, tip: 0, valid_until, action, memo, public_key: hex::encode(keys.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
    // The highest base fee fixed for the next blocks (`/api/fees`); only the actual one is charged.
    let fees = get(rpc, "/fees").await?;
    let part = |k: &str| fees["suggested"][k].as_str().and_then(|v| v.parse().ok()).ok_or_else(|| anyhow!("{rpc}/api/fees has no suggested {k}"));
    tx.fee = crate::econ::FeeSlot { flat: part("flat")?, rate: part("rate")?, max_block_bytes: 0 }.fee(tx.size());
    let tx = tx.sign(keys);
    let res = reqwest::Client::new().post(format!("{}/api/tx", rpc.trim_end_matches('/'))).json(&tx).send().await?;
    let ok = res.status().is_success();
    let v: Value = res.json().await?;
    if !ok {
        bail!("{}", v["error"].as_str().unwrap_or("rejected"));
    }
    let id = v["id"].as_str().unwrap_or_default().to_string();
    for _ in 0..120 {
        if let Ok(t) = get(rpc, &format!("/tx/{id}")).await {
            if t["status"] == "finalized" {
                return Ok(t);
            }
            if t["status"] == "failed" {
                bail!("tx {id} failed: {}", t["error"].as_str().unwrap_or("unknown reason"));
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    bail!("tx {id} still pending after 60s (it may have been dropped: check balance and nonce)")
}

pub fn run(cmd: ValidatorCmd) -> Result<()> {
    match cmd {
        ValidatorCmd::Init { home, genesis, public_p2p_addr, public_http_addr } => {
            if home.join("validator.key").exists() {
                bail!("{} already has a validator.key", home.display());
            }
            std::fs::create_dir_all(&home)?;
            let g: Genesis = if genesis.starts_with("http://") || genesis.starts_with("https://") {
                let g: Genesis = serde_json::from_value(rt()?.block_on(get(&genesis, "/genesis"))?)?;
                g.check_prefix().context("genesis")?;
                g
            } else {
                Genesis::load(Path::new(&genesis))?
            };
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
            let tx = rt.block_on(send(&rpc, &keys, &me, action))?;
            println!("✓ {}", tx["summary"].as_str().unwrap_or_default());
            println!("  It joins the active set at an epoch boundary if its stake ranks in the top max_validators (newcomers are at most a third of a set).");
            println!("  Others can back it: blockula validator delegate {me} <FANG> --key <key file> --rpc {rpc}");
            Ok(())
        }
        ValidatorCmd::Delegate { a, amount } => {
            let amount = parse_fang(&amount)?;
            admin(a, |v| Action::Delegate { validator: v, amount })
        }
        ValidatorCmd::Undelegate { a, shares, all } => {
            let shares: u64 = match (shares, all) {
                (Some(s), false) => s.parse().map_err(|_| anyhow!("--shares must be a whole number of shares"))?,
                (None, true) => {
                    let keys = load_seed(&a.key)?;
                    let rt = rt()?;
                    let from = match &a.from {
                        Some(f) => f.clone(),
                        None => rt.block_on(own_address(&a.rpc, &keys))?,
                    };
                    let stake = rt.block_on(get(&a.rpc, &format!("/staking/{from}")))?;
                    let held = stake["delegations"].as_array().into_iter().flatten().find(|d| d["validator"]["address"].as_str() == Some(a.validator.as_str())).and_then(|d| d["shares"].as_str()).ok_or_else(|| anyhow!("{from} has no stake with {}", a.validator))?;
                    held.parse::<u64>().map_err(|_| anyhow!("{held} shares is more than one tx can move; undelegate in parts with --shares"))?
                }
                _ => bail!("say how much: --shares N or --all"),
            };
            admin(a, |v| Action::Undelegate { validator: v, shares })
        }
        ValidatorCmd::Withdraw { key, rpc, from } => {
            let keys = load_seed(&key)?;
            let rt = rt()?;
            let from = match from {
                Some(f) => f,
                None => rt.block_on(own_address(&rpc, &keys))?,
            };
            let tx = rt.block_on(send(&rpc, &keys, &from, Action::WithdrawUnbonded {}))?;
            println!("✓ {}", tx["summary"].as_str().unwrap_or_default());
            Ok(())
        }
        ValidatorCmd::Unjail(a) => admin(a, |v| Action::Unjail { validator: v }),
        ValidatorCmd::Commission { bps, key, rpc, from } => {
            let keys = load_seed(&key)?;
            let rt = rt()?;
            let from = match from {
                Some(f) => f,
                None => rt.block_on(own_address(&rpc, &keys))?,
            };
            let tx = rt.block_on(send(&rpc, &keys, &from, Action::UpdateValidator { commission_bps: bps, p2p_addr: String::new(), http_addr: String::new() }))?;
            println!("✓ {}", tx["summary"].as_str().unwrap_or_default());
            Ok(())
        }
        ValidatorCmd::Stake { account, rpc } => {
            let v = rt()?.block_on(get(&rpc, &format!("/staking/{account}")))?;
            println!("{}", v["summary"].as_str().unwrap_or_default());
            for d in v["delegations"].as_array().into_iter().flatten() {
                println!("   {}  {} shares  worth {}{}", d["validator"]["address"].as_str().unwrap_or_default(), d["shares"].as_str().unwrap_or_default(), d["value_display"].as_str().unwrap_or_default(), if d["self_bond"] == true { "  (self-bond)" } else { "" });
            }
            for e in v["entries"].as_array().into_iter().flatten() {
                match e["type"].as_str() {
                    Some("unbonding") => println!("   unbonding {} from {}: {}", e["amount_display"].as_str().unwrap_or_default(), e["validator"]["address"].as_str().unwrap_or_default(), if e["ready"] == true { "ready to withdraw".to_string() } else { format!("free at block {}", e["release_at"]) }),
                    _ => println!("   redelegated {} from {} to {} (settles at block {})", e["amount_display"].as_str().unwrap_or_default(), e["from"]["address"].as_str().unwrap_or_default(), e["to"]["address"].as_str().unwrap_or_default(), e["release_at"]),
                }
            }
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

fn admin(a: AdminArgs, action: impl FnOnce(String) -> Action) -> Result<()> {
    let keys = load_seed(&a.key)?;
    let rt = rt()?;
    let from = match a.from {
        Some(f) => f,
        None => rt.block_on(own_address(&a.rpc, &keys))?,
    };
    let tx = rt.block_on(send(&a.rpc, &keys, &from, action(a.validator)))?;
    println!("✓ {}", tx["summary"].as_str().unwrap_or_default());
    Ok(())
}

fn write_secret(path: &Path, contents: &str) -> Result<()> {
    crate::chain::write_secret(path, contents)
}

// ------------------------------------------------------------------------------- wallet CLI

#[derive(Subcommand)]
pub enum WalletCmd {
    /// Create a wallet key (refuses to overwrite an existing one)
    New,
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
    },
    /// Claim a .boo name
    Name {
        name: String,
        #[arg(long, default_value = "http://localhost:7070")]
        rpc: String,
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
        WalletCmd::New => {
            std::fs::create_dir_all(dir)?;
            let keys = Keys::generate();
            let f = WalletFile { address: keys.address(), seed: keys.seed_hex() };
            write_secret(&dir.join("wallet.json"), &serde_json::to_string_pretty(&f)?)?;
            println!("🧛 New wallet {}", keys.address());
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
        WalletCmd::Send { to, amount, memo, rpc } => {
            let keys = wallet_keys(dir)?;
            let amount = parse_fang(&amount)?;
            let rt = rt()?;
            // Resolve names locally so we sign the exact recipient address.
            let to = if crate::crypto::is_address(&to) { to } else { rt.block_on(get(&rpc, &format!("/account/{to}")))?["account"]["address"].as_str().ok_or_else(|| anyhow!("can't resolve {to}"))?.to_string() };
            let me = rt.block_on(own_address(&rpc, &keys))?;
            let tx = rt.block_on(send_memo(&rpc, &keys, &me, Action::Transfer { to, amount }, memo))?;
            println!("✓ {}", tx["summary"].as_str().unwrap_or_default());
            Ok(())
        }
        WalletCmd::Name { name, rpc } => {
            let keys = wallet_keys(dir)?;
            let rt = rt()?;
            let me = rt.block_on(own_address(&rpc, &keys))?;
            let tx = rt.block_on(send(&rpc, &keys, &me, Action::RegisterName { name }))?;
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

    /// `--rpc https://…` must work for the CLI (wallet, validator register/approve).
    #[test]
    fn cli_rpc_supports_https() {
        let err = format!("{:#}", rt().unwrap().block_on(get("https://127.0.0.1:9", "/status")).unwrap_err());
        assert!(!err.contains("scheme"), "{err}");
    }
}
