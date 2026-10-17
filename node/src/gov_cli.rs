//! `blockula gov …` (proposals, conviction votes, vote locks, community distributions) and
//! `blockula multisig …` (M-of-N accounts). Talks to any node's HTTP API and signs with the CLI
//! wallet or a key file, like `blockula hunt` and `blockula contrib`.

use crate::contrib_cli::{Rpc, Signer};
use crate::crypto::{self, Keys};
use crate::governance::build_distribution;
use crate::types::*;
use crate::validator_cli::{get, load_seed, own_address, parse_fang, rt, wallet_keys};
use anyhow::{anyhow, bail, Context, Result};
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

fn print_proposal(p: &Value) {
    println!("#{} [{}] {}", p["id"], text(&p["status"]), text(&p["title"]));
    println!("   {} · by {} · deposit {}", text(&p["summary"]), name_of(&p["proposer"]), text(&p["deposit_display"]));
    let weight = |side: &str| text(&p[side]["weight"]["display"]).to_string();
    let end = match text(&p["status"]) {
        "voting" => format!("voting ends at block {}", p["voting_end"]),
        "queued" => format!("runs at block {}", p["execute_at"]),
        _ => text(&p["outcome"]).to_string(),
    };
    println!("   ayes {} · nays {} (weight) · {} voters · {end}", weight("ayes"), weight("nays"), p["voters"]);
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
                print_proposal(&p);
            }
            Ok(())
        }
        GovCmd::Show { id, rpc } => {
            let p = rt()?.block_on(get(&rpc.rpc, &format!("/proposal/{id}")))?;
            print_proposal(&p);
            if !text(&p["url"]).is_empty() {
                println!("   {}", text(&p["url"]));
            }
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
        GovCmd::Vote { id, side, amount, conviction, signer } => {
            let aye = match side.as_str() {
                "aye" | "yes" | "y" => true,
                "nay" | "no" | "n" => false,
                _ => bail!("vote aye or nay"),
            };
            let amount = parse_fang(&amount)?;
            // The vote binds the proposal shown here (its kind, title and link), decoded from the
            // node's answer rather than its summary: a node that misreports it makes the vote
            // fail, not land on something else (audit gap-7-6).
            let p = rt()?.block_on(get(&signer.rpc, &format!("/proposal/{id}")))?;
            let kind: ProposalKind = serde_json::from_value(p["kind"].clone()).context("the node's proposal kind doesn't parse")?;
            let (title, url) = (text(&p["title"]).to_string(), text(&p["url"]).to_string());
            println!("Voting {} with {} at conviction {conviction} on proposal #{id}: “{title}”{}", if aye { "aye" } else { "nay" }, fmt_fang(amount), if url.is_empty() { String::new() } else { format!(" ({url})") });
            println!("   it does: {}", serde_json::to_string(&kind)?);
            let proposal_digest = proposal_digest(&kind, &title, &url);
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
        MultisigCmd::Approve { id, signer } => {
            // The approval binds the stored action, printed decoded (not the node's summary of
            // it): a node that misreports it makes the approval fail (audit gap-7-6).
            let keys = keys_of(&signer, dir)?;
            let rt = rt()?;
            let account = rt.block_on(acting_for(&signer, &keys))?;
            let m = rt.block_on(get(&signer.rpc, &format!("/multisig/{account}")))?;
            let p = m["pending"].as_array().into_iter().flatten().find(|p| p["id"] == id).ok_or_else(|| anyhow!("{account} has no open multisig action #{id}"))?;
            let action: Action = serde_json::from_value(p["action"].clone()).context("the node's multisig action doesn't parse")?;
            println!("Approving multisig action #{id} of {account}, which does exactly this:\n{}", serde_json::to_string_pretty(&action)?);
            drop(rt);
            submit(dir, &signer, Action::MultisigApprove { id, action_digest: action_digest(&action) }).map(|_| ())
        }
        MultisigCmd::Show { account, rpc } => {
            let m = rt()?.block_on(get(&rpc.rpc, &format!("/multisig/{account}")))?;
            let owners = m["owners"].as_array().cloned().unwrap_or_default();
            println!("{} is a {}-of-{} multisig", name_of(&m["account"]), m["threshold"], owners.len());
            for o in &owners {
                println!("   owner {} {}", text(&o["key_id"]), text(&o["label"]));
            }
            for p in m["pending"].as_array().cloned().unwrap_or_default() {
                let expired = if p["expired"] == true { " (expired)" } else { "" };
                println!("   #{} {} · {}/{} approvals · expires at block {}{expired}", p["id"], text(&p["summary"]), p["valid_approvals"], m["threshold"], p["expires_at"]);
                // What an approval signs is the action itself: show it, not just the node's summary.
                println!("      action: {}", serde_json::to_string(&p["action"])?);
            }
            Ok(())
        }
    }
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
    }
}
