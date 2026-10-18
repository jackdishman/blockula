//! `blockula gov …` (proposals, conviction votes, vote locks, community distributions) and
//! `blockula multisig …` (M-of-N accounts). Talks to any node's HTTP API and signs with the CLI
//! wallet or a key file, like `blockula hunt` and `blockula contrib`.

use crate::contrib_cli::{confirm, Rpc, Signer};
use crate::crypto::{self, Keys};
use crate::governance::build_distribution;
use crate::types::*;
use crate::validator_cli::{get, load_seed, parse_fang, rt, wallet_keys};
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

/// The node at `--rpc`, pinned to the chain the command is for, and the account the signer acts
/// for on it: `--from` (on mainnet only as an address: `Remote::from_account`), or the key's own
/// address.
async fn acting_for(s: &Signer, dir: &Path, keys: &Keys) -> Result<(crate::cli_net::Remote, String)> {
    let remote = s.connect(dir).await?;
    let account = s.acting_for(&remote, keys).await?;
    Ok((remote, account))
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

/// A network address field (host:port or a URL) as written, quoted on one line.
fn quoted_addr(v: &str) -> String {
    format!("“{}”", one_line(v, 120).replace(['“', '”'], "\""))
}

/// A key id the chain could store: 32 bytes of lowercase hex.
fn key_id_ok(k: &str) -> bool {
    k.len() == 64 && crypto::is_lower_hex(k)
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
        // The same sentence as the SDK's describeKind and the node's static text (audit
        // review-r3-clients-1): a cancellation also supersedes every upgrade proposal still open.
        ProposalKind::CancelUpgrade { version, activation_height } => format!("Cancel the upgrade to protocol v{version} at block {activation_height} and any upgrade scheduled after it; upgrade proposals still open when it runs are superseded and can't take effect"),
        ProposalKind::ActivateFeature { feature } => format!("Turn on the feature {}", one_line(feature, 64)),
        ProposalKind::DeactivateFeature { feature } => format!("Turn off the feature {}", one_line(feature, 64)),
        ProposalKind::Text {} => "Nothing on chain: a text proposal (a signal; read its title and link)".into(),
    }
}

