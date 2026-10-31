//! Programs' governance parameters (docs/PROGRAMS-V0.md §4.5): seven keys, kept in META
//! `programs` as a sorted `key → u64` map (`ProgramsMeta::params`), so a later protocol version adds
//! a key without re-encoding the record: a key the map doesn't hold reads as its default
//! ([`param`]).
//!
//! | Key | Default | Bounds | Class |
//! |---|---|---|---|
//! | `program_value_cap` (drops) | 10,000,000 FANG | [0, `params::MAX_SUPPLY`] | council |
//! | `program_fuel_floor_rate` (milli-drops per fuel) | 250 | [1, 1,000,000] | council |
//! | `program_max_tx_fuel` | 10,000,000 | [1,000,000, `meter::MAX_TX_FUEL`] | council |
//! | `program_max_block_fuel` | 40,000,000 | [20,000,000, `meter::CALIBRATED_BLOCK_FUEL`] | council |
//! | `program_storage_price` (drops per byte) | 100,000 | [1,000, 10,000,000] | council |
//! | `program_state_soft_bytes` | 64 MiB | [16 MiB, [`MAX_STATE_SOFT_BYTES`]] | council |
//! | `program_min_upgrade_delay_blocks` | 2 days | [1 hour, 90 days] | token holders only |
//!
//! **When a key is programs'.** A `ParamChange` of a `program_*` key comes here only where the
//! version in force is ≥ v2 **and** META `programs` exists, that is where programs aren't
//! `Dormant` ([`is_program_param`]): before programs first switch on it's unknown, exactly as in
//! v1, so v2 changes nothing while programs are dormant (§1.2). v1 refuses it statelessly, in the
//! codec (`ProposalKind::check`); the codec now lets these seven keys through, so admission refuses
//! them while they aren't programs' ([`admit`]), in the same words and with the same effect (the
//! transaction can't be included, and nobody is charged), and `ChainParams` refuses them past it.
//! The defaults therefore run for at least one vote and timelock after programs first switch on.
//!
//! **Classes.** Every key but the minimum upgrade delay is the bootstrap council's to pass too
//! ([`COUNCIL_PARAMS`], read by `params::tokens_only`, and so by `governance::needs_supermajority`
//! and `council_may_pass`), as docs/PROGRAMS.md 14.2 says.
//!
//! **Bounds** are code constants: W1's `meter::MAX_TX_FUEL` and `meter::CALIBRATED_BLOCK_FUEL`, and
//! the rest here. Only a protocol version moves them, after re-measurement (§4.5, §9.2).

use super::{status, Status};
use crate::chain::{Journal, Params, State};
use crate::params::{blocks_per_day, MAX_SUPPLY};
use crate::types::{Action, ProposalKind, DROPS_PER_FANG};
use anyhow::{anyhow, bail, ensure, Result};
use blockula_vm::config::{MAX_CODE_BYTES, MAX_INTERFACE_BYTES};
use blockula_vm::meter::{CALIBRATED_BLOCK_FUEL, MAX_TX_FUEL, SCHEDULE_V0};
use std::collections::BTreeMap;

/// Every key starts with this; nothing else in `ChainParams` does (`params_keep_to_their_prefix`).
pub const PREFIX: &str = "program_";

/// The upper bound of `program_state_soft_bytes`: 512 MiB. Program state may grow to twice the
/// soft cap (the price curve's ceiling, §4.6), so 1 GiB at most.
pub const MAX_STATE_SOFT_BYTES: u64 = 512 << 20;
// Twice the bound, plus 1 GiB of native state, stays within what a snapshot may carry (§4.6;
// docs/PROGRAMS.md 14.2), so no parameter in bounds can make the state unsnapshottable.
const _: () = assert!(2 * MAX_STATE_SOFT_BYTES + (1 << 30) <= crate::snapshot::MAX_STATE_BYTES);

/// The upper bound of `program_fuel_floor_rate`, in milli-drops per fuel.
pub const MAX_FUEL_FLOOR_RATE: u64 = 1_000_000;
// The floor is a rate the controller can hold: at most the rate's ceiling.
const _: () = assert!(MAX_FUEL_FLOOR_RATE <= super::MAX_FUEL_RATE);

/// The lower bound of `program_max_block_fuel`.
pub const MIN_BLOCK_FUEL: u64 = 20_000_000;
// So `program_max_tx_fuel ≤ program_max_block_fuel` holds by the bounds alone (20M ≤ 20M) …
const _: () = assert!(MAX_TX_FUEL <= MIN_BLOCK_FUEL);
// … and the largest upload (256 KiB of code and 16 KiB of interface: 19,825,488 fuel, which
// signs no `fuel_limit` and so isn't held to `program_max_tx_fuel`) fits a block at the lower
// bound (§2.2, §4.5).
const _: () = assert!(SCHEDULE_V0.upload_fuel(MAX_CODE_BYTES, MAX_INTERFACE_BYTES) <= MIN_BLOCK_FUEL);
// `program_max_block_fuel`'s range is a range.
const _: () = assert!(MIN_BLOCK_FUEL <= CALIBRATED_BLOCK_FUEL);

