//! The validators' pause of programs (docs/PROGRAMS-V0.md §7.2; `ProgramsPause`, tag 69, §2.5):
//! f + 1 validators of the current epoch's active set stop new program transactions for three days,
//! from the block after the signature that reached the threshold. It moves no funds and lapses on
//! its own at `until` (there's no unpause); while it runs, `status` is `Paused` (§1.1), so tags
//! 62–64 are refused at admission and so are native credits to programs (`super::credit`).
//!
//! Follows `GhostPause` (`ghost::pause_vote`), with the limits a set as small as `blockula-1`'s
//! needs (A25), all of them read from META `programs` (`pause`, `votes`, `pause_signed`):
//!
//! | Rule | |
//! |---|---|
//! | Who signs | a validator of the current epoch's active set, from its own account (its consensus key) or its owner's (an owner key): one signature per validator, whichever key made it ([`validator_of`]) |
//! | Votes | a signature waits for the others [`VOTE_DAYS`] (a day), across epoch boundaries; older ones, and those of validators that left the set, are dropped before counting; a validator with a live vote can't sign again |
//! | Threshold | f + 1 of the active set, `ghost::pause_threshold(n)`: a new run, or the running pause renewed |
//! | Length | [`PAUSE_DAYS`] (3 days) from the block after; a renewal moves `until` to 3 days after its block, never back |
//! | Window | a validator that started or renewed a pause may sign again only [`SIGN_WINDOW_DAYS`] (30 days) later, unless a fix is pending ([`fix_pending`]); a signature that never reached the threshold spends nothing |
//! | The cap | a run (a pause, then renewals while it runs) ends by `started` + [`MAX_SPAN_DAYS`] (30 days), fix pending or not; a new run starts no earlier than `started` + 2 × that (60 days) |
//!
//! Every limit is an admission check ([`may_sign`]), so a refused signature is never charged. The
//! last run's record stays in META after it lapses, which is what the spacing between runs reads,
//! so no end-of-block step is needed. A pause isn't tied to an activation: it holds whatever the
//! feature says, `On` or exit-only (§2.7).
//!
//! The spans are days at the chain's block time ([`Spans`]): code constants, frozen with v2's other
//! program rules (§9.2).

use super::state::{meta_mut, Pause};
use super::{status, Status};
use crate::chain::{short, Journal, Params, State};
use crate::governance::ProposalStatus;
use crate::params::blocks_per_day;
use crate::types::ProposalKind;
use crate::upgrade;
use anyhow::{anyhow, bail, ensure, Result};
use std::collections::BTreeSet;

/// How long a pause signature waits for the others (`PAUSE_VOTE_BLOCKS`): a day.
pub const VOTE_DAYS: u64 = 1;
/// How long a pause lasts from the block after the signature that started or renewed it
/// (`PROGRAMS_PAUSE_BLOCKS`): 3 days.
pub const PAUSE_DAYS: u64 = 3;
/// How long a validator that started or renewed a pause waits to sign again, unless a fix is
/// pending (`PAUSE_SIGN_WINDOW`): 30 days.
pub const SIGN_WINDOW_DAYS: u64 = 30;
/// The longest a run of pauses lasts from its start; a new run waits twice this from the last
/// run's start (`PAUSE_MAX_SPAN`, the maintainer's cap of 2026-10-20): 30 days.
pub const MAX_SPAN_DAYS: u64 = 30;
// A run's first pause ends before its cap, so the cap only ever clips a renewal.
const _: () = assert!(PAUSE_DAYS < MAX_SPAN_DAYS);

/// §7.2's spans in blocks, at a chain's block time (`params::blocks_per_day`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spans {
    /// `PAUSE_VOTE_BLOCKS`: a vote counts at heights `[at, at + vote_blocks)`.
    pub vote_blocks: u64,
    /// `PROGRAMS_PAUSE_BLOCKS`: a pause reached at `h` runs `[h + 1, h + 1 + pause_blocks)`.
    pub pause_blocks: u64,
    /// `PAUSE_SIGN_WINDOW`: a validator whose last pause was at `at` waits until `at + sign_window`.
    pub sign_window: u64,
    /// `PAUSE_MAX_SPAN`: a run ends by `started + max_span`; the next starts at `started + 2 × max_span`
    /// or later.
    pub max_span: u64,
}

