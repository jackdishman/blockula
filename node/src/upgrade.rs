//! Upgrades, halts and feature flags (P-UPGRADE, MAINNET-DESIGN 3.9, DEC-2): the protocol-version
//! schedule that `ScheduleUpgrade` proposals extend and `CancelUpgrade` proposals cut back, the
//! feature flags `ActivateFeature` and `DeactivateFeature` flip (Ghost Mode ships off), and the
//! node-local side of upgrades: what this binary supports, the operator's `--halt-height`, and
//! why a node stopped.
//!
//! How a node follows the schedule:
//! * Every block carries the version in force at its height ([`version_at`]); verifiers reject
//!   any other (`consensus.rs`). An upgrade activates at least [`MIN_UPGRADE_LEAD`] blocks after
//!   the block that schedules it, and can only be cancelled until that many blocks before it, so
//!   a node within that many blocks of a height knows its version for certain.
//! * Blocks also carry `signal`, the newest version the proposer's binary supports, so readiness
//!   is visible on chain (`/api/upgrade`).
//! * A node never executes a height whose version its binary doesn't support: it stops after the
//!   block before, abstains from voting on later blocks, and exits ([`Halt::exit_code`]) or, as a
//!   full node, keeps serving reads with the reason in `/api/health` (docs/OPERATIONS.md
//!   "Upgrades"). `--halt-height H` takes the same path after executing `H`.
//!
//! State: the schedule and what cancellations withdrew (META key `upgrade`) and the flags
//! (namespace `state::ns::FEATURES`).
//!
//! A withdrawn version number is spent (review-fix-ops-1): halts and readiness compare only
//! numbers, so a fixed release can't share one with the release it replaces (binaries of the
//! pulled one would run the re-scheduled version with the pulled rules); it ships as the next
//! number. So are the numbers of upgrade proposals that passed and were still waiting to run
//! when a cancellation ran (review-fix-ops-2: their releases may be out). A cancellation also
//! supersedes every `ScheduleUpgrade` proposed before it ran: they were proposed against the
//! schedule it withdrew from, and their binaries build on the withdrawn rules, so they fail when
//! they'd run ([`check_not_superseded`]).
//!
//! Versions only move up in small steps, at most [`MAX_VERSION_STEP`] past the newest version
//! ever scheduled or withdrawn ([`newest_version`]), and an upgrade activates at most
//! [`MAX_UPGRADE_HORIZON_DAYS`] after the block that schedules it (review-r2-gov-1): no proposal,
//! however it passed, can spend the number space or park the schedule out of reach, so a
//! cancellation always leaves room to schedule the fixed release. No arithmetic on version
//! numbers here can overflow (it saturates).

// The governance levers are called by P-GOV.
#![cfg_attr(not(test), allow(dead_code))]

use crate::chain::{Chain, Journal, State};
use crate::consensus::{PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSION};
use crate::state::codec::{decode, encode};
use crate::state::{meta, ns, JMap, SubState, TMap, Table};
use anyhow::{bail, ensure, Result};
use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// An upgrade activates at least this many blocks after the block that schedules it. Proposers
/// and verifiers within this many blocks of a height (`consensus.rs` waits until it is) therefore
/// know the version in force there, whatever executes in between. Above `MAX_EXEC_LAG` and the
/// 64 ancestors a proposer replays; a code bound, not a parameter, because lowering it would let
/// a verifier reject a valid block.
pub const MIN_UPGRADE_LEAD: u64 = 64;

/// A `ScheduleUpgrade`'s version is at most this far past [`newest_version`] (review-r2-gov-1):
/// enough for a release line to skip a few numbers, never enough for one proposal (a
/// fat-fingered `u32::MAX`, `-1 >>> 0` from a script, or a deliberate one) to spend the number
/// space, which a cancellation would then make permanent. A code bound, like the lead.
pub const MAX_VERSION_STEP: u32 = 16;

