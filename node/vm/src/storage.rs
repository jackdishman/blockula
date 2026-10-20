//! Storage economics (docs/PROGRAMS.md 6.2, 6.3): how many bytes an entry accounts for, and what a
//! byte costs to bond as program state fills. Pure functions of their arguments, so the overlay
//! (which prices each write as it happens) and the node (which serves prices and checks its
//! totals) compute them alike.

use crate::config::{MAX_KEY_BYTES, MAX_VALUE_BYTES};

/// Bytes an entry accounts for beyond its key and value: its payer, deposit and capacity, and its
/// share of the state tree and the record's key prefix.
pub const ENTRY_OVERHEAD: usize = 96;
/// Capacities are whole multiples of this many bytes.
pub const CAPACITY_UNIT: usize = 64;
/// The largest capacity an entry can have: a maximum key and value.
pub const MAX_CAPACITY: u32 = capacity(MAX_KEY_BYTES, MAX_VALUE_BYTES);

/// An entry's accounted size (6.2): key + value + [`ENTRY_OVERHEAD`], rounded up to a multiple of
/// [`CAPACITY_UNIT`]. Its deposit bonds this many bytes, and an overwrite whose capacity is at most
/// the entry's bonds nothing (6.3). Callers pass lengths already checked against
/// `MAX_KEY_BYTES` and `MAX_VALUE_BYTES`; anything longer saturates at `u32::MAX`.
pub const fn capacity(key_len: usize, value_len: usize) -> u32 {
    let bytes = key_len.saturating_add(value_len).saturating_add(ENTRY_OVERHEAD);
    let rounded = bytes.div_ceil(CAPACITY_UNIT).saturating_mul(CAPACITY_UNIT);
    if rounded > u32::MAX as usize { u32::MAX } else { rounded as u32 }
}

/// 6.3's price curve: drops per accounted byte when program state holds `used` bytes,
/// `base × 2^⌊8 × used / soft⌋`, where `base` is `program_storage_price` and `soft` is
/// `program_state_soft_bytes`. Integer shifts only. `used` counts the bonded bytes themselves:
/// it's state's bytes once the write lands (the overlay's convention for entries, and the node's
/// for upload blobs and program records), never before. `None` past the ceiling, `2 × soft` (a write
/// that would take state there is refused, `TrapKind::StateFull`), or if the price wouldn't fit a
/// `u64` (it does for every parameter in bounds: at most 10⁷ × 2¹⁶).
pub const fn price(base: u64, soft: u64, used: u64) -> Option<u64> {
    if soft == 0 || used as u128 > 2 * soft as u128 {
        return None;
    }
    // ≤ 16, since used ≤ 2 × soft.
    let doublings = (8 * used as u128 / soft as u128) as u32;
    base.checked_mul(1 << doublings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_rounds_up_to_whole_units() {
        assert_eq!(capacity(1, 0), 128, "1 + 0 + 96 = 97");
        assert_eq!(capacity(1, 8), 128, "a counter: 105");
        assert_eq!(capacity(32, 0), 128, "exactly 128");
        assert_eq!(capacity(33, 0), 192, "129");
        assert_eq!(capacity(33, 8), 192);
        assert_eq!(capacity(0, 0), 128, "keys are 1–128 bytes, but the rule holds at 0");
        assert_eq!(MAX_CAPACITY, 16_640, "128 + 16,384 + 96 = 16,608, rounded up");
        assert_eq!(capacity(usize::MAX, 1), u32::MAX, "saturates, never wraps");
        for k in 1..=MAX_KEY_BYTES {
            for v in [0, 1, 63, 64, 65, MAX_VALUE_BYTES] {
                let c = capacity(k, v) as usize;
                assert!(c.is_multiple_of(CAPACITY_UNIT) && c >= k + v + ENTRY_OVERHEAD && c < k + v + ENTRY_OVERHEAD + CAPACITY_UNIT, "{k} {v}");
            }
        }
    }

    #[test]
    fn price_doubles_every_eighth_of_the_soft_cap() {
        let (base, soft) = (100_000, 512 << 20);
        assert_eq!(price(base, soft, 0), Some(100_000));
        assert_eq!(price(base, soft, soft / 8 - 1), Some(100_000));
        assert_eq!(price(base, soft, soft / 8), Some(200_000));
        assert_eq!(price(base, soft, soft / 2), Some(1_600_000));
        assert_eq!(price(base, soft, soft - 1), Some(base << 7));
        assert_eq!(price(base, soft, soft), Some(base << 8));
        assert_eq!(price(base, soft, 2 * soft), Some(base << 16), "the ceiling itself is allowed");
        assert_eq!(price(base, soft, 2 * soft + 1), None, "past the ceiling");
        assert_eq!(price(base, soft, u64::MAX), None);
        // The parameters' bounds (14.2): the dearest byte still fits.
        assert_eq!(price(10_000_000, 64 << 20, 128 << 20), Some(10_000_000 << 16));
        assert_eq!(price(u64::MAX, 1, 1), None, "an overflow is no price");
        assert_eq!(price(1, 0, 0), None, "no soft cap, no room");
        // Monotonic in `used`.
        let mut last = 0;
        for used in (0..=2 * soft).step_by((soft / 64) as usize) {
            let p = price(base, soft, used).unwrap();
            assert!(p >= last);
            last = p;
        }
    }
}