/// What an action does (a multisig's pending action), in plain English, from the decoded action.
/// The CLI prints the action's exact JSON under it.
pub(crate) fn describe_action(a: &Action) -> String {
    // Only a key id the chain could store (64 lowercase hex) is shortened; anything else is the
    // node's invention and is said to be unreadable (and `action_review` won't approve it).
    let key = |k: &str| if key_id_ok(k) { format!("{}…{}", &k[..12], &k[k.len() - 8..]) } else { "an unreadable key id".to_string() };
    match a {
        Action::Transfer { to, amount } => format!("Send {} to {}", fang_text(*amount), account(to)),
        Action::RegisterName { name } => format!("Register the name {}", one_line(name, 40)),
        Action::AddKey { key_id, label, role: KeyRole::Owner, .. } => format!("Add key {} to the account as an owner key (full control of the account){}", key(key_id), quoted("label", label)),
        Action::AddKey { key_id, label, role: KeyRole::Session { limit, period_blocks, expires_at }, .. } => format!("Add key {} to the account as a session key that may send up to {} (fees included) every {period_blocks} blocks, until block #{expires_at}, and nothing else{}", key(key_id), fang_text(*limit), quoted("label", label)),
        Action::RemoveKey { key_id } => format!("Remove key {} from the account", key(key_id)),
        Action::RegisterValidator { consensus_key, p2p_addr, http_addr, bond, .. } => {
            // The consensus key decides who signs for (and can be slashed with) the bonded pool:
            // named by its key id, the 32 bytes the validator's address carries.
            let consensus = match hex::decode(consensus_key) {
                Ok(pk) if pk.len() == crypto::PUBLIC_KEY_LEN && crypto::is_lower_hex(consensus_key) => format!("consensus key {}", key(&key_id(&pk))),
                _ => "an unreadable consensus key".to_string(),
            };
            format!("Register a validator with {consensus} (it signs for the validator; the validator's address carries the same key id), p2p address {}, HTTP address {}, bonding {} from the account", quoted_addr(p2p_addr), quoted_addr(http_addr), fang_text(*bond))
        }
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
        // Each address separately: on chain an empty one means "keep it" (review-r3-clients-5).
        Action::UpdateValidator { commission_bps, p2p_addr, http_addr } => format!(
            "Set the account's validator commission to {commission_bps} basis points{}{}",
            if p2p_addr.is_empty() { String::new() } else { format!(", its p2p address to {}", quoted_addr(p2p_addr)) },
            if http_addr.is_empty() { String::new() } else { format!(", its HTTP address to {}", quoted_addr(http_addr)) }
        ),
        Action::Propose { kind, deposit, title, .. } => format!("Propose, bonding {}: {}{}", fang_text(*deposit), describe_kind(kind), quoted("title", title)),
        // What #id is comes from the digest, never from the id (audit gap-7-6): `action_review`
        // reads the proposal and checks it against this digest.
        Action::Vote { proposal_id, aye, amount, conviction, proposal_digest } => format!("Vote {} on proposal #{proposal_id} with {} at conviction {conviction}, binding the proposal whose content has digest {}", if *aye { "aye" } else { "nay" }, fang_text(*amount), one_line(proposal_digest, 64)),
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
            let (_, me) = rt()?.block_on(acting_for(&signer, dir, &keys))?;
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
            // confirmed (audit gap-7-6, review-r2-clients-3). The account is the one the tx is
            // signed for (an address on mainnet), and the read must be that account's.
            let keys = keys_of(&signer, dir)?;
            let rt = rt()?;
            let (remote, account) = rt.block_on(acting_for(&signer, dir, &keys))?;
            let m = rt.block_on(get(&remote.rpc, &format!("/multisig/{account}")))?;
            let answered = text(&m["account"]["address"]);
            ensure!(answered == account, "{} answered for {:?}, not {account}: it isn't showing this account's actions. Nothing was signed.", remote.rpc, one_line(answered, 80));
            let p = m["pending"].as_array().into_iter().flatten().find(|p| p["id"] == id).ok_or_else(|| anyhow!("{account} has no open multisig action #{id}"))?;
            let action: Action = serde_json::from_value(p["action"].clone()).context("the node's multisig action doesn't parse")?;
            let proposal = match &action {
                Action::Vote { proposal_id, .. } => rt.block_on(get(&remote.rpc, &format!("/proposal/{proposal_id}"))).ok(),
                _ => None,
            };
            let (lines, approvable) = action_review(&action, proposal.as_ref());
            let d = action_digest(&action);
            println!("Multisig action #{id} of {account}:");
            for l in &lines {
                println!("   {l}");
            }
            println!("   action: {}\n   digest: {d}", serde_json::to_string_pretty(&action)?);
            ensure!(approvable, "this CLI can't confirm what multisig action #{id} does (above): don't approve it from here. Nothing was signed.");
            let matched = check_digest(digest.as_deref(), &d, &format!("multisig action #{id}"))?;
            // `--from` by name (off mainnet): the account is the node's word, and the digest binds
            // the action, not the account it counts on (review-r3-clients-6), so it's confirmed too.
            let by_name = signer.from.as_deref().is_some_and(|f| !crypto::is_address(f.trim()));
            if by_name {
                println!("   account: {account}, which {} says {} is (UNPROVEN: a node can answer for another account)", remote.rpc, one_line(signer.from.as_deref().unwrap_or_default(), 80));
            }
            if !matched || by_name {
                confirm(&format!("Approve exactly this{}? If it reaches the threshold, this approval runs it.", if by_name { format!(", on {account}") } else { String::new() }), yes)?;
            }
            let tx = rt.block_on(crate::validator_cli::send_memo(&remote, &keys, &account, Action::MultisigApprove { id, action_digest: d }, String::new()))?;
            println!("✓ {}", tx["summary"].as_str().unwrap_or_default());
            println!("  {}/#/tx/{}", remote.rpc.trim_end_matches('/'), tx["id"].as_str().unwrap_or_default());
            Ok(())
        }
        MultisigCmd::Show { account, rpc } => {
            let rt = rt()?;
            let m = rt.block_on(get(&rpc.rpc, &format!("/multisig/{account}")))?;
            // The proposals pending votes name, read so each vote can be checked against what it binds.
            let mut proposals = std::collections::BTreeMap::new();
            for p in m["pending"].as_array().into_iter().flatten() {
                if let Ok(Action::Vote { proposal_id, .. }) = serde_json::from_value::<Action>(p["action"].clone()) {
                    if let Ok(v) = rt.block_on(get(&rpc.rpc, &format!("/proposal/{proposal_id}"))) {
                        proposals.insert(proposal_id, v);
                    }
                }
            }
            for l in multisig_lines(&m, &account, &rpc.rpc, &proposals) {
                println!("{l}");
            }
            Ok(())
        }
    }
}

