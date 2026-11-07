//! `blockula launch`: a new chain in a minute. It asks what the chain is called, who can reach
//! it and how it runs (or takes the answers as flags, with `--yes`), summons it, writes a short
//! guide into its home (`LAUNCH.md`: how to start it again, what friends run to join, the apps),
//! optionally serves it over HTTPS on a tailnet (`tailscale serve`), and starts it.
//!
//! Everything it does, `blockula summon` and `blockula run` can do by hand; this only asks the
//! questions in order and picks the flags.

use crate::chain::{self, Brand, Chain, Genesis, SummonOpts, DEFAULT_TOKEN};
use crate::types::DROPS_PER_FANG;
use anyhow::{anyhow, bail, ensure, Result};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// How the validators reach each other (docs/LAUNCH.md, step 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Reach {
    /// This machine only: a chain to try out
    Local,
    /// A Tailscale tailnet: friends anywhere, private
    Tailscale,
    /// One home network (LAN)
    Home,
    /// The open internet: a public IP or DNS name with ports forwarded
    Internet,
}

impl Reach {
    const ALL: [Reach; 4] = [Reach::Local, Reach::Tailscale, Reach::Home, Reach::Internet];
    fn label(self) -> &'static str {
        match self {
            Reach::Local => "Just this machine",
            Reach::Tailscale => "Tailscale",
            Reach::Home => "One home network",
            Reach::Internet => "The open internet",
        }
    }
    fn note(self) -> &'static str {
        match self {
            Reach::Local => "try it out alone; nobody else can reach it",
            Reach::Tailscale => "friends anywhere, and only your tailnet can reach it",
            Reach::Home => "every machine on the same Wi-Fi or LAN",
            Reach::Internet => "a public IP or DNS name, TCP 30300 and 7070 forwarded",
        }
    }
}

/// The answers, as flags (`blockula launch --help`). Anything left out is asked, or with `--yes`
/// takes its default.
#[derive(clap::Args, Debug, Default, Clone)]
pub struct LaunchArgs {
    /// The chain's name, as wallets and explorers show it ("Jack in the Block")
    #[arg(long)]
    pub name: Option<String>,
    /// Its ticker: 2–8 characters, A–Z and 0–9 (default: from the name's initials)
    #[arg(long)]
    pub token: Option<String>,
    /// A sentence about it
    #[arg(long)]
    pub description: Option<String>,
    /// The chain id, and its home's directory name (default: from the name)
    #[arg(long)]
    pub chain_id: Option<String>,
    /// Who can reach it (default: tailscale when it's installed, else local)
    #[arg(long, value_enum)]
    pub reach: Option<Reach>,
    /// This machine's address as the others reach it (default: detected)
    #[arg(long)]
    pub host: Option<String>,
    /// Milliseconds between blocks (default 5000)
    #[arg(long)]
    pub block_time_ms: Option<u64>,
    /// No faucet: the validator holds the starting supply and hands it out
    #[arg(long)]
    pub no_faucet: bool,
    /// Minimum validator bond, in units (default: 50 with a faucet, 10000 without)
    #[arg(long)]
    pub min_bond: Option<String>,
    /// Programs on from block 1 (experimental WebAssembly programs)
    #[arg(long)]
    pub programs: bool,
    /// Serve the explorer over HTTPS on your tailnet (`tailscale serve`; Tailscale only)
    #[arg(long)]
    pub https: bool,
    /// Where the chain lives (default: ~/.blockula/<chain id>)
    #[arg(long)]
    pub home: Option<PathBuf>,
    /// Don't ask: use the flags and the defaults (needs --name)
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Make the chain but don't start it
    #[arg(long)]
    pub no_start: bool,
}

/// What `launch` will make.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub name: String,
    pub description: String,
    pub token: String,
    pub chain_id: String,
    pub reach: Reach,
    /// `--host` (None for a local chain).
    pub host: Option<String>,
    pub block_time_ms: u64,
    pub faucet: bool,
    /// The bond in drops, when it isn't summon's default.
    pub min_bond: Option<u64>,
    pub programs: bool,
    pub https: bool,
}

impl Plan {
    pub fn summon_opts(&self) -> SummonOpts {
        let o = SummonOpts {
            faucet: self.faucet,
            block_time_ms: self.block_time_ms,
            http_port: Some(chain::SUMMON_HTTP_PORT),
            epoch_blocks: chain::summon_epoch_blocks(self.block_time_ms),
            min_bond: self.min_bond,
            host: self.host.clone(),
            token: Some(self.token.clone()).filter(|t| t != DEFAULT_TOKEN),
            brand: Some(Brand { name: self.name.clone(), description: self.description.clone() }),
            ..SummonOpts::new(&self.chain_id, 1)
        };
        if self.programs {
            o.with_programs()
        } else {
            o
        }
    }

