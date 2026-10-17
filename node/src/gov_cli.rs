//! `blockula gov …` (proposals, conviction votes, vote locks, community distributions) and
//! `blockula multisig …` (M-of-N accounts). Talks to any node's HTTP API and signs with the CLI
//! wallet or a key file, like `blockula hunt` and `blockula contrib`.

use crate::contrib_cli::{confirm, Rpc, Signer};
use crate::crypto::{self, Keys};
use crate::governance::build_distribution;
use crate::types::*;
use crate::validator_cli::{get, load_seed, own_address, parse_fang, rt, wallet_keys};
use anyhow::{anyhow, bail, ensure, Context, Result};
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn keys_of(s: &Signer, dir: &Path) -> Result<Keys> {
    match &s.key {
        Some(p) => load_seed(p),
        None => wallet_keys(dir),
    }
}

/// An address the node resolves a name to, unproven (printed): only where an address is needed.
async fn resolve(rpc: &str, who: &str) -> Result<String> {
    crate::cli_net::resolve_unproven(rpc, who).await
}

/// The account the signer acts for: `--from`, or the key's own address.
async fn acting_for(s: &Signer, keys: &Keys) -> Result<String> {
    match &s.from {
        Some(a) => resolve(&s.rpc, a).await,
        None => own_address(&s.rpc, keys).await,
    }
}

/// Signs and submits `action` for the chain `s` pins, waits for it, and prints the summary and
/// explorer link.
fn submit(dir: &Path, s: &Signer, action: Action) -> Result<Value> {
    submit_at(dir, s, |_, _| Ok(action))
}

/// [`submit`] for an action that carries an absolute height: `make(height, blocks_per_day)`
/// builds it from the status read that sets `valid_until`, with the day length from the pinned
/// genesis, never the node's word (audit gap-7-3).
fn submit_at(dir: &Path, s: &Signer, make: impl FnOnce(u64, u64) -> Result<Action>) -> Result<Value> {
    let keys = keys_of(s, dir)?;
    let tx = rt()?.block_on(async {
        let remote = s.connect(dir).await?;
        let from = s.acting_for(&remote, &keys).await?;
        let day = remote.blocks_per_day().await?;
        crate::validator_cli::send_memo_with(&remote, &keys, &from, |height| make(height, day), String::new()).await
    })?;
    println!("✓ {}", tx["summary"].as_str().unwrap_or_default());
    println!("  {}/#/tx/{}", s.rpc.trim_end_matches('/'), tx["id"].as_str().unwrap_or_default());
    Ok(tx)
}

fn text(v: &Value) -> &str {
    v.as_str().unwrap_or_default()
}

fn name_of(v: &Value) -> String {
    v["name"].as_str().or(v["short"].as_str()).unwrap_or_default().to_string()
}

// ------------------------------------------------------- what an approval or vote binds, in words
//
// Audit gap-7-6: a vote binds `proposal_digest(kind, title, url)`, an approval `action_digest`, a
// sealed judgment `seance_pairs_digest`. The node's own `summary` text is covered by none of them,
// so the CLI describes what it decoded itself (and prints the digest), and treats the summary as
// the node's note at most. The SDK's `describeKind` / `describeAction` say the same things.

/// Drops as FANG with every digit, in groups of three: "10,000,000 FANG", "0.5 FANG".
pub(crate) fn fang_text(drops: u64) -> String {
    let whole = (drops / DROPS_PER_FANG).to_string();
    let mut g = String::new();
    for (i, c) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i).is_multiple_of(3) {
            g.push(',');
        }
        g.push(c);
    }
    match drops % DROPS_PER_FANG {
        0 => format!("{g} FANG"),
        f => format!("{g}.{} FANG", format!("{f:09}").trim_end_matches('0')),
    }
}

/// An account field: an address in full (check it), or a `.boo` name the chain resolves when it runs.
fn account(v: &str) -> String {
    if crypto::is_address(v) {
        return v.to_string();
    }
    match normalize_name(v) {
        Ok(n) => format!("{n}{NAME_SUFFIX} (a name, not an address: the chain resolves it to whoever owns it when this runs)"),
        Err(_) => format!("{:?} (not an account)", one_line(v, 80)),
    }
}

/// A stranger's text on one line, without control or format characters, at most `max` characters.
fn one_line(v: &str, max: usize) -> String {
    let t: String = v.chars().map(|c| if c.is_control() || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}') { ' ' } else { c }).collect();
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() > max {
        format!("{}…", t.chars().take(max).collect::<String>())
    } else {
        t
    }
}

fn quoted(label: &str, v: &str) -> String {
    if v.trim().is_empty() {
        String::new()
    } else {
        // No curly quotes of its own, so the text can't close the quote early.
        format!(" ({label}, as written: “{}”)", one_line(v, 200).replace(['“', '”'], "\""))
    }
}

/// What a proposal kind does, in plain English, from the kind itself (exact: decoded as the chain
/// stores it). This, not the node's summary, is what `blockula gov` shows a voter.
pub(crate) fn describe_kind(kind: &ProposalKind) -> String {
    match kind {
        ProposalKind::ParamChange { key, value } => format!("Set the chain parameter {} to {value} (in that parameter's own units: drops, blocks, basis points…)", one_line(key, 64)),
        ProposalKind::TreasurySpend { to, amount } => format!("Pay {} from the treasury to {}", fang_text(*amount), account(to)),
        ProposalKind::Distribution { distribution_id, merkle_root, total, claim_until } => format!("Distribute {} from the community account as distribution #{distribution_id}, to the accounts in the Merkle tree with root {} (check it against the published tree), claimable until block #{claim_until}", fang_text(*total), one_line(merkle_root, 64)),
        ProposalKind::SetRepo { repo, weight: 0 } => format!("Remove the repo {} from the contribution pool", one_line(repo, 140)),
        ProposalKind::SetRepo { repo, weight } => format!("Put the repo {} in the contribution pool with weight {weight}", one_line(repo, 140)),
        ProposalKind::GrantSoul { to, amount, reason } => format!("Grant {} Soul to {}{}", crate::poc::fmt_soul(*amount), account(to), quoted("reason", reason)),
        ProposalKind::FlagSybil { account: a, reason } => format!("Flag {} as a sybil (its voucher is slashed, its unvested rewards clawed back){}", account(a), quoted("reason", reason)),
        ProposalKind::UnflagSybil { account: a, reason } => format!("Lift the sybil flag on {} (nothing the flag took comes back){}", account(a), quoted("reason", reason)),
        ProposalKind::RejectClaim { claim_id, reason } => format!("Reject contribution claim #{claim_id}{}", quoted("reason", reason)),
        ProposalKind::Clawback { account: a, reason } => format!("Claw back the unvested contribution rewards of {}{}", account(a), quoted("reason", reason)),
        ProposalKind::UnlinkGithub { handle } => format!("Unlink the GitHub handle @{} and hold it for the proposer to link for one voting period", one_line(handle, 39)),
        ProposalKind::RefundHunt { hunt_id } => format!("Refund every funder of hunt #{hunt_id} what they put in"),
        ProposalKind::ScheduleUpgrade { version, activation_height } => format!("Switch the chain to protocol version {version} at block #{activation_height} (nodes that can't run it stop there)"),
        ProposalKind::CancelUpgrade { version, activation_height } => format!("Cancel the scheduled upgrade to protocol version {version} at block #{activation_height}, and any upgrade scheduled after it"),
        ProposalKind::ActivateFeature { feature } => format!("Turn on the feature {}", one_line(feature, 64)),
        ProposalKind::DeactivateFeature { feature } => format!("Turn off the feature {}", one_line(feature, 64)),
        ProposalKind::Text {} => "Nothing on chain: a text proposal (a signal; read its title and link)".into(),
    }
}