/// One of programs' parameters. A key is a string on chain (a `ParamChange` names it); in code it's
/// this, so a reader can't ask for one that doesn't exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Param {
    /// The most FANG programs may control (`value_held + sponsored`, §7.1), in drops.
    ValueCap,
    /// The floor under the congestion-priced fuel rate (§5.6), in milli-drops per fuel.
    FuelFloorRate,
    /// The most fuel one transaction may sign for (`fuel_limit`).
    MaxTxFuel,
    /// The most fuel a block may count (§5.4).
    MaxBlockFuel,
    /// The storage price at an empty state, in drops per byte (§4.6).
    StoragePrice,
    /// Where the storage price curve's soft cap sits (§4.6), in bytes.
    StateSoftBytes,
    /// The least notice an upgradable program's authority must give, in blocks (stored, inert
    /// until upgrades exist: A19).
    MinUpgradeDelayBlocks,
}

impl Param {
    /// Every parameter, in key order (the map's).
    pub const ALL: [Param; 7] = [Param::FuelFloorRate, Param::MaxBlockFuel, Param::MaxTxFuel, Param::MinUpgradeDelayBlocks, Param::StateSoftBytes, Param::StoragePrice, Param::ValueCap];

    pub const fn key(self) -> &'static str {
        match self {
            Param::ValueCap => "program_value_cap",
            Param::FuelFloorRate => "program_fuel_floor_rate",
            Param::MaxTxFuel => "program_max_tx_fuel",
            Param::MaxBlockFuel => "program_max_block_fuel",
            Param::StoragePrice => "program_storage_price",
            Param::StateSoftBytes => "program_state_soft_bytes",
            Param::MinUpgradeDelayBlocks => "program_min_upgrade_delay_blocks",
        }
    }

    pub fn from_key(key: &str) -> Option<Param> {
        Param::ALL.into_iter().find(|p| p.key() == key)
    }

    /// Its value until governance changes it, on a chain with these genesis constants (the
    /// minimum upgrade delay counts days in blocks).
    pub fn default(self, p: &Params) -> u64 {
        match self {
            Param::ValueCap => 10_000_000 * DROPS_PER_FANG,
            Param::FuelFloorRate => 250,
            Param::MaxTxFuel => 10_000_000,
            Param::MaxBlockFuel => 40_000_000,
            Param::StoragePrice => 100_000,
            Param::StateSoftBytes => 64 << 20,
            Param::MinUpgradeDelayBlocks => 2 * blocks_per_day(p.block_time_ms),
        }
    }

    /// `(min, max)`, inclusive, on a chain with these genesis constants.
    pub fn bounds(self, p: &Params) -> (u64, u64) {
        let day = blocks_per_day(p.block_time_ms);
        match self {
            Param::ValueCap => (0, MAX_SUPPLY),
            Param::FuelFloorRate => (1, MAX_FUEL_FLOOR_RATE),
            Param::MaxTxFuel => (1_000_000, MAX_TX_FUEL),
            Param::MaxBlockFuel => (MIN_BLOCK_FUEL, CALIBRATED_BLOCK_FUEL),
            Param::StoragePrice => (1_000, 10_000_000),
            Param::StateSoftBytes => (16 << 20, MAX_STATE_SOFT_BYTES),
            Param::MinUpgradeDelayBlocks => ((day / 24).max(1), 90 * day),
        }
    }
}

/// The keys the bootstrap council may pass on its own (`params::tokens_only` excludes them): all
/// of programs' parameters but `program_min_upgrade_delay_blocks`, which protects a program's
/// users from its authority and so is the token holders' alone (§4.5).
pub const COUNCIL_PARAMS: &[&str] = &[
    Param::FuelFloorRate.key(),
    Param::MaxBlockFuel.key(),
    Param::MaxTxFuel.key(),
    Param::StateSoftBytes.key(),
    Param::StoragePrice.key(),
    Param::ValueCap.key(),
];

/// The value of `p` in force on `state`: the map's, or for a key it doesn't hold, the default (a
/// key a later version adds needs no migration). Before programs first switch on there's no map,
/// so it's the default too (nothing reads one then but fees, at the floor's default, §5.1).
pub fn param(state: &State, p: Param) -> u64 {
    state.programs.meta.as_ref().and_then(|m| m.params.get(p.key()).copied()).unwrap_or_else(|| p.default(&state.params))
}