/// An upgrade activates at most this many days after the block that schedules it. Far enough
/// for any schedule governance runs (the longest vote and timelock the parameters allow, then a
/// cancellation's vote, take 270 days), and it keeps an activation height a JavaScript number
/// holds exactly and the schedule within reach (a later upgrade must come after it).
pub const MAX_UPGRADE_HORIZON_DAYS: u64 = 365;

/// Exit codes of a node that stops on purpose (docs/OPERATIONS.md: systemd's
/// `RestartPreventExitStatus=10 11 12` keeps it down until an operator acts).
pub const EXIT_HALT_HEIGHT: i32 = 10;
pub const EXIT_UPGRADE_REQUIRED: i32 = 11;
pub const EXIT_DOUBLE_SIGN_RISK: i32 = 12;

/// Features governance can switch on and off. A name outside this list can't be activated.
pub const KNOWN_FEATURES: &[&str] = &["ghost"];

/// Feature names: 1–32 of `a-z`, `0-9`, `_` and `-` (stateless; [`KNOWN_FEATURES`] is checked
/// where a feature is switched).
pub fn check_feature_name(f: &str) -> Result<()> {
    if f.is_empty() || f.len() > crate::types::MAX_FEATURE || !f.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-') {
        bail!("feature names are 1–{} of a-z, 0-9, '_' and '-'", crate::types::MAX_FEATURE);
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Feature {
    /// Height it was switched on (0: at genesis).
    pub activated_at: u64,
}

/// Protocol `version` is in force from `activation_height` on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct ScheduledUpgrade {
    pub version: u32,
    pub activation_height: u64,
}

/// What `CancelUpgrade`s withdrew: the version numbers they spent and when the latest ran.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Withdrawals {
    /// The highest version a cancellation withdrew (0: none). No version up to it is ever
    /// scheduled again.
    pub highest: u32,
    /// The block the latest cancellation ran in: a `ScheduleUpgrade` proposed in it or before
    /// is superseded.
    pub last_at: Option<u64>,
}

/// The upgrade module's slice of the state (namespace `state::ns::FEATURES`, META key `upgrade`:
/// the schedule, then the withdrawals).
#[derive(Clone, Debug, Default)]
pub struct Upgrades {
    /// Active features by name.
    pub features: TMap<String, Feature>,
    /// Scheduled protocol versions, by activation height (ascending).
    pub schedule: Vec<ScheduledUpgrade>,
    pub withdrawals: Withdrawals,
}

impl SubState for Upgrades {
    fn tables(&self) -> Vec<(u8, &dyn Table)> {
        let Upgrades { features, schedule: _, withdrawals: _ } = self;
        vec![(ns::FEATURES, features)]
    }

    fn tables_mut(&mut self) -> Vec<(u8, &mut dyn Table)> {
        let Upgrades { features, schedule: _, withdrawals: _ } = self;
        vec![(ns::FEATURES, features)]
    }

    fn meta_records(&self) -> Vec<(&'static [u8], Vec<u8>)> {
        vec![(meta::UPGRADE, encode(&(&self.schedule, &self.withdrawals)))]
    }

    fn load_meta(&mut self, key: &[u8], value: &[u8]) -> Result<bool> {
        if key != meta::UPGRADE {
            return Ok(false);
        }
        (self.schedule, self.withdrawals) = decode(value)?;
        Ok(true)
    }
}

/// Undo log for [`Upgrades`] (part of `chain::Journal`).
#[derive(Default)]
pub struct UpgradeJournal {
    pub features: JMap<String, Feature>,
    pub schedule: Option<(Vec<ScheduledUpgrade>, Withdrawals)>,
}

impl UpgradeJournal {
    pub fn absorb(&mut self, other: UpgradeJournal) {
        let UpgradeJournal { features, schedule } = other;
        self.features.absorb(features);
        if self.schedule.is_none() {
            self.schedule = schedule;
        }
    }

    pub fn rollback(self, u: &mut Upgrades) {
        let UpgradeJournal { features, schedule } = self;
        features.restore(&mut u.features);
        if let Some((s, w)) = schedule {
            (u.schedule, u.withdrawals) = (s, w);
        }
    }