/// What an action does (a multisig's pending action), in plain English, from the decoded action.
/// The CLI prints the action's exact JSON under it.
pub(crate) fn describe_action(a: &Action) -> String {
    let key = |k: &str| format!("{}…{}", &k[..k.len().min(12)], &k[k.len().saturating_sub(8)..]);
    match a {
        Action::Transfer { to, amount } => format!("Send {} to {}", fang_text(*amount), account(to)),
        Action::RegisterName { name } => format!("Register the name {}", one_line(name, 40)),
        Action::AddKey { key_id, label, role: KeyRole::Owner, .. } => format!("Add key {} to the account as an owner key (full control of the account){}", key(key_id), quoted("label", label)),
        Action::AddKey { key_id, label, role: KeyRole::Session { limit, period_blocks, expires_at }, .. } => format!("Add key {} to the account as a session key that may send up to {} (fees included) every {period_blocks} blocks, until block #{expires_at}, and nothing else{}", key(key_id), fang_text(*limit), quoted("label", label)),
        Action::RemoveKey { key_id } => format!("Remove key {} from the account", key(key_id)),
        Action::RegisterValidator { bond, .. } => format!("Register a validator, bonding {} from the account", fang_text(*bond)),
        Action::CreateHunt { repo, issue, amount, expires_at, title } => format!("Escrow {} as a bounty on {}#{issue}, until block #{expires_at}{}", fang_text(*amount), one_line(repo, 140), quoted("title", title)),
        Action::FundHunt { hunt_id, amount } => format!("Add {} to hunt #{hunt_id}", fang_text(*amount)),
        Action::AwardHunt { hunt_id, to, pr, .. } => format!("Pay hunt #{hunt_id}'s bounty to {} for PR #{pr}", account(to)),
        Action::GrantSoul { to, amount, reason } => format!("Grant {} Soul to {} (admin chains only){}", crate::poc::fmt_soul(*amount), account(to), quoted("reason", reason)),
        Action::Vouch { account: a } => format!("Stake 10 FANG vouching for {}", account(a)),
        Action::Unvouch { account: a } => format!("Withdraw the vouch for {}", account(a)),
        Action::JoinJury { bond } => format!("Join the Séance juror pool, bonding {}", fang_text(*bond)),
        Action::FundPool { amount } => format!("Add {} to the contribution pool", fang_text(*amount)),
        Action::Delegate { validator, amount } => format!("Stake {} with validator {}", fang_text(*amount), account(validator)),
        Action::Undelegate { validator, amount, .. } if *amount > 0 => format!("Start unbonding {} of stake from validator {} (the chain converts it to pool shares at its own price when this runs)", fang_text(*amount), account(validator)),
        Action::Undelegate { validator, shares, .. } => format!("Start unbonding {shares} pool shares from validator {}", account(validator)),
        Action::Redelegate { from_validator, to_validator, shares } => format!("Move {shares} pool shares from validator {} to validator {}", account(from_validator), account(to_validator)),
        Action::WithdrawUnbonded {} => "Withdraw the stake that has finished unbonding".into(),
        Action::UpdateValidator { commission_bps, p2p_addr, .. } => format!("Set the account's validator commission to {commission_bps} basis points{}", if p2p_addr.is_empty() { "" } else { " and change its addresses" }),
        Action::Propose { kind, deposit, title, .. } => format!("Propose, bonding {}: {}{}", fang_text(*deposit), describe_kind(kind), quoted("title", title)),
        Action::Vote { proposal_id, aye, amount, conviction, .. } => format!("Vote {} on proposal #{proposal_id} with {} at conviction {conviction}", if *aye { "aye" } else { "nay" }, fang_text(*amount)),
        Action::Unlock {} => "Release the account's expired vote locks".into(),
        Action::MultisigCreate { owners, threshold } => format!("Make the account a {threshold}-of-{} multisig of the owner keys {}", owners.len(), owners.iter().map(|k| key(k)).collect::<Vec<_>>().join(", ")),
        Action::RenewName { name, years } => format!("Renew the name {} for {years} year(s)", one_line(name, 40)),
        Action::TransferName { name, to } => format!("Offer the name {} to {} (theirs once they accept it; offered to the account itself, it accepts or withdraws an offer)", one_line(name, 40), account(to)),
        Action::ReleaseName { name } => format!("Release the name {}: anyone can register it afterwards", one_line(name, 40)),
        Action::ClaimDistribution { distribution_id, amount, .. } => format!("Claim {} from community distribution #{distribution_id}", fang_text(*amount)),
        Action::Shield { amount, .. } => format!("Move {} into the Ghost Mode pool", fang_text(*amount)),
        Action::Unshield { to, amount, .. } => format!("Pay {} out of the Ghost Mode pool to {}", fang_text(*amount), account(to)),
        Action::ValidatorReady { epoch } => format!("Signal that the account's candidate validator node is running, to join a set of one or two at the end of epoch {epoch} (validators)"),
        Action::AdmitValidator { validator, epoch } => format!("Admit candidate validator {} into a set of one or two at the end of epoch {epoch} (validators)", account(validator)),
        other => {
            let t = serde_json::to_value(other).ok().and_then(|v| v["type"].as_str().map(String::from)).unwrap_or_default();
            format!("A `{t}` action: read its exact fields below")
        }
    }
}