/// What the CLI can say about a pending multisig action, and whether it may be approved: the
/// action decoded and described, refused when it isn't something the chain could have stored
/// (review-r3-clients-7). A `vote` binds a proposal by its content's digest, not its id (audit
/// gap-7-6): `proposal` is the node's read of the proposal it names, and it's described only if
/// its content hashes to the digest the vote binds (a node can't make other content do that).
pub(crate) fn action_review(action: &Action, proposal: Option<&Value>) -> (Vec<String>, bool) {
    let mut out = vec![format!("does: {}", describe_action(action))];
    if let Err(e) = action.check_as_multisig_action() {
        out.push(format!("but it isn't an action the chain could have stored ({e:#}): the node's answer is malformed. Don't approve it."));
        return (out, false);
    }
    let Action::Vote { proposal_id, proposal_digest: bound, .. } = action else { return (out, true) };
    let ok = match proposal.map(bound_proposal) {
        None => {
            out.push(format!("on: the node didn't serve proposal #{proposal_id}, so what this vote backs is unknown. Don't approve it until `blockula gov show {proposal_id}`, on a node you trust, prints the digest {bound}."));
            false
        }
        Some(Err(e)) => {
            out.push(format!("on: this CLI can't decode the node's proposal #{proposal_id} ({e:#}). Don't approve it."));
            false
        }
        Some(Ok((kind, title, _, d))) if d == *bound => {
            out.push(format!("on: proposal #{proposal_id} (its content has exactly the digest this vote binds): {}{}", describe_kind(&kind), quoted("title", &title)));
            true
        }
        Some(Ok((kind, _, _, d))) => {
            out.push(format!("on: NOT what the node shows as proposal #{proposal_id}. The node's #{proposal_id} ({}) has digest {d}, and this vote binds {bound}: the node is showing another proposal, or the vote was made for content #{proposal_id} doesn't have. Don't approve it.", describe_kind(&kind)));
            false
        }
    };
    (out, ok)
}

