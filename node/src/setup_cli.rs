//! `blockula validator setup`: a validator on this machine for a chain someone else runs, in one
//! command. It asks for the chain's node and genesis hash (or takes them as flags), works out how
//! the chain's validators reach each other (a tailnet, a home network or the open internet) and
//! this machine's address on it, then does every step of docs/BECOME-A-VALIDATOR.md: the
//! validator's home and key, a full node for its public HTTP address on an internet chain, the
//! bond from the faucet (or waits for the owner to send it), the node as a background service that
//! starts with the computer (launchd on a Mac, systemd on Linux), and `register`. It ends with the
//! one command the chain's owner runs to let it in.
//!
//! Run it again at any time: it picks up where it stopped, and never makes a second key or starts
//! a second copy of one.

use crate::chain::Genesis;
use crate::launch_cli::{Machine, Prompter};
use crate::types::{fmt_fang, DROPS_PER_FANG};
use crate::validator_cli::{self as vcli, get, private_host, rt, ValidatorCmd, WalletCmd};
use anyhow::{anyhow, bail, ensure, Context, Result};
use serde_json::Value;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// `blockula validator setup`'s answers, as flags. Anything left out is asked, or with `--yes`
/// takes its default.
#[derive(clap::Args, Debug, Default, Clone)]
pub struct SetupArgs {
    /// Any node of the chain (http://… or https://…), as its owner sent it
    #[arg(long)]
    pub node: Option<String>,
    /// The chain's genesis hash (64 hex characters), from its owner: the node must report it
    #[arg(long)]
    pub genesis_hash: Option<String>,
    /// This machine's address as the chain's other validators reach it: its Tailscale or home
    /// network IP (default: detected), or on an internet chain a DNS name or public IP
    #[arg(long)]
    pub host: Option<String>,
    /// Other validators reach this machine through a relay server that forwards TCP 30300 and 7171
    /// (docs/BECOME-A-VALIDATOR.md, Path C): --host is the relay's address
    #[arg(long)]
    pub relay: bool,
    /// The validator's home (default: ~/.blockula/<chain id>-validator)
    #[arg(long)]
    pub home: Option<PathBuf>,
    /// Self-bond, in the chain's units (default: the chain's minimum)
    #[arg(long)]
    pub bond: Option<String>,
    /// Don't install a background service: run the node from this terminal (Ctrl-C stops it)
    #[arg(long)]
    pub foreground: bool,
    /// Don't ask: use the flags and the defaults (needs --node and --genesis-hash)
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Print what it would do, and do nothing
    #[arg(long)]
    pub dry_run: bool,
}

/// How a chain's validators reach each other, from the addresses they publish.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reach {
    /// One machine (loopback addresses): a second validator can only run beside the first
    Local,
    /// One home network (private addresses)
    Home,
    /// A Tailscale tailnet (100.64.0.0/10 addresses, or *.ts.net names)
    Tailnet,
    /// The open internet (public IPs or DNS names)
    Internet,
}

impl Reach {
    pub fn label(self) -> &'static str {
        match self {
            Reach::Local => "one machine",
            Reach::Home => "a home network",
            Reach::Tailnet => "Tailscale",
            Reach::Internet => "the open internet",
        }
    }

    /// How the validators publishing `addr` (`host:port` or a URL) are reached.
    pub fn of_addr(addr: &str) -> Reach {
        let host = host_of(addr).to_ascii_lowercase();
        match private_host(addr) {
            _ if host.trim_end_matches('.').ends_with(".ts.net") => Reach::Tailnet,
            Some("a shared (carrier-grade NAT) address") => Reach::Tailnet,
            Some("a loopback address") => Reach::Local,
            Some(_) if host == "localhost" || host.ends_with(".localhost") => Reach::Local,
            Some(_) => Reach::Home,
            None => Reach::Internet,
        }
    }

    /// The chain's: the widest of its validators' addresses (one public validator makes it an
    /// internet chain, whose newcomers need public addresses too).
    pub fn of_chain<'a>(addrs: impl IntoIterator<Item = &'a str>) -> Reach {
        addrs.into_iter().map(Reach::of_addr).max().unwrap_or(Reach::Local)
    }
}

/// The host of `host:port`, `[v6]:port` or a URL.
fn host_of(addr: &str) -> &str {
    let a = addr.trim();
    let rest = a.strip_prefix("https://").or_else(|| a.strip_prefix("http://")).unwrap_or(a);
    let hostport = rest.split(['/', '?', '#']).next().unwrap_or_default();
    match hostport.strip_prefix('[') {
        Some(r) => r.split(']').next().unwrap_or_default(),
        None if hostport.parse::<std::net::IpAddr>().is_ok() => hostport,
        None => hostport.rsplit_once(':').map_or(hostport, |(h, _)| h),
    }
}

/// How the node keeps running once setup is done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceKind {
    /// A launchd agent (macOS): starts when you log in, restarts if it crashes, keeps the Mac awake
    Launchd,
    /// A systemd user service (Linux): starts with the computer (with lingering on), restarts if it crashes
    Systemd,
    /// From this terminal, until Ctrl-C
    Foreground,
}

impl ServiceKind {
    /// What this machine can run: launchd on a Mac, systemd's user manager on Linux.
    pub fn detect() -> ServiceKind {
        if cfg!(target_os = "macos") {
            ServiceKind::Launchd
        } else if cfg!(target_os = "linux") && std::process::Command::new("systemctl").args(["--user", "show-environment"]).output().is_ok_and(|o| o.status.success()) {
            ServiceKind::Systemd
        } else {
            ServiceKind::Foreground
        }
    }

    fn label(self) -> &'static str {
        match self {
            ServiceKind::Launchd => "in the background (launchd): it starts when you log in, restarts if it stops by accident, and keeps the Mac from idle sleep",
            ServiceKind::Systemd => "in the background (systemd --user): it starts with the computer and restarts if it stops by accident",
            ServiceKind::Foreground => "from this terminal: Ctrl-C stops it",
        }
    }
}

/// What the chain's node says about it.
#[derive(Clone, Debug)]
pub struct ChainInfo {
    pub node: String,
    pub status: Value,
    pub genesis: Genesis,
    pub validators: Value,
}

impl ChainInfo {
    pub fn fetch(node: &str) -> Result<ChainInfo> {
        let rt = rt()?;
        let status = rt.block_on(get(node, "/status")).with_context(|| format!("{node} doesn't answer as a Blockula node"))?;
        let genesis: Genesis = serde_json::from_value(rt.block_on(get(node, "/genesis"))?)?;
        genesis.check_prefix().context("genesis")?;
        let validators = rt.block_on(get(node, "/validators"))?;
        Ok(ChainInfo { node: node.to_string(), status, genesis, validators })
    }

    pub fn chain_id(&self) -> &str {
        &self.genesis.chain_id
    }

    /// The chain's name, or its id when it has none.
    pub fn name(&self) -> String {
        self.status["name"].as_str().map(String::from).unwrap_or_else(|| self.genesis.chain_id.clone())
    }

    pub fn token(&self) -> String {
        self.status["token"].as_str().unwrap_or("FANG").to_string()
    }

    pub fn reported_hash(&self) -> String {
        self.status["genesis_hash"].as_str().unwrap_or_default().to_ascii_lowercase()
    }

    /// Every p2p address the chain's validators publish (genesis and active).
    pub fn validator_addrs(&self) -> Vec<String> {
        let mut out: Vec<String> = self.genesis.validators.iter().map(|v| v.p2p_addr.clone()).collect();
        out.extend(self.validators["active"].as_array().into_iter().flatten().filter_map(|v| v["p2p_addr"].as_str().map(String::from)));
        out
    }

    pub fn reach(&self) -> Reach {
        Reach::of_chain(self.validator_addrs().iter().map(String::as_str))
    }

