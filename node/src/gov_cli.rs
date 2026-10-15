//! `blockula gov …` (proposals, conviction votes, vote locks, community distributions) and
//! `blockula multisig …` (M-of-N accounts). Talks to any node's HTTP API and signs with the CLI
//! wallet or a key file, like `blockula hunt` and `blockula contrib`.

use crate::contrib_cli::{Rpc, Signer};
use crate::crypto::{self, Keys};
use crate::governance::build_distribution;
use crate::types::*;
use crate::validator_cli::{get, load_seed, own_address, parse_fang, rt, send_memo, wallet_keys};
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

async fn resolve(rpc: &str, who: &str) -> Result<String> {
    if crypto::is_address(who) {
        return Ok(who.to_string());
    }
    get(rpc, &format!("/account/{who}")).await?["account"]["address"].as_str().map(String::from).ok_or_else(|| anyhow!("can't resolve {who}"))
}

/// The account the signer acts for: `--from`, or the key's own address.
async fn acting_for(s: &Signer, keys: &Keys) -> Result<String> {
    match &s.from {
        Some(a) => resolve(&s.rpc, a).await,
        None => own_address(&s.rpc, keys).await,
    }
}

/// Signs and submits `action`, waits for it, and prints the summary and explorer link.
fn submit(dir: &Path, s: &Signer, action: Action) -> Result<Value> {
    let keys = keys_of(s, dir)?;
    let tx = rt()?.block_on(async {
        let from = acting_for(s, &keys).await?;
        send_memo(&s.rpc, &keys, &from, action, String::new()).await
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
    /// (amounts in FANG). The tree commits to the distribution's id (the proposal's), so build
    /// it right before proposing; prints (or writes) the root and every recipient's proof.
    Tree {
        entries: PathBuf,
        /// Distribution id (default: the chain's next proposal id)
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
    title: String,
    /// https:// link to the full text and discussion
    #[arg(long, default_value = "")]
    url: String,
    /// Deposit in FANG (default: the chain's minimum)
    #[arg(long)]
    deposit: Option<String>,
    #[command(flatten)]
    signer: Signer,
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
        key: String,
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
    /// Unlink a GitHub handle
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
            submit(dir, &signer, Action::Vote { proposal_id: id, aye, amount: parse_fang(&amount)?, conviction }).map(|_| ())
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
                None => rt()?.block_on(get(&rpc.rpc, "/proposals?limit=0"))?["next_id"].as_u64().ok_or_else(|| anyhow!("no next_id from {}", rpc.rpc))?,
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
    let (kind, meta) = match kind {
        KindCmd::Text { meta } => (ProposalKind::Text {}, meta),
        KindCmd::Param { key, value, meta } => {
            let value = value.parse::<u64>().or_else(|_| parse_fang(&value).map_err(|_| anyhow!("value must be a whole number, or FANG for amounts (e.g. 1000.5)")))?;
            (ProposalKind::ParamChange { key, value }, meta)
        }
        KindCmd::Spend { to, amount, meta } => (ProposalKind::TreasurySpend { to: rt.block_on(resolve(&meta.signer.rpc, &to))?, amount: parse_fang(&amount)? }, meta),
        KindCmd::Distribution { tree, days, meta } => {
            let t = read_tree(&tree)?;
            let status = rt.block_on(get(&meta.signer.rpc, "/status"))?;
            let next = rt.block_on(get(&meta.signer.rpc, "/proposals?limit=0"))?["next_id"].as_u64().unwrap_or_default();
            let id = t["distribution_id"].as_u64().unwrap_or_default();
            if id != next {
                bail!("{} was built for distribution #{id}, but this proposal will be #{next}: rebuild it with `blockula gov tree --id {next}`", tree.display());
            }
            let block_ms = status["block_time_ms"].as_u64().unwrap_or(1000).max(1);
            let claim_until = status["height"].as_u64().unwrap_or(0) + days * 86_400_000 / block_ms;
            let total: u64 = text(&t["total"]).parse().context("bad total in the tree")?;
            (ProposalKind::Distribution { merkle_root: text(&t["merkle_root"]).to_string(), total, claim_until }, meta)
        }
        KindCmd::SetRepo { repo, weight, meta } => (ProposalKind::SetRepo { repo, weight }, meta),
        KindCmd::GrantSoul { to, soul, reason, meta } => (ProposalKind::GrantSoul { to: rt.block_on(resolve(&meta.signer.rpc, &to))?, amount: parse_soul(&soul)?, reason }, meta),
        KindCmd::FlagSybil { account, reason, meta } => (ProposalKind::FlagSybil { account: rt.block_on(resolve(&meta.signer.rpc, &account))?, reason }, meta),
        KindCmd::RejectClaim { claim, reason, meta } => (ProposalKind::RejectClaim { claim_id: claim, reason }, meta),
        KindCmd::Clawback { account, reason, meta } => (ProposalKind::Clawback { account: rt.block_on(resolve(&meta.signer.rpc, &account))?, reason }, meta),
        KindCmd::UnlinkGithub { handle, meta } => (ProposalKind::UnlinkGithub { handle }, meta),
        KindCmd::RefundHunt { hunt, meta } => (ProposalKind::RefundHunt { hunt_id: hunt }, meta),
        KindCmd::Upgrade { version, activation_height, meta } => (ProposalKind::ScheduleUpgrade { version, activation_height }, meta),
        KindCmd::Activate { feature, meta } => (ProposalKind::ActivateFeature { feature }, meta),
        KindCmd::Deactivate { feature, meta } => (ProposalKind::DeactivateFeature { feature }, meta),
    };
    kind.check()?;
    let deposit = match &meta.deposit {
        Some(d) => parse_fang(d)?,
        None => text(&rt.block_on(get(&meta.signer.rpc, "/proposals?limit=0"))?["params"]["proposal_deposit"]).parse().context("no proposal_deposit from the node")?,
    };
    drop(rt);
    submit(dir, &meta.signer, Action::Propose { kind, deposit, title: meta.title, url: meta.url }).map(|_| ())
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
        MultisigCmd::Approve { id, signer } => submit(dir, &signer, Action::MultisigApprove { id }).map(|_| ()),
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
            }
            Ok(())
        }
    }
}
