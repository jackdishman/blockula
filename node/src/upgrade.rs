//! Upgrades and feature flags (P-UPGRADE, MAINNET-DESIGN 3.9, DEC-2): the protocol-version
//! schedule that `ScheduleUpgrade` proposals change, the check that halts a node before a height
//! its binary can't execute, and the feature flags `ActivateFeature`/`DeactivateFeature` flip
//! (Ghost Mode ships off).
//!
//! Scaffold: the schedule and the flags (from genesis `features`) as state records,
//! [`version_at`] and [`check_next`]. A map is added by declaring it in [`Upgrades`] under this
//! module's namespace (`state::ns` 47; take more from 64 up) and journaling it in
//! [`UpgradeJournal`].

// Scaffold: P-UPGRADE and P-GOV call into this.
#![cfg_attr(not(test), allow(dead_code))]

use crate::chain::State;
use crate::consensus::{PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSION};
use crate::state::codec::{decode, encode};
use crate::state::{meta, ns, JMap, SubState, TMap, Table};
use anyhow::{bail, ensure, Result};
use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};

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

/// The protocol version in force at `height`.
pub fn version_at(state: &State, height: u64) -> u32 {
    state.upgrade.schedule.iter().filter(|u| u.activation_height <= height).map(|u| u.version).next_back().unwrap_or(PROTOCOL_VERSION)
}

/// The last end-of-block step: `Err` when this binary can't execute `next_height` (a version it
/// doesn't support is scheduled), so the node halts cleanly instead of diverging.
pub fn check_next(state: &State, next_height: u64) -> Result<()> {
    let v = version_at(state, next_height);
    if v > SUPPORTED_PROTOCOL_VERSION {
        bail!("block {next_height} runs protocol v{v}, but this binary supports up to v{SUPPORTED_PROTOCOL_VERSION}: upgrade blockula to keep following the chain");
    }
    Ok(())
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
}