impl Spans {
    /// The spans on a chain with these genesis constants.
    pub fn of(p: &Params) -> Spans {
        let days = |n: u64| n.saturating_mul(blocks_per_day(p.block_time_ms));
        Spans { vote_blocks: days(VOTE_DAYS), pause_blocks: days(PAUSE_DAYS), sign_window: days(SIGN_WINDOW_DAYS), max_span: days(MAX_SPAN_DAYS) }
    }

    /// Where run `p` must end by: `started + max_span`. A renewal is clipped here, and once `until`
    /// is here the run can't be renewed.
    pub fn cap(&self, p: &Pause) -> u64 {
        p.started.saturating_add(self.max_span)
    }

    /// The first height a signature may start a new run at, after run `p`: `started + 2 × max_span`.
    pub fn next_run(&self, p: &Pause) -> u64 {
        p.started.saturating_add(self.max_span.saturating_mul(2))
    }
}

/// The validator a `ProgramsPause` from `sender`, signed with key `signer`, counts for at `height`:
/// one in that epoch's active set whose consensus key signed from its own account, or whose owner
/// signed with one of its keys (`staking::validator_for_signer`, as `GhostPause`). Both forms name
/// the same validator, so each has one signature however many keys can make it (§1.7).
pub fn validator_of(state: &State, sender: &str, signer: &str, height: u64) -> Result<String> {
    let epoch = state.params.epoch_of(height);
    crate::staking::validator_for_signer(state, sender, signer, epoch).ok_or_else(|| anyhow!("only a validator in epoch {epoch}'s active set (its consensus key or its owner) can pause programs"))
}

/// Whether `validator` (of the current active set: [`validator_of`]) may sign a pause at `height`,
/// or why not: tag 69's admission check, so a refused signature is never charged, and the
/// condition under which its consensus-key form is a validator duty (`staking::duty_of`). In order:
/// programs exist; the cap (a running pause that reached it, or a new run too soon after the last
/// one's start), whether or not a fix is pending; no live vote of its own; and the window, which a
/// pending fix waives.
pub fn may_sign(state: &State, validator: &str, height: u64) -> Result<()> {
    if status(state, height) == Status::Dormant {
        bail!("programs have never been switched on here, so there's nothing to pause");
    }
    let m = state.programs.meta.as_ref().ok_or_else(|| anyhow!("programs have no state"))?;
    may_sign_with(&Record { pause: m.pause.as_ref(), votes: &m.votes, pause_signed: &m.pause_signed }, &Spans::of(&state.params), validator, height, || fix_pending(state, height))
}

/// What [`may_sign`] reads of META `programs`: the last run of pauses (`None` before the first), the
/// pending votes and each validator's last pause, as `/api/params` serves them, so that `blockula
/// program pause` checks a signature with this code and not a copy of it.
pub struct Record<'a> {
    pub pause: Option<&'a Pause>,
    pub votes: &'a std::collections::BTreeMap<String, u64>,
    pub pause_signed: &'a std::collections::BTreeMap<String, u64>,
}

/// [`may_sign`]'s rules over `m`, at `height` with the spans `s` (the chain's block time): the cap,
/// the spacing between runs, a live vote, and the window, which `fix_pending` (asked only when the
/// window would otherwise refuse) waives.
pub fn may_sign_with(m: &Record, s: &Spans, validator: &str, height: u64, fix_pending: impl FnOnce() -> bool) -> Result<()> {
    // The arms are in height order: `from < until` (programs' invariants).
    match m.pause {
        // A pause that a signature in this block started: it runs from the next block, and a
        // signature counts toward renewing it from there.
        Some(p) if height < p.from => bail!("programs are paused from block {}: a pause was signed in this block", p.from),
        // A running pause: a signature counts toward renewing it, until the run reaches its cap.
        Some(p) if height < p.until => ensure!(p.until < s.cap(p), "this pause has reached its {MAX_SPAN_DAYS}-day cap: it ends at block {} and can't be renewed, and programs can be paused again from block {}", p.until, s.next_run(p)),
        // The last run lapsed: a new one waits for the spacing from its start.
        Some(p) => ensure!(s.next_run(p) <= height, "programs can be paused again from block {}: the last run of pauses started at block {}, and runs start at least {} days apart", s.next_run(p), p.started, 2 * MAX_SPAN_DAYS),
        None => {}
    }
    if let Some(at) = m.votes.get(validator).filter(|at| height < at.saturating_add(s.vote_blocks)) {
        bail!("{} already signed a pause at block {at}; its signature counts until block {}", short(validator), at.saturating_add(s.vote_blocks).saturating_sub(1));
    }
    if let Some(at) = m.pause_signed.get(validator).filter(|at| height < at.saturating_add(s.sign_window)) {
        ensure!(fix_pending(), "{} started or renewed a pause at block {at}: it may sign again from block {}, or while a fix is pending (a ScheduleUpgrade or DeactivateFeature {{ programs }} proposal voting or queued, or an upgrade scheduled)", short(validator), at.saturating_add(s.sign_window));
    }
    Ok(())
}

