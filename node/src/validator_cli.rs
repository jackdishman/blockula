//! `blockula validator …`: set up a validator node, register it on-chain, stake with validators
//! (delegate, undelegate, withdraw), unjail and set commission, admit a newcomer into a set of
//! one or two, and (on dev and test chains, for admins) pin or remove validators. Talks to any
//! node's HTTP API.

use crate::chain::Genesis;
use crate::cli_net::{ChainPin, Remote};
use crate::crypto::Keys;
use crate::node::NodeAddrs;
use crate::types::*;
use anyhow::{anyhow, bail, ensure, Context, Result};
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
        /// host:port other validators dial for p2p: a public IP or DNS name (TCP 30300 by
        /// convention). Required on public chains; elsewhere 127.0.0.1 with a free port
        #[arg(long)]
        public_p2p_addr: Option<String>,
        /// This validator's public HTTP address: the URL of a full node you run for it
        /// (https://…), since the validator's own HTTP API stays private. Required on public
        /// chains; elsewhere 127.0.0.1 with a free port
        #[arg(long)]
        public_http_addr: Option<String>,
        /// Accept loopback, private and unspecified hosts in the addresses on a public chain (when
        /// the other validators really reach this one over a private network)
        #[arg(long)]
        allow_private_addr: bool,
    },
    /// Bond FANG and register this validator (signed by the validator's own account, which
    /// must already hold the bond plus fees — on testnets, use the faucet). Start its node first
    /// (`blockula run`): this checks it answers. Then it prints how this chain lets it in (stake
    /// ranking at an epoch boundary, an admin's approval, or a small set's admission)
    Register {
        #[arg(long)]
        home: PathBuf,
        /// Any node's HTTP URL
        #[arg(long)]
        rpc: String,
        /// Self-bond in FANG (default: the chain's minimum)
        #[arg(long)]
        bond: Option<String>,
        /// This validator's own node, if it doesn't listen where `blockula run` puts it for this
        /// home (http://127.0.0.1 and the port of its http_addr, else 7070)
        #[arg(long)]
        node_rpc: Option<String>,
        /// Register without checking that this validator's node is running
        #[arg(long)]
        node_not_running: bool,
        /// Accept loopback, private and unspecified hosts in node.json's addresses on a public chain
        #[arg(long)]
        allow_private_addr: bool,
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
        /// Account to act for (an address on mainnet), if not the validator's owner
        #[arg(long)]
        from: Option<String>,
        #[command(flatten)]
        pin: ChainPin,
    },
    /// Change the addresses your validator publishes on-chain: after moving it to another machine
    /// or IP, or to replace a loopback address. Sends `update_validator` with its current
    /// commission, signed by its owner's key or by its consensus key for the owner (once an epoch)
    SetAddr {
        /// host:port other validators dial for p2p: a public IP or DNS name
        #[arg(long, required_unless_present = "http")]
        p2p: Option<String>,
        /// Its public HTTP address: the URL of a full node you run for it (https://…)
        #[arg(long)]
        http: Option<String>,
        /// The signing key's file (bare hex, `blockula keygen` JSON or a wallet.json): the owner's
        /// key, or the validator's consensus key (its home's validator.key)
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        rpc: String,
        /// The owner's account (an address on mainnet), if the key is an added key on it
        #[arg(long)]
        from: Option<String>,
        /// Also record the new addresses in this validator home's node.json, which its node
        /// advertises (restart the node to use them)
        #[arg(long)]
        home: Option<PathBuf>,
        /// Accept loopback, private and unspecified hosts on a public chain
        #[arg(long)]
        allow_private_addr: bool,
        #[command(flatten)]
        pin: ChainPin,
    },
    /// Admit a candidate into a set of one or two members (signed by this member's consensus key).
    /// Such a set needs every member, the newcomer too, for each block: admit only a validator
    /// whose operator you trust to keep it online. It joins at the end of this epoch once a quorum
    /// of the members admitted it and its node showed up (it does that itself, in the epoch's last
    /// tenth); an admission counts for this epoch only, and each member admits one an epoch
    Admit {
        /// The candidate's address (boo1…), confirmed with its operator. Not a name: a node's
        /// answer for a name could be anyone's candidate, and an admission can't be taken back
        candidate: String,
        /// This member's consensus key (its home's validator.key)
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        rpc: String,
        /// Account that pays the fee (default: the member's owner, as for its other system txs;
        /// or the validator's own address). An address on mainnet
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
    /// Account to act for, if the key is an added key on another account (default: the key's own
    /// address). An address on mainnet
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
    // The validity window (and any height in the action) counts from it: a height the chain can't
    // have reached yet would let the node hold the tx and release it later (review-r3-clients-2).
    remote.check_height(height).await?;
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
    run_in(cmd, &crate::cli_net::blockula_dir())
}

