//! `blockula hunt …` (bounties on GitHub issues), `blockula contrib …` (Proof of Contribution:
//! claims, vouches, Séance jury duty, rewards, and the Season 0 admin levers), `blockula name …`
//! (renew, transfer, release) and `blockula stake …` (delegating to validator pools). Talks to
//! any node's HTTP API and signs with the CLI wallet or a key file.

use crate::cli_net::{ChainPin, Remote};
use crate::crypto::Keys;
use crate::types::*;
use crate::validator_cli::{get, load_seed, own_address, parse_fang, rt, wallet_keys};
use anyhow::{anyhow, bail, ensure, Context, Result};
use clap::{Args, Subcommand};
use serde_json::Value;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

/// Who signs, and where to send it.
#[derive(Args, Clone)]
pub struct Signer {
    /// File holding a 32-byte hex seed (e.g. an admin's key file or a validator's validator.key).
    /// Default: the CLI wallet (~/.blockula/wallet.json, see `blockula wallet new`)
    #[arg(long)]
    pub key: Option<PathBuf>,
    /// Act for this account (name or address) if the key was added to another account
    #[arg(long)]
    pub from: Option<String>,
    #[arg(long, default_value = "http://localhost:7070")]
    pub rpc: String,
    /// The chain the node at --rpc must be on (WAL-4)
    #[command(flatten)]
    pub pin: ChainPin,
}

#[derive(Args, Clone)]
pub struct Rpc {
    #[arg(long, default_value = "http://localhost:7070")]
    pub rpc: String,
}

impl Signer {
    fn keys(&self, dir: &Path) -> Result<Keys> {
        match &self.key {
            Some(p) => load_seed(p),
            None => wallet_keys(dir),
        }
    }

    async fn from(&self, keys: &Keys) -> Result<String> {
        match &self.from {
            Some(a) => resolve(&self.rpc, a).await,
            None => own_address(&self.rpc, keys).await,
        }
    }

    /// The node at `--rpc`, checked to be on the chain this command is for.
    pub(crate) async fn connect(&self, dir: &Path) -> Result<Remote> {
        Remote::connect(dir, &self.rpc, &self.pin).await
    }

    /// The account to sign for on `remote`'s chain: `--from`, or the key's own address. A name in
    /// `--from` is resolved by the node, unproven; on mainnet that isn't enough to sign for
    /// (audit gap-7-1): pass the address.
    pub(crate) async fn acting_for(&self, remote: &Remote, keys: &Keys) -> Result<String> {
        match &self.from {
            Some(a) => from_on_chain(a, &remote.resolve(a).await?, &remote.chain_id, &remote.rpc),
            None => Ok(remote.address(keys)),
        }
    }
}

/// `--from` as signed: an address as given; a name only off mainnet, where the CLI has printed
/// the node's (unproven) answer for it. On mainnet the account every tx names must be one the
/// user typed as an address, so one node can't pick it (audit gap-7-1).
pub(crate) fn from_on_chain(given: &str, resolved: &str, chain_id: &str, rpc: &str) -> Result<String> {
    if crate::crypto::is_address(given.trim()) || chain_id != crate::chain::MAINNET_CHAIN_ID {
        return Ok(resolved.to_string());
    }
    bail!("--from {given}: {rpc} says that's {resolved}, but that's one node's word, and on mainnet the CLI only signs for an account given as an address. Check the address with the account's owner and pass --from {resolved}. Nothing was signed.")
}

/// An address the node resolves a name to, unproven (printed): only where an address is needed.
async fn resolve(rpc: &str, who: &str) -> Result<String> {
    crate::cli_net::resolve_unproven(rpc, who).await
}

/// Signs and submits `action` for the chain `s` pins, waits for it, and prints the summary and
/// explorer link.
fn submit(dir: &Path, s: &Signer, action: Action) -> Result<Value> {
    submit_at(dir, s, |_, _| Ok(action))
}

/// [`submit`] for an action that carries an absolute height: `make(height, blocks_per_day)`
/// builds it from the status read that sets `valid_until`, with the day length from the pinned
/// genesis (audit gap-7-3).
fn submit_at(dir: &Path, s: &Signer, make: impl FnOnce(u64, u64) -> Result<Action>) -> Result<Value> {
    let keys = s.keys(dir)?;
    let rt = rt()?;
    let tx = rt.block_on(async {
        let remote = s.connect(dir).await?;
        let from = s.acting_for(&remote, &keys).await?;
        let day = remote.blocks_per_day().await?;
        crate::validator_cli::send_memo_with(&remote, &keys, &from, |height| make(height, day), String::new()).await
    })?;
    println!("✓ {}", tx["summary"].as_str().unwrap_or_default());
    println!("  {}/#/tx/{}", s.rpc.trim_end_matches('/'), tx["id"].as_str().unwrap_or_default());
    Ok(tx)
}

fn name_of(v: &Value) -> String {
    v["name"].as_str().or(v["short"].as_str()).unwrap_or_default().to_string()
}

pub(crate) fn confirm(question: &str, yes: bool) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        bail!("{question}\n  (not a terminal; pass --yes to confirm)");
    }
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    if !matches!(line.trim(), "y" | "Y" | "yes") {
        bail!("cancelled");
    }
    Ok(())
}

// ------------------------------------------------------------------------------------- hunts