/// Counts a `ProgramsPause` from `sender`, signed with `signer`, at `height` (its execution,
/// journaled in `j`): [`may_sign`] again, then the votes that still count (younger than
/// [`Spans::vote_blocks`], of validators in the active set), this one with them. At f + 1 of the
/// active set (`ghost::pause_threshold`), with no pause running a new run starts (`from`, `started`:
/// the next block; `until`: [`PAUSE_DAYS`] after it), and the window's expired entries are dropped;
/// with one running it's renewed (`until` moves to [`PAUSE_DAYS`] after the next block, never back,
/// and never past `started` + [`MAX_SPAN_DAYS`]; `from` and `started` stay). Either way the counted
/// voters become its `signers`, each one's `pause_signed` becomes `height`, and the votes clear.
/// Below the threshold only the votes change, so a lone signature spends no window.
pub fn sign(state: &mut State, j: &mut Journal, sender: &str, signer: &str, height: u64) -> Result<()> {
    let validator = validator_of(state, sender, signer, height)?;
    may_sign(state, &validator, height)?;
    let s = Spans::of(&state.params);
    let set: BTreeSet<String> = state.active_sets.get(&state.params.epoch_of(height)).into_iter().flatten().cloned().collect();
    let needed = crate::ghost::pause_threshold(set.len());
    let m = meta_mut(state, j).ok_or_else(|| anyhow!("programs have no state"))?;
    m.votes.retain(|v, at| set.contains(v) && height < at.saturating_add(s.vote_blocks));
    m.votes.insert(validator, height);
    if m.votes.len() < needed {
        return Ok(());
    }
    let signers: Vec<String> = std::mem::take(&mut m.votes).into_keys().collect();
    let next = height.checked_add(1).ok_or_else(|| anyhow!("no block follows block {height}"))?;
    let until = next.checked_add(s.pause_blocks).ok_or_else(|| anyhow!("a pause from block {next} ends past the last block"))?;
    match &mut m.pause {
        // `may_sign` admitted this signature only while `until` is under the cap, so the clipped
        // value is never below the old one.
        Some(p) if p.from <= height && height < p.until => {
            p.until = p.until.max(until).min(s.cap(p));
            p.signers = signers.clone();
        }
        _ => {
            m.pause_signed.retain(|_, at| height < at.saturating_add(s.sign_window));
            m.pause = Some(Pause { from: next, until, started: next, signers: signers.clone() });
        }
    }
    for v in signers {
        m.pause_signed.insert(v, height);
    }
    Ok(())
}

/// Whether a fix for programs is pending at `height`, which waives the window (§7.2, §7.4): a
/// `ScheduleUpgrade` or a `DeactivateFeature { programs }` proposal is voting or queued, or an upgrade
/// is scheduled above `height`. Cheap to make true (a proposal's deposit), which is why the cap
/// holds whatever it says.
pub fn fix_pending(state: &State, height: u64) -> bool {
    let g = &state.governance;
    let proposed = g.open_proposals.iter().filter_map(|id| g.proposals.get(id)).any(|p| {
        matches!(p.status, ProposalStatus::Voting | ProposalStatus::Queued)
            && match &p.kind {
                ProposalKind::ScheduleUpgrade { .. } => true,
                ProposalKind::DeactivateFeature { feature } => feature == upgrade::PROGRAMS_FEATURE,
                _ => false,
            }
    });
    proposed || upgrade::next_after(state, height).is_some()
}

#[cfg(test)]
mod tests;