/// Every parameter at its default: the map `on_activate` writes when programs first switch on.
pub fn defaults(p: &Params) -> BTreeMap<String, u64> {
    Param::ALL.into_iter().map(|k| (k.key().to_string(), k.default(p))).collect()
}

/// Whether a `ParamChange` of `key` at `height` is programs': it starts with `program_`, and
/// programs aren't `Dormant` at `height` (v2 is in force there, and programs have switched on at
/// least once). Otherwise it goes to `ChainParams`, which refuses a `program_*` key as unknown, as
/// v1 does (§4.5). Asked at admission ([`admit`]) and at the proposal's own block when it's made
/// (`governance::prepare_kind`), and at the executing block when it runs (`State::exec_set_param`,
/// at `econ.height`).
pub fn is_program_param(state: &State, key: &str, height: u64) -> bool {
    key.starts_with(PREFIX) && status(state, height) != Status::Dormant
}

/// Refuses, at admission (`programs::admit`, so in a block and in the mempool alike), a
/// `ParamChange` of a `program_*` key that isn't programs' at `height` ([`is_program_param`]),
/// proposed on its own or inside a `MultisigPropose` (which holds one action, never another
/// `MultisigPropose`), in the words v1's codec refuses it with ("unknown parameter" and the key;
/// under "the proposed action" when nested). v1 refuses it in `ProposalKind::check`, statelessly;
/// v2's codec lets programs' keys through so that a block can carry one once programs exist, and
/// this keeps it unadmittable everywhere else. Either way the transaction can't be included and
/// nobody is charged, so a block carrying one is invalid on both binaries (§1.2).
pub fn admit(state: &State, action: &Action, height: u64) -> Result<()> {
    let (key, nested) = match action {
        Action::Propose { kind: ProposalKind::ParamChange { key, .. }, .. } => (key, false),
        Action::MultisigPropose { action } => match &**action {
            Action::Propose { kind: ProposalKind::ParamChange { key, .. }, .. } => (key, true),
            _ => return Ok(()),
        },
        _ => return Ok(()),
    };
    if !key.starts_with(PREFIX) || is_program_param(state, key, height) {
        return Ok(());
    }
    let unknown = anyhow!("unknown parameter `{key}`");
    // `Action::check_format` wraps a nested action's refusal so.
    Err(if nested { unknown.context("the proposed action") } else { unknown })
}

/// A map of programs' parameters is valid: only known keys, each within its bounds, and
/// `program_max_tx_fuel ≤ program_max_block_fuel` (which the bounds already make true). Keys it
/// doesn't hold are at their defaults, which are within bounds.
pub fn check_map(params: &BTreeMap<String, u64>, p: &Params) -> Result<()> {
    for (key, value) in params {
        let k = Param::from_key(key).ok_or_else(|| anyhow!("unknown parameter `{key}` (known: {})", Param::ALL.map(Param::key).join(", ")))?;
        let (min, max) = k.bounds(p);
        if !(min..=max).contains(value) {
            bail!("parameter `{key}` must be {min}–{max}, got {value}");
        }
    }
    let get = |k: Param| params.get(k.key()).copied().unwrap_or_else(|| k.default(p));
    ensure!(get(Param::MaxTxFuel) <= get(Param::MaxBlockFuel), "program_max_tx_fuel can't exceed program_max_block_fuel");
    Ok(())
}

/// Whether `key` may be set to `value` (a `ParamChange` routed here by [`is_program_param`]): the
/// map with the change must pass [`check_map`]. Programs exist (the routing says so).
pub fn check_param(state: &State, key: &str, value: u64) -> Result<()> {
    let m = state.programs.meta.as_ref().ok_or_else(|| anyhow!("programs have never been switched on"))?;
    let mut next = m.params.clone();
    next.insert(key.to_string(), value);
    check_map(&next, &state.params)
}