/// With `--digest`, checks the content read now against what the user was shown (its digest):
/// a mismatch refuses before anything is signed. `Ok(true)`: it matched; `Ok(false)`: no digest
/// was given (the caller then shows the content and asks).
pub(crate) fn check_digest(shown: Option<&str>, now: &str, what: &str) -> Result<bool> {
    let Some(shown) = shown else { return Ok(false) };
    let shown = shown.trim().to_ascii_lowercase();
    ensure!(shown.len() == 64 && shown.bytes().all(|b| b.is_ascii_hexdigit()), "--digest must be the full 64-hex digest that was printed with {what}");
    ensure!(shown == now, "{what} doesn't read the way it did when you were shown it: its digest is now {now}, not {shown}. Nothing was signed. The node may be showing different content to different reads: look at it again, ideally through another node, before signing.");
    Ok(true)
}

// ------------------------------------------------------------------------------- governance

#[derive(Subcommand)]
pub enum GovCmd {
    /// List proposals, newest first, with the rules they're judged by
    List {
        /// voting, queued, rejected, executed or failed
        #[arg(long)]
        status: Option<String>,
        #[command(flatten)]
        rpc: Rpc,
    },
    /// Show a proposal: what it does, its tally, its votes
    Show {
        id: u64,
        #[command(flatten)]
        rpc: Rpc,
    },
    /// Put something to a vote (bonds the deposit: refunded if the vote reaches quorum, burned if not)
    Propose {
        #[command(subcommand)]
        kind: KindCmd,
    },
    /// Vote with FANG you hold (liquid, staked or unbonding; not unvested). Conviction 0 counts
    /// 0.1× and locks only while voting lasts; 1–6 count 1×–6× and lock 1, 2, 4, 8, 16 or 32
    /// lock periods after it ends. Voting again replaces your vote.
    Vote {
        id: u64,
        /// aye or nay
        side: String,
        /// FANG to vote (and lock) with
        amount: String,
        #[arg(long, default_value = "1")]
        conviction: u8,
        /// The proposal's digest as `gov show` / `gov list` printed it: the vote binds exactly
        /// that content, and is refused if the proposal reads differently now (audit gap-7-6)
        #[arg(long)]
        digest: Option<String>,
        /// Without --digest, don't ask for confirmation after printing what the vote binds
        #[arg(long)]
        yes: bool,
        #[command(flatten)]
        signer: Signer,
    },
    /// Release vote locks that have expired
    Unlock {
        #[command(flatten)]
        signer: Signer,
    },
    /// Build a distribution's Merkle tree from `[{"address": "boo1…", "amount": "1.5"}, …]`
    /// (amounts in FANG). The tree commits to the distribution's id, one no distribution has
    /// yet, which its proposal names; prints (or writes) the root and every recipient's proof.
    Tree {
        entries: PathBuf,
        /// Distribution id (default: the chain's next free one, `next_distribution_id`)
        #[arg(long)]
        id: Option<u64>,
        /// Write the tree here instead of printing it
        #[arg(long)]
        out: Option<PathBuf>,
        #[command(flatten)]
        rpc: Rpc,
    },
    /// Claim your share of a distribution, with the tree file its proposal published
    Claim {
        tree: PathBuf,
        #[command(flatten)]
        signer: Signer,
    },
}

/// What every proposal carries.
#[derive(Args, Clone)]
pub struct Meta {
    /// One line: what and why (at most 96 bytes)
    #[arg(long)]
    pub(crate) title: String,
    /// https:// link to the full text and discussion
    #[arg(long, default_value = "")]
    pub(crate) url: String,
    /// Deposit in FANG (default: the chain's minimum as the node reports it, at most
    /// --max-deposit). All of it burns if the vote misses quorum
    #[arg(long)]
    pub(crate) deposit: Option<String>,
    /// The most a node-reported default deposit may be, in FANG (audit gap-7-5: a node can report
    /// any deposit); pass --deposit to bond an amount you checked
    #[arg(long, default_value = "1000")]
    pub(crate) max_deposit: String,
    #[command(flatten)]
    pub(crate) signer: Signer,
}

#[derive(Subcommand)]
pub enum KindCmd {
    /// A signal with no on-chain effect
    Text {
        #[command(flatten)]
        meta: Meta,
    },
    /// Set a chain parameter (see /api/proposals for the current values)
    Param {
        /// The parameter's key, e.g. max_validators
        #[arg(value_name = "KEY")]
        param: String,
        /// A whole number (blocks, bps, ppm, drops), or FANG for amounts (e.g. 1000.5)
        value: String,
        #[command(flatten)]
        meta: Meta,
    },
    /// Pay FANG from the treasury
    Spend {
        to: String,
        amount: String,
        #[command(flatten)]
        meta: Meta,
    },
    /// Distribute FANG from the community account to a tree built with `blockula gov tree`
    Distribution {
        tree: PathBuf,
        /// Claims close after this many days (of blocks) from now
        #[arg(long, default_value = "90")]
        days: u64,
        #[command(flatten)]
        meta: Meta,
    },
    /// Put a repo in the contribution pool with a weight (1–100), or take it out with 0
    SetRepo {
        repo: String,
        weight: u64,
        #[command(flatten)]
        meta: Meta,
    },
    /// Grant Soul (up to 100)
    GrantSoul {
        to: String,
        /// Soul, e.g. 10 or 2.5
        soul: String,
        #[arg(long)]
        reason: String,
        #[command(flatten)]
        meta: Meta,
    },
    /// Flag an account as a sybil
    FlagSybil {
        account: String,
        #[arg(long)]
        reason: String,
        #[command(flatten)]
        meta: Meta,
    },
    /// Lift a sybil flag (the appeal path; nothing the flag took comes back)
    UnflagSybil {
        account: String,
        #[arg(long)]
        reason: String,
        #[command(flatten)]
        meta: Meta,
    },
    /// Reject a contribution claim
    RejectClaim {
        claim: u64,
        #[arg(long)]
        reason: String,
        #[command(flatten)]
        meta: Meta,
    },
    /// Claw back an account's unvested contribution rewards
    Clawback {
        account: String,
        #[arg(long)]
        reason: String,
        #[command(flatten)]
        meta: Meta,
    },
    /// Unlink a GitHub handle (a squatter's) and hold it for you, the proposer, to link for one
    /// voting period. Propose it from the account that should get the handle.
    UnlinkGithub {
        handle: String,
        #[command(flatten)]
        meta: Meta,
    },
    /// Refund every funder of an open hunt exactly what they put in
    RefundHunt {
        hunt: u64,
        #[command(flatten)]
        meta: Meta,
    },
    /// Schedule a protocol upgrade
    Upgrade {
        version: u32,
        activation_height: u64,
        #[command(flatten)]
        meta: Meta,
    },
    /// Withdraw a scheduled protocol upgrade (and any scheduled after it) before it activates.
    /// Needs the supermajority and runs right after its vote (no timelock), at least 64 blocks
    /// before the activation height
    CancelUpgrade {
        version: u32,
        /// The activation height it's scheduled at (default: looked up in /api/upgrade)
        #[arg(long)]
        at: Option<u64>,
        #[command(flatten)]
        meta: Meta,
    },
    /// Switch a feature on (e.g. ghost)
    Activate {
        feature: String,
        #[command(flatten)]
        meta: Meta,
    },
    /// Switch a feature off
    Deactivate {
        feature: String,
        #[command(flatten)]
        meta: Meta,
    },
}