/// What `multisig show` prints: each pending action described from its decoded content (a vote
/// with the proposal it binds: `proposals`, read by id), its exact JSON and the digest an approval
/// binds (`multisig approve --digest`); the node's summary only as its labelled note (audit
/// gap-7-6). The account is printed as the user gave it: an approval counts on whatever account
/// it's signed `from`, and the digest binds only the action, so the CLI never hands over an
/// address it got from the node as the `--from` to use (review-r3-clients-6).
pub(crate) fn multisig_lines(m: &Value, given: &str, rpc: &str, proposals: &std::collections::BTreeMap<u64, Value>) -> Vec<String> {
    let given = given.trim();
    let owners = m["owners"].as_array().cloned().unwrap_or_default();
    let answered = text(&m["account"]["address"]);
    let label = one_line(text(&m["account"]["name"]), 40);
    let shape = format!("a {}-of-{} multisig", m["threshold"], owners.len());
    // Whether the node's answer is for the account the user asked about, as far as can be known.
    let mut usable = true;
    let mut out = if crypto::is_address(given) {
        if answered == given {
            vec![format!("{given}{} is {shape}", if label.is_empty() { String::new() } else { format!(" ({label}, as the node labels it)") })]
        } else {
            usable = false;
            vec![format!("{rpc} answered for {:?}, not {given}: it isn't showing this account's actions. Don't approve anything below.", one_line(answered, 80))]
        }
    } else {
        vec![
            format!("{} is {shape}, as {rpc} reports it", one_line(given, 40)),
            format!("   {rpc} says that's {} (UNPROVEN: a node can answer for another account; on mainnet `multisig approve` takes --from as the multisig's address, which you check with its owners)", one_line(answered, 80)),
        ]
    };
    for o in &owners {
        // Printed only as a key id the chain could store: the node's string, as is, could carry
        // line breaks or terminal escapes that draw a fake action and digest (resume-core-1).
        let key = text(&o["key_id"]);
        if key_id_ok(key) {
            out.push(format!("   owner {key} {}", one_line(text(&o["label"]), 64)));
        } else {
            usable = false;
            out.push(format!("   owner: an unreadable key id ({:?}): the node's answer isn't a multisig the chain could store. Don't approve anything below.", one_line(key, 80)));
        }
    }
    for p in m["pending"].as_array().cloned().unwrap_or_default() {
        let expired = if p["expired"] == true { " (expired)" } else { "" };
        out.push(format!("   #{} · {}/{} approvals · expires at block {}{expired}", p["id"], p["valid_approvals"], m["threshold"], p["expires_at"]));
        match serde_json::from_value::<Action>(p["action"].clone()) {
            Ok(a) => {
                let proposal = match &a {
                    Action::Vote { proposal_id, .. } => proposals.get(proposal_id),
                    _ => None,
                };
                let (lines, approvable) = action_review(&a, proposal);
                out.extend(lines.into_iter().map(|l| format!("      {l}")));
                let d = action_digest(&a);
                out.push(format!("      action: {}", serde_json::to_string(&a).unwrap_or_default()));
                if approvable && usable {
                    out.push(format!("      digest: {d} (blockula multisig approve {} --from {} --digest {d})", p["id"], one_line(given, 70)));
                } else {
                    out.push(format!("      digest: {d}"));
                }
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
        let m = json!({ "account": { "name": "foundation.boo", "address": mallory }, "threshold": 2, "owners": [{ "key_id": "ab".repeat(32), "label": "a" }, { "key_id": "cd".repeat(32), "label": "b" }], "pending": [{ "id": 7, "action": serde_json::to_value(&action).unwrap(), "summary": "pay the monthly grants", "valid_approvals": 1, "expires_at": 500 }] });
        let lines = multisig_lines(&m, "foundation.boo", "http://n", &Default::default());
        assert!(lines.contains(&format!("      does: Send 50,000,000 FANG to {mallory}")), "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with(&format!("      digest: {} (blockula multisig approve 7 --from foundation.boo --digest", action_digest(&action)))));
        assert!(lines.iter().any(|l| l == "      the node's own note (its text, not bound by any approval): pay the monthly grants"));
    }

    /// Audit review-r3-clients-6: `multisig show <name>` prints the account as given and the
    /// node's address for it as UNPROVEN, never as the `--from` to approve with; given an address,
    /// a node answering for another account is called out and no approval command is printed.
    #[test]
    fn multisig_show_never_hands_over_the_nodes_address() {
        let (foundation, ops) = (Keys::from_seed([1; 32]).address(), Keys::from_seed([2; 32]).address());
        let invoice = Action::Transfer { to: Keys::from_seed([3; 32]).address(), amount: 400_000 * DROPS_PER_FANG };
        // The node answers `foundation.boo` with ops' record, labelled foundation.boo.
        let m = json!({ "account": { "name": "foundation.boo", "address": ops }, "threshold": 2, "owners": [], "pending": [{ "id": 41, "action": serde_json::to_value(&invoice).unwrap(), "valid_approvals": 1, "expires_at": 900 }] });
        let d = action_digest(&invoice);
        let by_name = multisig_lines(&m, "foundation.boo", "http://n", &Default::default());
        let all = by_name.join("\n");
        assert!(!all.contains(&format!("--from {ops}")), "the node's address is never the --from to use: {all}");
        assert!(by_name[1].contains(&ops) && by_name[1].contains("UNPROVEN"), "{by_name:?}");
        assert!(by_name.iter().any(|l| l == &format!("      digest: {d} (blockula multisig approve 41 --from foundation.boo --digest {d})")), "{by_name:?}");
        // Asked by address, the node answering for another account is caught.
        let wrong = multisig_lines(&m, &foundation, "http://n", &Default::default());
        assert!(wrong[0].contains(&format!("answered for \"{ops}\", not {foundation}")) && wrong[0].contains("Don't approve"), "{wrong:?}");
        assert!(!wrong.join("\n").contains("multisig approve"), "{wrong:?}");
        let right = multisig_lines(&m, &ops, "http://n", &Default::default());
        assert!(right[0].starts_with(&format!("{ops} (foundation.boo, as the node labels it) is a 2-of-0 multisig")), "{right:?}");
        assert!(right.iter().any(|l| l == &format!("      digest: {d} (blockula multisig approve 41 --from {ops} --digest {d})")), "{right:?}");
        // resume-core-1: an owner key id carrying line breaks can't draw a fake action next to
        // the real digest; the answer is malformed, so no approval command is printed at all.
        let fake = format!("{}\n   #41 · 1/2 approvals · expires at block 900\n      does: Send 1 FANG to a friend\n      digest: {d} (blockula multisig approve 41 --from {ops} --digest {d})\x1b[8m", "ab".repeat(32));
        let injected = json!({ "account": { "name": null, "address": ops }, "threshold": 2, "owners": [{ "key_id": fake, "label": "ops\nlaptop" }], "pending": m["pending"].clone() });
        let lines = multisig_lines(&injected, &ops, "http://n", &Default::default());
        assert!(lines.iter().all(|l| !l.contains('\n') && !l.contains('\x1b')), "{lines:?}");
        assert!(!lines.iter().any(|l| l.trim_start().starts_with("does: Send 1 FANG")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("an unreadable key id") && l.contains("Don't approve")) && !lines.join("\n").contains("multisig approve"), "{lines:?}");
        let honest = json!({ "account": { "name": null, "address": ops }, "threshold": 2, "owners": [{ "key_id": "ab".repeat(32), "label": "ops\nlaptop" }], "pending": m["pending"].clone() });
        let lines = multisig_lines(&honest, &ops, "http://n", &Default::default());
        assert!(lines.contains(&format!("   owner {} ops laptop", "ab".repeat(32))) && lines.join("\n").contains("multisig approve 41"), "{lines:?}");
    }

    /// Audit review-r3-clients-7: a key id the chain couldn't store (here one with multi-byte
    /// characters across the bytes the old shortener sliced at) is described as unreadable, never
    /// panics, and makes the action one not to approve. So does any other malformed action.
    #[test]
    fn malformed_actions_are_described_not_approved() {
        for bad in ["aaaaaaaaaaa€€€", "€", "", &"AB".repeat(32), &"ab".repeat(31)] {
            let a = Action::RemoveKey { key_id: bad.to_string() };
            assert_eq!(describe_action(&a), "Remove key an unreadable key id from the account");
            let (lines, ok) = action_review(&a, None);
            assert!(!ok && lines[1].contains("isn't an action the chain could have stored") && lines[1].contains("Don't approve"), "{lines:?}");
            let owners = Action::MultisigCreate { owners: vec![bad.to_string(), "cd".repeat(32)], threshold: 1 };
            assert!(describe_action(&owners).contains("an unreadable key id, cdcdcdcdcdcd…cdcdcdcd"));
            assert!(!action_review(&owners, None).1);
        }
        let good = Action::RemoveKey { key_id: "ab".repeat(32) };
        assert_eq!(describe_action(&good), "Remove key abababababab…abababab from the account");
        assert!(action_review(&good, None).1);
        // Through `multisig show`, from the node's JSON.
        let m = json!({ "account": { "name": null, "address": Keys::from_seed([1; 32]).address() }, "threshold": 2, "owners": [], "pending": [{ "id": 3, "action": { "type": "remove_key", "key_id": "aaaaaaaaaaa€€€" }, "valid_approvals": 1, "expires_at": 9 }] });
        let lines = multisig_lines(&m, &Keys::from_seed([1; 32]).address(), "http://n", &Default::default());
        assert!(lines.iter().any(|l| l.contains("an unreadable key id")) && !lines.join("\n").contains("multisig approve"), "{lines:?}");
    }

    /// Audit review-r3-clients-5 and review-r3-clients-1: an address-only validator update says
    /// which address moves; a registration names its consensus key; a cancellation says it
    /// supersedes the upgrade proposals still open.
    #[test]
    fn validator_and_upgrade_descriptions_say_everything_the_action_does() {
        let only_http = Action::UpdateValidator { commission_bps: 500, p2p_addr: String::new(), http_addr: "https://evil.example".into() };
        assert_eq!(describe_action(&only_http), "Set the account's validator commission to 500 basis points, its HTTP address to “https://evil.example”");
        let both = Action::UpdateValidator { commission_bps: 5, p2p_addr: "1.2.3.4:9000".into(), http_addr: "https://v.example".into() };
        assert_eq!(describe_action(&both), "Set the account's validator commission to 5 basis points, its p2p address to “1.2.3.4:9000”, its HTTP address to “https://v.example”");
        assert_eq!(describe_action(&Action::UpdateValidator { commission_bps: 7, p2p_addr: String::new(), http_addr: String::new() }), "Set the account's validator commission to 7 basis points");
        let k = Keys::from_seed([4; 32]);
        let id = key_id(&k.public_key());
        let reg = Action::RegisterValidator { consensus_key: hex::encode(k.public_key()), p2p_addr: "1.2.3.4:9000".into(), http_addr: "https://v.example".into(), bond: 1_000_000 * DROPS_PER_FANG, pop: String::new() };
        assert_eq!(describe_action(&reg), format!("Register a validator with consensus key {}…{} (it signs for the validator; the validator's address carries the same key id), p2p address “1.2.3.4:9000”, HTTP address “https://v.example”, bonding 1,000,000 FANG from the account", &id[..12], &id[56..]));
        let junk = Action::RegisterValidator { consensus_key: "zz".into(), p2p_addr: String::new(), http_addr: String::new(), bond: 1, pop: String::new() };
        assert!(describe_action(&junk).starts_with("Register a validator with an unreadable consensus key"));
        assert_eq!(describe_kind(&ProposalKind::CancelUpgrade { version: 2, activation_height: 90_000 }), "Cancel the upgrade to protocol v2 at block 90000 and any upgrade scheduled after it; upgrade proposals still open when it runs are superseded and can't take effect");
    }

    /// Audit gap-7-6 through a multisig: a pending `vote` is described by the proposal whose
    /// content hashes to the digest it binds. A node showing another proposal under that id, or
    /// none, gets "don't approve", never its description.
    #[test]
    fn a_multisig_vote_is_described_by_the_proposal_it_binds() {
        let mallory = Keys::from_seed([66; 32]).address();
        let drain = ProposalKind::TreasurySpend { to: mallory.clone(), amount: 10_000_000 * DROPS_PER_FANG };
        let truth = proposal_digest(&drain, "Community garden", "");
        let vote = Action::Vote { proposal_id: 12, aye: true, amount: 100_000_000 * DROPS_PER_FANG, conviction: 6, proposal_digest: truth.clone() };
        assert_eq!(describe_action(&vote), format!("Vote aye on proposal #12 with 100,000,000 FANG at conviction 6, binding the proposal whose content has digest {truth}"));
        let honest = json!({ "id": 12, "kind": serde_json::to_value(&drain).unwrap(), "title": "Community garden", "url": "" });
        let (lines, ok) = action_review(&vote, Some(&honest));
        assert!(ok, "{lines:?}");
        assert_eq!(lines[1], format!("on: proposal #12 (its content has exactly the digest this vote binds): Pay 10,000,000 FANG from the treasury to {mallory} (title, as written: “Community garden”)"));
        // The co-signer's node shows a harmless text proposal as #12.
        let garlic = json!({ "id": 12, "kind": { "type": "text" }, "title": "Plant garlic", "url": "", "summary": "plant garlic" });
        let (lines, ok) = action_review(&vote, Some(&garlic));
        assert!(!ok && lines[1].starts_with("on: NOT what the node shows as proposal #12") && lines[1].contains(&truth) && lines[1].contains("Don't approve"), "{lines:?}");
        assert!(!lines.join(" ").contains("Pay 10,000,000"), "the node's text proposal isn't described as what the vote backs");
        let (lines, ok) = action_review(&vote, None);
        assert!(!ok && lines[1].contains("didn't serve proposal #12") && lines[1].contains(&truth), "{lines:?}");
        // `multisig show` prints no approval command for it.
        let m = json!({ "account": { "name": "foundation.boo", "address": mallory }, "threshold": 2, "owners": [], "pending": [{ "id": 5, "action": serde_json::to_value(&vote).unwrap(), "valid_approvals": 1, "expires_at": 900 }] });
        let lied = multisig_lines(&m, &mallory, "http://n", &[(12, garlic)].into_iter().collect());
        assert!(lied.iter().any(|l| l.contains("NOT what the node shows")) && !lied.join("\n").contains("multisig approve"), "{lied:?}");
        let told = multisig_lines(&m, &mallory, "http://n", &[(12, honest)].into_iter().collect());
        assert!(told.iter().any(|l| l.contains("Pay 10,000,000 FANG from the treasury")) && told.join("\n").contains("multisig approve 5"), "{told:?}");
    }
}