#[derive(Subcommand)]
pub enum HuntCmd {
    /// List hunts, biggest bounty first
    List {
        /// open, awarded, cancelled or expired
        #[arg(long, default_value = "open")]
        status: String,
        #[command(flatten)]
        rpc: Rpc,
    },
    /// Show one hunt
    Show {
        id: u64,
        #[command(flatten)]
        rpc: Rpc,
    },
    /// Put a bounty on a GitHub issue (escrowed from your account)
    Create {
        /// owner/name
        repo: String,
        issue: u64,
        /// Bounty in FANG (at least 10)
        amount: String,
        #[arg(long)]
        title: String,
        /// How long it stays open (max ~365)
        #[arg(long, default_value = "30")]
        days: u64,
        #[command(flatten)]
        signer: Signer,
    },
    /// Add FANG to an open hunt
    Fund {
        id: u64,
        amount: String,
        #[command(flatten)]
        signer: Signer,
    },
    /// Pay a hunt's bounty for a merged PR (only the hunt's creator can). Checks on
    /// GitHub that the PR merged into the hunt's repo, and pays the account its author linked
    /// with `link_github`, unless you name a recipient with --to.
    Award {
        /// Hunt id (see `blockula hunt list`)
        id: u64,
        /// The merged pull request's number, in the hunt's repo
        #[arg(long)]
        pr: u64,
        /// Recipient (.boo name or boo1… address). Default: the PR author's linked account
        #[arg(long)]
        to: Option<String>,
        /// The recipient's Familiar that did the work (its label or key id)
        #[arg(long)]
        familiar: Option<String>,
        /// Don't ask GitHub (pay --to without checking the merge)
        #[arg(long)]
        skip_github: bool,
        /// Don't ask for confirmation
        #[arg(long)]
        yes: bool,
        #[command(flatten)]
        signer: Signer,
    },
    /// Refund every funder (the creator after expiry; on dev and test chains an admin any time)
    Cancel {
        id: u64,
        #[command(flatten)]
        signer: Signer,
    },
}

fn print_hunt(h: &Value) {
    println!("#{} {} · {} [{}]", h["id"], h["amount_display"].as_str().unwrap_or_default(), h["title"].as_str().unwrap_or_default(), h["status"].as_str().unwrap_or_default());
    println!("   {}  (expires at block {})", h["issue_url"].as_str().unwrap_or_default(), h["expires_at"]);
    if let Some(to) = h["awarded_to"].as_object() {
        println!("   won by {} for {}", name_of(&Value::Object(to.clone())), h["pr_url"].as_str().unwrap_or_default());
    }
}

pub fn hunt(cmd: HuntCmd, dir: &Path) -> Result<()> {
    match cmd {
        HuntCmd::List { status, rpc } => {
            let v = rt()?.block_on(get(&rpc.rpc, &format!("/hunts?status={status}")))?;
            println!("{} hunts · open bounties {}", v["count"], v["open_bounties_display"].as_str().unwrap_or_default());
            for h in v["hunts"].as_array().cloned().unwrap_or_default() {
                print_hunt(&h);
            }
            Ok(())
        }
        HuntCmd::Show { id, rpc } => {
            let h = rt()?.block_on(get(&rpc.rpc, &format!("/hunt/{id}")))?;
            print_hunt(&h);
            println!("   created by {} · funders: {}", name_of(&h["creator"]), h["funders"].as_array().map(|f| f.iter().map(|x| format!("{} {}", name_of(&x["account"]), x["amount_display"].as_str().unwrap_or_default())).collect::<Vec<_>>().join(", ")).unwrap_or_default());
            Ok(())
        }
        HuntCmd::Create { repo, issue, amount, title, days, signer } => {
            check_repo(&repo)?;
            let amount = parse_fang(&amount)?;
            // The expiry counts `days` days of the genesis's blocks from the status the tx is
            // signed against, not a separate read or the node's block time (audit gap-7-3).
            submit_at(dir, &signer, |height, day| Ok(Action::CreateHunt { repo, issue, amount, expires_at: height.saturating_add(days.saturating_mul(day)), title }))?;
            Ok(())
        }
        HuntCmd::Fund { id, amount, signer } => submit(dir, &signer, Action::FundHunt { hunt_id: id, amount: parse_fang(&amount)? }).map(|_| ()),
        HuntCmd::Award { id, pr, to, familiar, skip_github, yes, signer } => award(dir, id, pr, to, familiar, skip_github, yes, &signer),
        HuntCmd::Cancel { id, signer } => submit(dir, &signer, Action::CancelHunt { hunt_id: id }).map(|_| ()),
    }
}

#[allow(clippy::too_many_arguments)]
fn award(dir: &Path, id: u64, pr: u64, to: Option<String>, familiar: Option<String>, skip_github: bool, yes: bool, signer: &Signer) -> Result<()> {
    let rt = rt()?;
    let rpc = signer.rpc.as_str();
    let h = rt.block_on(get(rpc, &format!("/hunt/{id}")))?;
    let status = h["status"].as_str().unwrap_or_default();
    if status != "open" && status != "expired" {
        bail!("hunt #{id} is {status}");
    }
    let repo = h["repo"].as_str().unwrap_or_default().to_string();
    let issue = h["issue"].as_u64().unwrap_or(0);
    let mut why = String::new();
    let pr_info = if skip_github {
        if to.is_none() {
            bail!("--skip-github needs --to (there's no PR author to look up)");
        }
        None
    } else {
        let p = rt.block_on(crate::github::fetch_pr(&repo, pr)).with_context(|| format!("checking {repo}#{pr} on GitHub (set GITHUB_TOKEN if rate limited, or --skip-github)"))?.ok_or_else(|| anyhow!("{repo}#{pr} doesn't exist on GitHub"))?;
        if !p.base_repo.eq_ignore_ascii_case(&repo) {
            bail!("PR #{pr} is in {}, not {repo}", p.base_repo);
        }
        if !p.merged {
            bail!("{repo}#{pr} isn't merged ({}). Award it once it merges, or pass --skip-github --to …", p.state);
        }
        if !p.on_default_branch() {
            bail!("{repo}#{pr} merged into {}, not the default branch ({}). Pass --skip-github --to … to award it anyway", p.base_branch, p.default_branch);
        }
        let mention = format!("#{issue}");
        if !p.body.contains(&mention) && !p.title.contains(&mention) {
            println!("⚠️  PR #{pr} doesn't mention issue {mention}; double-check it's the right PR.");
        }
        why = format!(" “{}” by @{}", p.title, p.author);
        Some(p)
    };
    let recipient = match (&to, &pr_info) {
        // A name is signed as the name: the chain resolves it (audit gap-7-1).
        (Some(t), _) => crate::cli_net::account_ref(t)?,
        (None, Some(p)) => {
            let link = rt.block_on(get(rpc, &format!("/github/{}", p.author))).map_err(|_| anyhow!("@{} hasn't linked a Blockula account (they can run link_github); or pass --to <name>", p.author))?;
            if link["verified"] != true {
                bail!("@{}'s link isn't verified: {}. Pass --to to award anyway.", p.author, link["reason"].as_str().unwrap_or("unknown"));
            }
            let addr = link["account"]["address"].as_str().map(String::from).ok_or_else(|| anyhow!("bad /api/github response"))?;
            ensure!(crate::crypto::is_address(&addr), "{rpc} linked @{} to {addr:?}, which isn't an address", p.author);
            // The link is the node's word (audit gap-7-1): the address is printed and confirmed.
            eprintln!("   @{} → {addr} (the account {rpc} says their GitHub link names, unproven: check the address, or pass --to)", p.author);
            addr
        }
        (None, None) => unreachable!(),
    };
    let acct = rt.block_on(get(rpc, &format!("/account/{recipient}")))?;
    let familiar_key = match &familiar {
        None => None,
        Some(f) => {
            let list = acct["familiars"].as_array().cloned().unwrap_or_default();
            let hit = list.iter().find(|x| x["label"] == f.as_str() || x["key_id"] == f.as_str()).ok_or_else(|| anyhow!("{} has no Familiar “{f}”", name_of(&acct["account"])))?;
            hit["key_id"].as_str().map(String::from)
        }
    };
    confirm(&format!("Award {} (hunt #{id}, {repo}#{issue}) to {} ({recipient}) for PR #{pr}{why}?", h["amount_display"].as_str().unwrap_or_default(), name_of(&acct["account"])), yes)?;
    submit(dir, signer, Action::AwardHunt { hunt_id: id, to: recipient, pr, familiar_key })?;
    Ok(())
}

