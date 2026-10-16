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
//! Names (`alice.boo`, `--from`) are still resolved by the node: the CLI prints what it resolved.

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

    /// `keys`' own account on this chain.
    pub fn address(&self, keys: &Keys) -> String {
        keys.address_with(&self.prefix)
    }

    /// An account by address, or a name resolved by this node (printed, since the node could lie).
    pub async fn resolve(&self, who: &str) -> Result<String> {
        if crate::crypto::is_address(who) {
            return Ok(who.to_string());
        }
        let addr = get(&self.rpc, &format!("/account/{who}")).await?["account"]["address"].as_str().map(String::from).ok_or_else(|| anyhow!("can't resolve {who}"))?;
        ensure!(crate::crypto::is_address(&addr), "{} resolved {who} to {addr:?}, which isn't an address", self.rpc);
        eprintln!("   {who} → {addr} (as {} resolves it)", self.rpc);
        Ok(addr)
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

    const DROPS: u64 = crate::types::DROPS_PER_FANG;
}