    pub fn min_bond(&self) -> u64 {
        self.validators["min_bond"].as_str().and_then(|s| s.parse().ok()).or_else(|| self.genesis.chain_params().ok().map(|c| c.min_self_bond)).unwrap_or(0)
    }

    pub fn active_count(&self) -> usize {
        self.validators["active"].as_array().map_or(0, Vec::len)
    }
}

/// What `setup` will do.
#[derive(Clone, Debug, PartialEq)]
pub struct SetupPlan {
    pub node: String,
    pub chain_id: String,
    pub chain_name: String,
    pub token: String,
    pub genesis_hash: String,
    pub faucet: bool,
    pub reach: Reach,
    /// This machine's address as the other validators reach it (the relay's, with `relay`).
    pub host: String,
    pub relay: bool,
    pub p2p_port: u16,
    /// The validator's own HTTP API.
    pub http_port: u16,
    /// On an internet chain, the full node this machine runs as the validator's public HTTP address.
    pub full_port: Option<u16>,
    pub home: PathBuf,
    pub full_home: Option<PathBuf>,
    pub service: ServiceKind,
    /// `--bond`, in the chain's units, as given.
    pub bond: Option<String>,
    /// The chain's minimum self-bond, in drops.
    pub min_bond: u64,
}

/// `[v6]` for an IPv6 host, else the host.
fn bracket(host: &str) -> String {
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

impl SetupPlan {
    pub fn p2p_addr(&self) -> String {
        format!("{}:{}", bracket(&self.host), self.p2p_port)
    }

    /// The HTTP address the chain publishes for this validator: its own API on a tailnet or a home
    /// network (as `blockula create`'s validators publish theirs), the full node's URL on the
    /// internet (a validator's own API stays private there).
    pub fn http_addr(&self) -> String {
        match self.full_port {
            Some(p) => format!("http://{}:{p}", bracket(&self.host)),
            None => format!("{}:{}", bracket(&self.host), self.http_port),
        }
    }

    /// The validator's own API, from this machine.
    pub fn local_rpc(&self) -> String {
        format!("http://127.0.0.1:{}", self.http_port)
    }

    fn any_iface(&self) -> &'static str {
        if self.host.contains(':') {
            "::"
        } else {
            "0.0.0.0"
        }
    }

    /// `blockula run`'s arguments for the validator.
    pub fn validator_args(&self) -> Vec<String> {
        let mut a = vec!["run".to_string(), "--home".into(), self.home.display().to_string(), "--port".into(), self.http_port.to_string()];
        match self.reach {
            Reach::Local => {}
            // Others reach its API too, as on a chain `blockula create` made.
            Reach::Home | Reach::Tailnet => a.extend(["--bind".into(), self.any_iface().into()]),
            // Its API stays on this machine; the full node serves the chain.
            Reach::Internet => a.extend(["--p2p-bind".into(), self.any_iface().into()]),
        }
        if self.relay {
            // Every connection comes from the relay's one address (docs/BECOME-A-VALIDATOR.md, Path C).
            a.extend(["--p2p-preset".into(), "public-nat".into()]);
        }
        a
    }

    /// `blockula run`'s arguments for the full node, on an internet chain.
    pub fn full_args(&self) -> Option<Vec<String>> {
        let (home, port) = (self.full_home.as_ref()?, self.full_port?);
        Some(vec!["run".into(), "--home".into(), home.display().to_string(), "--bind".into(), self.any_iface().into(), "--port".into(), port.to_string()])
    }

    /// The node's services: the validator's, and on an internet chain the full node's.
    pub fn units(&self) -> Vec<Unit> {
        let id = unit_slug(&self.chain_id);
        let mut out = vec![Unit { label: format!("app.blockula.{id}.validator"), systemd: format!("blockula-{id}-validator.service"), description: format!("Blockula validator for {}", self.chain_name), args: self.validator_args(), log: self.home.join("node.log") }];
        if let (Some(args), Some(home)) = (self.full_args(), &self.full_home) {
            out.push(Unit { label: format!("app.blockula.{id}.full"), systemd: format!("blockula-{id}-full.service"), description: format!("Blockula full node for {}", self.chain_name), args, log: home.join("node.log") });
        }
        out
    }

    fn bond_display(&self) -> String {
        let drops = self.bond.as_deref().and_then(|b| vcli::parse_fang(b).ok()).unwrap_or(self.min_bond);
        in_token(drops, &self.token)
    }

    /// The review, one line per choice.
    pub fn summary(&self) -> Vec<(&'static str, String)> {
        let mut out = vec![("Chain", format!("{} ({}), id {}", self.chain_name, self.token, self.chain_id))];
        out.push(("Its node", self.node.clone()));
        out.push(("Reached over", self.reach.label().to_string()));
        out.push(("This machine", if self.relay { format!("{} (a relay)", self.host) } else { self.host.clone() }));
        out.push(("p2p", self.p2p_addr()));
        out.push(("Public HTTP", match self.full_port {
            Some(_) => format!("{} (a full node on this machine)", self.http_addr()),
            None => self.http_addr(),
        }));
        if self.reach == Reach::Internet {
            out.push(("Open to the internet", format!("TCP {} and {}{}", self.p2p_port, self.full_port.unwrap_or(7171), if self.relay { " on the relay" } else { ": forward them to this machine in your router" })));
        }
        out.push(("Bond", format!("{}{}", self.bond_display(), if self.faucet { ", from the faucet" } else { ": the chain has no faucet, so its owner sends it" })));
        out.push(("Home", self.home.display().to_string()));
        out.push(("Runs", self.service.label().to_string()));
        out
    }
}

/// An amount of drops as a number of units ("50", "1.5").
fn amount(drops: u64) -> String {
    let s = fmt_fang(drops);
    s.rsplit_once(' ').map_or(s.clone(), |(n, _)| n.to_string())
}

/// An amount of drops in the chain's ticker.
fn in_token(drops: u64, token: &str) -> String {
    format!("{} {token}", amount(drops))
}

/// A chain id as it appears in a service's name.
fn unit_slug(chain_id: &str) -> String {
    chain_id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c.to_ascii_lowercase() } else { '-' }).collect()
}

/// The first port from `from` (up to 100 on) that nothing listens on, here or on any interface,
/// and isn't in `taken`.
pub fn first_free_port(from: u16, taken: &[u16]) -> Result<u16> {
    (from..from.saturating_add(100)).find(|p| !taken.contains(p) && std::net::TcpListener::bind(("0.0.0.0", *p)).is_ok() && std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok()).ok_or_else(|| anyhow!("no free port from {from} to {}", from.saturating_add(99)))
}

/// A node URL as typed: `http://` added when there's no scheme, no trailing slash.
pub fn normalize_node(s: &str) -> Result<String> {
    let s = s.trim().trim_end_matches('/');
    ensure!(!s.is_empty(), "enter the chain's node URL, as its owner sent it (http://… or https://…)");
    let url = if s.starts_with("http://") || s.starts_with("https://") { s.to_string() } else { format!("http://{s}") };
    reqwest::Url::parse(&url).map_err(|e| anyhow!("{url} isn't a URL: {e}"))?;
    Ok(url)
}

fn check_hash(given: &str, reported: &str) -> Result<String> {
    let h = given.trim().to_ascii_lowercase();
    ensure!(h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()), "a genesis hash is 64 hex characters (the chain's owner has it: `blockula create` printed it, and their node's /api/status shows it)");
    ensure!(h == reported, "this node reports genesis {reported}, not {h}: it isn't the chain you were told about. Check the hash and the node with the chain's owner. Nothing was made");
    Ok(h)
}

/// This machine's address for a chain reached over `reach`, as the flag gives it or detected.
fn check_host(h: &str, reach: Reach) -> Result<String> {
    let h = h.trim().trim_start_matches('[').trim_end_matches(']').to_string();
    crate::chain::check_host_port(&format!("{}:1", bracket(&h))).map_err(|_| anyhow!("{h:?} isn't a host: give an IP address or a DNS name, without a port"))?;
    if let (Reach::Internet, Some(why)) = (reach, private_host(&format!("{}:1", bracket(&h)))) {
        bail!("{h} is {why}: the chain's other validators are on the internet and can't reach it. Give this machine's public IP or a DNS name for it (a dynamic DNS name follows a home IP that changes), or a relay's (--relay)");
    }
    Ok(h)
}

