//! `blockula hunt …` (bounties on GitHub issues) and `blockula contrib …` (Proof of Contribution:
//! claims, vouches, Séance jury duty, rewards, and the Season 0 admin levers). Talks to any
//! node's HTTP API and signs with the CLI wallet or a key file.

use crate::crypto::{self, Keys};
use crate::types::*;
use crate::validator_cli::{get, load_seed, own_address, parse_fang, rt, send_memo, wallet_keys};
use anyhow::{anyhow, bail, Context, Result};
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
}

async fn resolve(rpc: &str, who: &str) -> Result<String> {
    if crypto::is_address(who) {
        return Ok(who.to_string());
    }
    get(rpc, &format!("/account/{who}")).await?["account"]["address"].as_str().map(String::from).ok_or_else(|| anyhow!("can't resolve {who}"))
}

/// Signs and submits `action`, waits for it, and prints the summary and explorer link.
fn submit(dir: &Path, s: &Signer, action: Action) -> Result<Value> {
    let keys = s.keys(dir)?;
    let rt = rt()?;
    let tx = rt.block_on(async {
        let from = s.from(&keys).await?;
        send_memo(&s.rpc, &keys, &from, action, String::new()).await
    })?;
    println!("✓ {}", tx["summary"].as_str().unwrap_or_default());
    println!("  {}/#/tx/{}", s.rpc.trim_end_matches('/'), tx["id"].as_str().unwrap_or_default());
    Ok(tx)
}

fn name_of(v: &Value) -> String {
    v["name"].as_str().or(v["short"].as_str()).unwrap_or_default().to_string()
}

fn confirm(question: &str, yes: bool) -> Result<()> {
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
            let status = rt()?.block_on(get(&signer.rpc, "/status"))?;
            let block_ms = status["block_time_ms"].as_u64().unwrap_or(1000).max(1);
            let expires_at = status["height"].as_u64().unwrap_or(0) + days * 86_400_000 / block_ms;
            submit(dir, &signer, Action::CreateHunt { repo, issue, amount: parse_fang(&amount)?, expires_at, title })?;
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
        (Some(t), _) => rt.block_on(resolve(rpc, t))?,
        (None, Some(p)) => {
            let link = rt.block_on(get(rpc, &format!("/github/{}", p.author))).map_err(|_| anyhow!("@{} hasn't linked a Blockula account (they can run link_github); or pass --to <name>", p.author))?;
            if link["verified"] != true {
                bail!("@{}'s link isn't verified: {}. Pass --to to award anyway.", p.author, link["reason"].as_str().unwrap_or("unknown"));
            }
            link["account"]["address"].as_str().map(String::from).ok_or_else(|| anyhow!("bad /api/github response"))?
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
    confirm(&format!("Award {} (hunt #{id}, {repo}#{issue}) to {} for PR #{pr}{why}?", h["amount_display"].as_str().unwrap_or_default(), name_of(&acct["account"])), yes)?;
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
    /// in ~/.blockula/seances/ for `reveal`.
    Vote {
        seance: u64,
        #[arg(long)]
        votes: String,
        /// Claim ids you think are spam (comma-separated)
        #[arg(long, value_delimiter = ',')]
        spam: Vec<u64>,
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
            let to = rt()?.block_on(resolve(&signer.rpc, &account))?;
            submit(dir, &signer, Action::Vouch { account: to }).map(|_| ())
        }
        ContribCmd::Unvouch { account, signer } => {
            let to = rt()?.block_on(resolve(&signer.rpc, &account))?;
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
                    let url = |id: &Value| claims.iter().find(|c| c["id"] == *id).map(|c| c["pr_url"].as_str().unwrap_or_default().to_string()).unwrap_or_default();
                    println!("\nYou're on this jury. For each pair, which PR mattered more? a, b, or = (about equal):");
                    for (i, p) in seat["assigned_pairs"].as_array().cloned().unwrap_or_default().iter().enumerate() {
                        println!("  {:>2}. a = claim #{} {}\n      b = claim #{} {}", i + 1, p[0], url(&p[0]), p[1], url(&p[1]));
                    }
                    println!("\nThen: blockula contrib vote {id} --votes <{} chars of a/b/=> [--spam <claim ids>]", seat["pairs"]);
                    println!("Status: {}", if seat["revealed"] == true { "revealed" } else if seat["committed"] == true { "sealed (reveal after the sealing phase)" } else { "not voted yet" });
                }
                None => println!("\n{} jurors: {}", s["jurors"].as_array().map(|a| a.len()).unwrap_or(0), s["jurors"].as_array().cloned().unwrap_or_default().iter().map(|j| format!("{}{}", name_of(&j["juror"]), if j["revealed"] == true { " ✓" } else if j["committed"] == true { " 🔒" } else { "" })).collect::<Vec<_>>().join(", ")),
            }
            Ok(())
        }
        ContribCmd::Vote { seance, votes, spam, signer } => {
            let keys = signer.keys(dir)?;
            let rt = rt()?;
            let account = rt.block_on(signer.from(&keys))?;
            let chain_id = rt.block_on(get(&signer.rpc, "/status"))?["chain_id"].as_str().unwrap_or_default().to_string();
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
            submit(dir, &signer, Action::CommitVotes { seance_id: seance, commitment })?;
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
                let to = rt()?.block_on(resolve(&signer.rpc, &account))?;
                submit(dir, &signer, Action::GrantSoul { to, amount: parse_soul(&soul)?, reason }).map(|_| ())
            }
            AdminCmd::Reject { claim, reason, signer } => submit(dir, &signer, Action::RejectClaim { claim_id: claim, reason }).map(|_| ()),
            AdminCmd::FlagSybil { account, reason, signer } => {
                let who = rt()?.block_on(resolve(&signer.rpc, &account))?;
                submit(dir, &signer, Action::FlagSybil { account: who, reason }).map(|_| ())
            }
            AdminCmd::Clawback { account, reason, signer } => {
                let who = rt()?.block_on(resolve(&signer.rpc, &account))?;
                submit(dir, &signer, Action::Clawback { account: who, reason }).map(|_| ())
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soul_amounts_parse() {
        assert_eq!(parse_soul("10").unwrap(), 10_000);
        assert_eq!(parse_soul("2.5").unwrap(), 2_500);
        assert_eq!(parse_soul("0.001").unwrap(), 1);
        assert!(parse_soul("1.0001").is_err());
        assert!(parse_soul("-1").is_err());
    }
}
