//! Which chain a signing CLI command is for, and checking that the node behind `--rpc` is on it
//! (WAL-4, audit F51). The same rules as the MCP (`mcp/src/network.ts`), sharing its
//! `known-chains.json`:
//!
//! * `--network mainnet|testnet|local|custom` (or `BLOCKULA_NETWORK`) says which network you mean.
//!   Without it, it's read from `--rpc`: the public testnet's URL is `testnet`, this machine is
//!   `local`, anything else `custom`. `mainnet` and `testnet` pin their chain id and prefix.
//! * `--chain-id` and `--genesis-hash` (or `BLOCKULA_CHAIN_ID`, `BLOCKULA_GENESIS_HASH`) pin them
//!   for any node. A chain id this release knows (`node/checkpoints/`) must come with its genesis.
//! * What isn't pinned is pinned by the first status the node reports (trust on first use) and,
//!   except for local dev chains (which `blockula dev --reset` replaces), remembered in
//!   `known-chains.json` by network and URL, so a node that later reports another chain is refused.
//! * Mainnet is never trusted on first use: a node reporting `blockula-1` is refused unless you
//!   said mainnet (`--network mainnet`, `--chain-id blockula-1`, or a pin remembered from that).
//!   A node can't turn a testnet command into a signed mainnet transaction.
//! * Plain `http://` only to this machine or a private network, where nothing on the way can
//!   answer for the node.
//!
//! Every status a tx is built from is checked again, so a node can't switch chains between reads.
//!
//! Names (audit gap-7-1): where the chain resolves a name itself (transfers, hunt awards, vouches,
//! Soul grants, flags, clawbacks, name offers, proposal targets) the CLI signs the name
//! ([`account_ref`]), so a lying node can't redirect it. Where an address is needed (`--from`),
//! the node resolves it, unproven, and the CLI prints the address it got.

use crate::chain::MAINNET_CHAIN_ID;
use crate::crypto::Keys;
use crate::validator_cli::get;
use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The public testnet's node (as in the MCP).
pub const TESTNET_RPC: &str = "https://blockula-testnet.fly.dev";
/// The public testnet's chain id (deploy/launch.sh; never reused, DEC-7).
pub const TESTNET_CHAIN_ID: &str = "blockula-testnet-2";
/// Where pins are remembered, in the blockula directory (shared with the MCP).
pub const KNOWN_CHAINS_FILE: &str = "known-chains.json";

/// The chain a signing command is for (flattened into every command that signs).
#[derive(clap::Args, Clone, Debug, Default)]
pub struct ChainPin {
    /// The network this is for: mainnet, testnet, local or custom (default: BLOCKULA_NETWORK, else
    /// read from --rpc). The node must be on it, and mainnet is never assumed
    #[arg(long)]
    pub network: Option<String>,
    /// Refuse a node on any other chain id (default: BLOCKULA_CHAIN_ID)
    #[arg(long)]
    pub chain_id: Option<String>,
    /// Refuse a node whose chain starts from another genesis (default: BLOCKULA_GENESIS_HASH)
    #[arg(long)]
    pub genesis_hash: Option<String>,
}