/// Works out the plan from the flags, the chain and this machine, asking `p` what the flags leave
/// out (or, without `p`, taking the defaults). `free` picks a free port from the one given.
pub fn plan<R: BufRead, W: Write>(mut p: Option<&mut Prompter<R, W>>, a: &SetupArgs, info: &ChainInfo, m: &Machine, service: ServiceKind, free: &dyn Fn(u16, &[u16]) -> Result<u16>) -> Result<SetupPlan> {
    let reported = info.reported_hash();
    let genesis_hash = match (&a.genesis_hash, p.as_deref_mut()) {
        (Some(h), _) => check_hash(h, &reported)?,
        (None, Some(p)) => p.ask("Its genesis hash (64 hex characters, from the chain's owner)", None, |h| check_hash(h, &reported))?,
        (None, None) => bail!("--genesis-hash is needed: the chain's 64-hex genesis hash, from its owner (it pins the exact chain, so nobody can swap in another)"),
    };
    let reach = info.reach();
    if let Some(p) = p.as_deref_mut() {
        writeln!(p.out, "  {} is reached over {}.", info.name(), reach.label())?;
    }
    let detected = match reach {
        Reach::Local => Some("127.0.0.1".to_string()),
        Reach::Home => m.lan_ip.clone(),
        Reach::Tailnet => m.tailscale_ip.clone(),
        Reach::Internet => None,
    };
    let relay = a.relay;
    let host = match (&a.host, reach, p.as_deref_mut()) {
        (Some(h), _, _) => check_host(h, reach)?,
        (None, Reach::Local, _) => "127.0.0.1".into(),
        (None, Reach::Tailnet, _) if detected.is_none() => bail!("{} runs on a Tailscale tailnet, and Tailscale isn't up on this machine. Install it (https://tailscale.com/download), sign in, and ask the chain's owner to share their machine with you (or invite you to their tailnet); then run this again. Nothing was made", info.name()),
        (None, Reach::Home, _) if detected.is_none() => bail!("{} runs on a home network, and this machine isn't on one: join the same Wi-Fi or LAN as its owner, or pass --host. Nothing was made", info.name()),
        (None, _, Some(p)) if reach == Reach::Internet => {
            writeln!(p.out, "  The other validators dial this machine over the internet, at a DNS name or public IP with TCP 30300 and 7171 forwarded to it (or at a relay's, docs/BECOME-A-VALIDATOR.md Path C).")?;
            p.ask("This machine's public DNS name or IP (or the relay's)", None, |h| check_host(h, reach))?
        }
        (None, Reach::Internet, None) => bail!("--host is needed: this machine's public DNS name or IP (or its relay's, with --relay), where the other validators reach it"),
        (None, _, Some(p)) => {
            let d = detected.clone().unwrap_or_default();
            p.ask("This machine's address on it", Some(&d), |h| check_host(h, reach))?
        }
        (None, _, None) => detected.clone().unwrap_or_default(),
    };
    let relay = match (relay, reach, p.as_deref_mut()) {
        (true, _, _) => {
            ensure!(reach == Reach::Internet, "--relay is for an internet chain: {} is reached over {}", info.name(), reach.label());
            true
        }
        (false, Reach::Internet, Some(p)) if a.host.is_none() => p.confirm("Is that a relay server that forwards to this machine (Path C)?", false)?,
        _ => false,
    };
    let base = crate::cli_net::blockula_dir();
    let home = a.home.clone().unwrap_or_else(|| base.join(format!("{}-validator", info.chain_id())));
    let p2p_port = free(30300, &[])?;
    let http_port = free(7070, &[p2p_port])?;
    let (full_port, full_home) = match reach {
        // On the internet the chain publishes a full node's URL, and the validator's API stays private.
        Reach::Internet => (Some(free(7171, &[p2p_port, http_port])?), Some(base.join(info.chain_id()))),
        _ => (None, None),
    };
    let service = match (service, p) {
        _ if a.foreground => ServiceKind::Foreground,
        (ServiceKind::Foreground, _) => ServiceKind::Foreground,
        (s, Some(p)) => match p.confirm("Run it in the background, and start it again whenever this computer starts?", true)? {
            true => s,
            false => ServiceKind::Foreground,
        },
        (s, None) => s,
    };
    if let Some(b) = &a.bond {
        let drops = vcli::parse_fang(b)?;
        ensure!(drops >= info.min_bond(), "--bond {b} is below the chain's minimum self-bond, {}", in_token(info.min_bond(), &info.token()));
    }
    Ok(SetupPlan {
        node: info.node.clone(),
        chain_id: info.chain_id().to_string(),
        chain_name: info.name(),
        token: info.token(),
        genesis_hash,
        faucet: info.genesis.faucet,
        reach,
        host,
        relay,
        p2p_port,
        http_port,
        full_port,
        home,
        full_home,
        service,
        bond: a.bond.clone(),
        min_bond: info.min_bond(),
    })
}

// ------------------------------------------------------------------------------- services

/// One background service: the validator, or its full node.
#[derive(Clone, Debug, PartialEq)]
pub struct Unit {
    /// launchd's label (`app.blockula.<chain id>.validator`).
    pub label: String,
    /// systemd's unit name (`blockula-<chain id>-validator.service`).
    pub systemd: String,
    pub description: String,
    /// `blockula`'s arguments (`run --home …`).
    pub args: Vec<String>,
    /// Where launchd and a foreground run write its log.
    pub log: PathBuf,
}

/// Exit codes of a node that stopped on purpose (src/upgrade.rs: a halt height, an upgrade it
/// needs, slashing protection, a program fault): starting it again would only stop again, or risk
/// a double-sign, so no service restarts it.
const DELIBERATE_STOPS: [i32; 4] = [10, 11, 12, 13];

/// launchd's agent can't skip exit codes, so a small `sh` runs the node: it holds a no-idle-sleep
/// assertion while the node runs (`caffeinate -w`), passes launchd's stop on to it, and exits 0
/// (no restart) when the node stopped on purpose.
const LAUNCHD_WRAPPER: &str = r#""$0" "$@" &
pid=$!
/usr/bin/caffeinate -i -w "$pid" &
trap 'kill -TERM "$pid" 2>/dev/null' TERM INT
wait "$pid"; c=$?
while kill -0 "$pid" 2>/dev/null; do wait "$pid"; c=$?; done
case "$c" in 10|11|12|13) echo "blockula stopped on purpose (exit $c): not starting it again" >&2; exit 0;; esac
exit "$c""#;

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The launchd agent's plist (`~/Library/LaunchAgents/<label>.plist`).
pub fn launchd_plist(exe: &Path, u: &Unit) -> String {
    let mut args = vec!["/bin/sh".to_string(), "-c".into(), LAUNCHD_WRAPPER.into(), exe.display().to_string()];
    args.extend(u.args.iter().cloned());
    let args: String = args.iter().map(|a| format!("    <string>{}</string>\n", xml(a))).collect();
    let log = xml(&u.log.display().to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- {description}: made by `blockula validator setup` -->
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
{args}  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>ThrottleInterval</key>
  <integer>10</integer>
  <key>SoftResourceLimits</key>
  <dict>
    <key>NumberOfFiles</key>
    <integer>65536</integer>
  </dict>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
        description = xml(&u.description),
        label = xml(&u.label),
    )
}

/// An argument for systemd's ExecStart: quoted, with `\`, `"`, `%` and `$` escaped.
fn systemd_arg(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%").replace('$', "$$"))
}

