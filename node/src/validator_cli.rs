//! `blockula validator …`: set up a validator node, register it on-chain, and (for Season 0
//! admins) approve or remove validators. Talks to any node's HTTP API.

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
    /// must already hold the bond plus fees — on testnets, use the faucet)
    Register {
        #[arg(long)]
        home: PathBuf,
        /// Any node's HTTP URL
        #[arg(long)]
        rpc: String,
        /// Bond in FANG (default: the chain's minimum)
        #[arg(long)]
        bond: Option<String>,
    },
    /// Season 0 admins: approve a pending validator (it joins at the next epoch)
    Approve(AdminArgs),
    /// Season 0 admins: remove a validator (it leaves at the next epoch)
    Remove(AdminArgs),
    /// Reclaim the bond of a removed validator (signed by the account that registered it)
    Unbond(AdminArgs),
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

pub(crate) fn load_seed(path: &Path) -> Result<Keys> {
    let seed = hex::decode(std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?.trim())?;
    Ok(Keys::from_seed(seed.try_into().map_err(|_| anyhow!("{} must hold a 32-byte hex seed", path.display()))?))
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
    let mut tx = Tx { chain_id: status["chain_id"].as_str().unwrap_or_default().into(), from: from.into(), nonce, fee: 0, action, memo, public_key: hex::encode(keys.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
    tx.fee = min_fee(tx.size());
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
                serde_json::from_value(rt()?.block_on(get(&genesis, "/genesis"))?)?
            } else {
                Genesis::load(Path::new(&genesis))?
            };
            let p2p = match public_p2p_addr { Some(a) => a, None => format!("127.0.0.1:{}", crate::chain::free_port()?) };
            let http = match public_http_addr { Some(a) => a, None => format!("127.0.0.1:{}", crate::chain::free_port()?) };
            crate::chain::check_host_port(&p2p).context("--public-p2p-addr")?;
            crate::chain::check_http_addr(&http).context("--public-http-addr")?;
            let keys = Keys::generate();
            std::fs::write(home.join("genesis.json"), serde_json::to_string_pretty(&g)?)?;
            write_secret(&home.join("validator.key"), &keys.seed_hex())?;
            std::fs::write(home.join("node.json"), serde_json::to_string_pretty(&NodeAddrs { p2p_addr: p2p.clone(), http_addr: http.clone() })?)?;
            println!("🧛 Validator home ready at {}", home.display());
            println!("   address (account + validator id): {}", keys.address());
            println!("   consensus key: ML-DSA-65, {}…", &hex::encode(keys.public_key())[..32]);
            println!("   advertised p2p {p2p} · http {http}");
            println!("\nNext:");
            println!("   1. Fund {} with at least {} plus fees (testnets: POST /api/faucet {{\"to\": \"{}\"}})", keys.address(), fmt_fang(g.min_bond), keys.address());
            println!("   2. blockula validator register --home {} --rpc <node url>", home.display());
            println!("   3. Ask a validator admin to approve {}", keys.address());
            println!("   4. blockula run --home {} --bind 0.0.0.0   (syncs now, votes from its activation epoch)", home.display());
            Ok(())
        }
        ValidatorCmd::Register { home, rpc, bond } => {
            let keys = load_seed(&home.join("validator.key"))?;
            let g = Genesis::load(&home.join("genesis.json"))?;
            let addrs: NodeAddrs = match g.validators.iter().find(|v| v.address == keys.address()) {
                Some(v) => NodeAddrs { p2p_addr: v.p2p_addr.clone(), http_addr: v.http_addr.clone() },
                None => serde_json::from_slice(&std::fs::read(home.join("node.json")).context("node.json missing — run `blockula validator init`")?)?,
            };
            let rt = rt()?;
            let bond = match bond {
                Some(b) => parse_fang(&b)?,
                None => rt.block_on(get(&rpc, "/validators"))?["min_bond"].as_str().and_then(|s| s.parse().ok()).unwrap_or(g.min_bond),
            };
            let seed: [u8; 32] = hex::decode(std::fs::read_to_string(home.join("validator.key"))?.trim())?.try_into().map_err(|_| anyhow!("bad validator.key"))?;
            let pop = crate::chain::make_pop(seed, &g.chain_id, &keys.address(), &addrs.p2p_addr, &addrs.http_addr);
            let action = Action::RegisterValidator { consensus_key: hex::encode(keys.public_key()), p2p_addr: addrs.p2p_addr, http_addr: addrs.http_addr, bond, pop };
            let tx = rt.block_on(send(&rpc, &keys, &keys.address(), action))?;
            println!("✓ {}", tx["summary"].as_str().unwrap_or_default());
            println!("  Now ask a validator admin to run: blockula validator approve {} --key <admin key file> --rpc {rpc}", keys.address());
            Ok(())
        }
        ValidatorCmd::Approve(a) => admin(a, |v| Action::ApproveValidator { validator: v }),
        ValidatorCmd::Remove(a) => admin(a, |v| Action::RemoveValidator { validator: v }),
        ValidatorCmd::Unbond(a) => admin(a, |v| Action::Unbond { validator: v }),
        ValidatorCmd::Pop { home, owner } => {
            let g = Genesis::load(&home.join("genesis.json"))?;
            let seed: [u8; 32] = hex::decode(std::fs::read_to_string(home.join("validator.key"))?.trim())?.try_into().map_err(|_| anyhow!("bad validator.key"))?;
            let keys = Keys::from_seed(seed);
            let addrs: NodeAddrs = match g.validators.iter().find(|v| v.address == keys.address()) {
                Some(v) => NodeAddrs { p2p_addr: v.p2p_addr.clone(), http_addr: v.http_addr.clone() },
                None => serde_json::from_slice(&std::fs::read(home.join("node.json"))?)?,
            };
            println!("{}", serde_json::json!({ "consensus_key": hex::encode(keys.public_key()), "p2p_addr": addrs.p2p_addr, "http_addr": addrs.http_addr, "owner": owner, "pop": crate::chain::make_pop(seed, &g.chain_id, &owner, &addrs.p2p_addr, &addrs.http_addr) }));
            Ok(())
        }
        ValidatorCmd::List { rpc } => {
            let v = rt()?.block_on(get(&rpc, "/validators"))?;
            println!("epoch {} · {} blocks until the next epoch", v["epoch"], v["blocks_until_next_epoch"]);
            for (title, key) in [("active", "active"), ("next epoch", "next_epoch"), ("pending approval", "pending"), ("approved", "approved"), ("removed", "removed")] {
                let list = v[key].as_array().cloned().unwrap_or_default();
                println!("{title} ({}):", list.len());
                for x in list {
                    println!("   {}  {}  bond {}", x["validator"]["address"].as_str().unwrap_or_default(), x["p2p_addr"].as_str().unwrap_or_default(), x["bond_display"].as_str().unwrap_or_default());
                }
            }
            Ok(())
        }
    }
}