/// The activation height of the upgrade to `version` in `/api/upgrade`'s schedule.
fn scheduled_at(upgrade: &Value, version: u32) -> Result<u64> {
    let schedule = upgrade["schedule"].as_array().into_iter().flatten();
    schedule.filter(|u| u["version"].as_u64() == Some(version as u64)).find_map(|u| u["activation_height"].as_u64()).ok_or_else(|| anyhow!("no upgrade to v{version} is scheduled (see /api/upgrade)"))
}

fn parse_soul(s: &str) -> Result<u64> {
    let (whole, frac) = s.trim().split_once('.').unwrap_or((s.trim(), ""));
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) || frac.len() > 3 || !frac.bytes().all(|b| b.is_ascii_digit()) {
        bail!("Soul is a number with up to 3 decimals, e.g. 10 or 2.5");
    }
    Ok(whole.parse::<u64>()?.saturating_mul(crate::poc::SOUL) + format!("{frac:0<3}").parse::<u64>()?)
}

/// A tree file (`blockula gov tree`).
fn read_tree(path: &Path) -> Result<Value> {
    let v: Value = serde_json::from_slice(&std::fs::read(path).with_context(|| format!("reading {}", path.display()))?).with_context(|| format!("{} isn't JSON", path.display()))?;
    if v["merkle_root"].as_str().is_none() || v["distribution_id"].as_u64().is_none() || !v["claims"].is_array() {
        bail!("{} isn't a distribution tree (see `blockula gov tree`)", path.display());
    }
    Ok(v)
}

/// A proposal's kind as the node serves it, decoded exactly, and the digest a vote on it binds.
fn bound_proposal(p: &Value) -> Result<(ProposalKind, String, String, String)> {
    let kind: ProposalKind = serde_json::from_value(p["kind"].clone()).context("the node's proposal kind doesn't parse")?;
    let (title, url) = (text(&p["title"]).to_string(), text(&p["url"]).to_string());
    let digest = proposal_digest(&kind, &title, &url);
    Ok((kind, title, url, digest))
}

/// What `gov list` (and, with `detail`, `gov show`) prints for a proposal. What it does is the
/// CLI's own description of the decoded kind, with the digest a vote binds; the node's summary
/// is shown only with `detail`, labelled as the node's text (audit gap-7-6).
pub(crate) fn proposal_lines(p: &Value, detail: bool) -> Vec<String> {
    let mut out = vec![format!("#{} [{}] {}", p["id"], text(&p["status"]), one_line(text(&p["title"]), 120))];
    match bound_proposal(p) {
        Ok((kind, _, url, digest)) => {
            out.push(format!("   does: {}", describe_kind(&kind)));
            if detail {
                out.push(format!("   kind: {}", serde_json::to_string(&kind).unwrap_or_default()));
                if !url.is_empty() {
                    out.push(format!("   link: {}", one_line(&url, 200)));
                }
            }
            out.push(format!("   digest: {digest} (blockula gov vote {} … --digest {digest})", p["id"]));
        }
        Err(e) => out.push(format!("   does: this CLI can't decode it ({e:#}): don't vote on it")),
    }
    out.push(format!("   by {} · deposit {}", name_of(&p["proposer"]), text(&p["deposit_display"])));
    let weight = |side: &str| text(&p[side]["weight"]["display"]).to_string();
    let end = match text(&p["status"]) {
        "voting" => format!("voting ends at block {}", p["voting_end"]),
        "queued" => format!("runs at block {}", p["execute_at"]),
        _ => text(&p["outcome"]).to_string(),
    };
    out.push(format!("   ayes {} · nays {} (weight) · {} voters · {end}", weight("ayes"), weight("nays"), p["voters"]));
    if detail && !text(&p["summary"]).is_empty() {
        out.push(format!("   the node's own note (its text, not bound by any vote): {}", one_line(text(&p["summary"]), 300)));
    }
    out
}

fn print_proposal(p: &Value, detail: bool) {
    for l in proposal_lines(p, detail) {
        println!("{l}");
    }
}