/// The systemd user unit (`~/.config/systemd/user/<name>`).
pub fn systemd_unit(exe: &Path, u: &Unit) -> String {
    let exec: Vec<String> = std::iter::once(exe.display().to_string()).chain(u.args.iter().cloned()).map(|a| systemd_arg(&a)).collect();
    let stops: Vec<String> = DELIBERATE_STOPS.iter().map(i32::to_string).collect();
    format!(
        "# Made by `blockula validator setup`\n[Unit]\nDescription={}\nWants=network-online.target\nAfter=network-online.target\n\n[Service]\nExecStart={}\nRestart=on-failure\nRestartSec=5\n# A node that stops on purpose (a halt height, an upgrade it needs, slashing protection, a\n# program fault) would only stop again, or risk a double-sign: never restart it.\nRestartPreventExitStatus={}\nLimitNOFILE=65536\n\n[Install]\nWantedBy=default.target\n",
        u.description,
        exec.join(" "),
        stops.join(" ")
    )
}

fn user_home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

fn unit_file(kind: ServiceKind, u: &Unit) -> Option<PathBuf> {
    match kind {
        ServiceKind::Launchd => Some(user_home().join("Library/LaunchAgents").join(format!("{}.plist", u.label))),
        ServiceKind::Systemd => Some(std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| user_home().join(".config")).join("systemd/user").join(&u.systemd)),
        ServiceKind::Foreground => None,
    }
}

