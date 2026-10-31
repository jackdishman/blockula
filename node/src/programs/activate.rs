//! Switching programs on (docs/PROGRAMS-V0.md §1.3, §1.5): the one place program state comes into
//! being.
//!
//! Called right after the `programs` feature is inserted, by `upgrade::exec_activate_feature` (an
//! `ActivateFeature { programs }` proposal executing, journaled with it) and by
//! `Genesis::initial_state` for a genesis that lists the feature (no journal: it's the genesis
//! state, before block 1). Both callers have already checked that the version in force knows the
//! feature (v2 or later; at genesis, block 1's version) and that the chain lets governance switch
//! it on there (`upgrade::check_may_activate`: on `blockula-1`, only from v3).

use super::params;
use super::state::ProgramsMeta;
use crate::chain::{Journal, State};
use std::collections::BTreeMap;

/// Creates META `programs` the first time programs switch on: the default parameters (§4.5), the
/// fuel rate at the default floor, zero totals, no pause and no votes. Journaled in `j` when there
/// is one, so a proposal whose execution fails leaves nothing.
///
/// Idempotent: on a reactivation (META present: programs were on, then switched off, which leaves
/// them exit-only, §1.4) it does nothing. The feature flag being back is what lifts exit-only;
/// parameters, totals, the fuel rate, programs and any pause stay as they are.
pub fn on_activate(state: &mut State, j: Option<&mut Journal>) {
    if state.programs.meta.is_some() {
        return;
    }
    if let Some(j) = j {
        j.programs.save_meta(&state.programs);
    }
    let params = params::defaults(&state.params);
    let fuel_rate = params[params::Param::FuelFloorRate.key()];
    state.programs.meta = Some(ProgramsMeta {
        params,
        programs: 0,
        state_bytes: 0,
        value_held: 0,
        sponsored: 0,
        escrow: 0,
        fuel_rate,
        pause: None,
        votes: BTreeMap::new(),
        pause_signed: BTreeMap::new(),
    });
}

#[cfg(test)]
mod tests;