    /// Saves the schedule and the withdrawals before the first change to them.
    fn save(&mut self, u: &Upgrades) {
        if self.schedule.is_none() {
            self.schedule = Some((u.schedule.clone(), u.withdrawals.clone()));
        }
    }
}

/// Whether a feature is switched on.
pub fn is_active(state: &State, feature: &str) -> bool {
    state.upgrade.features.contains_key(feature)
}

// ---- levers (`ProposalKind::ScheduleUpgrade`, `CancelUpgrade`, `ActivateFeature`,
// `DeactivateFeature`): no signer checks, governance (P-GOV) calls them when a proposal executes.

/// The newest protocol version the chain has had scheduled (in force, still to come, or
/// withdrawn since), or [`PROTOCOL_VERSION`]: a new upgrade names a version above it, and at most
/// [`MAX_VERSION_STEP`] above it.
pub fn newest_version(state: &State) -> u32 {
    let scheduled = state.upgrade.schedule.last().map_or(PROTOCOL_VERSION, |u| u.version);
    PROTOCOL_VERSION.max(scheduled).max(state.upgrade.withdrawals.highest)
}

/// The latest block an upgrade scheduled by a lever running at `height` may activate at.
pub fn latest_activation(state: &State, height: u64) -> u64 {
    height.saturating_add(MAX_UPGRADE_HORIZON_DAYS.saturating_mul(crate::params::blocks_per_day(state.params.block_time_ms)))
}

/// Whether protocol `version` could be scheduled from `activation_height` on by a lever that runs
/// at `height`: at least [`MIN_UPGRADE_LEAD`] blocks later and at most
/// [`MAX_UPGRADE_HORIZON_DAYS`], after every scheduled upgrade, a newer version than all of them,
/// not a number a cancellation spent, and at most [`MAX_VERSION_STEP`] past [`newest_version`]. A
/// `ScheduleUpgrade` proposal is checked against it when it's made (with `height` the earliest
/// block it can run in) and again when it runs.
pub fn check_schedule_upgrade(state: &State, version: u32, activation_height: u64, height: u64) -> Result<()> {
    if height.checked_add(MIN_UPGRADE_LEAD).is_none_or(|earliest| activation_height < earliest) {
        bail!("an upgrade must activate at least {MIN_UPGRADE_LEAD} blocks after block {height}");
    }
    let latest = latest_activation(state, height);
    if activation_height > latest {
        bail!("an upgrade activates at most {MAX_UPGRADE_HORIZON_DAYS} days after the block that schedules it: block {latest} at the latest after block {height}");
    }
    if version <= version_at(state, activation_height) {
        bail!("protocol v{version} isn't newer than v{} (in force at block {activation_height})", version_at(state, activation_height));
    }
    let spent = state.upgrade.withdrawals.highest;
    if version <= spent {
        bail!("protocol v{version} isn't newer than v{spent}, which was scheduled and withdrawn (or passed and was superseded): a version number is never reused (a binary of the withdrawn release would run it), so ship the fixed release as v{} or later", spent.saturating_add(1));
    }
    let newest = newest_version(state);
    let most = newest.saturating_add(MAX_VERSION_STEP);
    if version > most {
        bail!("protocol v{version} is more than {MAX_VERSION_STEP} past v{newest}, the newest version scheduled or withdrawn: name v{}–v{most}", newest.saturating_add(1));
    }
    if let Some(last) = state.upgrade.schedule.last().filter(|u| u.activation_height >= activation_height) {
        bail!("v{} is already scheduled at block {}; a new upgrade must come after it", last.version, last.activation_height);
    }
    Ok(())
}