pub fn gov(cmd: GovCmd, dir: &Path) -> Result<()> {
    match cmd {
        GovCmd::List { status, rpc } => {
            let q = status.map(|s| format!("?status={s}")).unwrap_or_default();
            let v = rt()?.block_on(get(&rpc.rpc, &format!("/proposals{q}")))?;
            let p = &v["params"];
            println!("{} proposals · deposit {} · voting {} blocks, timelock {} · quorum {}‱ of the unlocked supply (at least {})", v["count"], text(&p["proposal_deposit_display"]), p["voting_blocks"], p["timelock_blocks"], p["quorum_bps"], text(&p["quorum_floor_display"]));
            if v["council_active"] == true {
                println!("   bootstrap council: ⅔ of the validators can pass proposals until block {}", p["bootstrap_end_height"]);
            }
            for p in v["proposals"].as_array().cloned().unwrap_or_default() {
                print_proposal(&p, false);
            }
            Ok(())
        }
        GovCmd::Show { id, rpc } => {
            let p = rt()?.block_on(get(&rpc.rpc, &format!("/proposal/{id}")))?;
            print_proposal(&p, true);
            if let Some(t) = p["tally"].as_object() {
                println!("   quorum {} of {} unlocked: {} · council {}/{} aye, {} nay", text(&t["quorum"]["display"]), text(&t["unlocked_supply"]["display"]), if t["quorum_reached"] == true { "reached" } else { "not reached" }, t["council_ayes"], t["council_size"], t["council_nays"]);
            }
            for v in p["votes"].as_array().cloned().unwrap_or_default() {
                let council = v["council"].as_object().map(|c| format!(" (council: {})", name_of(&Value::Object(c.clone())))).unwrap_or_default();
                println!("   {} {} {} at {}× → locked until block {}{council}", name_of(&v["voter"]), if v["aye"] == true { "aye" } else { "nay" }, text(&v["amount_display"]), if v["conviction"] == 0 { "0.1".to_string() } else { v["conviction"].to_string() }, v["locked_until"]);
            }
            Ok(())
        }
        GovCmd::Propose { kind } => propose(dir, kind),
        GovCmd::Vote { id, side, amount, conviction, digest, yes, signer } => {
            let aye = match side.as_str() {
                "aye" | "yes" | "y" => true,
                "nay" | "no" | "n" => false,
                _ => bail!("vote aye or nay"),
            };
            let amount = parse_fang(&amount)?;
            // The vote binds the proposal's kind, title and link, decoded from this read (never the
            // node's summary). That read is the node's word too, so it must be what the user was
            // shown: the digest `gov show` printed (--digest), or the content printed here and
            // confirmed (audit gap-7-6, review-r2-clients-3).
            let p = rt()?.block_on(get(&signer.rpc, &format!("/proposal/{id}")))?;
            let (kind, title, url, proposal_digest) = bound_proposal(&p)?;
            println!("Proposal #{id}: “{}”{}", one_line(&title, 120), if url.is_empty() { String::new() } else { format!(" ({})", one_line(&url, 200)) });
            println!("   does: {}", describe_kind(&kind));
            println!("   kind: {}", serde_json::to_string(&kind)?);
            println!("   digest: {proposal_digest}");
            let side = if aye { "aye" } else { "nay" };
            if !check_digest(digest.as_deref(), &proposal_digest, &format!("proposal #{id}"))? {
                confirm(&format!("Vote {side} on exactly this, locking {} at conviction {conviction}?", fmt_fang(amount)), yes)?;
            }
            submit(dir, &signer, Action::Vote { proposal_id: id, aye, amount, conviction, proposal_digest }).map(|_| ())
        }
        GovCmd::Unlock { signer } => submit(dir, &signer, Action::Unlock {}).map(|_| ()),
        GovCmd::Tree { entries, id, out, rpc } => {
            let raw: Value = serde_json::from_slice(&std::fs::read(&entries).with_context(|| format!("reading {}", entries.display()))?)?;
            let list: Vec<(String, u64)> = raw
                .as_array()
                .ok_or_else(|| anyhow!("expected [{{\"address\": …, \"amount\": …}}, …]"))?
                .iter()
                .map(|e| {
                    let a = e["address"].as_str().filter(|a| crypto::is_address(a)).ok_or_else(|| anyhow!("every entry needs a lowercase boo1… address: {e}"))?;
                    let amount = e["amount"].as_str().map(parse_fang).or_else(|| e["amount"].as_u64().map(|n| Ok(n * DROPS_PER_FANG))).ok_or_else(|| anyhow!("every entry needs an amount in FANG: {e}"))??;
                    Ok((a.to_string(), amount))
                })
                .collect::<Result<_>>()?;
            let id = match id {
                Some(id) => id,
                None => rt()?.block_on(get(&rpc.rpc, "/proposals?limit=0"))?["next_distribution_id"].as_u64().ok_or_else(|| anyhow!("no next_distribution_id from {}", rpc.rpc))?,
            };
            let (root, proofs) = build_distribution(id, &list)?;
            let total = list.iter().try_fold(0u64, |t, (_, a)| t.checked_add(*a)).ok_or_else(|| anyhow!("the amounts add up to more than u64 drops"))?;
            let tree = json!({
                "distribution_id": id,
                "merkle_root": hex::encode(root),
                "total": total.to_string(),
                "total_display": fmt_fang(total),
                "scheme": "BLAKE3 leaves (BLOCKULA_DISTRIBUTION_LEAF_V1, id, address, amount), sorted-pair nodes (BLOCKULA_DISTRIBUTION_NODE_V1); docs/GOVERNANCE.md",
                "claims": list.iter().zip(&proofs).map(|((a, amt), p)| json!({ "address": a, "amount": amt.to_string(), "proof": p.iter().map(hex::encode).collect::<Vec<_>>() })).collect::<Vec<_>>(),
            });
            let body = serde_json::to_string_pretty(&tree)?;
            match out {
                Some(path) => {
                    std::fs::write(&path, body)?;
                    println!("✓ distribution #{id}: {} to {} accounts, root {} → {}", fmt_fang(total), list.len(), hex::encode(root), path.display());
                }
                None => println!("{body}"),
            }
            Ok(())
        }
        GovCmd::Claim { tree, signer } => {
            let t = read_tree(&tree)?;
            let keys = keys_of(&signer, dir)?;
            let me = rt()?.block_on(acting_for(&signer, &keys))?;
            let entry = t["claims"].as_array().into_iter().flatten().find(|c| c["address"] == me.as_str()).ok_or_else(|| anyhow!("{me} isn't in this distribution"))?;
            let amount: u64 = text(&entry["amount"]).parse().context("bad amount in the tree")?;
            let proof: Vec<String> = entry["proof"].as_array().into_iter().flatten().filter_map(|h| h.as_str().map(String::from)).collect();
            submit(dir, &signer, Action::ClaimDistribution { distribution_id: t["distribution_id"].as_u64().unwrap_or_default(), amount, proof }).map(|_| ())
        }
    }
}