    /// The interface the node listens on: every one for a shared chain (`::` for an IPv6 host),
    /// so the others (and `tailscale serve`, which proxies to 127.0.0.1) reach it.
    pub fn bind(&self) -> &'static str {
        match &self.host {
            None => "127.0.0.1",
            Some(h) if h.contains(':') => "::",
            Some(_) => "0.0.0.0",
        }
    }

    /// The node's URL for this machine's browser.
    pub fn local_url(&self) -> String {
        format!("http://localhost:{}", chain::SUMMON_HTTP_PORT)
    }

    /// The passkey wallet the node serves (src/wallet.rs), for this machine's browser.
    pub fn wallet_url(&self) -> String {
        format!("{}/wallet/", self.local_url())
    }

    /// The node's URL for the others.
    pub fn shared_url(&self) -> Option<String> {
        self.host.as_ref().map(|h| chain::http_url(&format!("{}:{}", if h.contains(':') { format!("[{h}]") } else { h.clone() }, chain::SUMMON_HTTP_PORT)))
    }

    pub fn run_command(&self, home: &Path) -> String {
        let bind = if self.host.is_some() { format!(" --bind {}", self.bind()) } else { String::new() };
        format!("blockula run --home {}{bind}", display_home(home))
    }

    fn bond_display(&self) -> String {
        let drops = self.min_bond.unwrap_or(if self.faucet { 50 } else { 10_000 } * DROPS_PER_FANG);
        crate::types::fmt_fang(drops).replace(" FANG", &format!(" {}", self.token))
    }

    /// The review, one line per choice.
    pub fn summary(&self) -> Vec<(&'static str, String)> {
        let mut out = vec![("Name", self.name.clone())];
        if !self.description.is_empty() {
            out.push(("About", self.description.clone()));
        }
        out.push(("Ticker", self.token.clone()));
        out.push(("Chain id", self.chain_id.clone()));
        out.push(("Reached over", match &self.host {
            Some(h) => format!("{}, at {h}", self.reach.label()),
            None => self.reach.label().into(),
        }));
        out.push(("A block every", fmt_ms(self.block_time_ms)));
        out.push(("Faucet", if self.faucet { format!("on, 100 {} at a time", self.token) } else { format!("off: you hand out {} yourself", self.token) }));
        out.push(("Validator bond", self.bond_display()));
        out.push(("Programs", if self.programs { "on from block 1 (experimental)".into() } else { "off".into() }));
        if self.https {
            out.push(("HTTPS", "on your tailnet, with tailscale serve".into()));
        }
        out.push(("Wallet", if self.https { format!("{}, and for friends at https://<this machine>.<your tailnet>.ts.net/wallet/", self.wallet_url()) } else { self.wallet_url() }));
        out
    }
}

fn fmt_ms(ms: u64) -> String {
    if ms.is_multiple_of(1000) {
        let s = ms / 1000;
        format!("{s} second{}", if s == 1 { "" } else { "s" })
    } else {
        format!("{ms} ms")
    }
}

/// `~/…` for a home under the user's home directory, for commands people copy.
fn display_home(home: &Path) -> String {
    match std::env::var_os("HOME").map(PathBuf::from) {
        Some(h) if home.starts_with(&h) => format!("~/{}", home.strip_prefix(&h).unwrap().display()),
        _ => home.display().to_string(),
    }
}

/// "Jack in the Block" → "jack-in-the-block".
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out.trim_end_matches('-').chars().take(40).collect();
    out.trim_end_matches('-').to_string()
}

/// "Jack in the Block" → "JITB"; one word → its first four letters.
pub fn suggest_token(name: &str) -> String {
    let words: Vec<String> = name.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_ascii_uppercase).collect();
    let initials: String = words.iter().filter_map(|w| w.chars().find(char::is_ascii_alphabetic)).take(8).collect();
    let letters: String = words.concat().chars().filter(char::is_ascii_alphabetic).take(4).collect();
    [initials, letters].into_iter().find(|t| chain::check_token(t).is_ok()).unwrap_or_else(|| DEFAULT_TOKEN.into())
}

/// A chain id: 2–40 of a–z, 0–9 and single dashes, and not one of Blockula's own networks.
pub fn check_chain_id(id: &str) -> Result<()> {
    let shape = (2..=40).contains(&id.len()) && id.split('-').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
    ensure!(shape, "a chain id is 2–40 characters: a–z, 0–9 and single dashes (like jack-in-the-block)");
    ensure!(id != crate::chain::MAINNET_CHAIN_ID && id != "blockula-dev" && !id.starts_with("blockula-testnet-"), "{id} belongs to Blockula's own networks; pick another");
    Ok(())
}

fn parse_bond(s: &str) -> Result<u64> {
    let d = crate::validator_cli::parse_fang(s)?;
    ensure!(d > 0, "the bond must be more than 0");
    Ok(d)
}

/// This machine's address on its tailnet, when Tailscale is installed and up.
pub fn tailscale_ip() -> Option<String> {
    let out = std::process::Command::new("tailscale").args(["ip", "-4"]).output().ok()?;
    let ip = String::from_utf8_lossy(&out.stdout).lines().next()?.trim().to_string();
    (out.status.success() && ip.parse::<std::net::Ipv4Addr>().is_ok()).then_some(ip)
}

/// This machine's address on its local network: the one a route to the internet would leave
/// from (no packet is sent).
pub fn lan_ip() -> Option<String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    let ip = s.local_addr().ok()?.ip();
    // 100.64.0.0/10 is Tailscale's (and other VPNs'), not the local network.
    let vpn = matches!(ip, std::net::IpAddr::V4(v4) if v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]));
    (!ip.is_loopback() && !ip.is_unspecified() && !vpn).then(|| ip.to_string())
}

/// What the wizard knows about this machine.
#[derive(Clone, Debug, Default)]
pub struct Machine {
    pub tailscale_ip: Option<String>,
    pub lan_ip: Option<String>,
}

impl Machine {
    pub fn detect() -> Self {
        Machine { tailscale_ip: tailscale_ip(), lan_ip: lan_ip() }
    }
    fn host_for(&self, reach: Reach) -> Option<String> {
        match reach {
            Reach::Local => None,
            Reach::Tailscale => self.tailscale_ip.clone(),
            Reach::Home => self.lan_ip.clone(),
            Reach::Internet => None,
        }
    }
}