/// A `ScheduleUpgrade` of `version` proposed at `proposed_at` can still run: no cancellation ran
/// in that block or after it (review-fix-ops-2). One that did withdrew an entry of the schedule
/// the proposal was made against, and the proposal's release builds on the withdrawn rules, so
/// it ships again under a number past both the spent ones and its own.
pub fn check_not_superseded(state: &State, proposed_at: u64, version: u32) -> Result<()> {
    let w = &state.upgrade.withdrawals;
    if let Some(at) = w.last_at.filter(|at| proposed_at <= *at) {
        let next = w.highest.max(version).saturating_add(1);
        bail!("superseded: it was proposed at block {proposed_at}, and the CancelUpgrade that ran at block {at} withdrew upgrades from the schedule it was proposed against (versions through v{} are spent). Propose the upgrade again as v{next} or later: a release built as v{version} carries the withdrawn rules", w.highest);
    }
    Ok(())
}

/// Schedules protocol `version` from `activation_height` on ([`check_schedule_upgrade`]).
pub(crate) fn exec_schedule_upgrade(state: &mut State, j: &mut Journal, version: u32, activation_height: u64, height: u64) -> Result<()> {
    check_schedule_upgrade(state, version, activation_height, height)?;
    j.upgrade.save(&state.upgrade);
    state.upgrade.schedule.push(ScheduledUpgrade { version, activation_height });
    Ok(())
}

/// Withdraws the upgrade to `version` scheduled at `activation_height`
/// (`ProposalKind::CancelUpgrade`, audit F50), and every upgrade scheduled after it, which was
/// scheduled to follow it. Only while it is still at least [`MIN_UPGRADE_LEAD`] blocks away, as
/// scheduling one is: a verifier within that many blocks of a height still knows its version
/// for certain, and nothing that already activated (or is about to) is ever rewritten. Returns
/// what it withdrew. The schedule stays strictly increasing (a prefix of it is kept); the
/// withdrawn version numbers are spent, and so is `passed` (the highest version of the
/// `ScheduleUpgrade` proposals that passed and haven't run, 0 for none: they're superseded, and
/// their releases may be out), and every `ScheduleUpgrade` proposed by `height` is superseded
/// ([`Withdrawals`]).
pub(crate) fn exec_cancel_upgrade(state: &mut State, j: &mut Journal, version: u32, activation_height: u64, height: u64, passed: u32) -> Result<Vec<ScheduledUpgrade>> {
    let i = scheduled(state, version, activation_height)?;
    if height.checked_add(MIN_UPGRADE_LEAD).is_none_or(|earliest| activation_height < earliest) {
        bail!("v{version} activates at block {activation_height}, less than {MIN_UPGRADE_LEAD} blocks after block {height}: too late to cancel on chain");
    }
    j.upgrade.save(&state.upgrade);
    let withdrawn = state.upgrade.schedule.split_off(i);
    let w = &mut state.upgrade.withdrawals;
    w.highest = w.highest.max(withdrawn.last().expect("at least the named entry").version).max(passed);
    w.last_at = Some(height);
    Ok(withdrawn)
}

/// Where the upgrade to `version` at `activation_height` is in the schedule.
pub(crate) fn scheduled(state: &State, version: u32, activation_height: u64) -> Result<usize> {
    if let Some(i) = state.upgrade.schedule.iter().position(|u| u.version == version && u.activation_height == activation_height) {
        return Ok(i);
    }
    match state.upgrade.schedule.iter().find(|u| u.version == version) {
        Some(u) => bail!("v{version} is scheduled at block {}, not {activation_height}", u.activation_height),
        None => bail!("no upgrade to v{version} at block {activation_height} is scheduled"),
    }
}

/// Switches a known feature on.
pub(crate) fn exec_activate_feature(state: &mut State, j: &mut Journal, feature: &str, height: u64) -> Result<()> {
    if !KNOWN_FEATURES.contains(&feature) {
        bail!("unknown feature {feature} (known: {})", KNOWN_FEATURES.join(", "));
    }
    if is_active(state, feature) {
        bail!("{feature} is already on");
    }
    j.upgrade.features.touch(&state.upgrade.features, feature);
    state.upgrade.features.insert(feature.to_string(), Feature { activated_at: height });
    Ok(())
}