fn propose(dir: &Path, kind: KindCmd) -> Result<()> {
    let rt = rt()?;
    // A distribution's claim deadline is a height: it's set from the status the tx is signed
    // against (`days` days of the genesis's blocks), not a separate read (audit gap-7-3).
    let mut claim_days = None;
    let (kind, meta) = match kind {
        KindCmd::Text { meta } => (ProposalKind::Text {}, meta),
        KindCmd::Param { param, value, meta } => {
            let value = value.parse::<u64>().or_else(|_| parse_fang(&value).map_err(|_| anyhow!("value must be a whole number, or FANG for amounts (e.g. 1000.5)")))?;
            (ProposalKind::ParamChange { key: param, value }, meta)
        }
        KindCmd::Spend { to, amount, meta } => (ProposalKind::TreasurySpend { to: crate::cli_net::account_ref(&to)?, amount: parse_fang(&amount)? }, meta),
        KindCmd::Distribution { tree, days, meta } => {
            let t = read_tree(&tree)?;
            let id = t["distribution_id"].as_u64().unwrap_or_default();
            if rt.block_on(get(&meta.signer.rpc, &format!("/distribution/{id}"))).is_ok() {
                let next = rt.block_on(get(&meta.signer.rpc, "/proposals?limit=0"))?["next_distribution_id"].as_u64().unwrap_or_default();
                bail!("{} was built for distribution #{id}, which already exists: rebuild it with `blockula gov tree --id {next}`", tree.display());
            }
            let total: u64 = text(&t["total"]).parse().context("bad total in the tree")?;
            claim_days = Some(days);
            // claim_until is filled in from the signing status below (1 is a placeholder for check()).
            (ProposalKind::Distribution { distribution_id: id, merkle_root: text(&t["merkle_root"]).to_string(), total, claim_until: 1 }, meta)
        }
        KindCmd::SetRepo { repo, weight, meta } => (ProposalKind::SetRepo { repo, weight }, meta),
        KindCmd::GrantSoul { to, soul, reason, meta } => (ProposalKind::GrantSoul { to: crate::cli_net::account_ref(&to)?, amount: parse_soul(&soul)?, reason }, meta),
        KindCmd::FlagSybil { account, reason, meta } => (ProposalKind::FlagSybil { account: crate::cli_net::account_ref(&account)?, reason }, meta),
        KindCmd::UnflagSybil { account, reason, meta } => (ProposalKind::UnflagSybil { account: crate::cli_net::account_ref(&account)?, reason }, meta),
        KindCmd::RejectClaim { claim, reason, meta } => (ProposalKind::RejectClaim { claim_id: claim, reason }, meta),
        KindCmd::Clawback { account, reason, meta } => (ProposalKind::Clawback { account: crate::cli_net::account_ref(&account)?, reason }, meta),
        KindCmd::UnlinkGithub { handle, meta } => (ProposalKind::UnlinkGithub { handle }, meta),
        KindCmd::RefundHunt { hunt, meta } => (ProposalKind::RefundHunt { hunt_id: hunt }, meta),
        KindCmd::Upgrade { version, activation_height, meta } => (ProposalKind::ScheduleUpgrade { version, activation_height }, meta),
        KindCmd::CancelUpgrade { version, at, meta } => {
            let activation_height = match at {
                Some(h) => h,
                None => scheduled_at(&rt.block_on(get(&meta.signer.rpc, "/upgrade"))?, version)?,
            };
            (ProposalKind::CancelUpgrade { version, activation_height }, meta)
        }
        KindCmd::Activate { feature, meta } => (ProposalKind::ActivateFeature { feature }, meta),
        KindCmd::Deactivate { feature, meta } => (ProposalKind::DeactivateFeature { feature }, meta),
    };
    kind.check()?;
    // The default deposit is the node's word, and all of it burns if the vote misses quorum: it's
    // shown, and refused above --max-deposit (audit gap-7-5).
    let deposit = match &meta.deposit {
        Some(d) => parse_fang(d)?,
        None => {
            let d: u64 = text(&rt.block_on(get(&meta.signer.rpc, "/proposals?limit=0"))?["params"]["proposal_deposit"]).parse().context("no proposal_deposit from the node")?;
            let max = parse_fang(&meta.max_deposit)?;
            if d > max {
                bail!("{} reports a proposal deposit of {}, more than --max-deposit ({}): a node can report any deposit, and all of it burns if the vote misses quorum. Check it on another node, then pass --deposit", meta.signer.rpc, fmt_fang(d), fmt_fang(max));
            }
            println!("Bonding {} (the chain's proposal deposit, as {} reports it): refunded once the vote reaches quorum, burned if it doesn't.", fmt_fang(d), meta.signer.rpc);
            d
        }
    };
    drop(rt);
    let (title, url) = (meta.title, meta.url);
    submit_at(dir, &meta.signer, |height, day| {
        let mut kind = kind;
        if let (Some(days), ProposalKind::Distribution { claim_until, .. }) = (claim_days, &mut kind) {
            *claim_until = height.saturating_add(days.saturating_mul(day));
        }
        Ok(Action::Propose { kind, deposit, title, url })
    })
    .map(|_| ())
}

// --------------------------------------------------------------------------------- multisig

#[derive(Subcommand)]
pub enum MultisigCmd {
    /// Make the account M-of-N over its owner keys (all of them, by key id). From then on its
    /// owner keys only propose and approve; session keys keep working within their limits.
    /// A multisig changes its threshold by proposing this again.
    Create {
        /// Every owner key id on the account (comma-separated; see `blockula wallet` or /api/account)
        #[arg(long, value_delimiter = ',')]
        owners: Vec<String>,
        #[arg(long)]
        threshold: u8,
        #[command(flatten)]
        signer: Signer,
    },
    /// Propose an action for the account, as tx JSON (the API's `action` format), e.g.
    /// '{"type":"transfer","to":"bob.boo","amount":"5000000000"}'. It runs when `threshold`
    /// owner keys approved it (the proposer's key counts).
    Propose {
        action: String,
        #[command(flatten)]
        signer: Signer,
    },
    /// Approve an open proposal; the approval that reaches the threshold runs it
    Approve {
        id: u64,
        /// The action's digest as `multisig show` printed it: the approval binds exactly that
        /// action, and is refused if it reads differently now (audit gap-7-6)
        #[arg(long)]
        digest: Option<String>,
        /// Without --digest, don't ask for confirmation after printing what the approval binds
        #[arg(long)]
        yes: bool,
        #[command(flatten)]
        signer: Signer,
    },
    /// Show an account's threshold, owner keys and open proposals
    Show {
        account: String,
        #[command(flatten)]
        rpc: Rpc,
    },
}