/// [`run`] with `dir` as the blockula directory (its `known-chains.json` pins the nodes).
pub fn run_in(cmd: ValidatorCmd, dir: &Path) -> Result<()> {
    match cmd {
        ValidatorCmd::Init { home, genesis, genesis_hash, public_p2p_addr, public_http_addr, allow_private_addr } => {
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
            // On a public chain the addresses are what other validators dial and what the chain
            // publishes: never a loopback default (OPERATOR-3).
            let public = is_public_chain(&g);
            let (p2p, http) = match (public_p2p_addr, public_http_addr) {
                (Some(p), Some(h)) => (p, h),
                (p, h) if public => bail!(
                    "`{}` is a public chain, so its validators publish addresses others can reach: pass {}. Nothing was created",
                    g.chain_id,
                    [(p.is_none(), "--public-p2p-addr <host:port> (the public IP or DNS name and port other validators dial; TCP 30300 by convention)"), (h.is_none(), "--public-http-addr <https://…> (the public URL of a full node you run for this validator: its own HTTP API stays private)")].iter().filter(|(missing, _)| *missing).map(|(_, f)| *f).collect::<Vec<_>>().join(" and ")
                ),
                (p, h) => (
                    match p { Some(a) => a, None => format!("127.0.0.1:{}", crate::chain::free_port()?) },
                    match h { Some(a) => a, None => format!("127.0.0.1:{}", crate::chain::free_port()?) },
                ),
            };
            crate::chain::check_host_port(&p2p).context("--public-p2p-addr")?;
            crate::chain::check_http_addr(&http).context("--public-http-addr")?;
            check_reachable(public && !allow_private_addr, [("--public-p2p-addr", p2p.as_str()), ("--public-http-addr", http.as_str())], "Pass a public IP or DNS name (or --allow-private-addr if the other validators really reach this one over a private network). Nothing was created")?;
            std::fs::create_dir_all(&home)?;
            let keys = Keys::generate();
            let me = keys.address_with(&g.address_prefix);
            std::fs::write(home.join("genesis.json"), serde_json::to_string_pretty(&g)?)?;
            write_secret(&home.join("validator.key"), &keys.seed_hex())?;
            std::fs::write(home.join("node.json"), serde_json::to_string_pretty(&NodeAddrs { p2p_addr: p2p.clone(), http_addr: http.clone() })?)?;
            println!("🧛 Validator home ready at {}", home.display());
            println!("   address (account + validator id): {me}");
            println!("   consensus key: ML-DSA-65, {}…", &hex::encode(keys.public_key())[..32]);
            println!("   advertised p2p {p2p} · http {http}");
            if g.chain_id == crate::chain::MAINNET_CHAIN_ID {
                let cp = g.chain_params()?;
                eprintln!("{}", crate::cli_net::mainnet_staking_notice(&format!("{}%", crate::staking::fmt_bps(cp.slash_double_sign_bps)), &crate::cli_net::span_words(cp.unbonding_blocks, g.block_time_ms)));
            }
            println!("\nNext:");
            let faucet = if g.faucet { format!(" (test chains: POST /api/faucet {{\"to\": \"{me}\"}})") } else { String::new() };
            println!("   1. Fund {me} with at least {} plus fees{faucet}", fmt_fang(g.chain_params()?.min_self_bond));
            println!("   2. {}   (syncs now; its HTTP API stays on this machine)", run_command(&home, &p2p));
            println!("   3. blockula validator register --home {} --rpc <node url>   (once its node runs: it checks, then says how this chain lets it in)", home.display());
            Ok(())
        }
        ValidatorCmd::Register { home, rpc, bond, node_rpc, node_not_running, allow_private_addr } => {
            let keys = load_seed(&home.join("validator.key"))?;
            let g = Genesis::load(&home.join("genesis.json"))?;
            let me = keys.address_with(&g.address_prefix);
            let addrs: NodeAddrs = match g.validators.iter().find(|v| v.address == me) {
                Some(v) => NodeAddrs { p2p_addr: v.p2p_addr.clone(), http_addr: v.http_addr.clone() },
                None => serde_json::from_slice(&std::fs::read(home.join("node.json")).context("node.json missing — run `blockula validator init`")?)?,
            };
            // The addresses are signed into the registration and published: checked as at init,
            // for homes made before init checked them (OPERATOR-3).
            let node_json = home.join("node.json").display().to_string();
            let (p2p_field, http_field) = (format!("{node_json}'s p2p_addr"), format!("{node_json}'s http_addr"));
            check_reachable(
                is_public_chain(&g) && !allow_private_addr,
                [(p2p_field.as_str(), addrs.p2p_addr.as_str()), (http_field.as_str(), addrs.http_addr.as_str())],
                "Put a public IP or DNS name in node.json (or pass --allow-private-addr if the other validators really reach this one over a private network). Nothing was signed",
            )?;
            let rt = rt()?;
            // Its node must be up on this chain first (OPERATOR-11): a validator whose node is down
            // when it joins the set misses its turns and gets jailed.
            if !node_not_running {
                let local = node_rpc.unwrap_or_else(|| local_node_url(&addrs));
                rt.block_on(check_node_running(&local, &rpc, &crate::checkpoints::genesis_hash(&g)))?;
            }
            let bond = match bond {
                Some(b) => parse_fang(&b)?,
                None => rt.block_on(get(&rpc, "/validators"))?["min_bond"].as_str().and_then(|s| s.parse().ok()).unwrap_or(g.chain_params()?.min_self_bond),
            };
            let seed: [u8; 32] = hex::decode(std::fs::read_to_string(home.join("validator.key"))?.trim())?.try_into().map_err(|_| anyhow!("bad validator.key"))?;
            let pop = crate::chain::make_pop(seed, &g.chain_id, &me, &addrs.p2p_addr, &addrs.http_addr);
            let action = Action::RegisterValidator { consensus_key: hex::encode(keys.public_key()), p2p_addr: addrs.p2p_addr, http_addr: addrs.http_addr, bond, pop };
            // The node must be on the home's chain: its genesis pins it.
            let pin = ChainPin { network: None, chain_id: Some(g.chain_id.clone()), genesis_hash: Some(crate::checkpoints::genesis_hash(&g)) };
            let tx = rt.block_on(async {
                let remote = Remote::connect_in(dir, &rpc, &pin).await?;
                if remote.chain_id == crate::chain::MAINNET_CHAIN_ID {
                    eprintln!("{}", staking_notice(&remote).await?);
                }
                send(&remote, &keys, &me, action).await
            })?;
            println!("✓ {}", tx["summary"].as_str().unwrap_or_default());
            // The chain's actual way in, from its own answer (OPERATOR-11).
            match rt.block_on(get(&rpc, "/validators")) {
                Ok(set) => way_in(&set, &me).iter().for_each(|l| println!("  {l}")),
                Err(_) => println!("  It joins the active set at an epoch boundary if its stake ranks in the top max_validators (newcomers are at most a third of a set)."),
            }
            println!("  Others can back it: blockula stake delegate {me} <FANG> --key <key file> --rpc {rpc}");
            Ok(())
        }
        ValidatorCmd::Unjail(a) => admin(a, dir, |v| Action::Unjail { validator: v }),
        ValidatorCmd::Commission { bps, key, rpc, from, pin } => {
            let keys = load_seed(&key)?;
            let tx = rt()?.block_on(async {
                let remote = Remote::connect(dir, &rpc, &pin).await?;
                let from = match from {
                    Some(f) => remote.from_account(&f).await?,
                    // A validator's consensus key signs its terms for its owner (they're the owner's call).
                    None => validator_owner(&remote, &remote.address(&keys)).await?.unwrap_or_else(|| remote.address(&keys)),
                };
                send(&remote, &keys, &from, Action::UpdateValidator { commission_bps: bps, p2p_addr: String::new(), http_addr: String::new() }).await
            })?;
            println!("✓ {}", tx["summary"].as_str().unwrap_or_default());
            Ok(())
        }
        ValidatorCmd::SetAddr { p2p, http, key, rpc, from, home, allow_private_addr, pin } => {
            let keys = load_seed(&key)?;
            let (tx, p2p, http) = rt()?.block_on(async {
                let remote = Remote::connect(dir, &rpc, &pin).await?;
                let g = remote.genesis().await?;
                if let Some(p) = &p2p {
                    crate::chain::check_host_port(p).context("--p2p")?;
                }
                if let Some(h) = &http {
                    crate::chain::check_http_addr(h).context("--http")?;
                }
                let given: Vec<(&str, &str)> = [("--p2p", p2p.as_deref()), ("--http", http.as_deref())].into_iter().filter_map(|(f, a)| Some((f, a?))).collect();
                check_reachable(is_public_chain(&g) && !allow_private_addr, given, "Pass a public IP or DNS name (or --allow-private-addr if the other validators really reach this one over a private network). Nothing was signed")?;
                let set = get(&remote.rpc, "/validators").await?;
                let signer = remote.address(&keys);
                // Which validator: the one whose consensus key signs (for its owner), else the
                // only one the signing account owns, as the chain picks it (staking::owned_validator).
                let (record, from) = match find_validator(&set, &signer) {
                    Some(v) => {
                        let owner = v["owner"]["address"].as_str().unwrap_or(&signer).to_string();
                        let from = match from {
                            Some(f) => remote.from_account(&f).await?,
                            None => owner,
                        };
                        (v, from)
                    }
                    None => {
                        let from = match from {
                            Some(f) => remote.from_account(&f).await?,
                            None => signer.clone(),
                        };
                        (owned_validator(&set, &from)?, from)
                    }
                };
                let action = set_addr_action(&record, p2p.clone(), http.clone())?;
                let tx = send(&remote, &keys, &from, action).await?;
                Ok::<_, anyhow::Error>((tx, p2p, http))
            })?;
            println!("✓ {}", tx["summary"].as_str().unwrap_or_default());
            if let Some(home) = home {
                record_node_addrs(&home, p2p, http)?;
            }
            Ok(())
        }
        ValidatorCmd::Admit { candidate, key, rpc, from, pin } => {
            let keys = load_seed(&key)?;
            let tx = rt()?.block_on(async {
                let remote = Remote::connect(dir, &rpc, &pin).await?;
                let me = remote.address(&keys);
                let candidate = admit_candidate(&candidate, &remote.prefix)?;
                let set = get(&remote.rpc, "/validators").await?;
                if !set["active"].as_array().into_iter().flatten().any(|v| v["validator"]["address"].as_str() == Some(me.as_str())) {
                    bail!("{me} isn't in this epoch's set: only its members admit newcomers");
                }
                let from = match from {
                    Some(f) => remote.from_account(&f).await?,
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
        ValidatorCmd::Approve(a) => admin(a, dir, |v| Action::ApproveValidator { validator: v }),
        ValidatorCmd::Remove(a) => admin(a, dir, |v| Action::RemoveValidator { validator: v }),
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

/// The candidate `validator admit` signs: an address, on every chain (audit review-r3-staking-1,
/// review-r3-clients-3). The chain needs one, and resolving a name would make it one node's
/// answer: in a set of one, a single admission of the wrong candidate lets a stranger in, and a
/// stranger that stops voting halts the set for good. Nothing is resolved, so nothing is asked.
fn admit_candidate(candidate: &str, prefix: &str) -> Result<String> {
    let c = candidate.trim();
    if !crate::crypto::is_address(c) {
        bail!("`validator admit` takes the candidate's address ({prefix}1…), not {c:?}: an admission can't be taken back, and a node's answer for a name could be anyone's candidate. Get the address from the candidate's operator (it's listed under candidates in `blockula validator list`), check it with them, and pass it. Nothing was signed.");
    }
    if !c.starts_with(&format!("{prefix}1")) {
        bail!("{c} is an address on another network: this chain's addresses start with {prefix}1. Nothing was signed.");
    }
    Ok(c.to_string())
}

/// The owner of validator `v` per the node's `/api/validators` (`None`: `v` is no validator).
async fn validator_owner(remote: &Remote, v: &str) -> Result<Option<String>> {
    let set = get(&remote.rpc, "/validators").await?;
    let lists = ["active", "candidates", "jailed", "next_epoch"];
    Ok(lists.iter().flat_map(|k| set[*k].as_array().cloned().unwrap_or_default()).find(|r| r["validator"]["address"].as_str() == Some(v)).and_then(|r| r["owner"]["address"].as_str().map(String::from)))
}

/// Whether `g` is a public chain, whose validators must publish addresses others can reach:
/// mainnet, a public testnet (`blockula-testnet…`), or any chain whose genesis validators advertise
/// a public host. A chain `summon` made on this machine is none of these.
pub(crate) fn is_public_chain(g: &Genesis) -> bool {
    g.chain_id == crate::chain::MAINNET_CHAIN_ID || g.chain_id.starts_with("blockula-testnet") || g.validators.iter().any(|v| private_host(&v.p2p_addr).is_none() || private_host(&v.http_addr).is_none())
}

/// Why nobody outside this machine or its network could reach `addr` (`host:port`, or an
/// http(s) URL): its host is a loopback, private, link-local, shared (carrier-grade NAT) or
/// unspecified address, or a local name. `None` for a public IP or DNS name.
pub(crate) fn private_host(addr: &str) -> Option<&'static str> {
    use std::net::{IpAddr, Ipv4Addr};
    let a = addr.trim();
    let rest = a.strip_prefix("https://").or_else(|| a.strip_prefix("http://")).unwrap_or(a);
    let hostport = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = match hostport.strip_prefix('[') {
        Some(r) => r.split(']').next().unwrap_or_default(),
        None if hostport.parse::<IpAddr>().is_ok() => hostport,
        None => hostport.rsplit_once(':').map_or(hostport, |(h, _)| h),
    };
    let v4 = |ip: Ipv4Addr| {
        if ip.is_loopback() {
            Some("a loopback address")
        } else if ip.is_unspecified() {
            Some("unspecified (0.0.0.0)")
        } else if ip.is_private() {
            Some("a private address")
        } else if ip.is_link_local() {
            Some("a link-local address")
        } else if ip.octets()[0] == 100 && (ip.octets()[1] & 0xc0) == 64 {
            Some("a shared (carrier-grade NAT) address")
        } else {
            None
        }
    };
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => v4(ip),
        Ok(IpAddr::V6(ip)) => match ip.to_ipv4_mapped() {
            Some(m) => v4(m),
            None if ip.is_loopback() => Some("a loopback address"),
            None if ip.is_unspecified() => Some("unspecified (::)"),
            None if (ip.segments()[0] & 0xfe00) == 0xfc00 => Some("a private (unique local) address"),
            None if (ip.segments()[0] & 0xffc0) == 0xfe80 => Some("a link-local address"),
            None => None,
        },
        Err(_) => {
            let h = host.trim_end_matches('.').to_ascii_lowercase();
            let local = h.is_empty() || !h.contains('.') || h == "localhost" || [".localhost", ".local", ".internal", ".lan", ".home.arpa", ".localdomain"].iter().any(|s| h.ends_with(s));
            local.then_some("a local name")
        }
    }
}

/// With `enforce` (a public chain, no `--allow-private-addr`), refuses the first of `addrs`
/// (`(what, address)`) that only this machine or its network could reach, saying how to `fix` it.
pub(crate) fn check_reachable<'a>(enforce: bool, addrs: impl IntoIterator<Item = (&'a str, &'a str)>, fix: &str) -> Result<()> {
    if !enforce {
        return Ok(());
    }
    for (what, addr) in addrs {
        if let Some(why) = private_host(addr) {
            bail!("{what} {addr} is {why}: on a public chain the other validators, and anyone the chain shows it to, can't reach it. {fix}");
        }
    }
    Ok(())
}

/// The `blockula run` line `validator init` suggests: p2p on every interface when other machines
/// must reach it, HTTP left on this machine (OPERATOR-2).
fn run_command(home: &Path, p2p: &str) -> String {
    let loopback = private_host(p2p) == Some("a loopback address");
    let v6 = p2p.starts_with('[') || p2p.matches(':').count() > 1;
    match (loopback, v6) {
        (true, _) => format!("blockula run --home {}", home.display()),
        (false, true) => format!("blockula run --home {} --p2p-bind ::", home.display()),
        (false, false) => format!("blockula run --home {} --p2p-bind 0.0.0.0", home.display()),
    }
}

/// Where `blockula run` serves a validator home's HTTP API on this machine: the port of its
/// http_addr when that's `host:port`, else 7070 (`node::run_validator`).
fn local_node_url(addrs: &NodeAddrs) -> String {
    let port = (!addrs.http_addr.starts_with("http")).then(|| addrs.http_addr.rsplit(':').next().and_then(|p| p.parse::<u16>().ok())).flatten().unwrap_or(7070);
    format!("http://127.0.0.1:{port}")
}

/// How far behind the network a validator's node may be when it registers before `register`
/// says so.
const NODE_LAG_BLOCKS: u64 = 10;

/// Checks a validator's own node (at `local`) answers for the chain whose genesis hashes to
/// `genesis_hash` and has synced. Being behind is only said, not refused: until it's registered
/// (and a new epoch starts) other validators' p2p handshakes don't let its key in, so after the
/// catch-up at its start it stands still (consensus.rs tracks a set's candidates from each
/// epoch's start). Refusing a node a few blocks behind would refuse anyone who registers more than
/// a few seconds after starting it.
async fn check_node_running(local: &str, rpc: &str, genesis_hash: &str) -> Result<()> {
    let mine = get(local, "/status").await.map_err(|e| {
        anyhow!("this validator's node doesn't answer at {local} ({e:#}). Start it first (blockula run --home …) and let it sync: a validator whose node is down when it joins the set misses its turns and gets jailed. Pass --node-rpc if it listens elsewhere, or --node-not-running to register anyway. Nothing was signed")
    })?;
    ensure!(mine["genesis_hash"].as_str().map(str::to_ascii_lowercase).as_deref() == Some(genesis_hash), "the node at {local} is on another chain (genesis {}), not this home's ({genesis_hash}). Nothing was signed", mine["genesis_hash"].as_str().unwrap_or("not reported"));
    let height = mine["height"].as_u64().unwrap_or(0);
    ensure!(height > 0 || mine["fast_synced_from"].is_u64(), "this validator's node at {local} hasn't synced anything yet: wait until it has (its log says when), or pass --node-not-running. Nothing was signed");
    if let Some(net) = get(rpc, "/status").await.ok().and_then(|s| s["height"].as_u64()).filter(|net| height + NODE_LAG_BLOCKS < *net) {
        eprintln!("ℹ️  this validator's node is at #{height}, the network at #{net}: other validators let it follow over p2p once it's registered and a new epoch starts, and it catches up then. Keep it running.");
    }
    Ok(())
}

/// The three-line mainnet staking notice with this chain's figures (`/api/validators`, and the
/// block time of the pinned genesis).
pub(crate) async fn staking_notice(remote: &Remote) -> Result<String> {
    let set = get(&remote.rpc, "/validators").await?;
    let block_time_ms = remote.genesis().await?.block_time_ms;
    let slash = set["slash_double_sign_display"].as_str().unwrap_or("5%");
    let unbonding = set["unbonding_blocks"].as_u64().map(|b| crate::cli_net::span_words(b, block_time_ms)).unwrap_or_else(|| "21 days".into());
    Ok(crate::cli_net::mainnet_staking_notice(slash, &unbonding))
}

/// What `register` says about how its validator gets into the active set, from the chain's
/// `/api/validators`: a small set's admission, an admin's approval, or stake ranking at an epoch
/// boundary (with the lowest stake in the set).
pub(crate) fn way_in(set: &Value, me: &str) -> Vec<String> {
    let list = |k: &str| set[k].as_array().cloned().unwrap_or_default();
    let name = |l: &Value| l["name"].as_str().or(l["address"].as_str()).unwrap_or_default().to_string();
    let admins: Vec<String> = list("admins").iter().map(name).collect();
    let next = set["blocks_until_next_epoch"].as_u64().unwrap_or(0);
    let mut out = vec![];
    if list("next_epoch").iter().any(|v| v["validator"]["address"].as_str() == Some(me)) {
        out.push(format!("It's in the next epoch's set: it joins in {next} blocks. Keep its node running."));
        return out;
    }
    if let Some(a) = set["admission"].as_object() {
        let members: Vec<String> = a.get("members").and_then(Value::as_array).into_iter().flatten().map(name).collect();
        out.push(format!(
            "This set has {} member{}, and every one is needed for each block, so it takes a newcomer only once {} of them admit it in the same epoch. Ask a member's operator ({}) to run `blockula validator admit {me}` after checking the address with you.",
            members.len(),
            if members.len() == 1 { "" } else { "s" },
            a.get("quorum").and_then(Value::as_u64).unwrap_or(1),
            members.join(", ")
        ));
        out.push(format!("Keep this validator's node running: it signals it's ready itself, from block {} on, and joins at the end of that epoch.", a.get("ready_from").and_then(Value::as_u64).unwrap_or(0)));
        return out;
    }
    let churn = set["churn_limit"].as_u64().unwrap_or(0);
    if churn == 0 {
        out.push(match admins.is_empty() {
            true => "This set is too small to take newcomers by stake right now.".to_string(),
            false => format!("This set is too small to take newcomers by stake: an admin ({}) must pin it with `blockula validator approve {me}`.", admins.join(", ")),
        });
        return out;
    }
    let active = list("active");
    let max = set["max_validators"].as_u64().unwrap_or(0);
    let per_epoch = format!("at most {churn} newcomer{} join{} per epoch, highest stake first", if churn == 1 { "" } else { "s" }, if churn == 1 { "s" } else { "" });
    if (active.len() as u64) < max {
        out.push(format!("{} of {max} seats are taken: it joins at an epoch boundary (the next in {next} blocks) while it's eligible; {per_epoch}.", active.len()));
    } else {
        let lowest = active.iter().filter_map(|v| Some((v["stake"].as_str()?.parse::<u64>().ok()?, v["stake_display"].as_str().unwrap_or_default().to_string()))).min_by_key(|(s, _)| *s);
        let bar = lowest.map(|(_, d)| format!(": the lowest stake in the set is {d}")).unwrap_or_default();
        out.push(format!("It joins at an epoch boundary (the next in {next} blocks) once its pool's stake ranks in the top {max}{bar}; {per_epoch}."));
    }
    if !admins.is_empty() {
        out.push(format!("On this chain an admin ({}) can also pin it: `blockula validator approve {me}`.", admins.join(", ")));
    }
    out
}

/// Every validator `/api/validators` lists, wherever it lists it.
fn all_validators(set: &Value) -> Vec<Value> {
    let mut seen = std::collections::BTreeSet::new();
    ["active", "next_epoch", "joining", "leaving", "candidates", "jailed", "tombstoned", "removed"].iter().flat_map(|k| set[*k].as_array().cloned().unwrap_or_default()).filter(|v| v["validator"]["address"].as_str().is_some_and(|a| seen.insert(a.to_string()))).collect()
}

/// Validator `address`'s record in `/api/validators`.
fn find_validator(set: &Value, address: &str) -> Option<Value> {
    all_validators(set).into_iter().find(|v| v["validator"]["address"].as_str() == Some(address))
}

/// The one validator `owner` owns (as the chain picks it when its owner's key signs), or why not.
fn owned_validator(set: &Value, owner: &str) -> Result<Value> {
    let owned: Vec<Value> = all_validators(set).into_iter().filter(|v| v["owner"]["address"].as_str() == Some(owner) && !matches!(v["status"].as_str(), Some("removed" | "tombstoned"))).collect();
    match owned.len() {
        0 => bail!("{owner} isn't a validator and owns none: sign with the validator's consensus key (its home's validator.key) or its owner's key. Nothing was signed"),
        1 => Ok(owned.into_iter().next().expect("one")),
        _ => bail!("{owner} owns several validators: sign with the consensus key (validator.key) of the one to change. Nothing was signed"),
    }
}

/// The `update_validator` that `set-addr` signs: the validator's commission as the chain charges
/// it today (`record`, its `/api/validators` entry), so only the addresses change, and an empty
/// address for one that stays.
pub(crate) fn set_addr_action(record: &Value, p2p: Option<String>, http: Option<String>) -> Result<Action> {
    ensure!(p2p.is_some() || http.is_some(), "pass --p2p, --http or both");
    let commission_bps = record["commission_bps"].as_u64().ok_or_else(|| anyhow!("the node doesn't report the validator's commission"))?;
    Ok(Action::UpdateValidator { commission_bps, p2p_addr: p2p.unwrap_or_default(), http_addr: http.unwrap_or_default() })
}

/// After `set-addr`: the new addresses in a validator home's node.json, which its node advertises
/// (a genesis validator's node advertises its genesis entry instead, and doesn't read node.json).
fn record_node_addrs(home: &Path, p2p: Option<String>, http: Option<String>) -> Result<()> {
    let path = home.join("node.json");
    let g = Genesis::load(&home.join("genesis.json"))?;
    let keys = load_seed(&home.join("validator.key"))?;
    if g.validators.iter().any(|v| v.address == keys.address_with(&g.address_prefix)) {
        println!("  {} is a genesis validator: its node advertises its genesis p2p address, whatever the chain records. Give genesis validators DNS names, so moving one means changing a DNS record (docs/OPERATIONS.md, Moving a validator).", keys.address_with(&g.address_prefix));
        return Ok(());
    }
    let mut addrs: NodeAddrs = serde_json::from_slice(&std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?)?;
    if let Some(p) = p2p {
        addrs.p2p_addr = p;
    }
    if let Some(h) = http {
        addrs.http_addr = h;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&addrs)?)?;
    println!("  {} updated: restart the node (blockula run --home {}) to advertise p2p {} and http {}", path.display(), home.display(), addrs.p2p_addr, addrs.http_addr);
    Ok(())
}

fn admin(a: AdminArgs, dir: &Path, action: impl FnOnce(String) -> Action) -> Result<()> {
    let keys = load_seed(&a.key)?;
    let tx = rt()?.block_on(async {
        let remote = Remote::connect(dir, &a.rpc, &a.pin).await?;
        let from = match a.from {
            Some(f) => remote.from_account(&f).await?,
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
            if prefix == crate::crypto::ADDRESS_HRP {
                println!("   {}", crate::cli_net::UNRECOVERABLE_KEY);
            }
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
        let init = |home: &str, pin: Option<String>| run(ValidatorCmd::Init { home: root.join(home), genesis: file.clone(), genesis_hash: pin, public_p2p_addr: None, public_http_addr: None, allow_private_addr: false });
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

    /// What only this machine or its network can reach, and what anyone can (OPERATOR-3).
    #[test]
    fn private_hosts_are_told_from_public_ones() {
        for (addr, why) in [
            ("127.0.0.1:7070", "a loopback address"),
            ("http://127.0.0.1:7070", "a loopback address"),
            ("localhost:30300", "a local name"),
            ("0.0.0.0:30300", "unspecified (0.0.0.0)"),
            ("10.1.2.3:30300", "a private address"),
            ("172.20.0.5:7070", "a private address"),
            ("https://192.168.1.10/", "a private address"),
            ("169.254.1.1:1", "a link-local address"),
            ("100.64.3.4:30300", "a shared (carrier-grade NAT) address"),
            ("[::1]:30300", "a loopback address"),
            ("[fd12::1]:30300", "a private (unique local) address"),
            ("[fe80::1]:30300", "a link-local address"),
            ("[::ffff:10.0.0.1]:30300", "a private address"),
            ("blockula-v0.internal:7070", "a local name"),
            ("http://nas.local:7171", "a local name"),
            ("myserver:30300", "a local name"),
        ] {
            assert_eq!(private_host(addr), Some(why), "{addr}");
        }
        for addr in ["45.77.26.148:30300", "https://blockula-testnet.fly.dev", "https://rpc.example.org:8443/api", "[2001:db8::1]:30300", "v1.example.org:30300", "100.128.0.1:30300", "172.32.0.1:7070"] {
            assert_eq!(private_host(addr), None, "{addr}");
        }
    }

    /// Mainnet, a public testnet, or a genesis whose validators advertise public hosts is a public
    /// chain; a chain `summon` made on this machine isn't.
    #[test]
    fn public_chains_are_mainnet_testnets_and_public_genesis_addresses() {
        let root = std::env::temp_dir().join(format!("blockula-public-{}-{}", crate::chain::now_ms(), std::process::id()));
        let homes = crate::chain::Chain::summon(&root, &crate::chain::SummonOpts::new("local-chain", 1)).unwrap();
        let mut g = Genesis::load(&homes[0].join("genesis.json")).unwrap();
        assert!(!is_public_chain(&g));
        for id in [crate::chain::MAINNET_CHAIN_ID, "blockula-testnet-2", "blockula-testnet-9"] {
            assert!(is_public_chain(&Genesis { chain_id: id.into(), ..g.clone() }), "{id}");
        }
        g.validators[0].p2p_addr = "45.77.26.148:30300".into();
        assert!(is_public_chain(&g));
        std::fs::remove_dir_all(&root).ok();
    }

    /// OPERATOR-3: on a public chain `validator init` needs both addresses, refuses loopback and
    /// private hosts in them unless told --allow-private-addr, and creates nothing when it refuses.
    #[test]
    fn init_on_a_public_chain_needs_addresses_others_can_reach() {
        let root = std::env::temp_dir().join(format!("blockula-vinit-public-{}-{}", crate::chain::now_ms(), std::process::id()));
        let homes = crate::chain::Chain::summon(&root, &crate::chain::SummonOpts { address_prefix: "tboo".into(), ..crate::chain::SummonOpts::new("blockula-testnet-9", 1) }).unwrap();
        let file = homes[0].join("genesis.json").display().to_string();
        let init = |home: &str, p2p: Option<&str>, http: Option<&str>, allow: bool| run(ValidatorCmd::Init { home: root.join(home), genesis: file.clone(), genesis_hash: None, public_p2p_addr: p2p.map(String::from), public_http_addr: http.map(String::from), allow_private_addr: allow });
        let err = init("none", None, None, false).unwrap_err().to_string();
        assert!(err.contains("public chain") && err.contains("--public-p2p-addr <host:port>") && err.contains("--public-http-addr <https://…>") && err.contains("Nothing was created"), "{err}");
        let err = init("http-only", Some("45.77.26.148:30300"), None, false).unwrap_err().to_string();
        assert!(err.contains("--public-http-addr") && !err.contains("--public-p2p-addr"), "{err}");
        let err = init("loopback", Some("45.77.26.148:30300"), Some("127.0.0.1:7070"), false).unwrap_err().to_string();
        assert!(err.contains("--public-http-addr 127.0.0.1:7070 is a loopback address") && err.contains("--allow-private-addr"), "{err}");
        let err = init("private", Some("192.168.1.5:30300"), Some("https://rpc.example.org"), false).unwrap_err().to_string();
        assert!(err.contains("--public-p2p-addr 192.168.1.5:30300 is a private address"), "{err}");
        for refused in ["none", "http-only", "loopback", "private"] {
            assert!(!root.join(refused).exists(), "no home for a refusal: {refused}");
        }
        init("allowed", Some("10.0.0.5:30300"), Some("10.0.0.5:7070"), true).unwrap();
        init("public", Some("45.77.26.148:30300"), Some("https://rpc.example.org"), false).unwrap();
        let addrs: NodeAddrs = serde_json::from_slice(&std::fs::read(root.join("public/node.json")).unwrap()).unwrap();
        assert_eq!((addrs.p2p_addr.as_str(), addrs.http_addr.as_str()), ("45.77.26.148:30300", "https://rpc.example.org"));
        // The run line it prints opens p2p to other machines and leaves HTTP on this one.
        assert_eq!(run_command(Path::new("/v"), "45.77.26.148:30300"), "blockula run --home /v --p2p-bind 0.0.0.0");
        assert_eq!(run_command(Path::new("/v"), "[2001:db8::1]:30300"), "blockula run --home /v --p2p-bind ::");
        assert_eq!(run_command(Path::new("/v"), "127.0.0.1:30300"), "blockula run --home /v");
        // Its node listens where `blockula run` puts it: the http_addr's port, else 7070.
        assert_eq!(local_node_url(&addrs), "http://127.0.0.1:7070");
        assert_eq!(local_node_url(&NodeAddrs { p2p_addr: String::new(), http_addr: "127.0.0.1:7123".into() }), "http://127.0.0.1:7123");
        std::fs::remove_dir_all(&root).ok();
    }

    /// OPERATOR-11: what `register` says about the way into the set, from `/api/validators`.
    #[test]
    fn register_says_how_this_chain_lets_a_validator_in() {
        use serde_json::json;
        let me = "boo1me";
        let v = |addr: &str, stake: u64| json!({ "validator": { "address": addr, "name": null }, "stake": (stake * DROPS_PER_FANG).to_string(), "stake_display": format!("{stake} FANG") });
        let admission = json!({ "admins": [], "churn_limit": 0, "blocks_until_next_epoch": 40, "active": [v("boo1a", 10)], "next_epoch": [v("boo1a", 10)], "admission": { "members": [{ "address": "boo1a", "name": "alpha.boo" }], "quorum": 1, "ready_from": 3540 } });
        let lines = way_in(&admission, me).join("\n");
        assert!(lines.contains("once 1 of them admit it") && lines.contains("(alpha.boo)") && lines.contains("`blockula validator admit boo1me`") && lines.contains("from block 3540"), "{lines}");
        let small = json!({ "admins": [{ "address": "boo1admin", "name": null }], "churn_limit": 0, "blocks_until_next_epoch": 5, "active": [v("boo1a", 10)], "next_epoch": [], "admission": null });
        assert_eq!(way_in(&small, me), ["This set is too small to take newcomers by stake: an admin (boo1admin) must pin it with `blockula validator approve boo1me`."]);
        let full = json!({ "admins": [], "churn_limit": 1, "max_validators": 4, "blocks_until_next_epoch": 7, "active": [v("boo1a", 300), v("boo1b", 120), v("boo1c", 200), v("boo1d", 500)], "next_epoch": [], "admission": null });
        assert_eq!(way_in(&full, me), ["It joins at an epoch boundary (the next in 7 blocks) once its pool's stake ranks in the top 4: the lowest stake in the set is 120 FANG; at most 1 newcomer joins per epoch, highest stake first."]);
        let open = json!({ "admins": [{ "address": "boo1admin", "name": "vlad.boo" }], "churn_limit": 2, "max_validators": 31, "blocks_until_next_epoch": 7, "active": [v("boo1a", 1), v("boo1b", 1), v("boo1c", 1), v("boo1d", 1)], "next_epoch": [], "admission": null });
        let lines = way_in(&open, me);
        assert!(lines[0].starts_with("4 of 31 seats are taken") && lines[0].contains("at most 2 newcomers join per epoch") && lines[1].contains("(vlad.boo)"), "{lines:?}");
        let joining = json!({ "next_epoch": [v(me, 10)], "blocks_until_next_epoch": 3 });
        assert_eq!(way_in(&joining, me), ["It's in the next epoch's set: it joins in 3 blocks. Keep its node running."]);
    }

    /// A node with an API on a summoned chain of one validator, making blocks in the background.
    fn local_node(tag: &str) -> (PathBuf, Vec<PathBuf>, std::sync::Arc<parking_lot::Mutex<crate::chain::Chain>>, tokio::runtime::Runtime, String) {
        let root = std::env::temp_dir().join(format!("blockula-{tag}-{}-{}", crate::chain::now_ms(), std::process::id()));
        let homes = crate::chain::Chain::summon(&root, &crate::chain::SummonOpts::new(tag, 1)).unwrap();
        let chain = std::sync::Arc::new(parking_lot::Mutex::new(crate::chain::Chain::open(&homes[0]).unwrap()));
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let url = rt.block_on(async {
            let node = crate::api::Node::new(chain.clone(), None, vec![], Default::default());
            let app = crate::api::router(node).into_make_service_with_connect_info::<std::net::SocketAddr>();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            url
        });
        let producer = chain.clone();
        rt.spawn(async move {
            loop {
                producer.lock().produce_block().ok();
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });
        (root, homes, chain, rt, url)
    }

    /// OPERATOR-3: `set-addr` changes the validator's published addresses and nothing else (its
    /// current commission is signed again), and records them in its home's node.json.
    #[test]
    fn set_addr_changes_the_addresses_and_keeps_the_commission() {
        use serde_json::json;
        let rec = json!({ "commission_bps": 250 });
        assert!(matches!(set_addr_action(&rec, Some("1.2.3.4:30300".into()), None).unwrap(), Action::UpdateValidator { commission_bps: 250, ref p2p_addr, ref http_addr } if p2p_addr == "1.2.3.4:30300" && http_addr.is_empty()));
        assert!(matches!(set_addr_action(&rec, None, Some("https://rpc.example.org".into())).unwrap(), Action::UpdateValidator { commission_bps: 250, ref p2p_addr, ref http_addr } if p2p_addr.is_empty() && http_addr == "https://rpc.example.org"));
        assert!(set_addr_action(&rec, None, None).is_err());
        assert!(set_addr_action(&json!({}), Some("1.2.3.4:1".into()), None).is_err(), "no commission reported, nothing signed");

        let (root, homes, chain, rt, url) = local_node("setaddr");
        let key = homes[0].join("validator.key");
        let me = load_seed(&key).unwrap().address();
        chain.lock().faucet(&me, 100 * DROPS_PER_FANG).unwrap();
        let dir = root.join("cli");
        let wait_for = |what: &str, ok: &dyn Fn(&crate::chain::Chain) -> bool| {
            for _ in 0..200 {
                if ok(&chain.lock()) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            panic!("{what}");
        };
        wait_for("funded", &|c| c.state.account(&me).balance >= 100 * DROPS_PER_FANG);
        run_in(ValidatorCmd::Commission { bps: 100, key: key.clone(), rpc: url.clone(), from: None, pin: ChainPin::default() }, &dir).unwrap();
        let before = chain.lock().state.validators[&me].clone();
        assert_eq!(before.commission_bps, 100);
        let set_addr = |p2p: Option<&str>, http: Option<&str>, home: Option<PathBuf>| run_in(ValidatorCmd::SetAddr { p2p: p2p.map(String::from), http: http.map(String::from), key: key.clone(), rpc: url.clone(), from: None, home, allow_private_addr: false, pin: ChainPin::default() }, &dir);
        set_addr(None, Some("https://rpc.example.org"), None).unwrap();
        let after = chain.lock().state.validators[&me].clone();
        assert_eq!((after.http_addr.as_str(), after.p2p_addr.as_str(), after.commission_bps), ("https://rpc.example.org", before.p2p_addr.as_str(), 100));
        set_addr(Some("203.0.113.7:30300"), None, Some(homes[0].clone())).unwrap();
        let after = chain.lock().state.validators[&me].clone();
        assert_eq!((after.http_addr.as_str(), after.p2p_addr.as_str(), after.commission_bps), ("https://rpc.example.org", "203.0.113.7:30300", 100));
        // A key that's no validator and owns none is refused before anything is signed.
        let stranger = root.join("stranger.key");
        std::fs::write(&stranger, Keys::from_seed([42; 32]).seed_hex()).unwrap();
        let err = run_in(ValidatorCmd::SetAddr { p2p: Some("203.0.113.8:30300".into()), http: None, key: stranger, rpc: url.clone(), from: None, home: None, allow_private_addr: false, pin: ChainPin::default() }, &dir).unwrap_err().to_string();
        assert!(err.contains("isn't a validator") && err.contains("Nothing was signed"), "{err}");
        drop(rt);
        std::fs::remove_dir_all(&root).ok();
    }

    /// OPERATOR-11: `register` first checks the validator's own node answers for this chain.
    #[test]
    fn register_checks_its_node_is_running_on_this_chain() {
        let (root, homes, chain, rt, url) = local_node("regnode");
        let hash = crate::checkpoints::genesis_hash(&chain.lock().genesis);
        for _ in 0..200 {
            if chain.lock().height() > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        rt.block_on(check_node_running(&url, &url, &hash)).unwrap();
        let other = rt.block_on(check_node_running(&url, &url, &"ab".repeat(32))).unwrap_err().to_string();
        assert!(other.contains("on another chain") && other.contains("Nothing was signed"), "{other}");
        let down = rt.block_on(check_node_running("http://127.0.0.1:9", &url, &hash)).unwrap_err().to_string();
        assert!(down.contains("doesn't answer at http://127.0.0.1:9") && down.contains("--node-not-running") && down.contains("--node-rpc"), "{down}");
        drop((rt, homes));
        std::fs::remove_dir_all(&root).ok();
    }
}