/// Switches a feature off.
pub(crate) fn exec_deactivate_feature(state: &mut State, j: &mut Journal, feature: &str) -> Result<()> {
    if !is_active(state, feature) {
        bail!("{feature} isn't on");
    }
    j.upgrade.features.touch(&state.upgrade.features, feature);
    state.upgrade.features.remove(feature);
    Ok(())
}

/// The protocol version in force at `height`.
pub fn version_at(state: &State, height: u64) -> u32 {
    state.upgrade.schedule.iter().filter(|u| u.activation_height <= height).map(|u| u.version).next_back().unwrap_or(PROTOCOL_VERSION)
}

/// The next scheduled upgrade after `height`, if any.
pub fn next_after(state: &State, height: u64) -> Option<&ScheduledUpgrade> {
    state.upgrade.schedule.iter().find(|u| u.activation_height > height)
}

/// `Err` when this binary can't execute `next_height` (a version it doesn't support is
/// scheduled there), so the node halts cleanly instead of diverging.
pub fn check_next(state: &State, next_height: u64) -> Result<()> {
    check_next_with(state, next_height, SUPPORTED_PROTOCOL_VERSION).map_err(Into::into)
}

/// [`check_next`] for a node that supports protocol versions up to `supported`.
pub fn check_next_with(state: &State, next_height: u64, supported: u32) -> Result<(), Halt> {
    let version = version_at(state, next_height);
    if version > supported {
        return Err(Halt::UpgradeRequired { height: next_height, version, supported });
    }
    Ok(())
}

// ---- halts (node-local, never state)

/// Why a node stopped executing blocks on purpose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Halt {
    /// `--halt-height`: `height` is executed, as asked.
    AtHeight { height: u64 },
    /// Block `height` runs protocol `version`; this binary supports up to `supported`.
    UpgradeRequired { height: u64, version: u32, supported: u32 },
    /// Slashing protection saw this validator's key sign something this process didn't
    /// (`protection.rs`): another instance runs with the same key.
    DoubleSignRisk { reason: String },
}

impl Halt {
    /// The process exit code for this halt.
    pub fn exit_code(&self) -> i32 {
        match self {
            Halt::AtHeight { .. } => EXIT_HALT_HEIGHT,
            Halt::UpgradeRequired { .. } => EXIT_UPGRADE_REQUIRED,
            Halt::DoubleSignRisk { .. } => EXIT_DOUBLE_SIGN_RISK,
        }
    }

    /// Short machine-readable kind (`/api/health`, `/metrics`).
    pub fn kind(&self) -> &'static str {
        match self {
            Halt::AtHeight { .. } => "halt_height",
            Halt::UpgradeRequired { .. } => "upgrade_required",
            Halt::DoubleSignRisk { .. } => "double_sign_risk",
        }
    }
}

impl std::fmt::Display for Halt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Halt::AtHeight { height } => write!(f, "halted at block {height} (--halt-height): remove the flag, or raise it, to continue"),
            Halt::UpgradeRequired { height, version, supported } => {
                write!(f, "block {height} runs protocol v{version}, but this binary supports up to v{supported}: upgrade blockula to keep following the chain")
            }
            Halt::DoubleSignRisk { reason } => write!(f, "stopped to avoid double-signing: {reason}"),
        }
    }
}

impl std::error::Error for Halt {}

/// The halt a node is in, shared with its HTTP API (which reports it without the chain lock).
#[derive(Clone, Debug, Default)]
pub struct HaltSignal(Arc<parking_lot::Mutex<Option<Halt>>>);

impl HaltSignal {
    pub fn get(&self) -> Option<Halt> {
        self.0.lock().clone()
    }

    /// Records `halt` unless the node already halted (the first reason sticks).
    pub fn set(&self, halt: Halt) {
        self.0.lock().get_or_insert(halt);
    }

    fn clear(&self) {
        *self.0.lock() = None;
    }
}

/// A node's upgrade and halt settings (`Chain::halt`): not state, and not the same on every node.
#[derive(Clone, Debug)]
pub struct HaltControl {
    /// Newest protocol version this node runs: [`SUPPORTED_PROTOCOL_VERSION`] (the simulator sets
    /// others to model nodes running other binaries).
    pub supported: u32,
    /// `--halt-height`: execute this height, then stop.
    pub halt_height: Option<u64>,
    /// Set once the node stops executing.
    pub signal: HaltSignal,
}