/// Questions on `out`, answers from `inp`; a blank answer takes the default in brackets.
pub struct Prompter<R, W> {
    pub inp: R,
    pub out: W,
}

impl<R: BufRead, W: Write> Prompter<R, W> {
    fn line(&mut self) -> Result<String> {
        let mut s = String::new();
        if self.inp.read_line(&mut s)? == 0 {
            bail!("no more answers (stdin closed); pass the answers as flags with --yes");
        }
        Ok(s.trim().to_string())
    }

    /// Asks until `check` takes the answer.
    fn ask<T>(&mut self, q: &str, default: Option<&str>, check: impl Fn(&str) -> Result<T>) -> Result<T> {
        loop {
            match default {
                Some(d) if !d.is_empty() => write!(self.out, "  {q} [{d}]: ")?,
                _ => write!(self.out, "  {q}: ")?,
            }
            self.out.flush()?;
            let a = self.line()?;
            let a = if a.is_empty() { default.unwrap_or_default().to_string() } else { a };
            match check(&a) {
                Ok(v) => return Ok(v),
                Err(e) => writeln!(self.out, "    ✗ {e}")?,
            }
        }
    }

    fn confirm(&mut self, q: &str, default: bool) -> Result<bool> {
        loop {
            write!(self.out, "  {q} [{}]: ", if default { "Y/n" } else { "y/N" })?;
            self.out.flush()?;
            match self.line()?.to_ascii_lowercase().as_str() {
                "" => return Ok(default),
                "y" | "yes" => return Ok(true),
                "n" | "no" => return Ok(false),
                _ => writeln!(self.out, "    ✗ y or n")?,
            }
        }
    }

    fn choose(&mut self, heading: Option<&str>, options: &[String], default: usize) -> Result<usize> {
        if let Some(h) = heading {
            writeln!(self.out, "  {h}")?;
        }
        for (i, o) in options.iter().enumerate() {
            writeln!(self.out, "    {}) {o}", i + 1)?;
        }
        self.ask(&format!("Pick 1–{}", options.len()), Some(&(default + 1).to_string()), |a| a.parse::<usize>().ok().filter(|n| (1..=options.len()).contains(n)).map(|n| n - 1).ok_or_else(|| anyhow!("a number from 1 to {}", options.len())))
    }
}

fn check_name(name: &str) -> Result<String> {
    chain::check_brand(&Brand { name: name.into(), description: String::new() })?;
    Ok(name.into())
}

fn check_description(d: &str) -> Result<String> {
    chain::check_brand(&Brand { name: "x".into(), description: d.into() })?;
    Ok(d.into())
}

fn check_token(t: &str) -> Result<String> {
    let t = t.to_ascii_uppercase();
    chain::check_token(&t)?;
    Ok(t)
}

fn check_host(h: &str) -> Result<String> {
    ensure!(!h.is_empty(), "enter the address the others reach this machine at");
    chain::summon_host(h)?;
    Ok(h.to_string())
}

const BLOCK_TIMES: [(u64, &str); 4] = [(1_000, "1 second   snappy; about 5× the upload of the default"), (2_000, "2 seconds"), (5_000, "5 seconds  the default: light enough for home connections"), (10_000, "10 seconds lightest")];

/// The plan from the flags alone (`--yes`, or no terminal to ask on).
pub fn plan_from_flags(a: &LaunchArgs, m: &Machine) -> Result<Plan> {
    let name = check_name(a.name.as_deref().ok_or_else(|| anyhow!("--name is needed with --yes (or run `blockula launch` in a terminal to be asked)"))?)?;
    let token = check_token(&a.token.clone().unwrap_or_else(|| suggest_token(&name)))?;
    let description = check_description(a.description.as_deref().unwrap_or_default())?;
    let chain_id = a.chain_id.clone().unwrap_or_else(|| slugify(&name));
    check_chain_id(&chain_id)?;
    let reach = a.reach.unwrap_or(if m.tailscale_ip.is_some() { Reach::Tailscale } else { Reach::Local });
    let host = match reach {
        Reach::Local => None,
        _ => Some(check_host(&a.host.clone().or_else(|| m.host_for(reach)).ok_or_else(|| anyhow!("--host is needed: the address the others reach this machine at"))?)?),
    };
    ensure!(!a.https || reach == Reach::Tailscale, "--https serves the chain on a tailnet: it needs --reach tailscale");
    Ok(Plan {
        name,
        description,
        token,
        chain_id,
        reach,
        host,
        block_time_ms: a.block_time_ms.unwrap_or(5_000).max(1),
        faucet: !a.no_faucet,
        min_bond: a.min_bond.as_deref().map(parse_bond).transpose()?,
        programs: a.programs,
        https: a.https,
    })
}