pub fn multisig(cmd: MultisigCmd, dir: &Path) -> Result<()> {
    match cmd {
        MultisigCmd::Create { owners, threshold, signer } => submit(dir, &signer, Action::MultisigCreate { owners, threshold }).map(|_| ()),
        MultisigCmd::Propose { action, signer } => {
            let action: Action = serde_json::from_str(&action).context("the action must be tx action JSON, e.g. {\"type\":\"transfer\",\"to\":\"bob.boo\",\"amount\":\"5000000000\"}")?;
            submit(dir, &signer, Action::MultisigPropose { action: Box::new(action) }).map(|_| ())
        }
        MultisigCmd::Approve { id, digest, yes, signer } => {
            // The approval binds the stored action, decoded from this read (not the node's
            // summary of it). That read is the node's word too, so it must be what the user was
            // shown: the digest `multisig show` printed (--digest), or the action printed here and
            // confirmed (audit gap-7-6, review-r2-clients-3).
            let keys = keys_of(&signer, dir)?;
            let rt = rt()?;
            let account = rt.block_on(acting_for(&signer, &keys))?;
            let m = rt.block_on(get(&signer.rpc, &format!("/multisig/{account}")))?;
            let p = m["pending"].as_array().into_iter().flatten().find(|p| p["id"] == id).ok_or_else(|| anyhow!("{account} has no open multisig action #{id}"))?;
            let action: Action = serde_json::from_value(p["action"].clone()).context("the node's multisig action doesn't parse")?;
            let d = action_digest(&action);
            println!("Multisig action #{id} of {account}:\n   does: {}\n   action: {}\n   digest: {d}", describe_action(&action), serde_json::to_string_pretty(&action)?);
            if !check_digest(digest.as_deref(), &d, &format!("multisig action #{id}"))? {
                confirm("Approve exactly this? If it reaches the threshold, this approval runs it.", yes)?;
            }
            drop(rt);
            submit(dir, &signer, Action::MultisigApprove { id, action_digest: d }).map(|_| ())
        }
        MultisigCmd::Show { account, rpc } => {
            let m = rt()?.block_on(get(&rpc.rpc, &format!("/multisig/{account}")))?;
            for l in multisig_lines(&m, &account) {
                println!("{l}");
            }
            Ok(())
        }
    }
}