// ------------------------------------------------------------------------------ contribution

#[derive(Subcommand)]
pub enum ContribCmd {
    /// The pool, its repos and rules, and the open Séance
    Status {
        #[command(flatten)]
        rpc: Rpc,
    },
    /// Link your GitHub handle: first make a PUBLIC gist (as that user) containing your boo1
    /// address, then pass its id. Claims and hunt awards use this link.
    LinkGithub {
        handle: String,
        gist_id: String,
        #[command(flatten)]
        signer: Signer,
    },
    /// Claim a merged PR (authored by your linked GitHub handle) for the contribution pool
    Claim {
        /// owner/name (must be in the pool)
        repo: String,
        pr: u64,
        /// Credit one of your Familiars (its key id)
        #[arg(long)]
        familiar: Option<String>,
        #[command(flatten)]
        signer: Signer,
    },
    /// List claims (yours by default)
    Claims {
        /// Name or address (default: the wallet's account)
        #[arg(long)]
        account: Option<String>,
        #[command(flatten)]
        signer: Signer,
    },
    /// Stake 10 FANG on a newcomer (no Soul or judged work yet) being a real, distinct person (needs 10 Soul)
    Vouch {
        account: String,
        #[command(flatten)]
        signer: Signer,
    },
    /// Withdraw a vouch and its bond (after the lock), or, as the newcomer, decline one
    Unvouch {
        account: String,
        #[command(flatten)]
        signer: Signer,
    },
    /// Join (or top up) the Séance juror pool (needs 5 Soul)
    JoinJury {
        /// Bond in FANG
        #[arg(long, default_value = "10")]
        bond: String,
        #[command(flatten)]
        signer: Signer,
    },
    /// Leave the juror pool and get the bond back
    LeaveJury {
        #[command(flatten)]
        signer: Signer,
    },
    /// Show a Séance and, if you're on its jury, the pairs you must judge
    Seance {
        id: Option<u64>,
        #[command(flatten)]
        signer: Signer,
    },
    /// Seal your judgment: one of a, b or = per pair, in order (e.g. "ab=a"). The salt is kept
    /// in ~/.blockula/seances/ for `reveal`. The judgment binds the pairs it was made on: pass
    /// the pairs digest `contrib seance` printed (--digest), or check the pairs printed here.
    #[command(alias = "commit")]
    Vote {
        seance: u64,
        #[arg(long)]
        votes: String,
        /// Claim ids you think are spam (comma-separated)
        #[arg(long, value_delimiter = ',')]
        spam: Vec<u64>,
        /// The pairs digest `contrib seance` printed with the pairs you judged: refused if the
        /// node now gives other pairs (audit gap-7-6)
        #[arg(long)]
        digest: Option<String>,
        /// Without --digest, don't ask for confirmation after printing the pairs it binds
        #[arg(long)]
        yes: bool,
        #[command(flatten)]
        signer: Signer,
    },
    /// Reveal the judgment you sealed with `vote`
    Reveal {
        seance: u64,
        #[command(flatten)]
        signer: Signer,
    },
    /// Withdraw rewards that have vested
    Withdraw {
        #[command(flatten)]
        signer: Signer,
    },
    /// Add FANG to the contribution pool
    Fund {
        amount: String,
        #[command(flatten)]
        signer: Signer,
    },
    /// Season 0 admin levers
    Admin {
        #[command(subcommand)]
        cmd: AdminCmd,
    },
}

#[derive(Subcommand)]
pub enum AdminCmd {
    /// Put a repo in the pool with a weight (1–100), or remove it with 0
    SetRepo {
        repo: String,
        weight: u64,
        #[command(flatten)]
        signer: Signer,
    },
    /// Bootstrap Soul for an account (up to 100 per grant)
    GrantSoul {
        account: String,
        /// Soul, e.g. 10 or 2.5
        soul: String,
        #[arg(long)]
        reason: String,
        #[command(flatten)]
        signer: Signer,
    },
    /// Reject a claim (e.g. a reverted PR); a scored claim's unvested reward is clawed back
    Reject {
        claim: u64,
        #[arg(long)]
        reason: String,
        #[command(flatten)]
        signer: Signer,
    },
    /// Flag an account as a sybil (slashes its voucher, claws back its rewards)
    FlagSybil {
        account: String,
        #[arg(long)]
        reason: String,
        #[command(flatten)]
        signer: Signer,
    },
    /// Return an account's unvested rewards to the pool
    Clawback {
        account: String,
        #[arg(long)]
        reason: String,
        #[command(flatten)]
        signer: Signer,
    },
}