impl ChainPin {
    /// The flags, with the environment filling what they leave out.
    fn with_env(&self) -> ChainPin {
        let env = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        ChainPin {
            network: self.network.clone().or_else(|| env("BLOCKULA_NETWORK")),
            chain_id: self.chain_id.clone().or_else(|| env("BLOCKULA_CHAIN_ID")),
            genesis_hash: self.genesis_hash.clone().or_else(|| env("BLOCKULA_GENESIS_HASH")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetKey {
    Mainnet,
    Testnet,
    Local,
    Custom,
}

impl NetKey {
    fn name(self) -> &'static str {
        match self {
            NetKey::Mainnet => "mainnet",
            NetKey::Testnet => "testnet",
            NetKey::Local => "local",
            NetKey::Custom => "custom",
        }
    }
}

/// What the node must report in `/api/status` (field names as in the MCP's known-chains.json).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pin {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genesis_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

/// A network as configured: which one, its node, its pins, and whether first-use pins are kept.
#[derive(Clone, Debug)]
pub struct Network {
    pub key: NetKey,
    pub rpc: String,
    pub pin: Pin,
    pub remember: bool,
}

fn host(u: &reqwest::Url) -> String {
    u.host_str().unwrap_or_default().trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase()
}

fn is_local(u: &reqwest::Url) -> bool {
    matches!(host(u).as_str(), "localhost" | "127.0.0.1" | "::1")
}

/// This machine or a private network (where plain http can't be intercepted from the internet).
fn is_private(u: &reqwest::Url) -> bool {
    let h = host(u);
    let second = |p: &str| h.strip_prefix(p).and_then(|r| r.split('.').next()).and_then(|o| o.parse::<u8>().ok());
    is_local(u) || h.starts_with("10.") || h.starts_with("192.168.") || second("172.").is_some_and(|o| (16..=31).contains(&o)) || [".internal", ".local", ".lan"].iter().any(|s| h.ends_with(s))
}

/// A node URL: http(s) without credentials, plain http only on this machine or a private
/// network. Returns it without trailing slashes.
pub fn check_rpc_url(rpc: &str) -> Result<(String, reqwest::Url)> {
    let url = reqwest::Url::parse(rpc.trim()).map_err(|_| anyhow!("--rpc isn't a URL: {rpc}"))?;
    ensure!(matches!(url.scheme(), "http" | "https") && url.username().is_empty() && url.password().is_none(), "--rpc must be an http(s) URL without credentials: {rpc}");
    ensure!(url.scheme() == "https" || is_private(&url), "--rpc must use https unless the node is on this machine or a private network: {rpc}");
    Ok((rpc.trim().trim_end_matches('/').to_string(), url))
}

/// A chain id this release knows the genesis of (`node/checkpoints/`).
fn known_genesis(chain_id: &str) -> Option<String> {
    crate::checkpoints::embedded().iter().find(|k| k.chain_id == chain_id && !k.genesis_hash.is_empty()).map(|k| k.genesis_hash.clone())
}

/// The network for `rpc` and the pins in `args` (flags and environment already merged).
/// Contradictions are errors, never guesses.
pub fn resolve(rpc: &str, args: &ChainPin) -> Result<Network> {
    let (rpc, url) = check_rpc_url(rpc)?;
    let key = match args.network.as_deref().map(|n| n.trim().to_ascii_lowercase()) {
        Some(n) => match n.as_str() {
            "mainnet" => NetKey::Mainnet,
            "testnet" => NetKey::Testnet,
            "local" => NetKey::Local,
            "custom" => NetKey::Custom,
            _ => bail!("unknown network {n:?} (mainnet, testnet, local or custom)"),
        },
        None if rpc == TESTNET_RPC => NetKey::Testnet,
        None if is_local(&url) => NetKey::Local,
        None => NetKey::Custom,
    };
    ensure!(key != NetKey::Local || is_local(&url), "the local network is a node on this machine, not {rpc}");
    let mut pin = match key {
        NetKey::Mainnet => Pin { chain_id: Some(MAINNET_CHAIN_ID.into()), genesis_hash: None, prefix: Some(crate::crypto::ADDRESS_HRP.into()) },
        NetKey::Testnet => Pin { chain_id: Some(TESTNET_CHAIN_ID.into()), genesis_hash: None, prefix: Some(crate::crypto::TESTNET_HRP.into()) },
        NetKey::Local | NetKey::Custom => Pin::default(),
    };
    if let Some(id) = args.chain_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(net) = pin.chain_id.as_deref().filter(|net| *net != id) {
            bail!("chain id {id} contradicts the {} network ({net})", key.name());
        }
        pin.chain_id = Some(id.to_string());
    }
    if let Some(g) = args.genesis_hash.as_deref().map(|g| g.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()) {
        ensure!(g.len() == 64 && crate::crypto::is_lower_hex(&g), "--genesis-hash must be 64 hex characters");
        pin.genesis_hash = Some(g);
    }
    if let Some(known) = pin.chain_id.as_deref().and_then(known_genesis) {
        if let Some(g) = pin.genesis_hash.as_deref().filter(|g| *g != known) {
            bail!("genesis hash {g} contradicts {}'s known genesis {known}", pin.chain_id.as_deref().unwrap_or_default());
        }
        pin.genesis_hash = Some(known);
    }
    Ok(Network { key, rpc, pin, remember: key != NetKey::Local })
}

/// Why a node's status doesn't match `pin`, or `None` if it does. A chain id this release knows
/// must come with its known genesis, and mainnet must have been asked for.
pub fn mismatch(pin: &Pin, status: &Value) -> Option<String> {
    let Some(chain_id) = status["chain_id"].as_str() else { return Some("the node doesn't report a chain id".into()) };
    let genesis = status["genesis_hash"].as_str().map(str::to_ascii_lowercase);
    match pin.chain_id.as_deref() {
        Some(want) if want != chain_id => return Some(format!("the node is on chain `{chain_id}`, not `{want}`")),
        None if chain_id == MAINNET_CHAIN_ID => {
            return Some(format!("the node is on mainnet (`{MAINNET_CHAIN_ID}`, real FANG), and this command wasn't told to use mainnet. Pass --network mainnet if that's what you mean"));
        }
        _ => {}
    }
    if let Some(want) = pin.genesis_hash.clone().or_else(|| known_genesis(chain_id))
        && genesis.as_deref() != Some(want.as_str())
    {
        return Some(format!("the node's `{chain_id}` starts from genesis {}, not {want}", genesis.as_deref().unwrap_or("(none reported)")));
    }
    if let (Some(want), Some(got)) = (pin.prefix.as_deref(), status["address_prefix"].as_str())
        && want != got
    {
        return Some(format!("the node uses `{got}1…` addresses, not `{want}1…`"));
    }
    None
}

/// `pin` with every field the node reported that wasn't pinned yet.
pub fn complete_pin(pin: &Pin, status: &Value) -> Pin {
    let s = |k: &str| status[k].as_str().map(String::from);
    Pin {
        chain_id: pin.chain_id.clone().or_else(|| s("chain_id")),
        genesis_hash: pin.genesis_hash.clone().or_else(|| s("genesis_hash").map(|g| g.to_ascii_lowercase())),
        prefix: pin.prefix.clone().or_else(|| s("address_prefix")),
    }
}

/// The pin to enforce: the configured one, filled from what was remembered for this network. A
/// remembered field never overrides a configured one, and what was remembered for another chain
/// id (the network relaunched under a new one) isn't used at all.
pub fn effective_pin(net: &Network, remembered: Option<Pin>) -> Pin {
    let r = remembered.filter(|r| net.pin.chain_id.is_none() || r.chain_id.is_none() || r.chain_id == net.pin.chain_id).unwrap_or_default();
    Pin { chain_id: net.pin.chain_id.clone().or(r.chain_id), genesis_hash: net.pin.genesis_hash.clone().or(r.genesis_hash), prefix: net.pin.prefix.clone().or(r.prefix) }
}

/// The CLI's directory: `$BLOCKULA_HOME`, else `~/.blockula` (the wallet, `known-chains.json`,
/// dev homes).
pub fn blockula_dir() -> PathBuf {
    std::env::var_os("BLOCKULA_HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap()).join(".blockula"))
}

/// Chains remembered on first use, by network and node URL (`known-chains.json`, shared with the
/// MCP: entries are keyed `"<network> <url>"`).
pub struct KnownChains {
    pub file: PathBuf,
}

impl KnownChains {
    pub fn in_dir(dir: &Path) -> Self {
        Self { file: dir.join(KNOWN_CHAINS_FILE) }
    }

    fn id(net: &Network) -> String {
        format!("{} {}", net.key.name(), net.rpc)
    }

    fn all(&self) -> Result<BTreeMap<String, Value>> {
        match std::fs::read(&self.file) {
            Ok(b) => serde_json::from_slice(&b).map_err(|_| anyhow!("{} is corrupt; fix or delete it (it pins which chain each node must be on)", self.file.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(e).with_context(|| format!("reading {}", self.file.display())),
        }
    }

    pub fn get(&self, net: &Network) -> Result<Option<Pin>> {
        if !net.remember {
            return Ok(None);
        }
        Ok(self.all()?.get(&Self::id(net)).and_then(|v| serde_json::from_value(v.clone()).ok()))
    }

    pub fn save(&self, net: &Network, pin: &Pin) -> Result<()> {
        if !net.remember {
            return Ok(());
        }
        let mut all = self.all()?;
        all.insert(Self::id(net), serde_json::to_value(pin)?);
        if let Some(d) = self.file.parent() {
            std::fs::create_dir_all(d)?;
        }
        // Written aside and renamed, so a reader (the MCP, another command) never sees half a file.
        let tmp = self.file.with_extension("json.tmp");
        std::fs::remove_file(&tmp).ok();
        crate::chain::write_secret(&tmp, &serde_json::to_string_pretty(&all)?)?;
        std::fs::rename(&tmp, &self.file)?;
        Ok(())
    }
}

/// A node checked to be on the chain the command is for. Build txs only from its [`Remote::status`].
#[derive(Clone, Debug)]
pub struct Remote {
    pub rpc: String,
    pub chain_id: String,
    pub genesis_hash: String,
    pub prefix: String,
}

impl Remote {
    /// [`Remote::connect_in`] with the environment filling what the flags leave out (`dir`: the
    /// blockula directory, holding `known-chains.json`).
    pub async fn connect(dir: &Path, rpc: &str, args: &ChainPin) -> Result<Remote> {
        Self::connect_in(dir, rpc, &args.with_env()).await
    }

    /// Resolves the network, reads the node's status and refuses it unless it's on the pinned
    /// chain; pins (and, but for local chains, remembers in `dir`) whatever wasn't pinned yet.
    pub async fn connect_in(dir: &Path, rpc: &str, args: &ChainPin) -> Result<Remote> {
        let net = resolve(rpc, args)?;
        let known = KnownChains::in_dir(dir);
        let remembered = known.get(&net)?;
        let pin = effective_pin(&net, remembered.clone());
        let status = get(&net.rpc, "/status").await?;
        if let Some(why) = mismatch(&pin, &status) {
            let fix = if net.remember && remembered.is_some() {
                format!(" If this network was relaunched on purpose, remove its entry (\"{}\") from {}, or pass --genesis-hash.", KnownChains::id(&net), known.file.display())
            } else {
                String::new()
            };
            bail!("Refusing {} ({} network): {why}. Nothing was signed or sent.{fix}", net.rpc, net.key.name());
        }
        let full = complete_pin(&pin, &status);
        let (Some(chain_id), Some(genesis_hash)) = (full.chain_id.clone(), full.genesis_hash.clone()) else {
            bail!("{} didn't report its chain id and genesis hash; nothing was signed", net.rpc);
        };
        let prefix = full.prefix.clone().unwrap_or_else(|| crate::crypto::ADDRESS_HRP.to_string());
        ensure!(crate::crypto::is_prefix(&prefix), "{} reports an unknown address prefix {prefix:?}", net.rpc);
        if net.remember && remembered.as_ref() != Some(&full) {
            known.save(&net, &full)?;
            eprintln!("📌 {} is `{chain_id}` (genesis {}…): pinned in {}", net.rpc, &genesis_hash[..genesis_hash.len().min(16)], known.file.display());
        }
        Ok(Remote { rpc: net.rpc, chain_id, genesis_hash, prefix })
    }

    /// A fresh `/api/status`, refused unless it's still the pinned chain.
    pub async fn status(&self) -> Result<Value> {
        let status = get(&self.rpc, "/status").await?;
        let pin = Pin { chain_id: Some(self.chain_id.clone()), genesis_hash: Some(self.genesis_hash.clone()), prefix: Some(self.prefix.clone()) };
        if let Some(why) = mismatch(&pin, &status) {
            bail!("Refusing {}: {why}. Nothing was signed or sent.", self.rpc);
        }
        Ok(status)
    }

    /// Blocks per day on this chain, from its genesis: the genesis the node serves must hash to
    /// the pinned genesis hash, so the node can't stretch a "days" figure by misreporting its block
    /// time (audit gap-7-3).
    pub async fn blocks_per_day(&self) -> Result<u64> {
        let g: crate::chain::Genesis = serde_json::from_value(get(&self.rpc, "/genesis").await?).context("the node's genesis doesn't parse")?;
        let hash = hex::encode(crate::consensus::genesis_block(&g).digest_bytes());
        ensure!(hash == self.genesis_hash, "{} serves a genesis that hashes to {hash}, not the pinned {}: refusing its block time", self.rpc, self.genesis_hash);
        Ok(crate::params::blocks_per_day(g.block_time_ms))
    }

    /// `keys`' own account on this chain.
    pub fn address(&self, keys: &Keys) -> String {
        keys.address_with(&self.prefix)
    }

    /// An account by address, or a name resolved by this node, unproven (printed, since the node
    /// could lie). Only for fields that need an address; see [`account_ref`].
    pub async fn resolve(&self, who: &str) -> Result<String> {
        resolve_unproven(&self.rpc, who).await
    }
}

/// An account by address, or a name as `rpc` resolves it: unproven, so the address is printed for
/// the user to check (a node can point a name anywhere). Only where the chain needs an address.
pub async fn resolve_unproven(rpc: &str, who: &str) -> Result<String> {
    if crate::crypto::is_address(who) {
        return Ok(who.to_string());
    }
    let addr = get(rpc, &format!("/account/{who}")).await?["account"]["address"].as_str().map(String::from).ok_or_else(|| anyhow!("can't resolve {who}"))?;
    ensure!(crate::crypto::is_address(&addr), "{rpc} resolved {who} to {addr:?}, which isn't an address");
    eprintln!("   {who} → {addr} (as {rpc} resolves it, unproven: check the address)");
    Ok(addr)
}

/// A recipient as signed, for fields the chain resolves itself (transfers, hunt awards, vouches,
/// Soul grants, flags, clawbacks, name offers, proposal targets): an address as given, or a
/// `.boo` name signed as the name, which the chain resolves when the tx runs. No node is asked,
/// so none can redirect it (audit gap-7-1).
pub fn account_ref(who: &str) -> Result<String> {
    let w = who.trim();
    if crate::crypto::is_address(w) {
        return Ok(w.to_string());
    }
    ensure!(!w.starts_with("boo1") && !w.starts_with("tboo1"), "{w} isn't a valid address (check for typos; addresses are lowercase)");
    let n = crate::types::normalize_name(w).with_context(|| format!("{w} is neither an address nor a .boo name"))?;
    let name = format!("{n}{}", crate::types::NAME_SUFFIX);
    eprintln!("   {name}: signed as the name, so the chain resolves it when the tx runs (no node is asked)");
    Ok(name)
}

#[cfg(test)]
mod account_refs {
    #[test]
    fn names_are_signed_as_names_and_addresses_as_given() {
        let a = crate::crypto::Keys::from_seed([1; 32]).address();
        assert_eq!(super::account_ref(&a).unwrap(), a);
        assert_eq!(super::account_ref(" Alice.boo ").unwrap(), "alice.boo");
        assert_eq!(super::account_ref("alice").unwrap(), "alice.boo");
        assert!(super::account_ref("boo1notanaddress").is_err());
        assert!(super::account_ref("not a name!").is_err());
        // What the chain accepts in a resolved field.
        let t = crate::types::Action::Transfer { to: super::account_ref("alice").unwrap(), amount: 1 };
        assert!(crate::types::Tx { chain_id: "c".into(), genesis_hash: "00".repeat(32), from: a, nonce: 0, fee: 1, tip: 0, valid_until: 1, action: t, memo: String::new(), public_key: "00".repeat(crate::crypto::PUBLIC_KEY_LEN), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) }.check_format().is_ok());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pin(network: Option<&str>, chain_id: Option<&str>, genesis: Option<&str>) -> ChainPin {
        ChainPin { network: network.map(String::from), chain_id: chain_id.map(String::from), genesis_hash: genesis.map(String::from) }
    }

    fn status(chain_id: &str, genesis: &str, prefix: &str) -> Value {
        json!({ "chain_id": chain_id, "genesis_hash": genesis, "address_prefix": prefix, "height": 10 })
    }

    #[test]
    fn the_network_comes_from_the_flags_or_the_url_and_contradictions_are_errors() {
        let none = ChainPin::default();
        let net = resolve("https://blockula-testnet.fly.dev/", &none).unwrap();
        assert_eq!((net.key, net.rpc.as_str(), net.pin.chain_id.as_deref(), net.pin.prefix.as_deref()), (NetKey::Testnet, TESTNET_RPC, Some(TESTNET_CHAIN_ID), Some("tboo")));
        let local = resolve("http://localhost:7070", &none).unwrap();
        assert_eq!((local.key, local.remember, local.pin.clone()), (NetKey::Local, false, Pin::default()));
        assert_eq!(resolve("https://rpc.example", &none).unwrap().key, NetKey::Custom);
        let main = resolve("https://rpc.example", &pin(Some("Mainnet"), None, None)).unwrap();
        assert_eq!((main.key, main.pin.chain_id.as_deref(), main.pin.prefix.as_deref()), (NetKey::Mainnet, Some(MAINNET_CHAIN_ID), Some("boo")));
        assert!(resolve("https://rpc.example", &pin(Some("testnet"), Some("blockula-1"), None)).unwrap_err().to_string().contains("contradicts"));
        assert!(resolve("https://rpc.example", &pin(Some("local"), None, None)).unwrap_err().to_string().contains("this machine"));
        assert!(resolve("https://rpc.example", &pin(Some("moon"), None, None)).is_err());
        assert!(resolve("https://rpc.example", &pin(None, None, Some("abc"))).unwrap_err().to_string().contains("64 hex"));
        let g = "AB".repeat(32);
        assert_eq!(resolve("https://rpc.example", &pin(None, Some("x"), Some(&g))).unwrap().pin, Pin { chain_id: Some("x".into()), genesis_hash: Some("ab".repeat(32)), prefix: None });
        // Plain http only nearby.
        for ok in ["http://127.0.0.1:7070", "http://[::1]:7070", "http://10.8.0.2:7070", "http://172.20.1.1", "http://blockula-v0.internal:7070"] {
            check_rpc_url(ok).unwrap();
        }
        for bad in ["http://rpc.example", "http://172.32.0.1", "https://user:pw@rpc.example", "ftp://rpc.example", "not a url"] {
            assert!(check_rpc_url(bad).is_err(), "{bad}");
        }
    }

    /// The F51 exploit: a node you meant as a testnet (or anything not mainnet) answers as mainnet.
    #[test]
    fn mainnet_is_never_trusted_on_first_use() {
        let mainnet = status(MAINNET_CHAIN_ID, &"11".repeat(32), "boo");
        for net in [resolve("https://evil-testnet.example", &ChainPin::default()).unwrap(), resolve("http://127.0.0.1:7070", &ChainPin::default()).unwrap()] {
            let why = mismatch(&effective_pin(&net, None), &mainnet).expect("refused");
            assert!(why.contains("--network mainnet"), "{why}");
        }
        assert!(mismatch(&resolve(TESTNET_RPC, &ChainPin::default()).unwrap().pin, &mainnet).unwrap().contains("not `blockula-testnet-2`"));
        // Asked for: fine (its genesis pinned on first use until a release builds it in).
        let asked = resolve("https://rpc.example", &pin(Some("mainnet"), None, None)).unwrap();
        assert_eq!(mismatch(&asked.pin, &mainnet), None);
        assert_eq!(mismatch(&resolve("https://rpc.example", &pin(None, Some(MAINNET_CHAIN_ID), None)).unwrap().pin, &mainnet), None);
    }

    #[test]
    fn a_pinned_chain_genesis_or_prefix_must_match() {
        let g = "22".repeat(32);
        let p = Pin { chain_id: Some("c".into()), genesis_hash: Some(g.clone()), prefix: Some("tboo".into()) };
        assert_eq!(mismatch(&p, &status("c", &g.to_uppercase(), "tboo")), None);
        assert!(mismatch(&p, &status("d", &g, "tboo")).unwrap().contains("not `c`"));
        assert!(mismatch(&p, &status("c", &"33".repeat(32), "tboo")).unwrap().contains("genesis"));
        assert!(mismatch(&p, &status("c", &g, "boo")).unwrap().contains("tboo1"));
        assert!(mismatch(&p, &json!({ "genesis_hash": g })).unwrap().contains("chain id"));
        assert!(mismatch(&p, &json!({ "chain_id": "c" })).unwrap().contains("(none reported)"));
        // First use fills what wasn't pinned; remembered pins fill a network's gaps, but never
        // override it, and a relaunched network's old chain is ignored.
        let net = resolve(TESTNET_RPC, &ChainPin::default()).unwrap();
        let first = complete_pin(&net.pin, &status(TESTNET_CHAIN_ID, &g, "tboo"));
        assert_eq!(first, Pin { chain_id: Some(TESTNET_CHAIN_ID.into()), genesis_hash: Some(g.clone()), prefix: Some("tboo".into()) });
        assert_eq!(effective_pin(&net, Some(first.clone())), first);
        let old = Pin { chain_id: Some("blockula-testnet".into()), genesis_hash: Some("44".repeat(32)), prefix: None };
        assert_eq!(effective_pin(&net, Some(old)), net.pin);
    }

    #[test]
    fn known_chains_are_shared_with_the_mcp_and_survive_other_entries() {
        let dir = std::env::temp_dir().join(format!("blockula-known-{}-{}", crate::chain::now_ms(), std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // An entry the MCP wrote (its format), which the CLI reads and keeps.
        std::fs::write(dir.join(KNOWN_CHAINS_FILE), r#"{"testnet https://blockula-testnet.fly.dev":{"chainId":"blockula-testnet-2","genesisHash":"aa","prefix":"tboo"}}"#).unwrap();
        let known = KnownChains::in_dir(&dir);
        let testnet = resolve(TESTNET_RPC, &ChainPin::default()).unwrap();
        assert_eq!(known.get(&testnet).unwrap().unwrap().genesis_hash.as_deref(), Some("aa"));
        let custom = resolve("https://rpc.example", &ChainPin::default()).unwrap();
        let p = Pin { chain_id: Some("c".into()), genesis_hash: Some("bb".into()), prefix: Some("boo".into()) };
        known.save(&custom, &p).unwrap();
        assert_eq!(known.get(&custom).unwrap(), Some(p));
        assert!(known.get(&testnet).unwrap().is_some(), "the MCP's entry stays");
        let text = std::fs::read_to_string(dir.join(KNOWN_CHAINS_FILE)).unwrap();
        assert!(text.contains("\"custom https://rpc.example\"") && text.contains("\"genesisHash\""), "{text}");
        // Local chains aren't remembered.
        let local = resolve("http://localhost:7070", &ChainPin::default()).unwrap();
        known.save(&local, &Pin::default()).unwrap();
        assert_eq!(known.get(&local).unwrap(), None);
        std::fs::write(dir.join(KNOWN_CHAINS_FILE), "{").unwrap();
        assert!(known.get(&custom).unwrap_err().to_string().contains("corrupt"));
        std::fs::remove_dir_all(dir).ok();
    }

    /// A node in front of `honest` that rewrites its `/api/status` and counts the txs sent through it.
    async fn proxy(honest: String, rewrite: impl Fn(Value) -> Value + Clone + Send + Sync + 'static) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use axum::{routing::get, Json, Router};
        use std::sync::atomic::Ordering;
        let posts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (h1, h2, counted) = (honest.clone(), honest, posts.clone());
        let app = Router::new()
            .route(
                "/api/status",
                get(move || {
                    let (h, rewrite) = (h1.clone(), rewrite.clone());
                    async move { Json(rewrite(reqwest::get(format!("{h}/api/status")).await.unwrap().json::<Value>().await.unwrap())) }
                }),
            )
            .fallback(move |req: axum::extract::Request| {
                let (h, counted) = (h2.clone(), counted.clone());
                async move {
                    let (parts, body) = req.into_parts();
                    let body = axum::body::to_bytes(body, 1 << 24).await.unwrap();
                    if parts.method == axum::http::Method::POST {
                        counted.fetch_add(1, Ordering::SeqCst);
                    }
                    let res = reqwest::Client::new().request(parts.method, format!("{h}{}", parts.uri)).header("content-type", "application/json").body(body).send().await.unwrap();
                    (res.status(), res.text().await.unwrap())
                }
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, posts)
    }

    /// F51 end to end: the CLI wallet signs through an honest node, and refuses (before signing
    /// or sending anything) a node that claims mainnet, one that switches chains between reads,
    /// and one whose genesis changed since it was pinned.
    #[test]
    fn the_cli_signs_only_for_the_chain_it_pinned() {
        use crate::validator_cli::{wallet, WalletCmd};
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::Arc;
        let root = std::env::temp_dir().join(format!("blockula-clipin-{}-{}", crate::chain::now_ms(), std::process::id()));
        let homes = crate::chain::Chain::summon(&root, &crate::chain::SummonOpts::new("clipin", 1)).unwrap();
        let chain = Arc::new(parking_lot::Mutex::new(crate::chain::Chain::open(&homes[0]).unwrap()));
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let honest = rt.block_on(async {
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
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        });
        let dir = root.join("cli");
        wallet(WalletCmd::New { rpc: None, prefix: None }, &dir).unwrap();
        wallet(WalletCmd::Faucet { rpc: honest.clone() }, &dir).unwrap();
        let me = crate::validator_cli::wallet_keys(&dir).unwrap().address();
        for _ in 0..100 {
            if chain.lock().state.account(&me).balance > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let bob = Keys::from_seed([9; 32]).address();
        let send = |rpc: &str, pin: ChainPin| wallet(WalletCmd::Send { to: bob.clone(), amount: "1".into(), memo: String::new(), rpc: rpc.into(), pin }, &dir);
        // The honest local node: signed and finalized.
        send(&honest, ChainPin::default()).unwrap();
        assert_eq!(chain.lock().state.account(&bob).balance, DROPS);

        // A node that answers as mainnet (the exploit: a testnet send becomes a mainnet transfer).
        let (liar, posts) = rt.block_on(proxy(honest.clone(), |mut s| {
            s["chain_id"] = MAINNET_CHAIN_ID.into();
            s
        }));
        let err = send(&liar, ChainPin::default()).unwrap_err().to_string();
        assert!(err.contains("mainnet") && err.contains("Nothing was signed"), "{err}");
        assert_eq!(posts.load(Ordering::SeqCst), 0);

        // A node that is on the right chain for the first read, and another for the one the tx is built from.
        let reads = Arc::new(AtomicUsize::new(0));
        let r = reads.clone();
        let (switcher, posts) = rt.block_on(proxy(honest.clone(), move |mut s| {
            if r.fetch_add(1, Ordering::SeqCst) > 0 {
                s["chain_id"] = "other".into();
            }
            s
        }));
        let err = send(&switcher, ChainPin::default()).unwrap_err().to_string();
        assert!(err.contains("not `clipin`"), "{err}");
        assert_eq!(posts.load(Ordering::SeqCst), 0);
        // Or on another chain id than the one pinned with --chain-id.
        let err = send(&honest, ChainPin { chain_id: Some("blockula-testnet-2".into()), ..Default::default() }).unwrap_err().to_string();
        assert!(err.contains("not `blockula-testnet-2`"), "{err}");

        // A custom network is pinned on first use and remembered: the same node later serving
        // another genesis is refused, across commands.
        let forked = Arc::new(AtomicBool::new(false));
        let f = forked.clone();
        let (node, posts) = rt.block_on(proxy(honest.clone(), move |mut s| {
            if f.load(Ordering::SeqCst) {
                s["genesis_hash"] = "ee".repeat(32).into();
            }
            s
        }));
        let custom = ChainPin { network: Some("custom".into()), ..Default::default() };
        send(&node, custom.clone()).unwrap();
        assert_eq!(posts.load(Ordering::SeqCst), 1);
        let known: Value = serde_json::from_slice(&std::fs::read(dir.join(KNOWN_CHAINS_FILE)).unwrap()).unwrap();
        assert_eq!(known[format!("custom {node}")]["chainId"], "clipin");
        forked.store(true, Ordering::SeqCst);
        let err = send(&node, custom).unwrap_err().to_string();
        assert!(err.contains("starts from genesis") && err.contains(KNOWN_CHAINS_FILE), "{err}");
        assert_eq!(posts.load(Ordering::SeqCst), 1, "nothing more was sent");
        assert_eq!(chain.lock().state.account(&bob).balance, 2 * DROPS);
        drop(rt);
        std::fs::remove_dir_all(root).ok();
    }

    /// A node in front of `honest` that rewrites every GET's JSON answer with `f(path, answer)` and
    /// keeps the bodies of the txs posted through it.
    async fn liar(honest: String, f: impl Fn(&str, Value) -> Value + Clone + Send + Sync + 'static) -> (String, std::sync::Arc<parking_lot::Mutex<Vec<Value>>>) {
        let posted = std::sync::Arc::new(parking_lot::Mutex::new(vec![]));
        let kept = posted.clone();
        let app = axum::Router::new().fallback(move |req: axum::extract::Request| {
            let (h, f, kept) = (honest.clone(), f.clone(), kept.clone());
            async move {
                let (parts, body) = req.into_parts();
                let body = axum::body::to_bytes(body, 1 << 24).await.unwrap();
                if parts.method == axum::http::Method::POST {
                    kept.lock().push(serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null));
                }
                let res = reqwest::Client::new().request(parts.method.clone(), format!("{h}{}", parts.uri)).header("content-type", "application/json").body(body).send().await.unwrap();
                let status = res.status();
                let text = res.text().await.unwrap();
                let text = match (parts.method == axum::http::Method::GET, serde_json::from_str::<Value>(&text)) {
                    (true, Ok(v)) => f(parts.uri.path(), v).to_string(),
                    _ => text,
                };
                (status, text)
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, posted)
    }

    /// Audit gap-7-1, gap-7-3, gap-7-5 through the CLI, against a real node behind a lying one: a
    /// name is signed as the name (no node resolves it); a hunt's expiry counts the pinned
    /// genesis's block time from the status the tx is signed against, whatever the node says its
    /// block time or a lone status read's height is; a node reporting a huge proposal deposit is
    /// refused unless the deposit is given.
    #[test]
    fn the_cli_doesnt_take_the_nodes_word_for_names_expiries_or_deposits() {
        use crate::validator_cli::{wallet, WalletCmd};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let root = std::env::temp_dir().join(format!("blockula-clilie-{}-{}", crate::chain::now_ms(), std::process::id()));
        let homes = crate::chain::Chain::summon(&root, &crate::chain::SummonOpts::new("clilie", 1)).unwrap();
        let chain = Arc::new(parking_lot::Mutex::new(crate::chain::Chain::open(&homes[0]).unwrap()));
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let honest = rt.block_on(async {
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
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        });
        let dir = root.join("cli");
        wallet(WalletCmd::New { rpc: None, prefix: None }, &dir).unwrap();
        wallet(WalletCmd::Faucet { rpc: honest.clone() }, &dir).unwrap();
        let me = crate::validator_cli::wallet_keys(&dir).unwrap().address();
        for _ in 0..100 {
            if chain.lock().state.account(&me).balance > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // The liar says blocks take 1 ms (so "30 days" would be 2.6 billion blocks), answers every
        // other status read a year ahead, and points every name at Mallory.
        let mallory = Keys::from_seed([66; 32]).address();
        let reads = Arc::new(AtomicUsize::new(0));
        let alternate = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (r, a, m) = (reads.clone(), alternate.clone(), mallory.clone());
        let (node, posted) = rt.block_on(liar(honest.clone(), move |path, mut v| {
            if path == "/api/status" {
                v["block_time_ms"] = 1.into();
                if a.load(Ordering::SeqCst) && r.fetch_add(1, Ordering::SeqCst) % 2 == 1 {
                    v["height"] = (v["height"].as_u64().unwrap() + 31_536_000).into();
                }
            }
            if path.starts_with("/api/account/") && !path.contains("boo1") {
                v["account"]["address"] = m.clone().into();
            }
            if path == "/api/proposals" {
                v["params"]["proposal_deposit"] = (999_000 * DROPS).to_string().into();
            }
            v
        }));
        let signer = crate::contrib_cli::Signer { key: None, from: None, rpc: node.clone(), pin: ChainPin::default() };
        // A send to a name: the name is what's signed (here nobody owns it, so the chain refuses it).
        let err = wallet(WalletCmd::Send { to: "Count.boo".into(), amount: "1".into(), memo: String::new(), rpc: node.clone(), pin: ChainPin::default() }, &dir).unwrap_err().to_string();
        assert!(err.contains("no one has claimed count.boo"), "{err}");
        assert!(posted.lock().iter().all(|t| t["action"]["to"] != mallory.as_str()), "the node's answer for the name was never signed");
        assert_eq!(posted.lock().last().unwrap()["action"]["to"], "count.boo");
        // A 30-day hunt: 30 × 86,400 blocks (the genesis's 1 s blocks) after the height the tx
        // was signed against.
        alternate.store(true, Ordering::SeqCst);
        let hunt = crate::contrib_cli::HuntCmd::Create { repo: "jackdishman/blockula".into(), issue: 7, amount: "10".into(), title: "t".into(), days: 30, signer: signer.clone() };
        for _ in 0..2 {
            let before = posted.lock().len();
            match crate::contrib_cli::hunt(hunt_again(&hunt), &dir) {
                Ok(()) => {}
                // The lone-read year-ahead lie lands on the signing read: its valid_until is refused.
                Err(e) => assert!(e.to_string().contains("too far ahead") || e.to_string().contains("valid_until"), "{e}"),
            }
            let tx = posted.lock()[before..].last().cloned().unwrap();
            let (expires, valid_until) = (tx["action"]["expires_at"].as_u64().unwrap(), tx["valid_until"].as_u64().unwrap());
            assert_eq!(expires - (valid_until - 600), 30 * 86_400, "one status read, the genesis's block time: {tx}");
            // One more read, so the next attempt signs against the other kind of answer.
            rt.block_on(reqwest::get(format!("{node}/api/status"))).unwrap();
        }
        assert!(chain.lock().state.hunts.values().any(|h| h.creator == me), "the honest attempt went through");
        // gov propose without --deposit: the node's 999,000 FANG is refused before signing.
        let before = posted.lock().len();
        let meta = crate::gov_cli::Meta { title: "t".into(), url: String::new(), deposit: None, max_deposit: "1000".into(), signer: signer.clone() };
        let err = crate::gov_cli::gov(crate::gov_cli::GovCmd::Propose { kind: crate::gov_cli::KindCmd::Text { meta } }, &dir).unwrap_err().to_string();
        assert!(err.contains("more than --max-deposit"), "{err}");
        assert_eq!(posted.lock().len(), before, "nothing signed");
        // A node serving a genesis with another block time can't pass it off: it doesn't hash to the pin.
        let (forger, _) = rt.block_on(liar(honest.clone(), |path, mut v| {
            if path == "/api/genesis" {
                v["block_time_ms"] = 1.into();
            }
            v
        }));
        let err = rt.block_on(async { Remote::connect_in(&dir, &forger, &ChainPin::default()).await.unwrap().blocks_per_day().await }).unwrap_err().to_string();
        assert!(err.contains("not the pinned"), "{err}");
        assert_eq!(rt.block_on(async { Remote::connect_in(&dir, &honest, &ChainPin::default()).await.unwrap().blocks_per_day().await }).unwrap(), 86_400);
        drop(rt);
        std::fs::remove_dir_all(root).ok();
    }

    /// Audit gap-7-6 / review-r2-clients-3 through the CLI, against a real node behind lying ones
    /// (the round-2 reviewer's PoCs):
    /// (a) a node that keeps a proposal's kind, title and link true and lies only in its summary
    ///     ("plant garlic" on a 10,000,000 FANG treasury spend): `gov list`/`gov show` describe the
    ///     decoded kind, the summary is at most a labelled note;
    /// (b) a two-faced node, lying on the display read and truthful on the signing read: `gov vote
    ///     --digest` and `multisig approve --digest` with what was shown are refused before
    ///     anything is signed, without --digest they print the content and won't sign unconfirmed,
    ///     and with the true digest they go through.
    #[test]
    fn the_cli_signs_only_what_it_showed() {
        use crate::contrib_cli::{Rpc, Signer};
        use crate::gov_cli::{gov, multisig, multisig_lines, proposal_lines, GovCmd, KindCmd, Meta, MultisigCmd};
        use crate::types::{action_digest, proposal_digest, Action, KeyRole, ProposalKind};
        use crate::validator_cli::{wallet, WalletCmd};
        use std::io::IsTerminal;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let root = std::env::temp_dir().join(format!("blockula-clibind-{}-{}", crate::chain::now_ms(), std::process::id()));
        let mut opts = crate::chain::SummonOpts::new("clibind", 1);
        opts.params.insert("proposal_deposit".into(), DROPS);
        opts.params.insert("governance_spend_height".into(), 0);
        let homes = crate::chain::Chain::summon(&root, &opts).unwrap();
        let chain = Arc::new(parking_lot::Mutex::new(crate::chain::Chain::open(&homes[0]).unwrap()));
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let honest = rt.block_on(async {
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
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        });
        let dir = root.join("cli");
        wallet(WalletCmd::New { rpc: None, prefix: None }, &dir).unwrap();
        wallet(WalletCmd::Faucet { rpc: honest.clone() }, &dir).unwrap();
        let me_keys = crate::validator_cli::wallet_keys(&dir).unwrap();
        let me = me_keys.address();
        for _ in 0..100 {
            if chain.lock().state.account(&me).balance > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let mallory = Keys::from_seed([66; 32]).address();
        let signer = |rpc: &str| Signer { key: None, from: None, rpc: rpc.into(), pin: ChainPin::default() };
        // The real proposal: 10,000,000 FANG from the treasury to Mallory, titled "Community garden".
        let meta = Meta { title: "Community garden".into(), url: String::new(), deposit: Some("1".into()), max_deposit: "1000".into(), signer: signer(&honest) };
        gov(GovCmd::Propose { kind: KindCmd::Spend { to: mallory.clone(), amount: "10000000".into(), meta } }, &dir).unwrap();
        let id = rt.block_on(get(&honest, "/proposals")).unwrap()["proposals"][0]["id"].as_u64().unwrap();
        let drain = ProposalKind::TreasurySpend { to: mallory.clone(), amount: 10_000_000 * DROPS };
        let truth = proposal_digest(&drain, "Community garden", "");

        // (a) The summary-only lie.
        let garlic = "plant garlic in the community garden (no funds move)";
        let (liar1, _) = rt.block_on(liar(honest.clone(), move |path, mut v| {
            if path == "/api/proposals" {
                for p in v["proposals"].as_array_mut().into_iter().flatten() {
                    p["summary"] = garlic.into();
                }
            } else if path.starts_with("/api/proposal/") {
                v["summary"] = garlic.into();
            }
            v
        }));
        for p in [rt.block_on(get(&liar1, "/proposals")).unwrap()["proposals"][0].clone(), rt.block_on(get(&liar1, &format!("/proposal/{id}"))).unwrap()] {
            assert_eq!(p["summary"], garlic, "the node lies");
            let lines = proposal_lines(&p, false);
            assert!(lines.contains(&format!("   does: Pay 10,000,000 FANG from the treasury to {mallory}")), "{lines:?}");
            assert!(lines.iter().any(|l| l.starts_with(&format!("   digest: {truth}"))), "{lines:?}");
            assert!(!lines.join("\n").contains("garlic"), "gov list shows what the vote binds, not the summary: {lines:?}");
            assert!(proposal_lines(&p, true).iter().filter(|l| l.contains("garlic")).all(|l| l.starts_with("   the node's own note (its text, not bound by any vote)")));
        }
        gov(GovCmd::Show { id, rpc: Rpc { rpc: liar1.clone() } }, &dir).unwrap();
        gov(GovCmd::List { status: None, rpc: Rpc { rpc: liar1 } }, &dir).unwrap();

        // (b) Two-faced: the list (what `gov list` shows) says it's a harmless text proposal; the
        // single read `gov vote` makes tells the truth.
        let (two, posted) = rt.block_on(liar(honest.clone(), |path, mut v| {
            if path == "/api/proposals" {
                for p in v["proposals"].as_array_mut().into_iter().flatten() {
                    p["kind"] = serde_json::json!({ "type": "text" });
                }
            }
            v
        }));
        let shown = rt.block_on(get(&two, "/proposals")).unwrap()["proposals"][0].clone();
        let shown_lines = proposal_lines(&shown, false);
        assert!(shown_lines[1].contains("a text proposal"), "{shown_lines:?}");
        let lie = proposal_digest(&ProposalKind::Text {}, "Community garden", "");
        assert!(shown_lines.iter().any(|l| l.starts_with(&format!("   digest: {lie}"))));
        let vote = |side: &str, digest: Option<String>| gov(GovCmd::Vote { id, side: side.into(), amount: "1".into(), conviction: 6, digest, yes: false, signer: signer(&two) }, &dir);
        let err = vote("aye", Some(lie)).unwrap_err().to_string();
        assert!(err.contains(&format!("proposal #{id} doesn't read the way it did when you were shown it")) && err.contains("Nothing was signed"), "{err}");
        if !std::io::stdin().is_terminal() {
            let err = vote("aye", None).unwrap_err().to_string();
            assert!(err.contains("Vote aye on exactly this") && err.contains("pass --yes"), "without --digest it asks first: {err}");
        }
        assert!(posted.lock().is_empty(), "nothing signed for content the voter wasn't shown");
        // With the digest of what they saw (here the truth, shown by `gov show`), it's signed.
        vote("nay", Some(truth.clone())).unwrap();
        assert_eq!(posted.lock().len(), 1);
        assert_eq!(posted.lock()[0]["action"]["proposal_digest"], truth.as_str());

        // (b) for a 2-of-2 multisig: the wallet's own key and K2, which proposes a 50,000,000-drop
        // (50 FANG) transfer to Mallory, standing in for "the foundation drain".
        let k2 = Keys::from_seed([77; 32]);
        let k2_file = root.join("k2.key");
        std::fs::write(&k2_file, hex::encode([77u8; 32])).unwrap();
        rt.block_on(async {
            let remote = Remote::connect_in(&dir, &honest, &ChainPin::default()).await.unwrap();
            let add = crate::types::add_key(&k2, &remote.chain_id, &remote.genesis_hash, &me, "k2", KeyRole::Owner);
            crate::validator_cli::send_memo(&remote, &me_keys, &me, add, String::new()).await.unwrap();
        });
        let owners = vec![crate::types::key_id(&me_keys.public_key()), crate::types::key_id(&k2.public_key())];
        multisig(MultisigCmd::Create { owners, threshold: 2, signer: signer(&honest) }, &dir).unwrap();
        let drain_action = Action::Transfer { to: mallory.clone(), amount: 50 * DROPS };
        let by_k2 = Signer { key: Some(k2_file), from: Some(me.clone()), rpc: honest.clone(), pin: ChainPin::default() };
        multisig(MultisigCmd::Propose { action: serde_json::to_string(&drain_action).unwrap(), signer: by_k2 }, &dir).unwrap();
        let ms = rt.block_on(get(&honest, &format!("/multisig/{me}"))).unwrap();
        let ms_id = ms["pending"][0]["id"].as_u64().unwrap();
        assert_eq!(serde_json::from_value::<Action>(ms["pending"][0]["action"].clone()).unwrap(), drain_action);
        // The co-signer's node: the first read of the multisig (what `multisig show` displays)
        // shows a routine 5 FANG grant, every later one the truth.
        let routine = Action::Transfer { to: "grants.boo".into(), amount: 5 * DROPS };
        let reads = Arc::new(AtomicUsize::new(0));
        let (r, shown_action) = (reads.clone(), serde_json::to_value(&routine).unwrap());
        let (two_ms, posted_ms) = rt.block_on(liar(honest.clone(), move |path, mut v| {
            if path.starts_with("/api/multisig/") && r.fetch_add(1, Ordering::SeqCst) == 0 {
                v["pending"][0]["action"] = shown_action.clone();
                v["pending"][0]["summary"] = "pay the monthly grants".into();
            }
            v
        }));
        let display = multisig_lines(&rt.block_on(get(&two_ms, &format!("/multisig/{me}"))).unwrap(), &me);
        assert!(display.iter().any(|l| l.contains("does: Send 5 FANG to grants.boo")), "{display:?}");
        let shown_digest = action_digest(&routine);
        assert!(display.iter().any(|l| l.starts_with(&format!("      digest: {shown_digest}"))));
        let approve = |digest: Option<String>| multisig(MultisigCmd::Approve { id: ms_id, digest, yes: false, signer: signer(&two_ms) }, &dir);
        let err = approve(Some(shown_digest)).unwrap_err().to_string();
        assert!(err.contains(&format!("multisig action #{ms_id} doesn't read the way it did")), "{err}");
        if !std::io::stdin().is_terminal() {
            let err = approve(None).unwrap_err().to_string();
            assert!(err.contains("Approve exactly this?") && err.contains("pass --yes"), "{err}");
        }
        assert!(posted_ms.lock().is_empty(), "no approval signed");
        assert_eq!(chain.lock().state.account(&mallory).balance, 0, "the drain didn't run");
        // Approving what it really is (its digest from an honest `multisig show`) runs it.
        approve(Some(action_digest(&drain_action))).unwrap();
        assert_eq!(chain.lock().state.account(&mallory).balance, 50 * DROPS);
        drop(rt);
        std::fs::remove_dir_all(root).ok();
    }

    /// The Séance flavour of review-r2-clients-3: `contrib seance` shows the pairs (and their
    /// digest) from one read, `contrib vote` binds another. A node that flips the pairs between
    /// them is refused when the juror passes the digest they were shown, and without it the
    /// juror sees the pairs the vote binds before anything is signed.
    #[test]
    fn contrib_vote_binds_the_pairs_the_juror_was_shown() {
        use crate::contrib_cli::{contrib, pairs_digest, pair_lines, seat_pairs, ContribCmd, Signer};
        use crate::validator_cli::{wallet, WalletCmd};
        use std::io::IsTerminal;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let dir = std::env::temp_dir().join(format!("blockula-cliseance-{}-{}", crate::chain::now_ms(), std::process::id()));
        wallet(WalletCmd::New { rpc: None, prefix: None }, &dir).unwrap();
        let me = crate::validator_cli::wallet_keys(&dir).unwrap().address();
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let reads = Arc::new(AtomicUsize::new(0));
        let posts = Arc::new(AtomicUsize::new(0));
        let seance = |flipped: bool, me: &str| {
            let pair = if flipped { json!([[2, 1]]) } else { json!([[1, 2]]) };
            json!({ "id": 7, "phase": "commit", "claims": [{ "id": 1, "repo": "a/b", "pr": 10 }, { "id": 2, "repo": "c/d", "pr": 20 }], "jurors": [{ "juror": { "address": me }, "assigned_pairs": pair, "pairs": 1 }] })
        };
        let url = rt.block_on(async {
            use axum::{routing::get, Json, Router};
            let (r, p, m) = (reads.clone(), posts.clone(), me.clone());
            let app = Router::new()
                .route("/api/status", get(|| async { Json(json!({ "chain_id": "seance-test", "genesis_hash": "11".repeat(32), "address_prefix": "boo", "height": 5 })) }))
                // The first read (the display) shows [(1, 2)]; every later one (the vote's) [(2, 1)].
                .route("/api/seance/7", get(move || {
                    let flipped = r.fetch_add(1, Ordering::SeqCst) > 0;
                    let v = seance(flipped, &m);
                    async move { Json(v) }
                }))
                .fallback(move |req: axum::extract::Request| {
                    if req.method() == axum::http::Method::POST {
                        p.fetch_add(1, Ordering::SeqCst);
                    }
                    async { (axum::http::StatusCode::NOT_FOUND, Json(json!({ "error": "fake node: no such route" }))) }
                });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            url
        });
        let shown_view = rt.block_on(get(&url, "/seance/7")).unwrap();
        let shown = seat_pairs(&shown_view, 7, &me).unwrap();
        assert_eq!(pair_lines(&shown), vec!["   1. a = claim #1 a/b#10 (https://github.com/a/b/pull/10)\n      b = claim #2 c/d#20 (https://github.com/c/d/pull/20)".to_string()]);
        let shown_digest = pairs_digest(7, &me, &shown);
        let signer = Signer { key: None, from: None, rpc: url.clone(), pin: ChainPin::default() };
        // The juror prefers claim #1 ("a" in what they were shown).
        let vote = |digest: Option<String>| contrib(ContribCmd::Vote { seance: 7, votes: "a".into(), spam: vec![], digest, yes: false, signer: signer.clone() }, &dir);
        let err = vote(Some(shown_digest)).unwrap_err().to_string();
        assert!(err.contains("Séance #7's pairs for you doesn't read the way it did") && err.contains("Nothing was signed"), "{err}");
        if !std::io::stdin().is_terminal() {
            let err = vote(None).unwrap_err().to_string();
            assert!(err.contains("Seal this judgment on exactly these pairs?") && err.contains("pass --yes"), "{err}");
        }
        // With the digest of the pairs it would really bind, the check passes (the fake node then
        // can't serve the rest of a signing round, which is all that fails).
        let now = pairs_digest(7, &me, &seat_pairs(&seance(true, &me), 7, &me).unwrap());
        let err = vote(Some(now)).unwrap_err().to_string();
        assert!(!err.contains("doesn't read the way"), "{err}");
        assert_eq!(posts.load(Ordering::SeqCst), 0, "no judgment was sealed on flipped pairs");
        drop(rt);
        std::fs::remove_dir_all(dir).ok();
    }

    fn hunt_again(h: &crate::contrib_cli::HuntCmd) -> crate::contrib_cli::HuntCmd {
        match h {
            crate::contrib_cli::HuntCmd::Create { repo, issue, amount, title, days, signer } => crate::contrib_cli::HuntCmd::Create { repo: repo.clone(), issue: *issue, amount: amount.clone(), title: title.clone(), days: *days, signer: signer.clone() },
            _ => unreachable!(),
        }
    }

    const DROPS: u64 = crate::types::DROPS_PER_FANG;
}
