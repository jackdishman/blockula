//! Governance-adjustable chain parameters (MAINNET-DESIGN 3.4–3.9).
//!
//! [`ChainParams`] holds every parameter a `ParamChange` proposal may change: staking, fees,
//! issuance, governance itself, names, the beacon and Ghost Mode. Each has a stable string key
//! (its field name, used by genesis `params` and by proposals), a default (durations derived from
//! the block time), and code bounds that [`ChainParams::set`] enforces together with a few
//! cross-parameter rules ([`ChainParams::check`]). Genesis constants that never change (block
//! time, epoch length, address prefix) stay in [`Params`].
//!
//! The record is one META scalar (`chain_params`): the fields borsh-encoded in declaration order.
//! Adding, removing or reordering a parameter changes the state encoding (bump `SCHEMA_VERSION`);
//! a key is never renamed once a chain uses it.
//!
//! Bounds keep the chain working (no zero windows, no overflow, the issuance floors of 3.5); they
//! don't replace voting. Dev chains (`blockula dev`) override a few durations
//! ([`ChainParams::dev_overrides`]) so unbonding, voting and timelocks take minutes, not weeks.

use crate::chain::Params;
use crate::types::{DROPS_PER_FANG, MAX_TX_BYTES};
use anyhow::{anyhow, bail, ensure, Result};
use borsh::{BorshDeserialize, BorshSerialize};
use std::collections::BTreeMap;

const FANG: u64 = DROPS_PER_FANG;
const KIB: u64 = 1 << 10;
const MIB: u64 = 1 << 20;
/// The CBlock codec's bound on a block's txs (`max_block_bytes` can't exceed it without a fork).
pub const CODEC_MAX_BLOCK_BYTES: u64 = 4 * MIB;
/// The supply at genesis (DEC-5): the largest amount any FANG parameter may name.
pub const MAX_SUPPLY: u64 = 1_000_000_000 * FANG;