impl Default for HaltControl {
    fn default() -> Self {
        Self { supported: SUPPORTED_PROTOCOL_VERSION, halt_height: None, signal: HaltSignal::default() }
    }
}

impl HaltControl {
    /// Whether this node may still vote for, propose or execute a block at `height` given the
    /// state it has executed: not halted, not past `--halt-height`, and a version it supports.
    pub fn allows(&self, state: &State, height: u64) -> bool {
        self.signal.get().is_none() && self.halt_height.is_none_or(|h| height <= h) && version_at(state, height) <= self.supported
    }
}

/// Before executing `height`: refuses if the node halted, `height` is past `--halt-height`, or
/// its version isn't supported (whatever the caller did, nothing past a halt executes).
pub fn before_block(c: &Chain, height: u64) -> Result<()> {
    if let Some(h) = c.halt.signal.get() {
        return Err(h.into());
    }
    if let Some(h) = c.halt.halt_height.filter(|h| height > *h) {
        return Err(Halt::AtHeight { height: h }.into());
    }
    check_next_with(&c.state, height, c.halt.supported)?;
    Ok(())
}

/// The last step after `height` is executed and stored: if the node must stop here (it reached
/// `--halt-height`, or can't run the next height), records why and returns it as the error, which
/// leaves the block unacknowledged so consensus delivers nothing more.
pub fn after_block(c: &Chain, height: u64) -> Result<()> {
    let halt = match c.halt.halt_height {
        Some(h) if height >= h => Some(Halt::AtHeight { height }),
        _ => check_next_with(&c.state, height + 1, c.halt.supported).err(),
    };
    match halt {
        Some(h) => {
            c.halt.signal.set(h.clone());
            Err(h.into())
        }
        None => Ok(()),
    }
}

/// Sets this node's supported version and halt height and re-checks where it stands (at open,
/// and whenever they change): a node already at its halt point is halted at once, which the
/// caller reports (validators exit; full nodes serve reads).
pub fn configure(c: &mut Chain, supported: u32, halt_height: Option<u64>) {
    c.halt.supported = supported;
    c.halt.halt_height = halt_height;
    c.halt.signal.clear();
    let height = c.height();
    let _ = after_block(c, height);
}

/// What `/api/upgrade` reports about readiness: for each active validator, the newest version it
/// signalled in the blocks it proposed among `blocks` (height, proposer, signal), newest first.
pub fn signals<'a>(active: &[String], blocks: impl IntoIterator<Item = (u64, &'a str, u32)>) -> std::collections::BTreeMap<String, (u32, u64)> {
    let mut out = std::collections::BTreeMap::new();
    for (height, proposer, signal) in blocks {
        if active.iter().any(|a| a == proposer) && !out.contains_key(proposer) {
            out.insert(proposer.to_string(), (signal, height));
        }
    }
    out
}