/// The plan, asked a step at a time. A flag that was given is taken without asking.
pub fn plan_by_asking<R: BufRead, W: Write>(p: &mut Prompter<R, W>, a: &LaunchArgs, m: &Machine) -> Result<Plan> {
    writeln!(p.out, "\n🦇 Launch your own ledger. Press Enter to keep a [default].\n")?;

    writeln!(p.out, "1/4  Name it")?;
    let name = match &a.name {
        Some(n) => check_name(n)?,
        None => p.ask("Chain name", None, check_name)?,
    };
    let token = match &a.token {
        Some(t) => check_token(t)?,
        None => p.ask("Ticker (2–8 letters or digits)", Some(&suggest_token(&name)), check_token)?,
    };
    let description = match &a.description {
        Some(d) => check_description(d)?,
        None => p.ask("One line about it (optional)", None, check_description)?,
    };
    let chain_id = match &a.chain_id {
        Some(c) => {
            check_chain_id(c)?;
            c.clone()
        }
        None => p.ask("Chain id", Some(&slugify(&name)), |c| check_chain_id(c).map(|_| c.to_string()))?,
    };

    writeln!(p.out, "\n2/4  Who can reach it")?;
    let reach = match a.reach {
        Some(r) => r,
        None => {
            let options: Vec<String> = Reach::ALL
                .iter()
                .map(|r| {
                    let found = m.host_for(*r).map(|h| format!(" (found {h})")).unwrap_or_default();
                    format!("{:<18} {}{found}", r.label(), r.note())
                })
                .collect();
            let default = if m.tailscale_ip.is_some() { 1 } else { 0 };
            Reach::ALL[p.choose(None, &options, default)?]
        }
    };
    let host = match (reach, &a.host) {
        (Reach::Local, _) => None,
        (_, Some(h)) => Some(check_host(h)?),
        (r, None) => Some(p.ask("This machine's address, as the others reach it", m.host_for(r).as_deref(), check_host)?),
    };

    writeln!(p.out, "\n3/4  How it runs")?;
    let block_time_ms = match a.block_time_ms {
        Some(ms) => ms.max(1),
        None => {
            let options: Vec<String> = BLOCK_TIMES.iter().map(|(_, l)| l.to_string()).collect();
            BLOCK_TIMES[p.choose(Some("A block every"), &options, 2)?].0
        }
    };
    let faucet = if a.no_faucet { false } else { p.confirm(&format!("Faucet: anyone who reaches the chain gets 100 {token} at a time?"), reach != Reach::Internet)? };
    let min_bond = match &a.min_bond {
        Some(b) => Some(parse_bond(b)?),
        None => {
            let default = if faucet { "50" } else { "10000" };
            let b = p.ask(&format!("Validator bond, in {token}"), Some(default), |b| parse_bond(b).map(|d| (b.to_string(), d)))?;
            (b.0 != default).then_some(b.1)
        }
    };
    let programs = a.programs || p.confirm("Programs (experimental WebAssembly programs anyone can run)?", false)?;

    writeln!(p.out, "\n4/4  Apps")?;
    writeln!(p.out, "  ✓ The explorer is built into the node. Open it in Safari to add it to the Dock (Mac) or the Home Screen (iPhone).")?;
    let https = match reach {
        Reach::Tailscale if a.https => true,
        Reach::Tailscale => p.confirm("Serve it over HTTPS on your tailnet (tailscale serve)? Phones install it as an app best that way", true)?,
        _ => {
            ensure!(!a.https, "--https serves the chain on a tailnet: it needs --reach tailscale");
            false
        }
    };
    Ok(Plan { name, description, token, chain_id, reach, host, block_time_ms, faucet, min_bond, programs, https })
}

/// `<home>/LAUNCH.md`: what to keep after the launch.
pub fn guide(plan: &Plan, g: &Genesis, home: &Path, genesis_hash: &str, https_url: Option<&str>) -> String {
    let mut s = format!("# {}\n\n", plan.name);
    if !plan.description.is_empty() {
        s += &format!("{}\n\n", plan.description);
    }
    s += &format!("Made with `blockula launch`. Ticker {}, chain id `{}`.\n\n", plan.token, g.chain_id);
    s += &format!("- Genesis hash: `{genesis_hash}`\n- Explorer: {}", plan.local_url());
    if let Some(u) = plan.shared_url() {
        s += &format!(" (others: {u})");
    }
    if let Some(u) = https_url {
        s += &format!("\n- HTTPS on your tailnet: {u}");
    }
    s += &format!("\n- Wallet: {}", plan.wallet_url());
    if let Some(u) = https_url {
        s += &format!(" (friends: {u}/wallet/)");
    } else if let Some(u) = plan.shared_url() {
        s += &format!(" (others: {u}/wallet/)");
    }
    s += &format!("\n- Your validator's account: `{}`\n\n", g.validators[0].address);
    s += &format!("## Start it again\n\n```sh\n{}\n```\n\n", plan.run_command(home));
    s += "Back up `validator.key` in this folder: whoever holds it is your validator. Never run two copies of it.\n\n";
    // The one link friends need: the wallet this node serves.
    s += "## Invite friends\n\n";
    match (https_url, plan.shared_url()) {
        (Some(u), _) => s += &format!("Send them {u}/wallet/. They open it on a phone or laptop, make a wallet with a passkey{} and can pay right away. Nothing to install.\n\n", if plan.faucet { ", take coins from the faucet" } else { "" }),
        (None, Some(u)) => s += &format!(
            "Send them {u}/wallet/. It opens in any browser, but over plain http browsers won't make passkeys (it offers a browser-only key for testing). For passkeys, serve it over HTTPS: on Tailscale, `tailscale serve --bg {port}` (turn on HTTPS certificates in the Tailscale admin console first), then send https://<this machine>.<your tailnet>.ts.net/wallet/.\n\n",
            port = chain::SUMMON_HTTP_PORT
        ),
        (None, None) => s += &format!("The wallet is at {}, on this machine only: this chain is reached from this machine alone. Launch one with `--reach tailscale` (or `home`) to share it.\n\n", plan.wallet_url()),
    }
    if let Some(u) = plan.shared_url() {
        s += &format!(
            "## Friends run their own node\n\nThey install Blockula, then run:\n\n```sh\ncurl -fsSL https://github.com/jackdishman/blockula/releases/latest/download/install.sh | sh\nblockula join --genesis {u} --genesis-hash {genesis_hash} --home ~/.blockula/{id}\n```\n\nTheir node checks every block, and its explorer is at http://localhost:7171. To make them a validator, follow step 3 of https://github.com/jackdishman/blockula/blob/main/docs/LAUNCH.md and approve them with:\n\n```sh\nblockula validator approve <their validator address> --key {key} --rpc {local}\n```\n\n",
            id = g.chain_id,
            key = display_home(&home.join("validator.key")),
            local = plan.local_url(),
        );
    }
    if let Some(l) = listing(plan, g, genesis_hash, https_url) {
        s += &format!(
            "## List it on blockula.app\n\nAnyone can find it in the chain directory at https://blockula.app {}. This link opens GitHub with its `chains/{}.json` filled in: add your GitHub handle as the contact, check the rest, and open the pull request ({REGISTRY_REPO}/blob/main/chains/README.md says what gets listed).\n\n{}\n\n",
            l.note, g.chain_id, l.link
        );
    }
    s += &format!(
        "## Apps\n\n- **Explorer:** every node serves one. On a Mac, open it in Safari and choose File > Add to Dock. On an iPhone or iPad, open it in Safari and choose Share > Add to Home Screen. It installs as \"{}\".\n- **Wallet:** every node serves a passkey wallet at /wallet/ ({wallet} here). It talks to the node that serves it, so a friend's own node serves them one too. Passkeys need HTTPS (or localhost).\n- **Agents:** `claude mcp add blockula -- npx -y @blockula/mcp` (it uses the node at http://localhost:7070).\n",
        plan.name,
        wallet = plan.wallet_url(),
    );
    s
}