/// Sets `key` to `value` ([`check_param`] first; on error nothing changes), journaled. It applies
/// from the block after the one it runs in, like every `ParamChange`.
pub fn set_param(state: &mut State, j: &mut Journal, key: &str, value: u64) -> Result<()> {
    check_param(state, key, value)?;
    let m = super::state::meta_mut(state, j).expect("check_param found META");
    m.params.insert(key.to_string(), value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{ChainParams, COUNCIL_KEYS};

    /// Genesis constants with mainnet's 1 s blocks.
    fn one_second() -> Params {
        Params { block_time_ms: 1_000, ..Params::default() }
    }

    /// The table of §4.5 (frozen with v2's other program rules, §9.2), at mainnet's 1 s blocks.
    #[test]
    fn the_parameters_of_section_4_5() {
        let p = one_second();
        let table: Vec<(&str, u64, (u64, u64))> = Param::ALL.into_iter().map(|k| (k.key(), k.default(&p), k.bounds(&p))).collect();
        const MIB: u64 = 1 << 20;
        assert_eq!(
            table,
            vec![
                ("program_fuel_floor_rate", 250, (1, 1_000_000)),
                ("program_max_block_fuel", 40_000_000, (20_000_000, 60_000_000)),
                ("program_max_tx_fuel", 10_000_000, (1_000_000, 20_000_000)),
                ("program_min_upgrade_delay_blocks", 172_800, (3_600, 7_776_000)),
                ("program_state_soft_bytes", 64 * MIB, (16 * MIB, 512 * MIB)),
                ("program_storage_price", 100_000, (1_000, 10_000_000)),
                ("program_value_cap", 10_000_000 * DROPS_PER_FANG, (0, 1_000_000_000 * DROPS_PER_FANG)),
            ]
        );
        assert_eq!(MAX_SUPPLY, blockula_vm::abi::MAX_SUPPLY, "the VM's amounts and the cap's bound agree");
        // `ALL` is in key order (the map's), and every key has the prefix and round-trips.
        assert!(Param::ALL.windows(2).all(|w| w[0].key() < w[1].key()));
        for k in Param::ALL {
            assert!(k.key().starts_with(PREFIX) && Param::from_key(k.key()) == Some(k));
            let (min, max) = k.bounds(&p);
            assert!(min <= k.default(&p) && k.default(&p) <= max, "{k:?}'s default is within its bounds");
        }
        assert_eq!(Param::from_key("program_max_code_bytes"), None, "PRG's ninth and tenth keys don't exist in v0 (A17)");
        check_map(&defaults(&p), &p).unwrap();
        // Days count in blocks at the chain's block time: 2 s blocks halve them.
        let slow = Params { block_time_ms: 2_000, ..one_second() };
        assert_eq!((Param::MinUpgradeDelayBlocks.default(&slow), Param::MinUpgradeDelayBlocks.bounds(&slow)), (86_400, (1_800, 3_888_000)));
    }

    /// Every key but the minimum upgrade delay is the council's too; the delay is the token
    /// holders' alone. No `program_*` key is one of `ChainParams`', so routing them never shadows
    /// one.
    #[test]
    fn params_keep_to_their_prefix_and_classes() {
        let council: Vec<&str> = Param::ALL.into_iter().filter(|k| *k != Param::MinUpgradeDelayBlocks).map(Param::key).collect();
        assert_eq!(COUNCIL_PARAMS, council.as_slice());
        for k in Param::ALL {
            assert_eq!(crate::params::tokens_only(k.key()), k == Param::MinUpgradeDelayBlocks, "{k:?}");
        }
        for k in ChainParams::KEYS {
            assert!(!k.starts_with(PREFIX), "{k}");
            assert_eq!(crate::params::tokens_only(k), !COUNCIL_KEYS.contains(k), "{k}: unchanged for ChainParams' keys");
        }
        assert!(crate::params::tokens_only("program_a_key_added_later"), "a key is the token holders' until it's listed");
    }

    /// `check_map` refuses unknown keys, values out of bounds (at each bound and one past), and a
    /// transaction limit above the block's.
    #[test]
    fn a_map_holds_known_keys_within_their_bounds() {
        let p = one_second();
        let with = |key: &str, value: u64| {
            let mut m = defaults(&p);
            m.insert(key.to_string(), value);
            check_map(&m, &p)
        };
        for k in Param::ALL {
            let (min, max) = k.bounds(&p);
            with(k.key(), min).unwrap();
            with(k.key(), max).unwrap();
            assert_eq!(with(k.key(), max + 1).unwrap_err().to_string(), format!("parameter `{}` must be {min}–{max}, got {}", k.key(), max + 1));
            if min > 0 {
                assert!(with(k.key(), min - 1).is_err(), "{k:?}");
            }
        }
        assert!(with("program_warp", 1).unwrap_err().to_string().starts_with("unknown parameter `program_warp` (known: program_fuel_floor_rate, "));
        // The bounds make the cross rule hold (`MAX_TX_FUEL` ≤ `MIN_BLOCK_FUEL`, asserted at
        // compile time): at its closest, the transaction limit at its bound equals the block's.
        with(Param::MaxTxFuel.key(), MAX_TX_FUEL).unwrap();
        let mut m = defaults(&p);
        m.insert(Param::MaxBlockFuel.key().to_string(), MIN_BLOCK_FUEL);
        m.insert(Param::MaxTxFuel.key().to_string(), MAX_TX_FUEL);
        check_map(&m, &p).unwrap();
        m.insert(Param::MaxTxFuel.key().to_string(), MAX_TX_FUEL + 1);
        assert!(check_map(&m, &p).unwrap_err().to_string().contains("must be"), "a limit past the block's is past its own bound first");
        // A map that holds fewer keys is checked against the defaults of the rest.
        check_map(&BTreeMap::new(), &p).unwrap();
    }
}