fn admin(a: AdminArgs, action: impl FnOnce(String) -> Action) -> Result<()> {
    let keys = load_seed(&a.key)?;
    let from = a.from.unwrap_or_else(|| keys.address());
    let tx = rt()?.block_on(send(&a.rpc, &keys, &from, action(a.validator)))?;
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
            let a = rt()?.block_on(get(&rpc, &format!("/account/{}", keys.address())))?;
            println!("{}  {}", keys.address(), a["balance_display"].as_str().unwrap_or("0 FANG"));
            Ok(())
        }
        WalletCmd::Send { to, amount, memo, rpc } => {
            let keys = wallet_keys(dir)?;
            let amount = parse_fang(&amount)?;
            let rt = rt()?;
            // Resolve names locally so we sign the exact recipient address.
            let to = if crate::crypto::is_address(&to) { to } else { rt.block_on(get(&rpc, &format!("/account/{to}")))?["account"]["address"].as_str().ok_or_else(|| anyhow!("can't resolve {to}"))?.to_string() };
            let tx = rt.block_on(send_memo(&rpc, &keys, &keys.address(), Action::Transfer { to, amount }, memo))?;
            println!("✓ {}", tx["summary"].as_str().unwrap_or_default());
            Ok(())
        }
        WalletCmd::Name { name, rpc } => {
            let keys = wallet_keys(dir)?;
            let tx = rt()?.block_on(send(&rpc, &keys, &keys.address(), Action::RegisterName { name }))?;
            println!("✓ {}", tx["summary"].as_str().unwrap_or_default());
            Ok(())
        }
        WalletCmd::Faucet { rpc } => {
            let keys = wallet_keys(dir)?;
            let rt = rt()?;
            let res = rt.block_on(async {
                let res = reqwest::Client::new().post(format!("{}/api/faucet", rpc.trim_end_matches('/'))).json(&serde_json::json!({ "to": keys.address() })).send().await?;
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

    /// `--rpc https://…` must work for the CLI (wallet, validator register/approve).
    #[test]
    fn cli_rpc_supports_https() {
        let err = format!("{:#}", rt().unwrap().block_on(get("https://127.0.0.1:9", "/status")).unwrap_err());
        assert!(!err.contains("scheme"), "{err}");
    }
}