fn uid() -> Result<String> {
    let out = std::process::Command::new("id").arg("-u").output()?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn sh(cmd: &str, args: &[&str]) -> Result<std::process::Output> {
    std::process::Command::new(cmd).args(args).output().with_context(|| format!("running {cmd}"))
}

fn must(cmd: &str, args: &[&str]) -> Result<()> {
    let out = sh(cmd, args)?;
    ensure!(out.status.success(), "`{cmd} {}` failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    Ok(())
}

/// Installs and starts `u` as a background service (or starts it again with its new file).
fn install(kind: ServiceKind, exe: &Path, u: &Unit) -> Result<()> {
    let path = unit_file(kind, u).ok_or_else(|| anyhow!("no service to install"))?;
    let body = match kind {
        ServiceKind::Launchd => launchd_plist(exe, u),
        ServiceKind::Systemd => systemd_unit(exe, u),
        ServiceKind::Foreground => unreachable!(),
    };
    std::fs::create_dir_all(path.parent().expect("a directory"))?;
    let changed = std::fs::read_to_string(&path).ok().as_deref() != Some(body.as_str());
    match kind {
        ServiceKind::Launchd => {
            let target = format!("gui/{}", uid()?);
            let service = format!("{target}/{}", u.label);
            if changed {
                // Unload the old file first (fails harmlessly when it isn't loaded).
                let _ = sh("launchctl", &["bootout", &service]);
                std::fs::write(&path, &body)?;
            }
            let loaded = sh("launchctl", &["print", &service])?.status.success();
            if !loaded {
                must("launchctl", &["bootstrap", &target, &path.display().to_string()])?;
            }
            must("launchctl", &["kickstart", &service])?;
        }
        ServiceKind::Systemd => {
            if changed {
                std::fs::write(&path, &body)?;
                must("systemctl", &["--user", "daemon-reload"])?;
            }
            must("systemctl", &["--user", "enable", &u.systemd])?;
            if changed {
                must("systemctl", &["--user", "restart", &u.systemd])?;
            } else {
                must("systemctl", &["--user", "start", &u.systemd])?;
            }
        }
        ServiceKind::Foreground => unreachable!(),
    }
    println!("   ✓ {} runs as {}", u.description, path.display());
    Ok(())
}

/// How to stop, start and read `u`, for the summary and the home's README.md.
fn controls(kind: ServiceKind, exe: &Path, u: &Unit) -> Vec<(&'static str, String)> {
    match (kind, unit_file(kind, u)) {
        (ServiceKind::Launchd, Some(f)) => vec![
            ("Logs", format!("tail -f {}", u.log.display())),
            ("Stop", format!("launchctl bootout gui/$(id -u)/{}", u.label)),
            ("Start", format!("launchctl bootstrap gui/$(id -u) {}", f.display())),
        ],
        (ServiceKind::Systemd, _) => vec![
            ("Logs", format!("journalctl --user -u {} -f", u.systemd)),
            ("Stop", format!("systemctl --user stop {}", u.systemd)),
            ("Start", format!("systemctl --user start {}", u.systemd)),
        ],
        _ => vec![("Logs", format!("tail -f {}", u.log.display())), ("Start", format!("{} {}", exe.display(), u.args.join(" ")))],
    }
}

/// Starts each unit in this process's group (Ctrl-C stops them with it), logging to its file.
fn spawn(exe: &Path, units: &[Unit]) -> Result<Vec<std::process::Child>> {
    units
        .iter()
        .map(|u| {
            let log = std::fs::OpenOptions::new().create(true).append(true).open(&u.log)?;
            let child = std::process::Command::new(exe).args(&u.args).stdout(log.try_clone()?).stderr(log).spawn()?;
            println!("   ✓ {} started (pid {}, log {})", u.description, child.id(), u.log.display());
            Ok(child)
        })
        .collect()
}

// ------------------------------------------------------------------------------- the steps

/// Whether setup starts the node: never a second copy of a running one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    /// Start it now (a service that's installed already is only started if it isn't running)
    Now,
    /// It's running already from a terminal: leave it
    Running,
    /// It's running, but not as the service setup would install: installing one would start a
    /// second copy of the key, so leave it and say so
    RunningOutside,
}

pub fn start_decision(kind: ServiceKind, running: bool, installed: bool) -> Start {
    match (kind, running, installed) {
        (_, false, _) => Start::Now,
        (ServiceKind::Foreground, true, _) => Start::Running,
        (_, true, true) => Start::Now,
        (_, true, false) => Start::RunningOutside,
    }
}

/// The node at `rpc` answers for the chain whose genesis hashes to `hash`: its height, else None.
async fn node_height(rpc: &str, hash: &str) -> Option<u64> {
    let s = get(rpc, "/status").await.ok()?;
    (s["genesis_hash"].as_str().map(str::to_ascii_lowercase).as_deref() == Some(hash)).then(|| s["height"].as_u64().unwrap_or(0))
}

/// What a validator needs on its own account to register: the bond, plus a unit for fees.
fn needed(plan: &SetupPlan) -> Result<u64> {
    let bond = match &plan.bond {
        Some(b) => vcli::parse_fang(b)?,
        None => plan.min_bond,
    };
    Ok(bond + DROPS_PER_FANG)
}

async fn transferable(rpc: &str, me: &str) -> u64 {
    get(rpc, &format!("/account/{me}")).await.ok().and_then(|a| a["transferable"].as_str().and_then(|s| s.parse().ok())).unwrap_or(0)
}

/// The owner's key and node on a chain `blockula create` made: the paths its README names.
fn owner_flags(chain_id: &str) -> String {
    format!("--key ~/.blockula/{chain_id}/validator.key --rpc http://localhost:7070")
}

/// Funds the validator's account: from the faucet when the chain has one, else (or when it
/// refuses) by asking the chain's owner, and waits until the bond is there.
fn fund(plan: &SetupPlan, me: &str, dir: &Path, poll: Duration) -> Result<()> {
    let rt = rt()?;
    let need = needed(plan)?;
    let have = rt.block_on(transferable(&plan.node, me));
    if have >= need {
        println!("   ✓ {me} holds {}: enough for the bond", in_token(have, &plan.token));
        return Ok(());
    }
    if plan.faucet {
        match vcli::wallet(WalletCmd::Faucet { rpc: plan.node.clone(), to: Some(me.to_string()) }, dir) {
            Ok(()) => {}
            Err(e) => println!("   ⚠️  the faucet didn't pay: {e:#}"),
        }
        if rt.block_on(transferable(&plan.node, me)) >= need {
            return Ok(());
        }
    }
    let short = need.saturating_sub(rt.block_on(transferable(&plan.node, me)));
    println!("\n   The bond isn't there yet. Ask the chain's owner to send {} to {me}:", in_token(short, &plan.token));
    println!("     blockula wallet send {me} {} {}", amount(short), owner_flags(&plan.chain_id));
    println!("   Waiting for it (Ctrl-C stops; `blockula validator setup` again picks up here)…");
    loop {
        std::thread::sleep(poll);
        let have = rt.block_on(transferable(&plan.node, me));
        if have >= need {
            println!("   ✓ {} arrived", in_token(have, &plan.token));
            return Ok(());
        }
    }
}

/// Waits until the validator's own node at `rpc` answers for this chain and has synced something.
fn wait_for_node(plan: &SetupPlan, rpc: &str, log: &Path, timeout: Duration) -> Result<()> {
    let rt = rt()?;
    let start = Instant::now();
    let mut said: Option<Instant> = None;
    loop {
        let net = rt.block_on(async { get(&plan.node, "/status").await.ok().and_then(|s| s["height"].as_u64()) });
        match rt.block_on(node_height(rpc, &plan.genesis_hash)) {
            Some(h) if h > 0 => {
                println!("   ✓ its node is at block #{h}{}", net.map(|n| format!(" (the chain is at #{n})")).unwrap_or_default());
                return Ok(());
            }
            other if said.is_none_or(|t| t.elapsed() >= Duration::from_secs(15)) => {
                println!("   … {}", if other.is_some() { "its node is syncing" } else { "waiting for its node to answer" });
                said = Some(Instant::now());
            }
            _ => {}
        }
        if start.elapsed() > timeout {
            bail!("the validator's node didn't answer at {rpc} with a block of this chain within {} s: read its log ({}), then run `blockula validator setup` again", timeout.as_secs(), log.display());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// How this validator gets into the set, and what to send the owner.
fn next_steps(plan: &SetupPlan, me: &str, set: &Value) -> Vec<String> {
    let in_list = |k: &str| set[k].as_array().into_iter().flatten().any(|v| v["validator"]["address"].as_str() == Some(me));
    if in_list("active") {
        return vec![format!("✓ It's in the active set: it takes turns making blocks for {}.", plan.chain_name)];
    }
    if in_list("next_epoch") {
        return vec![format!("✓ It joins the set at the next epoch, in {} blocks. Keep it running.", set["blocks_until_next_epoch"].as_u64().unwrap_or(0))];
    }
    let mut out = vec![];
    let admins = set["admins"].as_array().is_some_and(|a| !a.is_empty());
    let cmd = if set["admission"].is_object() {
        Some(format!("blockula validator admit {me} {}", owner_flags(&plan.chain_id)))
    } else if admins && set["churn_limit"].as_u64().unwrap_or(0) == 0 {
        Some(format!("blockula validator approve {me} {}", owner_flags(&plan.chain_id)))
    } else {
        None
    };
    match cmd {
        Some(cmd) => {
            out.push("Last step: the chain's owner lets it in. Send them this, and confirm the address with them another way (a call, or a message from an account they know you by):".to_string());
            out.push(String::new());
            out.push(format!("  My validator for {} is {me} (p2p {}). Please let it in:", plan.chain_name, plan.p2p_addr()));
            out.push(format!("    {cmd}"));
            out.push(String::new());
            out.push("Keep this machine on: it joins at the end of the epoch after that, by itself.".to_string());
        }
        None => out.extend(vcli::way_in(set, me)),
    }
    // Each block needs a quorum of the set: in a set of up to three, that's every one.
    let n = set["active"].as_array().map_or(0, Vec::len) + 1;
    if crate::epochs::quorum(n) == n {
        out.push(format!("With it, the set has {n} validators, and each block needs every one: if this machine is off or asleep, the whole chain stops until it's back."));
    }
    out
}

/// `<home>/README.md`: what to keep once the validator is set up.
pub fn readme(plan: &SetupPlan, me: &str, exe: &Path) -> String {
    let mut s = format!("# Validator for {}\n\nMade with `blockula validator setup`. Chain id `{}`, genesis hash `{}`.\n\n", plan.chain_name, plan.chain_id, plan.genesis_hash);
    s += &format!("- Validator address: `{me}`\n- Chain node: {}\n- p2p: {}\n- Public HTTP: {}\n- Own API (this machine): {}\n\n", plan.node, plan.p2p_addr(), plan.http_addr(), plan.local_rpc());
    s += "## Keep it safe\n\n- `validator.key` in this folder is the validator. Back it up once, offline, and never put it in a chat, a commit or a log.\n- **Never run it in two places.** Two copies of one key sign twice, and the chain slashes 5% of the bond and bans the validator for good. Before you start it anywhere else, stop it here (below).\n\n";
    s += "## Run it\n\n";
    for u in plan.units() {
        s += &format!("{}:\n\n```sh\n", u.description);
        for (k, v) in controls(plan.service, exe, &u) {
            s += &format!("# {k}\n{v}\n");
        }
        s += "```\n\n";
    }
    s += &format!(
        "## Check it\n\n```sh\ncurl -s {node}/api/validators | grep -c {me}   # 1 or more: the chain knows it\ncurl -s {local}/api/health                    # 200 when its head is fresh\nblockula validator list --rpc {node}\n```\n\nIf it's jailed (off too long): `blockula validator unjail {me} --key {key} --rpc {node}` once the jail time is over.\n\n",
        node = plan.node,
        local = plan.local_rpc(),
        key = self::display(&plan.home.join("validator.key")),
    );
    s += "## Upgrade\n\nInstall the new release (`curl -fsSL https://github.com/jackdishman/blockula/releases/latest/download/install.sh | sh`), then Stop and Start it (above). Its key and data stay here.\n\n## Move it to another machine\n\nStop it here first, and make sure it stays stopped (for a service, Stop above, then delete its file). Then follow https://github.com/jackdishman/blockula/blob/main/docs/BECOME-A-VALIDATOR.md#moving-to-another-machine.\n";
    s
}

fn display(p: &Path) -> String {
    let home = user_home();
    match p.strip_prefix(&home) {
        Ok(r) if !home.as_os_str().is_empty() => format!("~/{}", r.display()),
        _ => p.display().to_string(),
    }
}

/// What the steps call out to: the binary the services run, how the node starts, and where its
/// own API answers (tests stand in for each).
pub struct Steps<'a> {
    pub exe: PathBuf,
    /// The blockula directory (`known-chains.json`, the faucet's and `register`'s pins).
    pub dir: PathBuf,
    /// Starts the node (as `plan.service` says); returns the processes to wait on.
    pub start: &'a mut dyn FnMut(&SetupPlan, &Path) -> Result<Vec<std::process::Child>>,
    /// The validator's own API, if not `plan.local_rpc()`.
    pub node_rpc: Option<String>,
    pub poll: Duration,
    pub node_timeout: Duration,
}

/// The real start: the services, or processes of this one.
pub fn start_for_real(plan: &SetupPlan, exe: &Path) -> Result<Vec<std::process::Child>> {
    let units = plan.units();
    match plan.service {
        ServiceKind::Foreground => spawn(exe, &units),
        kind => {
            for u in &units {
                install(kind, exe, u)?;
            }
            if kind == ServiceKind::Systemd {
                let linger = sh("loginctl", &["show-user", &std::env::var("USER").unwrap_or_default(), "--property=Linger"]).ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
                if linger.as_deref() != Some("Linger=yes") {
                    println!("   ℹ️  To keep it running while you're logged out, and start it at boot: sudo loginctl enable-linger $USER");
                }
            }
            Ok(vec![])
        }
    }
}

/// Does the plan: each step skipped when it's done already. Returns the validator's address and
/// the processes to wait on (a foreground run's).
pub fn execute(plan: &SetupPlan, steps: &mut Steps) -> Result<(String, Vec<std::process::Child>)> {
    let rt = rt()?;
    // 1. The validator's home and key: made once, never again.
    let me = if plan.home.join("validator.key").exists() {
        let g = Genesis::load(&plan.home.join("genesis.json"))?;
        ensure!(crate::checkpoints::genesis_hash(&g) == plan.genesis_hash, "{} holds a validator for another chain ({}): pass --home for this one", plan.home.display(), g.chain_id);
        let me = vcli::load_seed(&plan.home.join("validator.key"))?.address_with(&g.address_prefix);
        println!("1. Validator key: {} already holds {me}", display(&plan.home));
        me
    } else {
        let (g, keys, _, _) = vcli::init_home(&plan.home, &plan.node, Some(plan.genesis_hash.clone()), Some(plan.p2p_addr()), Some(plan.http_addr()), plan.reach != Reach::Internet)?;
        let me = keys.address_with(&g.address_prefix);
        println!("1. Validator key: made {me}, in {} (mode 600). Back it up once, offline.", display(&plan.home.join("validator.key")));
        me
    };
    // 2. On the internet, the full node the chain publishes as this validator's HTTP address.
    if let Some(full) = &plan.full_home {
        if full.join("genesis.json").exists() {
            let g = Genesis::load(&full.join("genesis.json"))?;
            ensure!(crate::checkpoints::genesis_hash(&g) == plan.genesis_hash, "{} holds another chain ({}): it can't be this validator's full node", full.display(), g.chain_id);
            println!("2. Full node: {} is set up", display(full));
        } else {
            crate::node::join(&plan.node, full, &crate::checkpoints::Pins { genesis_hash: Some(plan.genesis_hash.clone()), ..Default::default() }, true)?;
            println!("2. Full node: set up in {}", display(full));
        }
    }
    let set = rt.block_on(get(&plan.node, "/validators"))?;
    let registered = vcli::find_validator(&set, &me).is_some();
    // 3. The bond.
    if registered {
        println!("3. Bond: it's registered already");
    } else {
        println!("3. Bond: {} on {me}", in_token(needed(plan)? - DROPS_PER_FANG, &plan.token));
        fund(plan, &me, &steps.dir, steps.poll)?;
    }
    // 4. The node, unless it's running already (never a second copy).
    let local = steps.node_rpc.clone().unwrap_or_else(|| plan.local_rpc());
    let mut children = vec![];
    let running = rt.block_on(node_height(&local, &plan.genesis_hash)).is_some();
    let installed = plan.units().first().and_then(|u| unit_file(plan.service, u)).is_some_and(|f| f.exists());
    match start_decision(plan.service, running, installed) {
        Start::Running => println!("4. Node: it's running already at {local}"),
        Start::RunningOutside => println!("4. Node: it's running already at {local}, but not as a background service. Leaving it: a service would start a second copy of the key. To make it one, stop that node (Ctrl-C where it runs), then run `blockula validator setup` again."),
        Start::Now => {
            println!("4. Node: starting it {}", match plan.service {
                ServiceKind::Foreground => "from this terminal",
                _ => "as a background service",
            });
            children = (steps.start)(plan, &steps.exe)?;
        }
    }
    wait_for_node(plan, &local, &plan.home.join("node.log"), steps.node_timeout)?;
    // 5. Register.
    if registered {
        println!("5. Register: done already");
    } else {
        println!("5. Register:");
        vcli::run_in(ValidatorCmd::Register { home: plan.home.clone(), rpc: plan.node.clone(), bond: plan.bond.clone(), node_rpc: Some(local), node_not_running: false, allow_private_addr: plan.reach != Reach::Internet }, &steps.dir)?;
    }
    std::fs::write(plan.home.join("README.md"), readme(plan, &me, &steps.exe))?;
    let set = rt.block_on(get(&plan.node, "/validators"))?;
    println!();
    for l in next_steps(plan, &me, &set) {
        println!("{}", if l.is_empty() { l } else { format!("   {l}") });
    }
    println!();
    for u in plan.units() {
        for (k, v) in controls(plan.service, &steps.exe, &u) {
            println!("   {k:<6} {v}");
        }
    }
    println!("   Everything else: {}", display(&plan.home.join("README.md")));
    Ok((me, children))
}

/// `blockula validator setup`.
pub fn setup(a: &SetupArgs, dir: &Path) -> Result<()> {
    use std::io::IsTerminal;
    let interactive = !a.yes && std::io::stdin().is_terminal();
    let stdin = std::io::stdin();
    let mut prompter = Prompter { inp: stdin.lock(), out: std::io::stdout() };
    if interactive {
        writeln!(prompter.out, "🧛 A validator on this machine, for a chain someone else runs.\n")?;
    }
    let info = loop {
        let node = match (&a.node, interactive) {
            (Some(n), _) => normalize_node(n)?,
            (None, true) => prompter.ask("The chain's node URL (from its owner)", None, normalize_node)?,
            (None, false) => bail!("--node is needed: any node of the chain, as its owner sent it (http://… or https://…)"),
        };
        match ChainInfo::fetch(&node) {
            Ok(i) => break i,
            Err(e) if interactive && a.node.is_none() => writeln!(prompter.out, "    ✗ {e:#}")?,
            Err(e) => return Err(e),
        }
    };
    if interactive {
        writeln!(prompter.out, "  Found {} ({}) at block #{}, with {} validator{}.", info.name(), info.token(), info.status["height"].as_u64().unwrap_or(0), info.active_count(), if info.active_count() == 1 { "" } else { "s" })?;
    }
    let service = if a.foreground { ServiceKind::Foreground } else { ServiceKind::detect() };
    let plan = plan(interactive.then_some(&mut prompter), a, &info, &Machine::detect(), service, &first_free_port)?;
    writeln!(prompter.out, "\nReview")?;
    for (k, v) in plan.summary() {
        writeln!(prompter.out, "  {k:<21} {v}")?;
    }
    writeln!(prompter.out)?;
    if a.dry_run {
        writeln!(prompter.out, "Nothing was made (--dry-run). Run it again without --dry-run to set it up.")?;
        return Ok(());
    }
    if interactive && !prompter.confirm("Set it up?", true)? {
        bail!("nothing made");
    }
    drop(prompter);
    let exe = std::env::current_exe().context("finding this blockula binary")?;
    let mut start = start_for_real;
    let mut steps = Steps { exe, dir: dir.to_path_buf(), start: &mut start, node_rpc: None, poll: Duration::from_secs(5), node_timeout: Duration::from_secs(600) };
    let (_, children) = execute(&plan, &mut steps)?;
    if !children.is_empty() {
        println!("\n▶  The node runs from this terminal: Ctrl-C stops it, and `blockula validator setup` (or the Start line above) starts it again.");
        for mut c in children {
            c.wait()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(addrs: &[&str], faucet: bool) -> ChainInfo {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!("blockula-setup-info-{}-{}-{}", crate::chain::now_ms(), std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        let homes = crate::chain::Chain::summon(&root, &crate::chain::SummonOpts::new("friends", 1)).unwrap();
        let mut g = Genesis::load(&homes[0].join("genesis.json")).unwrap();
        std::fs::remove_dir_all(root).ok();
        g.faucet = faucet;
        for (v, a) in g.validators.iter_mut().zip(addrs) {
            v.p2p_addr = a.to_string();
        }
        let active: Vec<Value> = addrs.iter().skip(1).map(|a| serde_json::json!({ "p2p_addr": a })).collect();
        ChainInfo {
            node: "http://100.64.0.1:7070".into(),
            status: serde_json::json!({ "name": "Jack in the Block", "token": "JACK", "genesis_hash": "ab".repeat(32), "height": 40 }),
            genesis: g,
            validators: serde_json::json!({ "min_bond": (50 * DROPS_PER_FANG).to_string(), "active": active }),
        }
    }

    fn ports(from: u16, taken: &[u16]) -> Result<u16> {
        Ok((from..).find(|p| !taken.contains(p)).unwrap())
    }

    fn ask(script: &str, a: &SetupArgs, i: &ChainInfo, m: &Machine) -> (Result<SetupPlan>, String) {
        let mut out = Vec::new();
        let r = plan(Some(&mut Prompter { inp: script.as_bytes(), out: &mut out }), a, i, m, ServiceKind::Launchd, &ports);
        (r, String::from_utf8(out).unwrap())
    }

    const MACHINE: Machine = Machine { tailscale_ip: None, lan_ip: None };

    #[test]
    fn chains_are_reached_as_their_validators_publish() {
        assert_eq!(Reach::of_addr("127.0.0.1:30300"), Reach::Local);
        assert_eq!(Reach::of_addr("localhost:30300"), Reach::Local);
        assert_eq!(Reach::of_addr("192.168.1.20:30300"), Reach::Home);
        assert_eq!(Reach::of_addr("nas.local:30300"), Reach::Home);
        assert_eq!(Reach::of_addr("100.101.102.103:30300"), Reach::Tailnet);
        assert_eq!(Reach::of_addr("ghoul.tail1234.ts.net:30300"), Reach::Tailnet);
        assert_eq!(Reach::of_addr("ledger.example.org:30300"), Reach::Internet);
        assert_eq!(Reach::of_addr("[2001:db8::1]:30300"), Reach::Internet);
        assert_eq!(Reach::of_chain(["100.64.0.1:30300", "192.168.1.5:30300"]), Reach::Tailnet);
        assert_eq!(Reach::of_chain(["100.64.0.1:30300", "37.16.2.248:30300"]), Reach::Internet, "one public validator makes it an internet chain");
        assert_eq!(Reach::of_chain([]), Reach::Local);
    }

    /// A tailnet chain, as `blockula create --reach tailscale` makes one: the hash and Enter for
    /// the rest. The validator publishes its tailnet address and its own API, as the owner's does.
    #[test]
    fn a_tailnet_chain_takes_the_detected_address() {
        let i = info(&["100.64.0.1:30300"], true);
        let m = Machine { tailscale_ip: Some("100.70.0.9".into()), lan_ip: Some("192.168.1.20".into()) };
        let (plan, out) = ask(&format!("{}\n\n\n", "AB".repeat(32)), &SetupArgs::default(), &i, &m);
        let plan = plan.map_err(|e| format!("{e}: {out}")).unwrap();
        assert!(out.contains("Jack in the Block is reached over Tailscale"), "{out}");
        assert_eq!((plan.reach, plan.host.as_str(), plan.relay, plan.service), (Reach::Tailnet, "100.70.0.9", false, ServiceKind::Launchd));
        assert_eq!((plan.p2p_addr(), plan.http_addr()), ("100.70.0.9:30300".to_string(), "100.70.0.9:7070".to_string()));
        assert_eq!(plan.genesis_hash, "ab".repeat(32), "lowercased");
        assert!(plan.full_home.is_none() && plan.units().len() == 1);
        let args = plan.validator_args();
        assert_eq!(args[3..], ["--port", "7070", "--bind", "0.0.0.0"]);
        assert!(plan.summary().iter().any(|(k, v)| *k == "Bond" && v == "50 JACK, from the faucet"), "{:?}", plan.summary());
    }

    #[test]
    fn a_wrong_hash_is_asked_again_and_refused_as_a_flag() {
        let i = info(&["100.64.0.1:30300"], true);
        let m = Machine { tailscale_ip: Some("100.70.0.9".into()), lan_ip: None };
        let (plan, out) = ask(&format!("abc\n{}\n{}\n\nn\n", "cd".repeat(32), "ab".repeat(32)), &SetupArgs::default(), &i, &m);
        let plan = plan.unwrap();
        assert!(out.contains("64 hex characters") && out.contains("isn't the chain you were told about"), "{out}");
        assert_eq!(plan.service, ServiceKind::Foreground, "no background service, as answered");
        let flag = SetupArgs { genesis_hash: Some("cd".repeat(32)), ..Default::default() };
        assert!(plan_from(&flag, &i, &m).unwrap_err().to_string().contains("isn't the chain"));
        assert!(plan_from(&SetupArgs::default(), &i, &m).unwrap_err().to_string().contains("--genesis-hash is needed"));
    }

    fn plan_from(a: &SetupArgs, i: &ChainInfo, m: &Machine) -> Result<SetupPlan> {
        plan::<&[u8], Vec<u8>>(None, a, i, m, ServiceKind::Systemd, &ports)
    }

    #[test]
    fn without_tailscale_a_tailnet_chain_says_how_to_get_on_it() {
        let i = info(&["100.64.0.1:30300"], true);
        let a = SetupArgs { genesis_hash: Some("ab".repeat(32)), ..Default::default() };
        let e = plan_from(&a, &i, &MACHINE).unwrap_err().to_string();
        assert!(e.contains("Tailscale isn't up") && e.contains("Nothing was made"), "{e}");
        let home = info(&["192.168.1.5:30300"], true);
        assert!(plan_from(&a, &home, &MACHINE).unwrap_err().to_string().contains("isn't on one"));
        let p = plan_from(&a, &home, &Machine { tailscale_ip: None, lan_ip: Some("192.168.1.30".into()) }).unwrap();
        assert_eq!((p.reach, p.http_addr()), (Reach::Home, "192.168.1.30:7070".to_string()));
    }

    /// An internet chain: a public host is asked for (a private one refused), a full node on this
    /// machine is its public HTTP address, and the validator's own API stays on 127.0.0.1.
    #[test]
    fn an_internet_chain_runs_a_full_node_and_keeps_the_api_private() {
        let i = info(&["37.16.2.248:30300", "blockula-demo-v2.fly.dev:30300"], false);
        let (plan, out) = ask(&format!("{}\n192.168.1.20\nghoul.duckdns.org\n\n\n", "ab".repeat(32)), &SetupArgs::default(), &i, &MACHINE);
        let plan = plan.map_err(|e| format!("{e}: {out}")).unwrap();
        assert!(out.contains("is a private address") && out.contains("forwarded"), "{out}");
        assert_eq!((plan.reach, plan.host.as_str(), plan.relay), (Reach::Internet, "ghoul.duckdns.org", false));
        assert_eq!((plan.p2p_addr(), plan.http_addr()), ("ghoul.duckdns.org:30300".to_string(), "http://ghoul.duckdns.org:7171".to_string()));
        assert_eq!(plan.validator_args()[3..], ["--port", "7070", "--p2p-bind", "0.0.0.0"]);
        assert_eq!(plan.full_args().unwrap()[3..], ["--bind", "0.0.0.0", "--port", "7171"]);
        assert_eq!(plan.units().iter().map(|u| u.systemd.as_str()).collect::<Vec<_>>(), ["blockula-friends-validator.service", "blockula-friends-full.service"]);
        let s = plan.summary();
        assert!(s.iter().any(|(k, v)| *k == "Open to the internet" && v.contains("TCP 30300 and 7171") && v.contains("router")), "{s:?}");
        assert!(s.iter().any(|(k, v)| *k == "Bond" && v.contains("no faucet")), "{s:?}");
        // As flags: --host is needed, and --relay switches the preset.
        let a = SetupArgs { genesis_hash: Some("ab".repeat(32)), ..Default::default() };
        assert!(plan_from(&a, &i, &MACHINE).unwrap_err().to_string().contains("--host is needed"));
        let relayed = plan_from(&SetupArgs { host: Some("203.0.113.7".into()), relay: true, ..a.clone() }, &i, &MACHINE).unwrap();
        assert!(relayed.validator_args().ends_with(&["--p2p-preset".to_string(), "public-nat".into()]));
        let tailnet = info(&["100.64.0.1:30300"], true);
        assert!(plan_from(&SetupArgs { host: Some("100.70.0.9".into()), relay: true, ..a.clone() }, &tailnet, &MACHINE).is_err(), "--relay is for the internet");
        assert!(plan_from(&SetupArgs { bond: Some("1".into()), host: Some("203.0.113.7".into()), ..a }, &i, &MACHINE).unwrap_err().to_string().contains("below the chain's minimum"));
    }

    /// Never a second copy: a node that runs is left alone, unless it's the service itself.
    #[test]
    fn a_running_node_is_never_started_twice() {
        for kind in [ServiceKind::Launchd, ServiceKind::Systemd, ServiceKind::Foreground] {
            assert_eq!(start_decision(kind, false, false), Start::Now);
            assert_eq!(start_decision(kind, false, true), Start::Now);
        }
        assert_eq!(start_decision(ServiceKind::Foreground, true, false), Start::Running);
        assert_eq!(start_decision(ServiceKind::Launchd, true, true), Start::Now, "the service: starting it again is a no-op");
        assert_eq!(start_decision(ServiceKind::Launchd, true, false), Start::RunningOutside);
        assert_eq!(start_decision(ServiceKind::Systemd, true, false), Start::RunningOutside);
    }

    /// Ports in use move on: a second validator beside the chain's first one on this machine.
    #[test]
    fn busy_ports_move_on() {
        let i = info(&["127.0.0.1:30300"], true);
        let busy = |from: u16, taken: &[u16]| Ok((from + 1..).find(|p| !taken.contains(p)).unwrap());
        let p = plan::<&[u8], Vec<u8>>(None, &SetupArgs { genesis_hash: Some("ab".repeat(32)), ..Default::default() }, &i, &MACHINE, ServiceKind::Foreground, &busy).unwrap();
        assert_eq!((p.reach, p.p2p_addr(), p.http_addr(), p.local_rpc()), (Reach::Local, "127.0.0.1:30301".into(), "127.0.0.1:7071".into(), "http://127.0.0.1:7071".into()));
        assert_eq!(p.validator_args()[3..], ["--port", "7071"]);
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = held.local_addr().unwrap().port();
        assert_ne!(first_free_port(port, &[]).unwrap(), port);
    }

    /// The services: launchd's runs the node through a wrapper that keeps the Mac awake and doesn't
    /// restart a deliberate stop; systemd's never restarts exit codes 10–13; both quote their
    /// arguments.
    #[test]
    fn service_files() {
        let u = Unit { label: "app.blockula.friends.validator".into(), systemd: "blockula-friends-validator.service".into(), description: "Blockula validator for Jack & Jill".into(), args: vec!["run".into(), "--home".into(), "/Users/a b/.blockula/friends-validator".into()], log: "/Users/a b/.blockula/friends-validator/node.log".into() };
        let exe = Path::new("/Users/a b/.blockula/bin/blockula");
        let plist = launchd_plist(exe, &u);
        for want in ["<string>app.blockula.friends.validator</string>", "<string>/bin/sh</string>", "caffeinate -i -w", "10|11|12|13) ", "<string>/Users/a b/.blockula/bin/blockula</string>", "<string>--home</string>", "<key>SuccessfulExit</key>\n    <false/>", "Jack &amp; Jill", "node.log</string>"] {
            assert!(plist.contains(want), "{want}: {plist}");
        }
        assert!(!plist.contains("Jack & Jill"), "escaped");
        // The wrapper is valid sh, and on a Mac the plist is one launchd reads.
        let checked = std::process::Command::new("/bin/sh").args(["-n", "-c", LAUNCHD_WRAPPER]).status().unwrap();
        assert!(checked.success(), "sh -n");
        if cfg!(target_os = "macos") && Path::new("/usr/bin/plutil").exists() {
            let f = std::env::temp_dir().join(format!("blockula-setup-{}.plist", std::process::id()));
            std::fs::write(&f, &plist).unwrap();
            let out = std::process::Command::new("/usr/bin/plutil").arg("-lint").arg(&f).output().unwrap();
            std::fs::remove_file(&f).ok();
            assert!(out.status.success(), "plutil: {}", String::from_utf8_lossy(&out.stdout));
        }
        let unit = systemd_unit(exe, &u);
        assert!(unit.contains("ExecStart=\"/Users/a b/.blockula/bin/blockula\" \"run\" \"--home\" \"/Users/a b/.blockula/friends-validator\"\n"), "{unit}");
        assert!(unit.contains("RestartPreventExitStatus=10 11 12 13\n") && unit.contains("Restart=on-failure") && unit.contains("WantedBy=default.target"), "{unit}");
        assert_eq!(systemd_arg("50% $HOME"), "\"50%% $$HOME\"");
        assert_eq!(DELIBERATE_STOPS, [crate::upgrade::EXIT_HALT_HEIGHT, crate::upgrade::EXIT_UPGRADE_REQUIRED, crate::upgrade::EXIT_DOUBLE_SIGN_RISK, crate::upgrade::EXIT_PROGRAM_FAULT]);
        assert!(!DELIBERATE_STOPS.contains(&crate::upgrade::EXIT_PROGRAM_RESOURCES), "14 restarts: a restart may cure it");
    }

    /// The whole run against a node on a chain of one (its admin lets newcomers in): the key, the
    /// faucet's bond, the node (a stand-in here: the chain's own API), `register`, and the one
    /// command for the owner. Run again, it makes nothing new.
    #[test]
    fn setup_end_to_end() {
        let (root, homes, chain, _rt, url) = vcli::tests::local_node("setup");
        let dir = root.join("cli");
        std::fs::create_dir_all(&dir).unwrap();
        let g = Genesis::load(&homes[0].join("genesis.json")).unwrap();
        let hash = crate::checkpoints::genesis_hash(&g);
        let i = ChainInfo::fetch(&url).unwrap();
        assert_eq!((i.reach(), i.reported_hash()), (Reach::Local, hash.clone()));
        let a = SetupArgs { genesis_hash: Some(hash.clone()), home: Some(root.join("friends-validator")), ..Default::default() };
        let plan = plan::<&[u8], Vec<u8>>(None, &a, &i, &MACHINE, ServiceKind::Foreground, &first_free_port).unwrap();
        let started = std::cell::Cell::new(0);
        let mut start = |_: &SetupPlan, _: &Path| {
            started.set(started.get() + 1);
            Ok(vec![])
        };
        let mut steps = Steps { exe: "/bin/blockula".into(), dir: dir.clone(), start: &mut start, node_rpc: Some(url.clone()), poll: Duration::from_millis(100), node_timeout: Duration::from_secs(30) };
        let (me, _) = execute(&plan, &mut steps).unwrap();
        assert!(plan.home.join("validator.key").exists() && plan.home.join("README.md").exists());
        let set = rt().unwrap().block_on(get(&url, "/validators")).unwrap();
        assert!(vcli::find_validator(&set, &me).is_some(), "registered: {set}");
        assert!(chain.lock().state.account(&me).balance > 0, "the faucet paid the bond");
        let steps_out = next_steps(&plan, &me, &set);
        assert!(steps_out.iter().any(|l| l.contains(&format!("blockula validator approve {me} --key ~/.blockula/setup/validator.key"))), "{steps_out:?}");
        assert!(steps_out.iter().any(|l| l.contains("every one")), "a set of two needs both: {steps_out:?}");
        let readme = std::fs::read_to_string(plan.home.join("README.md")).unwrap();
        assert!(readme.contains(&me) && readme.contains("Never run it in two places"), "{readme}");
        // Again: same key, no second registration, no second node (the stand-in answers already).
        let key = std::fs::read_to_string(plan.home.join("validator.key")).unwrap();
        let (again, _) = execute(&plan, &mut steps).unwrap();
        assert_eq!(again, me);
        assert_eq!(std::fs::read_to_string(plan.home.join("validator.key")).unwrap(), key);
        drop(steps);
        assert_eq!(started.get(), 0, "its node answers already (the stand-in): never a second copy");
        std::fs::remove_dir_all(root).ok();
    }
}