/// The repo whose `chains/` directory blockula.app lists (chains/README.md).
const REGISTRY_REPO: &str = "https://github.com/jackdishman/blockula";

/// How to list a launched chain on blockula.app: GitHub's new-file page with its
/// `chains/<chain id>.json` filled in, and what to do first.
#[derive(Debug, Clone, PartialEq)]
pub struct Listing {
    pub link: String,
    /// What must be true before the pull request (HTTPS, Funnel), and what the listing offers.
    pub note: String,
}

/// The listing for a chain the internet can reach, or `None` for one only this machine, a home
/// network or a tailnet reaches. On the open internet it takes validators (its p2p address is
/// public), once a DNS name serves its node over HTTPS. A tailnet chain served over HTTPS can be
/// listed for its wallet only, once Tailscale Funnel opens it: Funnel carries HTTPS, not p2p.
/// The contact (and a missing description) are left as placeholders the directory's checker
/// refuses until they're filled in.
pub fn listing(plan: &Plan, g: &Genesis, genesis_hash: &str, https_url: Option<&str>) -> Option<Listing> {
    let (rpc, p2p, note) = match (plan.reach, plan.host.as_deref(), https_url) {
        (Reach::Internet, Some(h), _) => {
            let named = h.parse::<std::net::IpAddr>().is_err();
            let rpc = if named { format!("https://{h}") } else { "https://<a DNS name for this machine>".to_string() };
            let note = format!(
                "once {} serves this node over HTTPS (a reverse proxy such as Caddy: https://github.com/jackdishman/blockula/blob/main/docs/BUILD-ON-IT.md#4-put-it-online). It lists the chain with validators wanted",
                if named { rpc.clone() } else { "a DNS name".to_string() }
            );
            (rpc, Some(g.validators[0].p2p_addr.clone()), note)
        }
        (Reach::Tailscale, _, Some(u)) => (
            u.trim_end_matches('/').to_string(),
            None,
            format!("once `tailscale funnel --bg {}` opens it to the internet. Wallets only: validators can't reach a tailnet from outside", chain::SUMMON_HTTP_PORT),
        ),
        _ => return None,
    };
    let s = |v: &str| serde_json::to_string(v).expect("a string serializes");
    let description = if plan.description.is_empty() { "<one sentence about it>" } else { plan.description.as_str() };
    let mut json = format!(
        "{{\n  \"name\": {},\n  \"chain_id\": {},\n  \"token\": {},\n  \"description\": {},\n  \"rpc\": {},\n  \"genesis_hash\": {},\n  \"validators_wanted\": {}",
        s(&plan.name),
        s(&g.chain_id),
        s(&plan.token),
        s(description),
        s(&rpc),
        s(genesis_hash),
        p2p.is_some()
    );
    if let Some(p) = &p2p {
        json += &format!(",\n  \"p2p\": {}", s(p));
    }
    json += ",\n  \"contact\": \"<your GitHub handle>\"\n}\n";
    let link = format!("{REGISTRY_REPO}/new/main?filename={}&value={}", url_encode(&format!("chains/{}.json", g.chain_id)), url_encode(&json));
    Some(Listing { link, note })
}