/// Declares the parameters: `name: default, [min, max];`. Defaults and bounds may use `day`
/// (blocks per day at the chain's block time) and `epoch` (blocks per epoch).
macro_rules! chain_params {
    (|$day:ident, $epoch:ident| $( $(#[doc = $doc:literal])* $name:ident: $default:expr, [$min:expr, $max:expr]; )*) => {
        /// Every governance-adjustable parameter (see the module docs). Amounts are in drops,
        /// durations and heights in blocks.
        #[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
        pub struct ChainParams {
            $( $(#[doc = $doc])* pub $name: u64, )*
        }

        impl ChainParams {
            /// Every key, in declaration (and encoding) order.
            pub const KEYS: &'static [&'static str] = &[$(stringify!($name)),*];

            #[allow(unused_variables)]
            fn defaults_for($day: u64, $epoch: u64) -> Self {
                Self { $($name: $default),* }
            }

            #[allow(unused_variables)]
            fn bounds_for(key: &str, $day: u64, $epoch: u64) -> Option<(u64, u64)> {
                match key {
                    $(stringify!($name) => Some(($min, $max)),)*
                    _ => None,
                }
            }

            /// The value of `key`, if it's a parameter.
            pub fn get(&self, key: &str) -> Option<u64> {
                match key {
                    $(stringify!($name) => Some(self.$name),)*
                    _ => None,
                }
            }

            fn slot(&mut self, key: &str) -> Option<&mut u64> {
                match key {
                    $(stringify!($name) => Some(&mut self.$name),)*
                    _ => None,
                }
            }
        }
    };
}

chain_params! { |day, epoch|
    // ---- staking (3.4) ----
    /// Least self-bond a validator candidate needs (`RegisterValidator`, and to stay eligible).
    min_self_bond: 10_000 * FANG, [FANG, 100_000_000 * FANG];
    /// Active-set cap (DEC-3: 3f+1 = 31 until certificates shrink). At most the p2p layer's peer
    /// set (`consensus::MAX_PEERS_PER_SET`, 64): the active set is the primary peer set, and
    /// candidates only get the slots it leaves (consensus.rs caps them), so primary + candidate
    /// peers can never exceed it (the p2p layer panics past it).
    max_validators: 31, [1, crate::consensus::MAX_PEERS_PER_SET as u64];
    /// At least one epoch, so evidence for the current epoch can always land.
    unbonding_blocks: (21 * day).max(epoch), [10, 365 * day];
    slash_double_sign_bps: 500, [0, 10_000];
    slash_downtime_bps: 10, [0, 1_000];
    /// Leader slots a validator's liveness is judged over …
    downtime_window: 100, [1, 100_000];
    /// … and how many of them it may miss before it's jailed.
    downtime_max_missed: 50, [0, 100_000];
    jail_blocks: epoch, [1, 365 * day];
    /// The reporter's share of an equivocation slash (the rest is burned).
    reporter_reward_bps: 1_000, [0, 10_000];
    commission_max_bps: 10_000, [0, 10_000];
    /// Most a validator's commission may move per epoch.
    commission_max_change_bps: 100, [0, 10_000];
    /// Most an account with vesting locks may delegate to one pool, in bps of the supply (3.7).
    insider_delegation_cap_bps: 100, [0, 10_000];
    /// Most commission the bootstrap delegation's stake pays a validator (DEC-5: its rewards are
    /// the treasury's): a validator charging more keeps this much of the bootstrap's part and the
    /// rest goes to the treasury (audit F12, review-fix-gov-1). Raising it pays the validators.
    bootstrap_commission_cap_bps: 1_000, [0, 10_000];
    /// Oldest equivocation evidence accepted; at most `unbonding_blocks` of epochs.
    evidence_max_age_epochs: ((21 * day).max(epoch) / epoch).max(1), [1, 365 * day];

    // ---- fees (3.6) ----
    /// The base fee's floor, per tx and per byte (today's `min_fee`).
    base_fee_floor_flat: 10_000, [0, FANG];
    base_fee_floor_per_byte: 10, [1, 1_000_000];
    target_fullness_bps: 5_000, [100, 10_000];
    base_fee_max_change_bps: 1_250, [0, 10_000];
    /// Most tx bytes per block (DEC-12: 2 MiB; the codec bound is 4 MiB).
    max_block_bytes: 2 * MIB, [MAX_TX_BYTES as u64, CODEC_MAX_BLOCK_BYTES];
    /// Burned when a tx creates an account (DEC-6).
    account_creation_fee: FANG / 10, [0, 100 * FANG];
    /// How far ahead a tx's `valid_until` may be (an hour of blocks).
    max_tx_validity_blocks: day / 24, [10, day];
    /// Most a tx may tip, as a multiple of its base fee in the block that includes it: enough to
    /// bid for priority, never enough for one signature to move a balance to the proposer.
    tip_cap_multiple: 20, [1, 1_000];

    // ---- issuance (3.6): rate in ppm of the supply per year ----
    issuance_initial_ppm: 60_000, [5_000, 200_000];
    /// Yearly decay of the rate, in ppm of the rate.
    issuance_decay_ppm: 150_000, [0, 1_000_000];
    /// The rate never decays below this (3.5: at least 0.5%).
    issuance_floor_ppm: 15_000, [5_000, 200_000];
    /// Share of issuance to validators' pools (3.5: at least 50%) …
    issuance_stakers_bps: 7_000, [5_000, 10_000];
    /// … and to the contribution pool; the treasury gets the rest.
    issuance_pool_bps: 2_000, [0, 5_000];

    // ---- governance (3.5) ----
    /// Bonded by `Propose`: refunded once the proposal reaches quorum, burned otherwise.
    proposal_deposit: 1_000 * FANG, [FANG, 1_000_000 * FANG];
    voting_blocks: 7 * day, [10, 90 * day];
    timelock_blocks: 2 * day, [1, 90 * day];
    /// Quorum, in bps of the unlocked supply …
    quorum_bps: 400, [1, 10_000];
    /// … and never less than this many drops of voting weight.
    quorum_floor: 20_000_000 * FANG, [0, MAX_SUPPLY];
    /// Share of the vote upgrades, issuance changes and large spends need.
    supermajority_bps: 6_667, [5_001, 10_000];
    /// Treasury spends above this share of the treasury need the supermajority.
    treasury_large_bps: 100, [1, 10_000];
    /// One conviction lock period (votes lock for 1, 2, 4 … 32 of them).
    lock_period_blocks: 7 * day, [1, 90 * day];
    /// Spending proposals (treasury, distributions) are rejected before this height (≈ 6 months).
    governance_spend_height: 182 * day, [0, 3_650 * day];
    /// Until this height ⅔ of the active validators can pass a proposal (the bootstrap council).
    bootstrap_end_height: 365 * day, [0, 3_650 * day];
    /// How long a multisig proposal (`MultisigPropose`) waits for its approvals.
    multisig_expiry_blocks: 7 * day, [10, 90 * day];

    // ---- names (DEC-11) ----
    /// Registration price by name length: 3 characters, 4, and 5 or more.
    name_price_3: 100 * FANG, [FANG / 100, 1_000_000 * FANG];
    name_price_4: 25 * FANG, [FANG / 100, 1_000_000 * FANG];
    name_price_5: 5 * FANG, [FANG / 100, 1_000_000 * FANG];
    /// A year's renewal, in bps of the registration price.
    name_renewal_bps: 2_000, [0, 10_000];
    name_registration_blocks: 365 * day, [10, 3_650 * day];
    name_grace_blocks: 30 * day, [0, 365 * day];

    // ---- randomness beacon (3.8) ----
    /// Slash for committing to the beacon and not revealing.
    beacon_miss_slash_bps: 10, [0, 1_000];

    // ---- Ghost Mode (section 6) ----
    /// Most FANG the shielded pool may hold (0 while Ghost Mode is off).
    ghost_pool_cap: 0, [0, MAX_SUPPLY];
    /// Largest ghost tx (tags 50–52; the codec bound is `types::MAX_GHOST_TX_BYTES`) …
    ghost_max_tx_bytes: 256 * KIB, [0, MIB];
    /// … and most ghost tx bytes per block (bounds proof verification per block).
    ghost_max_block_bytes: MIB, [0, CODEC_MAX_BLOCK_BYTES];
    /// A spend's anchor stays usable this many blocks after a newer root replaced it.
    ghost_anchor_window: 1_024, [1, 30 * day];
    /// Flat fee on top of the byte fee for verifying a ghost tx's proof, burned.
    ghost_verify_fee: FANG / 1_000, [0, FANG];
}

/// Governance's own safety parameters (audit F15, F16). A `ParamChange` of one needs the
/// supermajority, can't pass by the bootstrap council, and stays within a band in code
/// ([`ChainParams::check_governance_change`]), so a vote can't make the next vote easier to win
/// than the design allows (the spend gate never opens earlier, the bootstrap council never
/// outlives its end, and voting, the timelock, quorum and the supermajority keep their floors),
/// nor make it impossible to win (the supermajority and quorum keep ceilings, and the spend gate
/// moves at most a year out; review-fix-gov-3), so the next vote can always undo it.
pub const GOVERNANCE_KEYS: &[&str] = &["voting_blocks", "timelock_blocks", "lock_period_blocks", "quorum_bps", "quorum_floor", "supermajority_bps", "treasury_large_bps", "governance_spend_height", "bootstrap_end_height"];

/// The validators' own economics (review-integrate-merges-1): what they're paid (issuance and
/// its split, the tip cap (audit F01: a higher one lets one signature hand its block's proposer
/// more), the commission bounds), what being one takes (the self-bond, the cap on insiders'
/// delegations to one pool) and how they're penalized (slashes, jail, downtime, the evidence
/// window, the reporter's share, beacon misses), plus the most commission the bootstrap
/// delegation's stake pays them (`bootstrap_commission_cap_bps`, audit F12: a higher one hands
/// the operators more of the treasury's bootstrap rewards). The bootstrap council is the
/// validators, so like [`GOVERNANCE_KEYS`] a change needs the token supermajority and never
/// passes by the council; the code bounds still apply.
pub const VALIDATOR_KEYS: &[&str] = &[
    "min_self_bond",
    "slash_double_sign_bps",
    "slash_downtime_bps",
    "downtime_window",
    "downtime_max_missed",
    "jail_blocks",
    "reporter_reward_bps",
    "commission_max_bps",
    "commission_max_change_bps",
    "insider_delegation_cap_bps",
    "bootstrap_commission_cap_bps",
    "evidence_max_age_epochs",
    "tip_cap_multiple",
    "issuance_initial_ppm",
    "issuance_decay_ppm",
    "issuance_floor_ppm",
    "issuance_stakers_bps",
    "issuance_pool_bps",
    "beacon_miss_slash_bps",
];

/// The parameters the bootstrap council may also change, by its ⅔ (simple-majority keys for
/// token votes): the active set's size, unbonding (it can't fall below the evidence window,
/// which is the token holders'), fees, deposits and periods outside governance's own, names and
/// Ghost Mode's limits. Every other key, including one added later and not listed here, is the
/// token holders' alone ([`tokens_only`]).
pub const COUNCIL_KEYS: &[&str] = &[
    "max_validators",
    "unbonding_blocks",
    "base_fee_floor_flat",
    "base_fee_floor_per_byte",
    "target_fullness_bps",
    "base_fee_max_change_bps",
    "max_block_bytes",
    "account_creation_fee",
    "max_tx_validity_blocks",
    "proposal_deposit",
    "multisig_expiry_blocks",
    "name_price_3",
    "name_price_4",
    "name_price_5",
    "name_renewal_bps",
    "name_registration_blocks",
    "name_grace_blocks",
    "ghost_pool_cap",
    "ghost_max_tx_bytes",
    "ghost_max_block_bytes",
    "ghost_anchor_window",
    "ghost_verify_fee",
];

/// Whether a `ParamChange` of `key` is the token holders' alone: it needs the supermajority and
/// the bootstrap council can't pass it. That's every key but [`COUNCIL_KEYS`]: governance's own
/// ([`GOVERNANCE_KEYS`]) and the validators' ([`VALIDATOR_KEYS`]).
pub fn tokens_only(key: &str) -> bool {
    !COUNCIL_KEYS.contains(&key)
}

/// How far past the block that changes it `governance_spend_height` may move (a year): a vote can
/// close spending again, but not for longer than that, so it reopens by itself (review-fix-gov-3).
pub const SPEND_GATE_MAX_DELAY_DAYS: u64 = 365;

/// The shortest evidence window (`evidence_max_age_epochs` × `epoch_blocks`, and so
/// `unbonding_blocks`, which can't be shorter) governance can set: two weeks, twice the weekly
/// checkpoint cadence (OPERATIONS.md "Weak subjectivity"). The weak-subjectivity period is half
/// the window, so it never falls below a week: a joiner's newest weekly checkpoint stays usable
/// whatever a vote does, and a peer claiming a shorter period lies (audit gap-6-1).
pub const EVIDENCE_WINDOW_FLOOR_DAYS: u64 = 14;

/// [`EVIDENCE_WINDOW_FLOOR_DAYS`] in epochs on a chain with these genesis constants (rounded up,
/// so the window is at least that long).
pub fn evidence_floor_epochs(p: &Params) -> u64 {
    EVIDENCE_WINDOW_FLOOR_DAYS.saturating_mul(blocks_per_day(p.block_time_ms)).div_ceil(p.epoch_blocks.max(1)).max(1)
}

/// Highest `Vote::conviction`.
pub const MAX_CONVICTION: u8 = 6;
/// Lock periods (`lock_period_blocks`) a vote at each conviction locks its amount for: none at 0,
/// then 1, 2, 4, 8, 16 and 32 …
pub const CONVICTION_PERIODS: [u64; 7] = [0, 1, 2, 4, 8, 16, 32];
/// … and its weight, in tenths of the amount: 0.1× with no lock, then 1×–6×.
pub const CONVICTION_TENTHS: [u64; 7] = [1, 10, 20, 30, 40, 50, 60];

/// A vote's weight: `amount` × the conviction multiplier (`None` past [`MAX_CONVICTION`]).
#[cfg_attr(not(test), allow(dead_code))] // governance.rs (P-GOV)
pub fn vote_weight(amount: u64, conviction: u8) -> Option<u128> {
    CONVICTION_TENTHS.get(conviction as usize).map(|t| amount as u128 * *t as u128 / 10)
}

/// Blocks per day at a block time (at least 1).
pub fn blocks_per_day(block_time_ms: u64) -> u64 {
    (86_400_000 / block_time_ms.max(1)).max(1)
}

/// Mainnet's shape (1 s blocks, 3,600-block epochs). Only `State::default()` uses it (tests, and
/// a state about to load its records); chains derive theirs from genesis.
impl Default for ChainParams {
    fn default() -> Self {
        Self::defaults_for(86_400, 3_600)
    }
}

impl ChainParams {
    /// The defaults for a chain with these genesis constants.
    pub fn defaults(p: &Params) -> Self {
        Self::defaults_for(blocks_per_day(p.block_time_ms), p.epoch_blocks.max(1))
    }

    /// `(min, max)` for `key` on a chain with these genesis constants.
    pub fn bounds(key: &str, p: &Params) -> Option<(u64, u64)> {
        Self::bounds_for(key, blocks_per_day(p.block_time_ms), p.epoch_blocks.max(1))
    }

    /// Defaults, then genesis overrides. Unknown keys, out-of-bounds values and broken
    /// cross-parameter rules are errors. The rules are checked once all overrides are in, so
    /// their order doesn't matter.
    pub fn with_overrides(p: &Params, overrides: &BTreeMap<String, u64>) -> Result<Self> {
        let mut c = Self::defaults(p);
        for (k, v) in overrides {
            *c.slot(k).ok_or_else(|| anyhow!("unknown parameter `{k}` (known: {})", Self::KEYS.join(", ")))? = *v;
        }
        c.check(p)?;
        Ok(c)
    }

    /// Changes one parameter (a `ParamChange` proposal): within its bounds, and only if the
    /// result still passes [`ChainParams::check`]. On error nothing changes.
    pub fn set(&mut self, key: &str, value: u64, p: &Params) -> Result<()> {
        let mut next = self.clone();
        *next.slot(key).ok_or_else(|| anyhow!("unknown parameter `{key}`"))? = value;
        next.check(p)?;
        *self = next;
        Ok(())
    }

    /// Every value within its bounds, and the rules that tie parameters together.
    pub fn check(&self, p: &Params) -> Result<()> {
        for key in Self::KEYS {
            let (min, max) = Self::bounds(key, p).expect("every key has bounds");
            let v = self.get(key).expect("every key has a value");
            if !(min..=max).contains(&v) {
                bail!("parameter `{key}` must be {min}–{max}, got {v}");
            }
        }
        ensure!(self.downtime_max_missed <= self.downtime_window, "downtime_max_missed can't exceed downtime_window");
        ensure!(self.issuance_floor_ppm <= self.issuance_initial_ppm, "issuance_floor_ppm can't exceed issuance_initial_ppm");
        ensure!(self.issuance_stakers_bps + self.issuance_pool_bps <= 10_000, "issuance_stakers_bps + issuance_pool_bps can't exceed 10000");
        // Evidence must arrive while the offender's stake can still be slashed.
        ensure!(self.evidence_max_age_epochs.saturating_mul(p.epoch_blocks.max(1)) <= self.unbonding_blocks, "evidence_max_age_epochs × epoch_blocks can't exceed unbonding_blocks");
        ensure!(self.ghost_max_tx_bytes <= self.ghost_max_block_bytes, "ghost_max_tx_bytes can't exceed ghost_max_block_bytes");
        Ok(())
    }

    /// The band governance keeps a safety parameter in, `(floor, ceiling)` with what each means:
    /// voting at least 3 days, the timelock and the lock period at least a day, quorum 1–10% of
    /// the unlocked supply and 5M–50M FANG (2.5× the defaults at most, so it stays reachable), the
    /// supermajority 60–80% (so a dust nay can't veto every upgrade, review-fix-gov-3), the
    /// large-spend line at most 5% of the treasury, and the evidence window at least
    /// [`EVIDENCE_WINDOW_FLOOR_DAYS`] (what joiners' weak-subjectivity period rests on, gap-6-1).
    /// `None` for keys without one.
    pub fn governance_band(key: &str, p: &Params) -> Option<((u64, &'static str), (u64, &'static str))> {
        let day = blocks_per_day(p.block_time_ms);
        let (min, max) = Self::bounds(key, p)?;
        Some(match key {
            "voting_blocks" => ((3 * day, "3 days"), (max, "the code bound")),
            "timelock_blocks" | "lock_period_blocks" => ((day, "a day"), (max, "the code bound")),
            "quorum_bps" => ((100, "1%"), (1_000, "10% of the unlocked supply")),
            "quorum_floor" => ((5_000_000 * FANG, "5M FANG"), (50_000_000 * FANG, "50M FANG")),
            "supermajority_bps" => ((6_000, "60%"), (8_000, "80%")),
            "treasury_large_bps" => ((min, "the code bound"), (500, "5% of the treasury")),
            "evidence_max_age_epochs" => ((evidence_floor_epochs(p), "14 days of epochs, the weak-subjectivity floor"), (max, "the code bound")),
            _ => return None,
        })
    }

    /// The rule a `ParamChange` of `key` to `value` at `height` follows on top of the bounds, for
    /// [`GOVERNANCE_KEYS`] and the evidence window: within [`ChainParams::governance_band`], or
    /// (on a chain whose genesis starts outside it) no further outside than it is. So a vote can
    /// neither make the next vote easier to win than the floors allow nor impossible to win, the
    /// next vote can always move it back, and the evidence window (with `unbonding_blocks`, which
    /// can't be shorter) never falls below [`EVIDENCE_WINDOW_FLOOR_DAYS`]. The spend gate never
    /// moves earlier, and later by at most [`SPEND_GATE_MAX_DELAY_DAYS`] past `height` (or where
    /// it already is); the bootstrap council's end never moves later.
    pub fn check_governance_change(&self, key: &str, value: u64, p: &Params, height: u64) -> Result<()> {
        let Some(old) = self.get(key) else { return Ok(()) };
        if let Some(((floor, low), (ceiling, high))) = Self::governance_band(key, p) {
            ensure!(value >= old.min(floor), "governance can lower {key} to {floor} at the least ({low}); it's {old}");
            ensure!(value <= old.max(ceiling), "governance can raise {key} to {ceiling} ({high}) at the most; it's {old}");
            return Ok(());
        }
        match key {
            "governance_spend_height" => {
                ensure!(value >= old, "the spend gate never moves earlier: governance_spend_height is {old}");
                let latest = height.saturating_add(SPEND_GATE_MAX_DELAY_DAYS.saturating_mul(blocks_per_day(p.block_time_ms))).max(old);
                ensure!(value <= latest, "governance can move the spend gate to block {latest} at the latest ({SPEND_GATE_MAX_DELAY_DAYS} days past block {height}); it's {old}");
                Ok(())
            }
            "bootstrap_end_height" => {
                ensure!(value <= old, "the bootstrap council never lasts longer: bootstrap_end_height is {old}");
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Whether these values already sit inside every band [`ChainParams::check_governance_change`]
    /// keeps (the evidence window's too), with the spend gate at most
    /// [`SPEND_GATE_MAX_DELAY_DAYS`] out (mainnet's genesis must).
    pub fn check_governance_floors(&self, p: &Params) -> Result<()> {
        for key in Self::KEYS {
            let v = self.get(key).expect("a parameter");
            if let Some(((floor, _), (ceiling, _))) = Self::governance_band(key, p) {
                ensure!(v >= floor, "parameter `{key}` ({v}) is past governance's floor ({floor})");
                ensure!(v <= ceiling, "parameter `{key}` ({v}) is past governance's ceiling ({ceiling})");
            }
        }
        let latest = SPEND_GATE_MAX_DELAY_DAYS.saturating_mul(blocks_per_day(p.block_time_ms));
        ensure!(self.governance_spend_height <= latest, "parameter `governance_spend_height` ({}) is past governance's ceiling ({latest})", self.governance_spend_height);
        Ok(())
    }

    /// Every parameter by key (for the API: amounts can exceed 2^53, so render them as strings).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn to_map(&self) -> BTreeMap<&'static str, u64> {
        Self::KEYS.iter().map(|k| (*k, self.get(k).expect("every key has a value"))).collect()
    }

    /// Short durations for `blockula dev` (written into its genesis `params`, like
    /// `PocTimes::dev`): unbonding, jail, voting, timelock and conviction periods in minutes,
    /// spending open from the start, and a deposit one faucet drip covers. Names, fees and
    /// issuance keep their real values. `blockula summon` (testnets) keeps the real durations.
    pub fn dev_overrides(block_time_ms: u64, epoch_blocks: u64) -> BTreeMap<String, u64> {
        let minute = (60_000 / block_time_ms.max(1)).max(1);
        let unbonding = (2 * minute).max(epoch_blocks.max(1)).max(10);
        [
            ("unbonding_blocks", unbonding),
            ("evidence_max_age_epochs", (unbonding / epoch_blocks.max(1)).max(1)),
            ("jail_blocks", minute),
            ("voting_blocks", minute.max(10)),
            ("timelock_blocks", (minute / 3).max(1)),
            ("lock_period_blocks", (minute / 3).max(1)),
            ("governance_spend_height", 0),
            ("proposal_deposit", 10 * FANG),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
    }

    /// How long a vote at `conviction` locks its amount (`None` past [`MAX_CONVICTION`]).
    #[cfg_attr(not(test), allow(dead_code))] // governance.rs (P-GOV)
    pub fn vote_lock_blocks(&self, conviction: u8) -> Option<u64> {
        CONVICTION_PERIODS.get(conviction as usize).map(|p| p.saturating_mul(self.lock_period_blocks))
    }

    /// What registering a name of `len` characters costs.
    pub fn name_price(&self, len: usize) -> u64 {
        match len {
            ..=3 => self.name_price_3,
            4 => self.name_price_4,
            _ => self.name_price_5,
        }
    }

    /// What renewing a name of `len` characters for `years` costs.
    pub fn name_renewal_price(&self, len: usize, years: u64) -> u64 {
        let per_year = self.name_price(len) as u128 * self.name_renewal_bps as u128 / 10_000;
        (per_year * years as u128).min(u64::MAX as u128) as u64
    }
}

/// `params: {key: value}` in genesis.json: values as strings (like every amount) or numbers.
pub mod u64_map {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::collections::BTreeMap;

    pub fn serialize<S: Serializer>(m: &BTreeMap<String, u64>, s: S) -> Result<S::Ok, S::Error> {
        s.collect_map(m.iter().map(|(k, v)| (k, v.to_string())))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<String, u64>, D::Error> {
        #[derive(Deserialize)]
        struct V(#[serde(with = "crate::types::u64_str")] u64);
        Ok(BTreeMap::<String, V>::deserialize(d)?.into_iter().map(|(k, V(v))| (k, v)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> Params {
        Params { block_time_ms: 1000, epoch_blocks: 3600, ..Default::default() }
    }

    #[test]
    fn defaults_are_in_bounds_and_derive_from_the_block_time() {
        let p = params();
        let c = ChainParams::defaults(&p);
        c.check(&p).unwrap();
        assert_eq!((c.unbonding_blocks, c.voting_blocks, c.max_tx_validity_blocks, c.jail_blocks), (21 * 86_400, 7 * 86_400, 3_600, 3_600));
        assert_eq!(c.evidence_max_age_epochs, 21 * 24);
        assert_eq!((c.quorum_floor, c.max_validators, c.max_block_bytes), (20_000_000 * FANG, 31, 2 * MIB));
        // Faster blocks, more blocks per duration; the defaults hold for any block time.
        let fast = Params { block_time_ms: 250, ..p.clone() };
        let f = ChainParams::defaults(&fast);
        f.check(&fast).unwrap();
        assert_eq!(f.unbonding_blocks, 4 * c.unbonding_blocks);
        for bt in [1, 100, 1000, 60_000] {
            for epoch in [2, 10, 600, 3600] {
                let p = Params { block_time_ms: bt, epoch_blocks: epoch, ..Default::default() };
                ChainParams::defaults(&p).check(&p).unwrap_or_else(|e| panic!("block time {bt} ms, epoch {epoch}: {e}"));
                let dev = ChainParams::with_overrides(&p, &ChainParams::dev_overrides(bt, epoch)).unwrap_or_else(|e| panic!("dev, block time {bt} ms, epoch {epoch}: {e}"));
                assert!(dev.voting_blocks <= ChainParams::defaults(&p).voting_blocks);
            }
        }
    }

    #[test]
    fn keys_get_and_set() {
        let p = params();
        let mut c = ChainParams::defaults(&p);
        assert_eq!(ChainParams::KEYS.len(), c.to_map().len());
        assert_eq!(ChainParams::KEYS.len(), borsh::to_vec(&c).unwrap().len() / 8, "one u64 per key");
        assert_eq!(c.get("max_validators"), Some(31));
        assert_eq!(c.get("nope"), None);
        c.set("max_validators", 64, &p).unwrap();
        assert_eq!(c.max_validators, 64);
        // Bounds, unknown keys and cross-parameter rules; a refused change leaves everything as it was.
        let before = c.clone();
        assert!(c.set("max_validators", 0, &p).unwrap_err().to_string().contains("1–64"));
        assert!(c.set("max_validators", 65, &p).is_err(), "never more than the p2p peer set holds");
        assert!(c.set("issuance_floor_ppm", 4_999, &p).is_err(), "the issuance floor can't go below 0.5%");
        assert!(c.set("issuance_stakers_bps", 4_999, &p).is_err(), "stakers keep at least half");
        assert!(c.set("max_block_bytes", CODEC_MAX_BLOCK_BYTES + 1, &p).is_err());
        assert!(c.set("bogus", 1, &p).unwrap_err().to_string().contains("unknown parameter"));
        assert!(c.set("downtime_max_missed", 101, &p).unwrap_err().to_string().contains("downtime_window"));
        assert!(c.set("issuance_pool_bps", 3_001, &p).unwrap_err().to_string().contains("can't exceed 10000"));
        assert!(c.set("unbonding_blocks", 86_400, &p).unwrap_err().to_string().contains("evidence_max_age_epochs"));
        assert_eq!(c, before);
        // Genesis overrides apply together, so their order doesn't matter.
        let o = BTreeMap::from([("downtime_max_missed".to_string(), 200), ("downtime_window".to_string(), 500)]);
        let g = ChainParams::with_overrides(&p, &o).unwrap();
        assert_eq!((g.downtime_max_missed, g.downtime_window), (200, 500));
        assert!(ChainParams::with_overrides(&p, &BTreeMap::from([("nope".to_string(), 1)])).is_err());
        // Name prices.
        assert_eq!((c.name_price(3), c.name_price(4), c.name_price(9)), (100 * FANG, 25 * FANG, 5 * FANG));
        assert_eq!(c.name_renewal_price(5, 2), 2 * FANG);
    }

    /// review-fix-gov-3: governance's safety parameters move within a band, both ways, so no vote
    /// leaves one where the next vote can't move it back; a chain that starts outside the band
    /// only moves toward it.
    #[test]
    fn safety_parameters_move_within_their_bands() {
        let p = params();
        let c = ChainParams::defaults(&p);
        let ok = |c: &ChainParams, k: &str, v: u64| c.check_governance_change(k, v, &p, 0).is_ok();
        assert!(ok(&c, "supermajority_bps", 8_000) && !ok(&c, "supermajority_bps", 8_001) && ok(&c, "supermajority_bps", 6_000) && !ok(&c, "supermajority_bps", 5_999));
        let strict = ChainParams { supermajority_bps: 8_000, quorum_bps: 1_000, quorum_floor: 50_000_000 * FANG, ..c.clone() };
        assert!(ok(&strict, "supermajority_bps", 6_667) && ok(&strict, "quorum_bps", 400) && ok(&strict, "quorum_floor", 20_000_000 * FANG), "back from the ceiling");
        assert!(!ok(&c, "quorum_bps", 1_001) && !ok(&c, "quorum_floor", 50_000_000 * FANG + 1));
        // Outside the band (a test chain's genesis): only toward it, and not past the far side.
        let loose = ChainParams { quorum_floor: 0, supermajority_bps: 9_000, ..c.clone() };
        assert!(ok(&loose, "quorum_floor", FANG) && !ok(&loose, "quorum_floor", 50_000_000 * FANG + 1));
        assert!(ok(&loose, "supermajority_bps", 8_500) && ok(&loose, "supermajority_bps", 9_000) && !ok(&loose, "supermajority_bps", 9_001));
        // The spend gate: never earlier, at most a year past the block that moves it.
        let year = SPEND_GATE_MAX_DELAY_DAYS * 86_400;
        let g = c.governance_spend_height;
        assert!(c.check_governance_change("governance_spend_height", 1_000 + year, &p, 1_000).is_ok());
        assert!(c.check_governance_change("governance_spend_height", 1_001 + year, &p, 1_000).is_err());
        assert!(c.check_governance_change("governance_spend_height", g - 1, &p, 0).is_err());
        assert!(c.check_governance_change("governance_spend_height", g, &p, 0).is_ok(), "where it is is always fine");
        // Mainnet's genesis must start inside every band.
        c.check_governance_floors(&p).unwrap();
        assert!(strict.check_governance_floors(&p).is_ok() && loose.check_governance_floors(&p).is_err());
    }

    /// gap-6-1: the evidence window, which the weak-subjectivity period is half of, never falls
    /// below two weeks by a vote (and unbonding can't be shorter than it), so the period never
    /// falls below a week. A chain whose genesis starts below the floor (dev chains) only moves up.
    #[test]
    fn the_evidence_window_has_a_floor() {
        let p = params();
        let c = ChainParams::defaults(&p);
        let floor = evidence_floor_epochs(&p);
        assert_eq!((floor, c.evidence_max_age_epochs), (14 * 24, 21 * 24), "mainnet: 14 of 21 days");
        let ok = |c: &ChainParams, k: &str, v: u64| c.check_governance_change(k, v, &p, 0);
        ok(&c, "evidence_max_age_epochs", floor).unwrap();
        let err = ok(&c, "evidence_max_age_epochs", floor - 1).unwrap_err().to_string();
        assert!(err.contains(&format!("to {floor} at the least")) && err.contains("weak-subjectivity floor"), "{err}");
        ok(&c, "evidence_max_age_epochs", 365 * 24).unwrap();
        // At the floor, unbonding can't go below it either (the cross-parameter rule), so the
        // weak-subjectivity period is at least a week.
        let mut at_floor = c.clone();
        at_floor.set("evidence_max_age_epochs", floor, &p).unwrap();
        assert!(at_floor.set("unbonding_blocks", floor * p.epoch_blocks - 1, &p).is_err());
        at_floor.set("unbonding_blocks", floor * p.epoch_blocks, &p).unwrap();
        assert_eq!(crate::checkpoints::ws_period_blocks(&at_floor, p.epoch_blocks), 7 * 86_400);
        // Rounded up to whole epochs: a 7-hour epoch takes 48 epochs (14 days exactly), a
        // 5-hour one 68 (14.2 days), never fewer than 14 days.
        assert_eq!(evidence_floor_epochs(&Params { epoch_blocks: 7 * 3_600, ..p.clone() }), 48);
        assert_eq!(evidence_floor_epochs(&Params { epoch_blocks: 5 * 3_600, ..p.clone() }), 68);
        assert_eq!(evidence_floor_epochs(&Params { epoch_blocks: 30 * 86_400, ..p.clone() }), 1);
        // A genesis below the floor (dev chains' minutes) keeps it or raises it, never lowers it.
        let dp = Params { epoch_blocks: 60, ..p.clone() };
        let dev = ChainParams::with_overrides(&dp, &ChainParams::dev_overrides(dp.block_time_ms, dp.epoch_blocks)).unwrap();
        let dev_ok = |v: u64| dev.check_governance_change("evidence_max_age_epochs", v, &dp, 0).is_ok();
        assert_eq!(dev.evidence_max_age_epochs, 2, "`blockula dev`: two minutes");
        assert!(dev_ok(2) && !dev_ok(1) && dev_ok(evidence_floor_epochs(&dp) + 1));
        // The public testnet's genesis (deploy/launch.sh): a week of unbonding and of evidence,
        // below the floor, so its weak-subjectivity period is 3.5 days, not mainnet's 10.5 or
        // hours. Governance can still change it: raise it (unbonding first, which can't be
        // shorter), never lower it, and once it's at the floor, never below the floor.
        let week = 7 * 86_400;
        let mut testnet = ChainParams::with_overrides(&p, &BTreeMap::from([("unbonding_blocks".to_string(), week), ("evidence_max_age_epochs".to_string(), week / p.epoch_blocks)])).unwrap();
        assert_eq!(testnet.evidence_max_age_epochs, 7 * 24);
        assert_eq!(crate::checkpoints::ws_period_blocks(&testnet, p.epoch_blocks), week / 2);
        let t_ok = |c: &ChainParams, k: &str, v: u64| c.check_governance_change(k, v, &p, 0).is_ok();
        assert!(t_ok(&testnet, "evidence_max_age_epochs", 7 * 24) && !t_ok(&testnet, "evidence_max_age_epochs", 7 * 24 - 1));
        assert!(t_ok(&testnet, "evidence_max_age_epochs", floor) && t_ok(&testnet, "evidence_max_age_epochs", 21 * 24));
        assert!(testnet.set("evidence_max_age_epochs", floor, &p).is_err(), "not past unbonding");
        testnet.set("unbonding_blocks", 21 * 86_400, &p).unwrap();
        testnet.set("evidence_max_age_epochs", floor, &p).unwrap();
        assert!(t_ok(&testnet, "evidence_max_age_epochs", floor) && !t_ok(&testnet, "evidence_max_age_epochs", floor - 1), "once at the floor, never below it");
        // Mainnet's genesis must start at or above it.
        assert!(at_floor.check_governance_floors(&p).is_ok());
        let short = ChainParams { evidence_max_age_epochs: floor - 1, ..c.clone() };
        assert!(short.check_governance_floors(&p).unwrap_err().to_string().contains("evidence_max_age_epochs"));
    }

    #[test]
    fn conviction_ladder() {
        assert_eq!(vote_weight(100, 0), Some(10), "0.1× with no lock");
        assert_eq!(vote_weight(100, 1), Some(100));
        assert_eq!(vote_weight(100, 6), Some(600));
        assert_eq!(vote_weight(100, 7), None);
        assert_eq!(vote_weight(u64::MAX, 6), Some(u64::MAX as u128 * 6), "no overflow");
        let c = ChainParams { lock_period_blocks: 10, ..ChainParams::defaults(&params()) };
        let locks: Vec<Option<u64>> = (0..=7).map(|k| c.vote_lock_blocks(k)).collect();
        assert_eq!(locks, [Some(0), Some(10), Some(20), Some(40), Some(80), Some(160), Some(320), None]);
        assert_eq!(ChainParams::defaults(&params()).vote_lock_blocks(6), Some(32 * 7 * 86_400), "32 weeks at 6×");
    }

    #[test]
    fn genesis_values_parse_as_strings_or_numbers() {
        #[derive(serde::Serialize, serde::Deserialize)]
        struct G {
            #[serde(with = "u64_map")]
            params: BTreeMap<String, u64>,
        }
        let g: G = serde_json::from_str(r#"{"params":{"max_validators":40,"quorum_floor":"20000000000000000"}}"#).unwrap();
        assert_eq!(g.params["quorum_floor"], 20_000_000 * FANG);
        assert_eq!(serde_json::to_string(&g).unwrap(), r#"{"params":{"max_validators":"40","quorum_floor":"20000000000000000"}}"#);
        assert!(serde_json::from_str::<G>(r#"{"params":{"x":-1}}"#).is_err());
    }
}