/// The upgrade module's consistency rules (part of `State::check_invariants`).
pub fn check_invariants(state: &State) -> Result<()> {
    let s = &state.upgrade.schedule;
    ensure!(s.windows(2).all(|w| w[0].activation_height < w[1].activation_height && w[0].version < w[1].version), "the upgrade schedule isn't increasing");
    let w = &state.upgrade.withdrawals;
    ensure!((w.highest > 0) == w.last_at.is_some(), "the upgrade withdrawals disagree: v{} withdrawn, last at {:?}", w.highest, w.last_at);
    ensure!(s.iter().all(|u| u.version != w.highest), "withdrawn v{} is still scheduled", w.highest);
    for f in state.upgrade.features.keys() {
        ensure!(KNOWN_FEATURES.contains(&f.as_str()), "unknown feature {f} is active");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_and_the_halt_check() {
        let mut st = State::default();
        assert_eq!(version_at(&st, 0), PROTOCOL_VERSION);
        check_next(&st, 1).unwrap();
        st.upgrade.schedule = vec![ScheduledUpgrade { version: SUPPORTED_PROTOCOL_VERSION + 1, activation_height: 100 }];
        check_invariants(&st).unwrap();
        assert_eq!(version_at(&st, 99), PROTOCOL_VERSION);
        check_next(&st, 99).unwrap();
        assert!(check_next(&st, 100).unwrap_err().to_string().contains("upgrade blockula"));
        assert!(check_feature_name("ghost").is_ok() && check_feature_name("Ghost").is_err() && check_feature_name("").is_err());
    }

    /// An upgrade activates at least `MIN_UPGRADE_LEAD` blocks out: that's what lets a verifier
    /// within that many blocks of a height know its version for certain.
    #[test]
    fn upgrades_need_the_lead_time() {
        let mut st = State::default();
        let mut j = Journal::default();
        let err = exec_schedule_upgrade(&mut st, &mut j, 2, 100 + MIN_UPGRADE_LEAD - 1, 100).unwrap_err().to_string();
        assert!(err.contains("at least 64 blocks after block 100"), "{err}");
        exec_schedule_upgrade(&mut st, &mut j, 2, 100 + MIN_UPGRADE_LEAD, 100).unwrap();
        assert!(exec_schedule_upgrade(&mut st, &mut j, 2, 500, 100).unwrap_err().to_string().contains("isn't newer"));
        assert!(exec_schedule_upgrade(&mut st, &mut j, 3, u64::MAX, u64::MAX - 1).is_err(), "the lead can't wrap");
        assert_eq!((version_at(&st, 163), version_at(&st, 164)), (PROTOCOL_VERSION, 2));
        assert_eq!(next_after(&st, 163).map(|u| u.version), Some(2));
        assert!(next_after(&st, 164).is_none());
        check_invariants(&st).unwrap();
    }

    /// F50: a `CancelUpgrade` withdraws exactly the entry it names, and whatever was scheduled to
    /// follow it, only while it's at least `MIN_UPGRADE_LEAD` blocks away: never one that already
    /// activated, never one inside the window a verifier relies on.
    #[test]
    fn cancelling_withdraws_an_upgrade_and_those_after_it() {
        let mut st = State::default();
        let mut j = Journal::default();
        exec_schedule_upgrade(&mut st, &mut j, 2, 200, 100).unwrap();
        exec_schedule_upgrade(&mut st, &mut j, 3, 400, 100).unwrap();
        exec_schedule_upgrade(&mut st, &mut j, 4, 600, 100).unwrap();
        let before = st.upgrade.schedule.clone();
        // Only the exact entry, only at least the lead before it, and nothing past it.
        assert!(exec_cancel_upgrade(&mut st, &mut j, 3, 401, 100, 0).unwrap_err().to_string().contains("scheduled at block 400, not 401"));
        assert!(exec_cancel_upgrade(&mut st, &mut j, 5, 700, 100, 0).unwrap_err().to_string().contains("no upgrade to v5"));
        assert!(exec_cancel_upgrade(&mut st, &mut j, 2, 200, 200 - MIN_UPGRADE_LEAD + 1, 0).unwrap_err().to_string().contains("too late"));
        assert!(exec_cancel_upgrade(&mut st, &mut j, 2, 200, 250, 0).unwrap_err().to_string().contains("too late"), "it already activated");
        assert!(exec_cancel_upgrade(&mut st, &mut j, 4, 600, u64::MAX, 0).is_err(), "the lead can't wrap");
        assert_eq!(st.upgrade.schedule, before, "refusals change nothing");
        // v3 goes, and v4, which was scheduled to follow it; v2 (already in force) stays.
        let withdrawn = exec_cancel_upgrade(&mut st, &mut j, 3, 400, 400 - MIN_UPGRADE_LEAD, 0).unwrap();
        assert_eq!(withdrawn, before[1..]);
        assert_eq!(st.upgrade.schedule, before[..1]);
        assert_eq!((version_at(&st, 399), version_at(&st, 400), version_at(&st, 600)), (2, 2, 2));
        check_invariants(&st).unwrap();
        // Nothing left to re-cancel, and the withdrawn version numbers are spent
        // (review-fix-ops-1): a binary of the pulled release would run a re-scheduled v3 with its
        // own rules, so the fixed release ships as v5.
        assert_eq!(st.upgrade.withdrawals, Withdrawals { highest: 4, last_at: Some(400 - MIN_UPGRADE_LEAD) });
        assert!(exec_cancel_upgrade(&mut st, &mut j, 3, 400, 300, 0).unwrap_err().to_string().contains("no upgrade to v3"));
        for v in [3, 4] {
            let e = exec_schedule_upgrade(&mut st, &mut j, v, 500, 400).unwrap_err().to_string();
            assert!(e.contains("never reused") && e.contains("as v5 or later"), "{e}");
        }
        exec_schedule_upgrade(&mut st, &mut j, 5, 500, 400).unwrap();
        assert_eq!(version_at(&st, 500), 5);
        check_invariants(&st).unwrap();
        // Superseded: anything proposed by the block the cancellation ran in.
        assert!(check_not_superseded(&st, 400 - MIN_UPGRADE_LEAD, 2).unwrap_err().to_string().contains("superseded"));
        check_not_superseded(&st, 401 - MIN_UPGRADE_LEAD, 2).unwrap();
        // The journal restores the schedule and the withdrawals as they were before the first change.
        j.upgrade.rollback(&mut st.upgrade);
        assert!(st.upgrade.schedule.is_empty());
        assert_eq!(st.upgrade.withdrawals, Withdrawals::default());
    }

    #[test]
    fn halts_say_why_with_distinct_exit_codes() {
        let halts = [
            Halt::AtHeight { height: 7 },
            Halt::UpgradeRequired { height: 8, version: 3, supported: 2 },
            Halt::DoubleSignRisk { reason: "x".into() },
        ];
        let codes: Vec<i32> = halts.iter().map(Halt::exit_code).collect();
        assert_eq!(codes, [EXIT_HALT_HEIGHT, EXIT_UPGRADE_REQUIRED, EXIT_DOUBLE_SIGN_RISK]);
        assert!(codes.iter().all(|c| ![0, 1, 2].contains(c)), "distinct from success, errors and usage errors");
        assert_eq!(halts.iter().map(Halt::kind).collect::<Vec<_>>(), ["halt_height", "upgrade_required", "double_sign_risk"]);
        assert!(halts[1].to_string().contains("block 8 runs protocol v3, but this binary supports up to v2"));
        // The first reason sticks.
        let signal = HaltSignal::default();
        signal.set(halts[0].clone());
        signal.set(halts[1].clone());
        assert_eq!(signal.get(), Some(halts[0].clone()));
        // What a node may vote on, propose or execute.
        let mut st = State::default();
        st.upgrade.schedule = vec![ScheduledUpgrade { version: SUPPORTED_PROTOCOL_VERSION + 1, activation_height: 50 }];
        let mut c = HaltControl { halt_height: Some(30), ..Default::default() };
        assert!(c.allows(&st, 30) && !c.allows(&st, 31));
        c.halt_height = None;
        assert!(c.allows(&st, 49) && !c.allows(&st, 50));
        c.supported += 1;
        assert!(c.allows(&st, 50));
        c.signal.set(halts[2].clone());
        assert!(!c.allows(&st, 1));
    }

    #[test]
    fn readiness_is_each_active_validators_newest_signal() {
        let active = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let blocks = [(10, "a", 2), (9, "b", 1), (8, "a", 1), (7, "x", 9), (6, "b", 2)];
        let s = signals(&active, blocks);
        assert_eq!(s.get("a"), Some(&(2, 10)));
        assert_eq!(s.get("b"), Some(&(1, 9)), "the newest block counts, not the highest signal");
        assert!(!s.contains_key("c") && !s.contains_key("x"), "no block yet; not in the set");
    }
}