/// Percent-encodes everything but RFC 3986's unreserved characters, for a query value.
fn url_encode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// `tailscale serve --bg <port>`, then this machine's HTTPS URL on the tailnet.
pub fn tailscale_serve(port: u16) -> Result<String> {
    let out = std::process::Command::new("tailscale").args(["serve", "--bg", &port.to_string()]).output().map_err(|e| anyhow!("couldn't run tailscale: {e}"))?;
    ensure!(out.status.success(), "tailscale serve: {}", String::from_utf8_lossy(&out.stderr).trim());
    let status = std::process::Command::new("tailscale").args(["status", "--json"]).output()?;
    let v: serde_json::Value = serde_json::from_slice(&status.stdout)?;
    let dns = v["Self"]["DNSName"].as_str().map(|d| d.trim_end_matches('.')).filter(|d| !d.is_empty()).ok_or_else(|| anyhow!("tailscale status has no DNS name for this machine (turn on MagicDNS)"))?;
    Ok(format!("https://{dns}"))
}

/// What `launch` made.
pub struct Launched {
    pub home: PathBuf,
    pub plan: Plan,
}

/// Asks (or reads the flags), summons, writes the guide and prints what to do next. The caller
/// starts the node.
pub fn launch(a: &LaunchArgs) -> Result<Launched> {
    use std::io::IsTerminal;
    let m = Machine::detect();
    let interactive = !a.yes && std::io::stdin().is_terminal();
    let plan = if interactive {
        let stdin = std::io::stdin();
        let mut p = Prompter { inp: stdin.lock(), out: std::io::stdout() };
        let plan = plan_by_asking(&mut p, a, &m)?;
        writeln!(p.out, "\nReview")?;
        for (k, v) in plan.summary() {
            writeln!(p.out, "  {k:<15} {v}")?;
        }
        writeln!(p.out)?;
        if !p.confirm("Launch it?", true)? {
            bail!("nothing made");
        }
        plan
    } else {
        plan_from_flags(a, &m)?
    };
    let home = a.home.clone().unwrap_or_else(|| crate::cli_net::blockula_dir().join(&plan.chain_id));
    if home.join("genesis.json").exists() {
        bail!("{} already has a chain. Start it with `blockula run --home {}`, or pick another --chain-id", home.display(), display_home(&home));
    }
    Chain::summon(&home, &plan.summon_opts())?;
    let g = Genesis::load(&home.join("genesis.json"))?;
    let hash = hex::encode(crate::consensus::genesis_block(&g).digest_bytes());
    let https = if plan.https {
        match tailscale_serve(chain::SUMMON_HTTP_PORT) {
            Ok(u) => Some(u),
            Err(e) => {
                eprintln!("⚠️  HTTPS isn't on: {e}. Turn on HTTPS certificates in the Tailscale admin console, then run `tailscale serve --bg 7070`.");
                None
            }
        }
    } else {
        None
    };
    std::fs::write(home.join("LAUNCH.md"), guide(&plan, &g, &home, &hash, https.as_deref()))?;

    println!("\n🪦 {} is summoned ({}).", plan.name, plan.token);
    println!("   Explorer:      {}", plan.local_url());
    if crate::wallet::embedded() {
        println!("   Wallet:        {}", plan.wallet_url());
    } else {
        println!("   Wallet:        not built into this binary (npm run build:wallet, then rebuild; releases have it)");
    }
    if let Some(u) = &https {
        println!("   HTTPS:         {u}  (anyone on your tailnet)");
    }
    // The one link to send: the wallet, where friends' browsers make passkeys.
    match (&https, plan.shared_url()) {
        (Some(u), _) => println!("   Send friends:  {u}/wallet/"),
        (None, Some(u)) => println!("   Send friends:  {u}/wallet/  (plain http: for passkeys, serve it over HTTPS with `tailscale serve --bg {}`)", chain::SUMMON_HTTP_PORT),
        (None, None) => {}
    }
    if let Some(u) = plan.shared_url() {
        println!("   Their own node: blockula join --genesis {u} --genesis-hash {hash} --home ~/.blockula/{}", plan.chain_id);
    }
    if let Some(l) = listing(&plan, &g, &hash, https.as_deref()) {
        println!("   List it on blockula.app ({}):", l.note);
        println!("                  {}", l.link);
    }
    println!("   Start again:   {}", plan.run_command(&home));
    println!("   Everything else (apps, adding validators): {}", display_home(&home.join("LAUNCH.md")));
    Ok(Launched { home, plan })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(script: &str, a: &LaunchArgs, m: &Machine) -> (Result<Plan>, String) {
        let mut out = Vec::new();
        let r = plan_by_asking(&mut Prompter { inp: script.as_bytes(), out: &mut out }, a, m);
        (r, String::from_utf8(out).unwrap())
    }

    const NO_NET: Machine = Machine { tailscale_ip: None, lan_ip: None };

    #[test]
    fn names_become_ids_and_tickers() {
        assert_eq!(slugify("Jack in the Block"), "jack-in-the-block");
        assert_eq!(slugify("  Café — Ledger!! "), "caf-ledger");
        assert_eq!(slugify("🦇"), "");
        assert_eq!(suggest_token("Jack in the Block"), "JITB");
        assert_eq!(suggest_token("Ledger"), "LEDG");
        assert_eq!(suggest_token("🦇"), "FANG");
        assert_eq!(suggest_token("9 Lives"), "LIVE", "one letter is too short for a ticker");
        assert_eq!(suggest_token("Jack 2 Block"), "JB");
    }

    #[test]
    fn chain_ids() {
        check_chain_id("jack-in-the-block").unwrap();
        for bad in ["", "a", "Jack", "jack--block", "-jack", "jack/../x", "blockula-1", "blockula-testnet-3", "blockula-dev"] {
            assert!(check_chain_id(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn enter_takes_every_default() {
        let m = Machine { tailscale_ip: Some("100.64.0.7".into()), lan_ip: Some("192.168.1.20".into()) };
        // A name, then Enter for everything else.
        let (plan, out) = ask("Jack in the Block\n\n\n\n\n\n\n\n\n\n\n", &LaunchArgs::default(), &m);
        let plan = plan.unwrap();
        assert_eq!(plan, Plan {
            name: "Jack in the Block".into(),
            description: String::new(),
            token: "JITB".into(),
            chain_id: "jack-in-the-block".into(),
            reach: Reach::Tailscale,
            host: Some("100.64.0.7".into()),
            block_time_ms: 5_000,
            faucet: true,
            min_bond: None,
            programs: false,
            https: true,
        });
        assert!(out.contains("Tailscale") && out.contains("(found 100.64.0.7)") && out.contains("(found 192.168.1.20)"), "{out}");
        assert_eq!(plan.bind(), "0.0.0.0");
        assert_eq!(plan.shared_url().as_deref(), Some("http://100.64.0.7:7070"));
    }

    #[test]
    fn wrong_answers_are_asked_again() {
        // Without Tailscale the default is this machine; 4 picks the internet, which asks for an
        // address and turns the faucet off by default.
        let script = "\nJack in the Block\nJA CK\nJACK\nJack's friends' ledger.\nNot An Id\n\n9\n4\n\nhttp://x.org\nledger.example.org\n1\n\n0\n500\ny\n";
        let (plan, out) = ask(script, &LaunchArgs::default(), &NO_NET);
        let plan = plan.unwrap();
        assert_eq!((plan.token.as_str(), plan.reach, plan.host.as_deref()), ("JACK", Reach::Internet, Some("ledger.example.org")));
        assert_eq!((plan.block_time_ms, plan.faucet, plan.min_bond, plan.programs, plan.https), (1_000, false, Some(500 * DROPS_PER_FANG), true, false));
        assert_eq!(plan.description, "Jack's friends' ledger.");
        for e in ["chain name must be", "token must be", "a chain id is", "a number from 1 to 4", "enter the address", "--host must be", "more than 0"] {
            assert!(out.contains(e), "{e}: {out}");
        }
    }

    #[test]
    fn flags_skip_their_questions() {
        let a = LaunchArgs { name: Some("Jack in the Block".into()), token: Some("jack".into()), reach: Some(Reach::Local), block_time_ms: Some(2_000), programs: true, ..Default::default() };
        // Only the description, the chain id, the faucet and the bond are asked.
        let (plan, out) = ask("\n\n\n\n", &a, &NO_NET);
        let plan = plan.unwrap();
        assert_eq!((plan.token.as_str(), plan.host.clone(), plan.block_time_ms, plan.programs, plan.https), ("JACK", None, 2_000, true, false));
        assert!(!out.contains("Chain name") && !out.contains("A block every") && !out.contains("Pick 1–"), "{out}");
        assert_eq!(plan.bind(), "127.0.0.1");
        // Run out of answers: an error, not a loop.
        assert!(ask("", &LaunchArgs::default(), &NO_NET).0.unwrap_err().to_string().contains("stdin closed"));
    }

    #[test]
    fn flags_alone() {
        let m = Machine { tailscale_ip: Some("100.64.0.7".into()), lan_ip: None };
        let plan = plan_from_flags(&LaunchArgs { name: Some("Jack in the Block".into()), https: true, ..Default::default() }, &m).unwrap();
        assert_eq!((plan.token.as_str(), plan.reach, plan.host.as_deref(), plan.https), ("JITB", Reach::Tailscale, Some("100.64.0.7"), true));
        assert!(plan_from_flags(&LaunchArgs::default(), &m).unwrap_err().to_string().contains("--name is needed"));
        assert!(plan_from_flags(&LaunchArgs { name: Some("X Y".into()), reach: Some(Reach::Internet), ..Default::default() }, &m).unwrap_err().to_string().contains("--host is needed"));
        assert!(plan_from_flags(&LaunchArgs { name: Some("X Y".into()), reach: Some(Reach::Local), https: true, ..Default::default() }, &m).is_err());
        let local = plan_from_flags(&LaunchArgs { name: Some("X Y".into()), ..Default::default() }, &NO_NET).unwrap();
        assert_eq!((local.reach, local.host), (Reach::Local, None));
    }

    /// The `value` query parameter of a listing link, decoded.
    fn listed_json(link: &str) -> serde_json::Value {
        let v = link.split("&value=").nth(1).unwrap();
        let bytes = v.as_bytes();
        let mut out = vec![];
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' {
                out.push(u8::from_str_radix(&v[i + 1..i + 3], 16).unwrap());
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        serde_json::from_slice(&out).unwrap()
    }

    /// A chain the internet reaches gets a "List it on blockula.app" link: with validators on the
    /// open internet, wallets only on a tailnet served over HTTPS (Funnel), none otherwise.
    #[test]
    fn listing_links() {
        let home = std::env::temp_dir().join(format!("blockula-launch-list-{}-{}", chain::now_ms(), std::process::id()));
        let plan = Plan {
            name: "Jack in the Block".into(),
            description: "Jack's friends' ledger.".into(),
            token: "JACK".into(),
            chain_id: "jack-in-the-block".into(),
            reach: Reach::Internet,
            host: Some("ledger.example.org".into()),
            block_time_ms: 5_000,
            faucet: true,
            min_bond: None,
            programs: false,
            https: false,
        };
        Chain::summon(&home, &plan.summon_opts()).unwrap();
        let g = Genesis::load(&home.join("genesis.json")).unwrap();
        let hash = hex::encode(crate::consensus::genesis_block(&g).digest_bytes());

        let l = listing(&plan, &g, &hash, None).unwrap();
        assert!(l.link.starts_with("https://github.com/jackdishman/blockula/new/main?filename=chains%2Fjack-in-the-block.json&value=%7B%0A"), "{}", l.link);
        assert!(l.note.contains("https://ledger.example.org serves this node over HTTPS") && l.note.contains("validators wanted"), "{}", l.note);
        let j = listed_json(&l.link);
        assert_eq!(j, serde_json::json!({
            "name": "Jack in the Block",
            "chain_id": "jack-in-the-block",
            "token": "JACK",
            "description": "Jack's friends' ledger.",
            "rpc": "https://ledger.example.org",
            "genesis_hash": hash,
            "validators_wanted": true,
            "p2p": "ledger.example.org:30300",
            "contact": "<your GitHub handle>",
        }));
        assert!(guide(&plan, &g, &home, &hash, None).contains(&format!("## List it on blockula.app\n\nAnyone can find it in the chain directory at https://blockula.app once https://ledger.example.org serves")));

        // An IP address can't be a listing's https name: the rpc is left for a DNS name.
        let ip = Plan { host: Some("203.0.113.10".into()), description: String::new(), ..plan.clone() };
        let j = listed_json(&listing(&ip, &g, &hash, None).unwrap().link);
        assert_eq!((j["rpc"].as_str(), j["description"].as_str()), (Some("https://<a DNS name for this machine>"), Some("<one sentence about it>")));

        // A tailnet: listed for its wallet once Funnel serves it, and only with HTTPS on.
        let ts = Plan { reach: Reach::Tailscale, host: Some("100.64.0.7".into()), https: true, ..plan.clone() };
        assert_eq!(listing(&ts, &g, &hash, None), None);
        let l = listing(&ts, &g, &hash, Some("https://box.tail1234.ts.net")).unwrap();
        assert!(l.note.contains("tailscale funnel --bg 7070") && l.note.contains("Wallets only"), "{}", l.note);
        let j = listed_json(&l.link);
        assert_eq!((j["rpc"].as_str(), j["validators_wanted"].as_bool(), j.get("p2p")), (Some("https://box.tail1234.ts.net"), Some(false), None));

        for (reach, host) in [(Reach::Local, None), (Reach::Home, Some("192.168.1.20".to_string()))] {
            let p = Plan { reach, host, ..plan.clone() };
            assert_eq!(listing(&p, &g, &hash, None), None, "{reach:?}");
            assert!(!guide(&p, &g, &home, &hash, None).contains("blockula.app"));
        }
        assert_eq!(url_encode("a b/é\"~"), "a%20b%2F%C3%A9%22~");
        std::fs::remove_dir_all(home).ok();
    }

    /// The plan summons what it says, and the guide names the real genesis hash.
    #[test]
    fn summons_the_plan_and_writes_the_guide() {
        let home = std::env::temp_dir().join(format!("blockula-launch-{}-{}", chain::now_ms(), std::process::id()));
        let plan = Plan {
            name: "Jack in the Block".into(),
            description: "Jack's friends' ledger.".into(),
            token: "JACK".into(),
            chain_id: "jack-in-the-block".into(),
            reach: Reach::Tailscale,
            host: Some("100.64.0.7".into()),
            block_time_ms: 2_000,
            faucet: false,
            min_bond: Some(500 * DROPS_PER_FANG),
            programs: false,
            https: false,
        };
        Chain::summon(&home, &plan.summon_opts()).unwrap();
        let g = Genesis::load(&home.join("genesis.json")).unwrap();
        assert_eq!((g.token.as_str(), g.brand.as_ref().unwrap().name.as_str(), g.block_time_ms, g.faucet), ("JACK", "Jack in the Block", 2_000, false));
        assert_eq!(g.chain_params().unwrap().min_self_bond, 500 * DROPS_PER_FANG);
        assert_eq!(g.validators[0].http_addr, "100.64.0.7:7070");
        let hash = hex::encode(crate::consensus::genesis_block(&g).digest_bytes());
        let guide = guide(&plan, &g, &home, &hash, Some("https://box.tail1234.ts.net"));
        for want in [format!("--genesis-hash {hash}"), "blockula join --genesis http://100.64.0.7:7070".into(), "https://box.tail1234.ts.net".into(), "Add to Dock".into(), "--bind 0.0.0.0".into()] {
            assert!(guide.contains(&want), "{want}: {guide}");
        }
        assert!(plan.summary().iter().any(|(k, v)| *k == "Validator bond" && v == "500 JACK"));
        // The wallet every node serves: the one link friends get.
        assert!(guide.contains("- Wallet: http://localhost:7070/wallet/ (friends: https://box.tail1234.ts.net/wallet/)"), "{guide}");
        assert!(guide.contains("Send them https://box.tail1234.ts.net/wallet/."), "{guide}");
        let plain = super::guide(&plan, &g, &home, &hash, None);
        assert!(plain.contains("Send them http://100.64.0.7:7070/wallet/") && plain.contains("tailscale serve --bg 7070"), "{plain}");
        assert!(plan.summary().iter().any(|(k, v)| *k == "Wallet" && v == "http://localhost:7070/wallet/"));
        let https = Plan { https: true, ..plan };
        assert!(https.summary().iter().any(|(k, v)| *k == "Wallet" && v.contains("ts.net/wallet/")));
        std::fs::remove_dir_all(home).ok();
    }
}