fn parse_soul(s: &str) -> Result<u64> {
    let (whole, frac) = s.trim().split_once('.').unwrap_or((s.trim(), ""));
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) || frac.len() > 3 || !frac.bytes().all(|b| b.is_ascii_digit()) {
        bail!("Soul is a number with up to 3 decimals, e.g. 10 or 2.5");
    }
    Ok(whole.parse::<u64>()?.saturating_mul(crate::poc::SOUL) + format!("{frac:0<3}").parse::<u64>()?)
}

fn print_claim(c: &Value) {
    let score = c["score_display"].as_str().map(|s| format!(" · score {s}")).unwrap_or_default();
    let paid = if c["payout"] != "0" { format!(" · earned {}", c["payout_display"].as_str().unwrap_or_default()) } else { String::new() };
    println!("   claim #{} {}#{} [{}]{score}{paid}", c["id"], c["repo"].as_str().unwrap_or_default(), c["pr"], c["status"].as_str().unwrap_or_default());
    if let Some(r) = c["reason"].as_str() {
        println!("      {r}");
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SealedVote {
    chain_id: String,
    seance: u64,
    account: String,
    votes: String,
    spam: Vec<u64>,
    salt: String,
}

/// One claim of a Séance pair as a judgment binds it: (id, repo, PR number).
type ClaimRef = (u64, String, u64);

/// `account`'s assigned pairs in a Séance (the node's `/api/seance/{id}` answer), in order, each
/// claim with its repo and PR: what a sealed judgment binds (`seance_pairs_digest`).
pub(crate) fn seat_pairs(s: &Value, seance: u64, account: &str) -> Result<Vec<[ClaimRef; 2]>> {
    let seat = s["jurors"].as_array().into_iter().flatten().find(|j| j["juror"]["address"] == account).ok_or_else(|| anyhow!("{account} isn't on the jury of Séance #{seance}"))?;
    let claims: Vec<Value> = s["claims"].as_array().cloned().unwrap_or_default();
    let claim = |id: &Value| -> Result<ClaimRef> {
        let id = id.as_u64().ok_or_else(|| anyhow!("Séance #{seance} lists a pair with a bad claim id: {id}"))?;
        let c = claims.iter().find(|c| c["id"] == id).ok_or_else(|| anyhow!("Séance #{seance} doesn't list claim #{id}"))?;
        Ok((id, c["repo"].as_str().unwrap_or_default().to_string(), c["pr"].as_u64().unwrap_or_default()))
    };
    seat["assigned_pairs"].as_array().into_iter().flatten().map(|p| Ok([claim(&p[0])?, claim(&p[1])?])).collect()
}

/// `seance_pairs_digest` of a seat's pairs.
pub(crate) fn pairs_digest(seance: u64, account: &str, pairs: &[[ClaimRef; 2]]) -> String {
    let refs: Vec<[(u64, &str, u64); 2]> = pairs.iter().map(|[a, b]| [(a.0, a.1.as_str(), a.2), (b.0, b.1.as_str(), b.2)]).collect();
    seance_pairs_digest(seance, account, &refs)
}

/// The pairs as a juror reads them: which claim and PR is `a`, which is `b`.
pub(crate) fn pair_lines(pairs: &[[ClaimRef; 2]]) -> Vec<String> {
    let pr = |c: &ClaimRef| format!("claim #{} {}#{} (https://github.com/{}/pull/{})", c.0, c.1, c.2, c.1, c.2);
    pairs.iter().enumerate().map(|(i, [a, b])| format!("  {:>2}. a = {}\n      b = {}", i + 1, pr(a), pr(b))).collect()
}

fn sealed_path(dir: &Path, chain_id: &str, seance: u64, account: &str) -> PathBuf {
    dir.join("seances").join(format!("{chain_id}-{seance}-{account}.json"))
}

pub fn contrib(cmd: ContribCmd, dir: &Path) -> Result<()> {
    match cmd {
        ContribCmd::Status { rpc } => {
            let v = rt()?.block_on(get(&rpc.rpc, "/contributions"))?;
            println!("🪦 contribution pool: {} (next Séance pays out up to {})", v["pool"]["balance_display"].as_str().unwrap_or_default(), v["pool"]["next_budget_display"].as_str().unwrap_or_default());
            let repos: Vec<String> = v["repos"].as_array().cloned().unwrap_or_default().iter().map(|r| format!("{} (weight {})", r["repo"].as_str().unwrap_or_default(), r["weight"])).collect();
            println!("   repos: {}", if repos.is_empty() { "none yet".into() } else { repos.join(", ") });
            println!("   claims: {}", v["claims"]);
            println!("   jurors: {} in the pool, {} active", v["jurors"]["pool"], v["jurors"]["active"]);
            match v["open_seance"].as_object() {
                Some(s) => println!("   Séance #{} is {} (sealed votes until block {}, reveals until {}): {} claims, {} jurors, budget {}", s["id"], s["phase"].as_str().unwrap_or_default(), s["commit_until"], s["reveal_until"], s["claims"].as_array().map(|a| a.len()).unwrap_or(0), s["jurors"].as_array().map(|a| a.len()).unwrap_or(0), s["budget_display"].as_str().unwrap_or_default()),
                None => println!("   no Séance open; the next can open from block {} once ≥ 2 claims are ready and ≥ {} jurors are available", v["next_seance_at"], v["rules"]["min_jury"]),
            }
            Ok(())
        }
        ContribCmd::LinkGithub { handle, gist_id, signer } => {
            submit(dir, &signer, Action::LinkGithub { handle: handle.clone(), gist_id })?;
            let v = rt()?.block_on(get(&signer.rpc, &format!("/github/{handle}")))?;
            println!("  {}: {}", if v["verified"] == true { "✓ verified" } else { "⚠️  not verified yet" }, v["reason"].as_str().unwrap_or_default());
            Ok(())
        }
        ContribCmd::Claim { repo, pr, familiar, signer } => submit(dir, &signer, Action::SubmitClaim { repo, pr, familiar_key: familiar }).map(|_| ()),
        ContribCmd::Claims { account, signer } => {
            let rt = rt()?;
            let who = match account.or(signer.from.clone()) {
                Some(a) => a,
                None => rt.block_on(own_address(&signer.rpc, &signer.keys(dir).map_err(|_| anyhow!("no wallet here; pass --account"))?))?,
            };
            let v = rt.block_on(get(&signer.rpc, &format!("/account/{who}")))?;
            let p = &v["contribution"];
            println!("{} · Soul {} · rewards: {} withdrawable, {} still vesting", name_of(&v["account"]), p["soul"]["display"].as_str().unwrap_or("0"), p["rewards"]["withdrawable_display"].as_str().unwrap_or_default(), p["rewards"]["unvested_display"].as_str().unwrap_or_default());
            for c in p["claims"].as_array().cloned().unwrap_or_default() {
                print_claim(&c);
            }
            Ok(())
        }
        ContribCmd::Vouch { account, signer } => {
            let to = crate::cli_net::account_ref(&account)?;
            submit(dir, &signer, Action::Vouch { account: to }).map(|_| ())
        }
        ContribCmd::Unvouch { account, signer } => {
            let to = crate::cli_net::account_ref(&account)?;
            submit(dir, &signer, Action::Unvouch { account: to }).map(|_| ())
        }
        ContribCmd::JoinJury { bond, signer } => submit(dir, &signer, Action::JoinJury { bond: parse_fang(&bond)? }).map(|_| ()),
        ContribCmd::LeaveJury { signer } => submit(dir, &signer, Action::LeaveJury {}).map(|_| ()),
        ContribCmd::Seance { id, signer } => {
            let rt = rt()?;
            let id = match id {
                Some(id) => id,
                None => rt.block_on(get(&signer.rpc, "/contributions"))?["open_seance"]["id"].as_u64().ok_or_else(|| anyhow!("no Séance is open; pass an id"))?,
            };
            let s = rt.block_on(get(&signer.rpc, &format!("/seance/{id}")))?;
            println!("🕯️  Séance #{id} · {} · sealed votes until block {}, reveals until {} · budget {}", s["phase"].as_str().unwrap_or_default(), s["commit_until"], s["reveal_until"], s["budget_display"].as_str().unwrap_or_default());
            let claims: Vec<Value> = s["claims"].as_array().cloned().unwrap_or_default();
            for c in &claims {
                print_claim(c);
            }
            let me = match (&signer.from, signer.keys(dir).ok()) {
                (Some(f), _) => Some(rt.block_on(resolve(&signer.rpc, f))?),
                (None, Some(k)) => Some(rt.block_on(own_address(&signer.rpc, &k))?),
                (None, None) => None,
            };
            let seat = s["jurors"].as_array().cloned().unwrap_or_default().into_iter().find(|j| me.as_deref().is_some_and(|m| j["juror"]["address"] == m));
            match seat {
                Some(seat) => {
                    // The pairs (and each claim's repo and PR) are what a judgment binds: printed
                    // with their digest, which `contrib vote --digest` checks (audit gap-7-6).
                    let me = me.unwrap_or_default();
                    let pairs = seat_pairs(&s, id, &me)?;
                    println!("\nYou're on this jury. For each pair, which PR mattered more? a, b, or = (about equal):");
                    for l in pair_lines(&pairs) {
                        println!("{l}");
                    }
                    let d = pairs_digest(id, &me, &pairs);
                    println!("\npairs digest: {d}");
                    println!("Then: blockula contrib vote {id} --votes <{} chars of a/b/=> --digest {d} [--spam <claim ids>]", pairs.len());
                    println!("Status: {}", if seat["revealed"] == true { "revealed" } else if seat["committed"] == true { "sealed (reveal after the sealing phase)" } else { "not voted yet" });
                }
                None => println!("\n{} jurors: {}", s["jurors"].as_array().map(|a| a.len()).unwrap_or(0), s["jurors"].as_array().cloned().unwrap_or_default().iter().map(|j| format!("{}{}", name_of(&j["juror"]), if j["revealed"] == true { " ✓" } else if j["committed"] == true { " 🔒" } else { "" })).collect::<Vec<_>>().join(", ")),
            }
            Ok(())
        }
        ContribCmd::Vote { seance, votes, spam, digest, yes, signer } => {
            let keys = signer.keys(dir)?;
            let rt = rt()?;
            let account = rt.block_on(signer.from(&keys))?;
            let chain_id = rt.block_on(get(&signer.rpc, "/status"))?["chain_id"].as_str().unwrap_or_default().to_string();
            // The judgment binds the pairs and PRs, read again here. That read is the node's word
            // too, so it must be what the juror judged: the pairs digest `contrib seance` printed
            // (--digest), or the pairs printed here and confirmed (audit gap-7-6, review-r2-clients-3).
            let s = rt.block_on(get(&signer.rpc, &format!("/seance/{seance}")))?;
            let pairs = seat_pairs(&s, seance, &account)?;
            if votes.len() != pairs.len() {
                bail!("you have {} assigned pairs: pass one of a, b or = for each (`blockula contrib seance {seance}` lists them)", pairs.len());
            }
            let pairs_digest = pairs_digest(seance, &account, &pairs);
            println!("Your judgment in Séance #{seance} binds these pairs, in order:");
            for (l, v) in pair_lines(&pairs).iter().zip(votes.chars()) {
                println!("{l}\n      your vote: {}", match v {
                    'a' => "a",
                    'b' => "b",
                    '=' => "= (about equal)",
                    _ => "(not a, b or =)",
                });
            }
            println!("pairs digest: {pairs_digest}");
            if !crate::gov_cli::check_digest(digest.as_deref(), &pairs_digest, &format!("Séance #{seance}'s pairs for you"))? {
                confirm("Seal this judgment on exactly these pairs?", yes)?;
            }
            let mut salt = [0u8; 16];
            getrandom::fill(&mut salt).map_err(|e| anyhow!("no randomness: {e}"))?;
            let salt = hex::encode(salt);
            let commitment = seance_commitment(seance, &account, &votes, &spam, &salt);
            let path = sealed_path(dir, &chain_id, seance, &account);
            std::fs::create_dir_all(path.parent().unwrap())?;
            // Replace the saved vote only once the new commitment is on-chain, so a failed
            // re-vote can't leave a file that no longer matches what you sealed.
            let tmp = path.with_extension("pending");
            std::fs::write(&tmp, serde_json::to_vec_pretty(&SealedVote { chain_id, seance, account, votes, spam, salt })?)?;
            // On failure the .pending file stays: if the commit lands anyway, `reveal` finds it.
            submit(dir, &signer, Action::CommitVotes { seance_id: seance, commitment, pairs_digest })?;
            std::fs::rename(&tmp, &path)?;
            println!("  sealed vote saved to {} — run `blockula contrib reveal {seance}` once revealing opens", path.display());
            Ok(())
        }
        ContribCmd::Reveal { seance, signer } => {
            let keys = signer.keys(dir)?;
            let rt = rt()?;
            let account = rt.block_on(signer.from(&keys))?;
            let chain_id = rt.block_on(get(&signer.rpc, "/status"))?["chain_id"].as_str().unwrap_or_default().to_string();
            let path = sealed_path(dir, &chain_id, seance, &account);
            // Reveal whichever saved vote matches what's sealed on-chain.
            let s = rt.block_on(get(&signer.rpc, &format!("/seance/{seance}")))?;
            let sealed = s["jurors"].as_array().and_then(|js| js.iter().find(|j| j["juror"]["address"] == account.as_str())).and_then(|j| j["commitment"].as_str().map(String::from)).ok_or_else(|| anyhow!("{account} hasn't sealed a vote for Séance #{seance}"))?;
            let v = [path.clone(), path.with_extension("pending")]
                .iter()
                .filter_map(|p| std::fs::read(p).ok().and_then(|b| serde_json::from_slice::<SealedVote>(&b).ok()))
                .find(|v| seance_commitment(seance, &account, &v.votes, &v.spam, &v.salt) == sealed)
                .ok_or_else(|| anyhow!("no saved vote in {} matches your sealed one (vote from this machine first)", path.parent().unwrap().display()))?;
            submit(dir, &signer, Action::RevealVotes { seance_id: seance, votes: v.votes, spam: v.spam, salt: v.salt }).map(|_| ())
        }
        ContribCmd::Withdraw { signer } => submit(dir, &signer, Action::WithdrawVested {}).map(|_| ()),
        ContribCmd::Fund { amount, signer } => submit(dir, &signer, Action::FundPool { amount: parse_fang(&amount)? }).map(|_| ()),
        ContribCmd::Admin { cmd } => match cmd {
            AdminCmd::SetRepo { repo, weight, signer } => submit(dir, &signer, Action::SetRepo { repo, weight }).map(|_| ()),
            AdminCmd::GrantSoul { account, soul, reason, signer } => {
                let to = crate::cli_net::account_ref(&account)?;
                submit(dir, &signer, Action::GrantSoul { to, amount: parse_soul(&soul)?, reason }).map(|_| ())
            }
            AdminCmd::Reject { claim, reason, signer } => submit(dir, &signer, Action::RejectClaim { claim_id: claim, reason }).map(|_| ()),
            AdminCmd::FlagSybil { account, reason, signer } => {
                let who = crate::cli_net::account_ref(&account)?;
                submit(dir, &signer, Action::FlagSybil { account: who, reason }).map(|_| ())
            }
            AdminCmd::Clawback { account, reason, signer } => {
                let who = crate::cli_net::account_ref(&account)?;
                submit(dir, &signer, Action::Clawback { account: who, reason }).map(|_| ())
            }
        },
    }
}

// ------------------------------------------------------------------------------------- names

#[derive(Subcommand)]
pub enum NameCmd {
    /// Who owns a name and until when, or what it costs
    Show {
        name: String,
        #[command(flatten)]
        rpc: Rpc,
    },
    /// Extend a name you own (1–10 years, paid at the renewal price per year, burned)
    Renew {
        name: String,
        #[arg(long, default_value = "1")]
        years: u8,
        #[command(flatten)]
        signer: Signer,
    },
    /// Offer a name you own to another account (.boo name or address): it stays yours until they
    /// accept it (`blockula name accept`). Offering it again replaces the offer
    Transfer {
        name: String,
        to: String,
        /// Don't ask for confirmation
        #[arg(long)]
        yes: bool,
        #[command(flatten)]
        signer: Signer,
    },
    /// Accept a name offered to you: it becomes yours
    Accept {
        name: String,
        #[command(flatten)]
        signer: Signer,
    },
    /// Withdraw your offer of a name you own
    Withdraw {
        name: String,
        #[command(flatten)]
        signer: Signer,
    },
    /// Give up a name you own: anyone can register it afterwards
    Release {
        name: String,
        /// Don't ask for confirmation
        #[arg(long)]
        yes: bool,
        #[command(flatten)]
        signer: Signer,
    },
}

pub fn name(cmd: NameCmd, dir: &Path) -> Result<()> {
    match cmd {
        NameCmd::Show { name, rpc } => {
            let v = rt()?.block_on(get(&rpc.rpc, &format!("/name/{name}")))?;
            let n = v["name"].as_str().unwrap_or_default();
            if v["available"] == true {
                println!("{n} is available for {}", v["price_display"].as_str().unwrap_or_default());
            } else {
                println!("{n} belongs to {} (registered at block {})", v["owner"]["address"].as_str().unwrap_or_default(), v["registered_at"]);
                // Expiry fields come with name renewals (DEC-11); older nodes don't report them.
                if let Some(e) = v["expires_at"].as_u64() {
                    println!("   expires at block {e}{}", v["grace_until"].as_u64().map(|g| format!(", renewable by its owner until block {g}")).unwrap_or_default());
                }
                if let Some(to) = v["offered_to"]["address"].as_str() {
                    println!("   offered to {to}: theirs once they accept it (blockula name accept {n})");
                }
            }
            Ok(())
        }
        NameCmd::Renew { name, years, signer } => {
            if !(1..=MAX_RENEW_YEARS).contains(&years) {
                bail!("renew for 1–{MAX_RENEW_YEARS} years");
            }
            submit(dir, &signer, Action::RenewName { name: normalize_name(&name)?, years }).map(|_| ())
        }
        NameCmd::Transfer { name, to, yes, signer } => {
            let n = normalize_name(&name)?;
            let to = crate::cli_net::account_ref(&to)?;
            confirm(&format!("Offer {n}{NAME_SUFFIX} to {to}? Once they accept it, only they can give it back."), yes)?;
            submit(dir, &signer, Action::TransferName { name: n, to }).map(|_| ())
        }
        // Both are a `TransferName` to the signer's own account: the chain tells them apart by
        // whether it owns the name.
        NameCmd::Accept { name, signer } | NameCmd::Withdraw { name, signer } => {
            let me = rt()?.block_on(signer.from(&signer.keys(dir)?))?;
            submit(dir, &signer, Action::TransferName { name: normalize_name(&name)?, to: me }).map(|_| ())
        }
        NameCmd::Release { name, yes, signer } => {
            let n = normalize_name(&name)?;
            confirm(&format!("Release {n}{NAME_SUFFIX}? Anyone can register it afterwards."), yes)?;
            submit(dir, &signer, Action::ReleaseName { name: n }).map(|_| ())
        }
    }
}

// ------------------------------------------------------------------------------------- stake

#[derive(Subcommand)]
pub enum StakeCmd {
    /// An account's stake: pools, shares, value and unbonding entries
    Show {
        /// Name or address (default: the wallet's account)
        #[arg(long)]
        account: Option<String>,
        #[command(flatten)]
        signer: Signer,
    },
    /// Delegate FANG to a validator's pool (it earns issuance and is slashed with the pool)
    Delegate {
        /// The validator's address
        validator: String,
        /// FANG, e.g. 100
        amount: String,
        #[command(flatten)]
        signer: Signer,
    },
    /// Start unbonding stake (withdraw it after the unbonding period): an amount, pool shares, or all of it
    Undelegate {
        validator: String,
        #[command(flatten)]
        shares: Unbonding,
        #[command(flatten)]
        signer: Signer,
    },
    /// Move pool shares to another validator without unbonding
    Redelegate {
        /// The validator the shares leave (its own name: `from` is the signer's `--from`)
        #[arg(value_name = "FROM")]
        from_validator: String,
        /// The validator they move to
        #[arg(value_name = "TO")]
        to_validator: String,
        #[command(flatten)]
        shares: Shares,
        #[command(flatten)]
        signer: Signer,
    },
    /// Pay out unbonding entries that have finished
    Withdraw {
        #[command(flatten)]
        signer: Signer,
    },
}

#[derive(Args, Clone)]
#[group(required = true, multiple = false)]
pub struct Shares {
    /// How many pool shares (see `blockula stake show`)
    #[arg(long)]
    shares: Option<u64>,
    /// Every share you hold in that pool
    #[arg(long)]
    all: bool,
}

/// What `stake undelegate` unbonds.
#[derive(Args, Clone)]
#[group(required = true, multiple = false)]
pub struct Unbonding {
    /// FANG, e.g. 10: the chain sells the shares worth that much at the pool's price when the tx
    /// runs, so no node's report of the price decides how much unbonds
    #[arg(long)]
    amount: Option<String>,
    /// How many pool shares (see `blockula stake show`)
    #[arg(long)]
    shares: Option<u64>,
    /// Every share you hold in that pool
    #[arg(long)]
    all: bool,
}

/// The delegations in a `/api/staking/{account}` response, as (validator address, shares). Tolerant
/// of the field names the endpoint has used: a validator label or address, shares as a string or
/// number.
/// `(validator, shares)` of each delegation in a `/api/staking/{account}` answer.
fn delegations(v: &Value) -> Vec<(String, u64)> {
    v["delegations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| Some((d["validator"]["address"].as_str()?.to_string(), d["shares"].as_str()?.parse().ok()?)))
        .collect()
}

/// `--shares N`, or with `--all` every share `account` holds with `validator`.
async fn shares_of(rpc: &str, account: &str, validator: &str, s: &Shares) -> Result<u64> {
    if let Some(n) = s.shares {
        return Ok(n);
    }
    let v = get(rpc, &format!("/staking/{account}")).await.with_context(|| format!("{rpc} doesn't serve /api/staking (is staking active on this chain?)"))?;
    delegations(&v).into_iter().find(|(a, _)| a == validator).map(|(_, n)| n).filter(|n| *n > 0).ok_or_else(|| anyhow!("{account} has no stake with {validator}"))
}

pub fn stake(cmd: StakeCmd, dir: &Path) -> Result<()> {
    match cmd {
        StakeCmd::Show { account, signer } => {
            let rt = rt()?;
            let who = match account.or(signer.from.clone()) {
                Some(a) => rt.block_on(resolve(&signer.rpc, &a))?,
                None => rt.block_on(own_address(&signer.rpc, &signer.keys(dir).map_err(|_| anyhow!("no wallet here; pass --account"))?))?,
            };
            let v = rt.block_on(get(&signer.rpc, &format!("/staking/{who}"))).with_context(|| format!("{} doesn't serve /api/staking (is staking active on this chain?)", signer.rpc))?;
            println!("{}", v["summary"].as_str().unwrap_or_default());
            for d in v["delegations"].as_array().into_iter().flatten() {
                println!("   {}  {} shares  worth {}{}", name_of(&d["validator"]), d["shares"].as_str().unwrap_or_default(), d["value_display"].as_str().unwrap_or_default(), if d["self_bond"] == true { "  (self-bond)" } else { "" });
            }
            for e in v["entries"].as_array().into_iter().flatten() {
                match e["type"].as_str() {
                    Some("unbonding") => println!("   unbonding {} from {}: {}", e["amount_display"].as_str().unwrap_or_default(), name_of(&e["validator"]), if e["ready"] == true { "ready to withdraw".to_string() } else { format!("free at block {}", e["release_at"]) }),
                    _ => println!("   redelegated {} from {} to {} (slashable by the first until block {})", e["amount_display"].as_str().unwrap_or_default(), name_of(&e["from"]), name_of(&e["to"]), e["release_at"]),
                }
            }
            Ok(())
        }
        StakeCmd::Delegate { validator, amount, signer } => submit(dir, &signer, Action::Delegate { validator, amount: parse_fang(&amount)? }).map(|_| ()),
        StakeCmd::Undelegate { validator, shares: Unbonding { amount: Some(amount), .. }, signer } => {
            let amount = parse_fang(&amount)?;
            if amount == 0 {
                bail!("undelegate more than 0 FANG");
            }
            submit(dir, &signer, Action::Undelegate { validator, shares: 0, amount }).map(|_| ())
        }
        StakeCmd::Undelegate { validator, shares: Unbonding { shares, all, .. }, signer } => {
            let keys = signer.keys(dir)?;
            let rt = rt()?;
            let me = rt.block_on(signer.from(&keys))?;
            let shares = rt.block_on(shares_of(&signer.rpc, &me, &validator, &Shares { shares, all }))?;
            submit(dir, &signer, Action::Undelegate { validator, shares, amount: 0 }).map(|_| ())
        }
        StakeCmd::Redelegate { from_validator, to_validator, shares, signer } => {
            let keys = signer.keys(dir)?;
            let rt = rt()?;
            let me = rt.block_on(signer.from(&keys))?;
            let shares = rt.block_on(shares_of(&signer.rpc, &me, &from_validator, &shares))?;
            submit(dir, &signer, Action::Redelegate { from_validator, to_validator, shares }).map(|_| ())
        }
        StakeCmd::Withdraw { signer } => submit(dir, &signer, Action::WithdrawUnbonded {}).map(|_| ()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `/api/staking` is read tolerantly: labels or bare addresses, shares as strings or numbers.
    #[test]
    fn staking_responses_parse() {
        // As node/src/api.rs serves it (`api::http_tests::validators_and_staking` checks the node's side).
        let v = serde_json::json!({ "bonded": "47", "delegations": [{ "validator": { "address": "boo1v", "name": "v.boo" }, "shares": "40", "value": "40", "self_bond": false }, { "validator": { "address": "boo1w", "name": null }, "shares": "7", "value": "7", "self_bond": true }], "entries": [] });
        assert_eq!(delegations(&v), vec![("boo1v".to_string(), 40), ("boo1w".to_string(), 7)]);
        assert!(delegations(&serde_json::json!({})).is_empty());
    }

    /// Audit gap-7-1: `--from` by name is the node's word; on mainnet it's refused, by address it's fine.
    #[test]
    fn from_by_name_needs_the_address_on_mainnet() {
        let a = crate::crypto::Keys::from_seed([5; 32]).address();
        assert_eq!(from_on_chain("alice.boo", &a, "blockula-testnet-2", "http://n").unwrap(), a);
        assert_eq!(from_on_chain(&a, &a, crate::chain::MAINNET_CHAIN_ID, "http://n").unwrap(), a);
        let err = from_on_chain("alice.boo", &a, crate::chain::MAINNET_CHAIN_ID, "http://n").unwrap_err().to_string();
        assert!(err.contains(&format!("pass --from {a}")) && err.contains("Nothing was signed"), "{err}");
    }

    /// `stake undelegate` takes exactly one of `--amount` (signed as an amount, converted by the
    /// chain: audit gap-7-5), `--shares` or `--all`.
    #[test]
    fn undelegate_takes_an_amount_shares_or_all() {
        use clap::Parser;
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            cmd: StakeCmd,
        }
        let parse = |args: &[&str]| Cli::try_parse_from(["blockula"].iter().chain(args)).map(|c| c.cmd);
        let Ok(StakeCmd::Undelegate { validator, shares: Unbonding { amount, shares, all }, .. }) = parse(&["undelegate", "boo1v", "--amount", "2.5"]) else { panic!("--amount parses") };
        assert_eq!((validator.as_str(), amount.as_deref(), shares, all), ("boo1v", Some("2.5"), None, false));
        assert!(matches!(parse(&["undelegate", "boo1v", "--shares", "7"]), Ok(StakeCmd::Undelegate { shares: Unbonding { shares: Some(7), amount: None, all: false }, .. })));
        assert!(matches!(parse(&["undelegate", "boo1v", "--all"]), Ok(StakeCmd::Undelegate { shares: Unbonding { all: true, amount: None, shares: None }, .. })));
        assert!(parse(&["undelegate", "boo1v"]).is_err(), "one of them is required");
        assert!(parse(&["undelegate", "boo1v", "--amount", "1", "--all"]).is_err(), "only one");
        assert!(parse(&["redelegate", "boo1v", "boo1w", "--amount", "1"]).is_err(), "redelegate moves shares");
        // The validators are positionals of their own: the source used to share the id `from` with
        // the signer's `--from` (a debug-build panic; in release the signer read the validator as
        // the account to act for).
        let Ok(StakeCmd::Redelegate { from_validator, to_validator, signer, .. }) = parse(&["redelegate", "boo1v", "boo1w", "--all"]) else { panic!("redelegate parses") };
        assert_eq!((from_validator.as_str(), to_validator.as_str(), signer.from), ("boo1v", "boo1w", None));
        let Ok(StakeCmd::Redelegate { from_validator, signer, .. }) = parse(&["redelegate", "boo1v", "boo1w", "--all", "--from", "vlad.boo"]) else { panic!("with --from") };
        assert_eq!((from_validator.as_str(), signer.from.as_deref()), ("boo1v", Some("vlad.boo")));
    }

    #[test]
    fn soul_amounts_parse() {
        assert_eq!(parse_soul("10").unwrap(), 10_000);
        assert_eq!(parse_soul("2.5").unwrap(), 2_500);
        assert_eq!(parse_soul("0.001").unwrap(), 1);
        assert!(parse_soul("1.0001").is_err());
        assert!(parse_soul("-1").is_err());
    }
}