/// What `multisig show` prints: each pending action described from its decoded content, its exact
/// JSON and the digest an approval binds (`multisig approve --digest`); the node's summary only as
/// its labelled note (audit gap-7-6).
pub(crate) fn multisig_lines(m: &Value, account: &str) -> Vec<String> {
    let owners = m["owners"].as_array().cloned().unwrap_or_default();
    // `approve --from` takes the address (a name there is refused on mainnet).
    let account = m["account"]["address"].as_str().filter(|a| crypto::is_address(a)).unwrap_or(account);
    let mut out = vec![format!("{} is a {}-of-{} multisig", name_of(&m["account"]), m["threshold"], owners.len())];
    for o in &owners {
        out.push(format!("   owner {} {}", text(&o["key_id"]), one_line(text(&o["label"]), 64)));
    }
    for p in m["pending"].as_array().cloned().unwrap_or_default() {
        let expired = if p["expired"] == true { " (expired)" } else { "" };
        out.push(format!("   #{} · {}/{} approvals · expires at block {}{expired}", p["id"], p["valid_approvals"], m["threshold"], p["expires_at"]));
        match serde_json::from_value::<Action>(p["action"].clone()) {
            Ok(a) => {
                let d = action_digest(&a);
                out.push(format!("      does: {}", describe_action(&a)));
                out.push(format!("      action: {}", serde_json::to_string(&a).unwrap_or_default()));
                out.push(format!("      digest: {d} (blockula multisig approve {} --from {} --digest {d})", p["id"], one_line(account, 70)));
            }
            Err(e) => out.push(format!("      this CLI can't decode its action ({e}): don't approve it")),
        }
        if !text(&p["summary"]).is_empty() {
            out.push(format!("      the node's own note (its text, not bound by any approval): {}", one_line(text(&p["summary"]), 300)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};

    #[derive(Parser)]
    struct Gov {
        #[command(subcommand)]
        cmd: GovCmd,
    }

    #[derive(Parser)]
    struct Ms {
        #[command(subcommand)]
        cmd: MultisigCmd,
    }

    /// The commands are well-formed (clap's own checks) and parse what the docs show.
    #[test]
    fn commands_parse() {
        Gov::command().debug_assert();
        Ms::command().debug_assert();
        let gov = |args: &str| Gov::try_parse_from(std::iter::once("gov").chain(args.split(' '))).map(|g| g.cmd);
        assert!(matches!(gov("propose param max_validators 40 --title t --key k.json").unwrap(), GovCmd::Propose { kind: KindCmd::Param { param, value, meta } } if param == "max_validators" && value == "40" && meta.signer.key.is_some()));
        assert!(matches!(gov("vote 3 aye 1.5 --conviction 6").unwrap(), GovCmd::Vote { id: 3, conviction: 6, .. }));
        assert!(matches!(gov("propose spend alice.boo 10 --title t --deposit 50").unwrap(), GovCmd::Propose { kind: KindCmd::Spend { .. } }));
        assert!(gov("propose text").is_err(), "a title is required");
        assert!(matches!(gov("propose cancel-upgrade 2 --at 90000 --title t").unwrap(), GovCmd::Propose { kind: KindCmd::CancelUpgrade { version: 2, at: Some(90_000), .. } }));
        assert!(matches!(gov("propose cancel-upgrade 2 --title t").unwrap(), GovCmd::Propose { kind: KindCmd::CancelUpgrade { version: 2, at: None, .. } }));
        let upgrade = json!({ "schedule": [{ "version": 2, "activation_height": 500, "active": true }, { "version": 3, "activation_height": 9_000, "active": false }] });
        assert_eq!(scheduled_at(&upgrade, 3).unwrap(), 9_000);
        assert!(scheduled_at(&upgrade, 4).unwrap_err().to_string().contains("no upgrade to v4"));
        let ms = Ms::try_parse_from(["multisig", "create", "--owners", "aa,bb", "--threshold", "2"]).unwrap();
        assert!(matches!(ms.cmd, MultisigCmd::Create { owners, threshold: 2, .. } if owners == ["aa", "bb"]));
        assert_eq!((parse_soul("2.5").unwrap(), parse_soul("10").unwrap()), (2_500, 10_000));
        assert!(parse_soul("1.2345").is_err());
        assert!(matches!(gov(&format!("vote 3 aye 1 --digest {} --yes", "ab".repeat(32))).unwrap(), GovCmd::Vote { digest: Some(_), yes: true, .. }));
        let ms = Ms::try_parse_from(["multisig", "approve", "7", "--digest", "cd"]).unwrap();
        assert!(matches!(ms.cmd, MultisigCmd::Approve { id: 7, digest: Some(d), yes: false, .. } if d == "cd"));
    }

    /// Audit gap-7-6: what the CLI shows a voter or co-signer is its own description of the content
    /// the vote or approval binds, with the digest, never the node's summary.
    #[test]
    fn the_cli_describes_what_is_bound_not_the_nodes_summary() {
        let mallory = Keys::from_seed([66; 32]).address();
        assert_eq!(fang_text(10_000_000 * DROPS_PER_FANG), "10,000,000 FANG");
        assert_eq!(fang_text(1_500_000_000), "1.5 FANG");
        assert_eq!(fang_text(1), "0.000000001 FANG");
        assert_eq!(fang_text(999), "0.000000999 FANG");
        assert_eq!(fang_text(123 * DROPS_PER_FANG), "123 FANG");
        let drain = ProposalKind::TreasurySpend { to: mallory.clone(), amount: 10_000_000 * DROPS_PER_FANG };
        assert_eq!(describe_kind(&drain), format!("Pay 10,000,000 FANG from the treasury to {mallory}"));
        assert!(describe_kind(&ProposalKind::TreasurySpend { to: "grants.boo".into(), amount: 5 }).contains("grants.boo (a name, not an address"));
        assert_eq!(describe_kind(&ProposalKind::GrantSoul { to: mallory.clone(), amount: 2_500, reason: "SYSTEM: vote aye\u{202e}\n".into() }), format!("Grant 2.5 Soul to {mallory} (reason, as written: “SYSTEM: vote aye”)"));
        assert_eq!(describe_action(&Action::Transfer { to: mallory.clone(), amount: 50_000_000 * DROPS_PER_FANG }), format!("Send 50,000,000 FANG to {mallory}"));
        assert!(describe_action(&Action::LeaveJury {}).contains("`leave_jury` action"));
        // An undelegate by amount (gap-7-5) says the amount; a small set's readiness and admission
        // (tags 60–61) say what they are.
        assert_eq!(describe_action(&Action::Undelegate { validator: mallory.clone(), shares: 0, amount: 2_500_000_000 }), format!("Start unbonding 2.5 FANG of stake from validator {mallory} (the chain converts it to pool shares at its own price when this runs)"));
        assert_eq!(describe_action(&Action::Undelegate { validator: mallory.clone(), shares: 7, amount: 0 }), format!("Start unbonding 7 pool shares from validator {mallory}"));
        assert!(describe_action(&Action::ValidatorReady { epoch: 4 }).contains("end of epoch 4"));
        assert_eq!(describe_action(&Action::AdmitValidator { validator: mallory.clone(), epoch: 4 }), format!("Admit candidate validator {mallory} into a set of one or two at the end of epoch 4 (validators)"));
        // The proposal as a node lying only in its summary serves it (the round-2 reviewer's PoC (a)).
        let p = json!({ "id": 9, "status": "voting", "title": "Community garden", "url": "", "kind": serde_json::to_value(&drain).unwrap(), "summary": "plant garlic in the community garden (no funds move)", "proposer": { "name": "mallory.boo" }, "deposit_display": "1 FANG", "voting_end": 99, "voters": 0 });
        let digest = proposal_digest(&drain, "Community garden", "");
        let list = proposal_lines(&p, false);
        assert_eq!(list[1], format!("   does: Pay 10,000,000 FANG from the treasury to {mallory}"));
        assert!(list[2].starts_with(&format!("   digest: {digest}")));
        assert!(!list.join("\n").contains("garlic"), "gov list doesn't show the node's summary: {list:?}");
        let show = proposal_lines(&p, true);
        assert!(show.iter().any(|l| l == "   the node's own note (its text, not bound by any vote): plant garlic in the community garden (no funds move)"), "{show:?}");
        assert!(!show.iter().any(|l| l.contains("does:") && l.contains("garlic")));
        // A kind this CLI can't decode isn't guessed at.
        let bad = proposal_lines(&json!({ "id": 1, "kind": { "type": "future_kind" } }), false);
        assert!(bad[1].contains("can't decode it") && bad[1].contains("don't vote on it"), "{bad:?}");
        // The digest check: absent (ask), matching, mismatching (refused), malformed.
        assert!(!check_digest(None, &digest, "proposal #9").unwrap());
        assert!(check_digest(Some(&digest.to_uppercase()), &digest, "proposal #9").unwrap());
        let err = check_digest(Some(&"00".repeat(32)), &digest, "proposal #9").unwrap_err().to_string();
        assert!(err.contains("proposal #9 doesn't read the way it did") && err.contains("Nothing was signed"), "{err}");
        assert!(check_digest(Some(&digest[..16]), &digest, "proposal #9").unwrap_err().to_string().contains("full 64-hex digest"));
        // A pending multisig action: described, its exact JSON, its digest; the summary as a note.
        let action = Action::Transfer { to: mallory.clone(), amount: 50_000_000 * DROPS_PER_FANG };
        let m = json!({ "account": { "name": "foundation.boo" }, "threshold": 2, "owners": [{ "key_id": "ab", "label": "a" }, { "key_id": "cd", "label": "b" }], "pending": [{ "id": 7, "action": serde_json::to_value(&action).unwrap(), "summary": "pay the monthly grants", "valid_approvals": 1, "expires_at": 500 }] });
        let lines = multisig_lines(&m, "foundation.boo");
        assert!(lines.contains(&format!("      does: Send 50,000,000 FANG to {mallory}")), "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with(&format!("      digest: {} (blockula multisig approve 7 --from foundation.boo --digest", action_digest(&action)))));
        assert!(lines.iter().any(|l| l == "      the node's own note (its text, not bound by any approval): pay the monthly grants"));
    }
}
