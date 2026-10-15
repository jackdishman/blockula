//! Upgrades, halts and feature flags (P-UPGRADE, MAINNET-DESIGN 3.9, DEC-2): the protocol-version
//! schedule that `ScheduleUpgrade` proposals change, the feature flags `ActivateFeature` and
//! `DeactivateFeature` flip (Ghost Mode ships off), and the node-local side of upgrades: what
//! this binary supports, the operator's `--halt-height`, and why a node stopped.
//!
//! How a node follows the schedule:
//! * Every block carries the version in force at its height ([`version_at`]); verifiers reject
//!   any other (`consensus.rs`). An upgrade activates at least [`MIN_UPGRADE_LEAD`] blocks after
//!   the block that schedules it, so a node within that many blocks of a height knows its version
//!   for certain.
//! * Blocks also carry `signal`, the newest version the proposer's binary supports, so readiness
//!   is visible on chain (`/api/upgrade`).
//! * A node never executes a height whose version its binary doesn't support: it stops after the
//!   block before, abstains from voting on later blocks, and exits ([`Halt::exit_code`]) or, as a
//!   full node, keeps serving reads with the reason in `/api/health` (docs/OPERATIONS.md
//!   "Upgrades"). `--halt-height H` takes the same path after executing `H`.
//!
//! State: the schedule (META key `upgrade`) and the flags (namespace `state::ns::FEATURES`).

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

/// The upgrade module's slice of the state (namespace `state::ns::FEATURES`, META key `upgrade`).
#[derive(Clone, Debug, Default)]
pub struct Upgrades {
    /// Active features by name.
    pub features: TMap<String, Feature>,
    /// Scheduled protocol versions, by activation height (ascending).
    pub schedule: Vec<ScheduledUpgrade>,
}

impl SubState for Upgrades {
    fn tables(&self) -> Vec<(u8, &dyn Table)> {
        let Upgrades { features, schedule: _ } = self;
        vec![(ns::FEATURES, features)]
    }

    fn tables_mut(&mut self) -> Vec<(u8, &mut dyn Table)> {
        let Upgrades { features, schedule: _ } = self;
        vec![(ns::FEATURES, features)]
    }

    fn meta_records(&self) -> Vec<(&'static [u8], Vec<u8>)> {
        vec![(meta::UPGRADE, encode(&self.schedule))]
    }

    fn load_meta(&mut self, key: &[u8], value: &[u8]) -> Result<bool> {
        if key != meta::UPGRADE {
            return Ok(false);
        }
        self.schedule = decode(value)?;
        Ok(true)
    }
}

/// Undo log for [`Upgrades`] (part of `chain::Journal`).
#[derive(Default)]
pub struct UpgradeJournal {
    pub features: JMap<String, Feature>,
    pub schedule: Option<Vec<ScheduledUpgrade>>,
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
        if let Some(s) = schedule {
            u.schedule = s;
        }
    }
}

/// Whether a feature is switched on.
pub fn is_active(state: &State, feature: &str) -> bool {
    state.upgrade.features.contains_key(feature)
}

// ---- levers (`ProposalKind::ScheduleUpgrade`, `ActivateFeature`, `DeactivateFeature`): no
// signer checks, governance (P-GOV) calls them when a proposal executes.

/// Schedules protocol `version` from `activation_height` on: at least [`MIN_UPGRADE_LEAD`] blocks
/// from now, after every scheduled upgrade, and a newer version than all of them.
pub(crate) fn exec_schedule_upgrade(state: &mut State, j: &mut Journal, version: u32, activation_height: u64, height: u64) -> Result<()> {
    if height.checked_add(MIN_UPGRADE_LEAD).is_none_or(|earliest| activation_height < earliest) {
        bail!("an upgrade must activate at least {MIN_UPGRADE_LEAD} blocks after block {height}");
    }
    if version <= version_at(state, activation_height) {
        bail!("protocol v{version} isn't newer than v{} (in force at block {activation_height})", version_at(state, activation_height));
    }
    if let Some(last) = state.upgrade.schedule.last().filter(|u| u.activation_height >= activation_height) {
        bail!("v{} is already scheduled at block {}; a new upgrade must come after it", last.version, last.activation_height);
    }
    if j.upgrade.schedule.is_none() {
        j.upgrade.schedule = Some(state.upgrade.schedule.clone());
    }
    state.upgrade.schedule.push(ScheduledUpgrade { version, activation_height });
    Ok(())
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
        assert!(s.get("c").is_none() && s.get("x").is_none(), "no block yet; not in the set");
    }
}
