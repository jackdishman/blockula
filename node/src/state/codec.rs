//! Canonical binary encoding of state records.
//!
//! Values are borsh 1.x (designed for consensus hashing): little-endian integers, `usize` as
//! `u64`, u32-length-prefixed strings and sequences, maps in key order, one-byte option and enum
//! tags (variants in declaration order). Decoding is strict: no trailing bytes, map keys strictly
//! increasing (`de_strict_order`), valid UTF-8, known tags. Every record type derives
//! `BorshSerialize`/`BorshDeserialize` next to its definition; changing a field, its order or an
//! enum's variants changes the state root, so it also bumps [`SCHEMA_VERSION`].
//!
//! Keys are bytes that sort like the map they come from: strings as UTF-8, integers big-endian.

use crate::types::KeyRole;
use anyhow::{anyhow, Result};
use borsh::{BorshDeserialize, BorshSerialize};
use std::borrow::Cow;
use std::io::{Read, Write};

/// Version of the state encoding (records, namespaces, tree). It is itself a record, so it is
/// committed to by the root. 2: the protocol v1 scaffold (`chain_params`, account locks,
/// validator commission, the module namespaces). 3 was each wave-3 package's own bump; 4 is the
/// integrated protocol v1 state: economics (name expiry, lock ids and vote locks, the `econ`
/// scalars: fee ring, issuance year, name sweep), staking (pools, delegations, unbonding entries,
/// punished faults and liveness under namespaces 32–36, its META scalar; validator records
/// without a bond, statuses candidate/active/removed/tombstoned), governance (its records and
/// `multisig_expiry_blocks`), the randomness beacon (its rounds, values and revert attestations;
/// the PoC scalars lose the block-hash seed) and Ghost Mode (namespaces 48–52, the `ghost` scalar
/// record, `ghost_anchor_window` and `ghost_verify_fee`). 5: the fee ring's slots also fix Ghost
/// Mode's per-block limits (`econ::FeeSlot`), so they're known `FEE_LAG` blocks ahead. 6 (the
/// audit fixes): `chain_params` gains `tip_cap_multiple` (tips are capped at a multiple of the
/// base fee); staking slashes once per stake (pools' `slashed_from`), entries indexed by validator
/// (namespace 64) and epochs' highest views (65); the governance scalars gain the recent treasury
/// spends, the closed proposals whose votes are being swept and the multisig sweep cursor;
/// `Distribution` proposals name their distribution id; `Params` lists the owners the bootstrap
/// delegation passes over; name records keep the account they're offered to (`offered_to`). 7
/// (the second audit round, pre-mainnet; one bump for every package): `chain_params` gains
/// `bootstrap_commission_cap_bps`, pools the height their consensus key last showed up
/// (`ready_at`), and liveness rings the `downtime_max_missed` they were kept under; a
/// distribution records the proposal that opened it (`Distribution::proposal`); name records
/// keep the grace they were registered with (`grace_blocks`); the `upgrade` scalar record follows
/// the schedule with what cancellations withdrew (`upgrade::Withdrawals`); Proof of Contribution
/// gains namespaces 26 (withdrawn vouches that still count, `poc::DroppedVouch`) and 27 (GitHub
/// handles governance holds for an account, `poc::HandleHold`). 8 (the third audit round): pools
/// keep the consensus key's last update for its owner (`key_update_at`, F04's once-an-epoch limit,
/// apart from `ready_at`, which only `ValidatorReady` writes now) and the candidate they last
/// admitted into a set of one or two (`admit`).
pub const SCHEMA_VERSION: u32 = 8;

pub fn encode<T: BorshSerialize + ?Sized>(v: &T) -> Vec<u8> {
    borsh::to_vec(v).expect("encoding to memory can't fail")
}

/// Strict inverse of [`encode`].
pub fn decode<T: BorshDeserialize>(bytes: &[u8]) -> Result<T> {
    borsh::from_slice(bytes).map_err(|e| anyhow!("bad record encoding: {e}"))
}

/// A map key as record key bytes, preserving order.
pub trait RecordKey: Sized {
    fn key_bytes(&self) -> Cow<'_, [u8]>;
    fn from_key_bytes(b: &[u8]) -> Result<Self>;
}

impl RecordKey for String {
    fn key_bytes(&self) -> Cow<'_, [u8]> {
        Cow::Borrowed(self.as_bytes())
    }
    fn from_key_bytes(b: &[u8]) -> Result<Self> {
        String::from_utf8(b.to_vec()).map_err(|_| anyhow!("record key isn't UTF-8"))
    }
}

impl RecordKey for u64 {
    fn key_bytes(&self) -> Cow<'_, [u8]> {
        Cow::Owned(self.to_be_bytes().to_vec())
    }
    fn from_key_bytes(b: &[u8]) -> Result<Self> {
        Ok(u64::from_be_bytes(b.try_into().map_err(|_| anyhow!("record key isn't 8 bytes"))?))
    }
}

/// Hashes and digests (Ghost Mode's nullifiers and roots), as their bytes.
impl RecordKey for [u8; 32] {
    fn key_bytes(&self) -> Cow<'_, [u8]> {
        Cow::Borrowed(self)
    }
    fn from_key_bytes(b: &[u8]) -> Result<Self> {
        b.try_into().map_err(|_| anyhow!("record key isn't 32 bytes"))
    }
}

// `KeyRole` lives in types.rs (the tx format); its state encoding is written out here.
impl BorshSerialize for KeyRole {
    fn serialize<W: Write>(&self, w: &mut W) -> std::io::Result<()> {
        match self {
            KeyRole::Owner => 0u8.serialize(w),
            KeyRole::Session { limit, period_blocks, expires_at } => {
                1u8.serialize(w)?;
                limit.serialize(w)?;
                period_blocks.serialize(w)?;
                expires_at.serialize(w)
            }
        }
    }
}

impl BorshDeserialize for KeyRole {
    fn deserialize_reader<R: Read>(r: &mut R) -> std::io::Result<Self> {
        match u8::deserialize_reader(r)? {
            0 => Ok(KeyRole::Owner),
            1 => Ok(KeyRole::Session { limit: u64::deserialize_reader(r)?, period_blocks: u64::deserialize_reader(r)?, expires_at: u64::deserialize_reader(r)? }),
            t => Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad key role tag {t}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn keys_sort_like_their_maps() {
        let ids = [0u64, 1, 255, 256, u64::MAX];
        let bytes: Vec<Vec<u8>> = ids.iter().map(|i| i.key_bytes().into_owned()).collect();
        assert!(bytes.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(u64::from_key_bytes(&bytes[3]).unwrap(), 256);
        assert!(u64::from_key_bytes(&[1, 2]).is_err());
        assert!(String::from_key_bytes(&[0xff]).is_err());
    }

    #[test]
    fn decoding_is_strict() {
        let role = KeyRole::Session { limit: 5, period_blocks: 10, expires_at: 99 };
        let b = encode(&role);
        assert_eq!(b.len(), 25);
        assert_eq!(decode::<KeyRole>(&b).unwrap(), role);
        assert!(decode::<KeyRole>(&[b.clone(), vec![0]].concat()).is_err(), "trailing bytes");
        assert!(decode::<KeyRole>(&[2]).is_err(), "unknown tag");
        assert!(decode::<Option<u64>>(&[2, 0, 0, 0, 0, 0, 0, 0, 0]).is_err(), "option tag");
        assert!(decode::<bool>(&[2]).is_err());
        // Maps must come sorted, without duplicates.
        let m = BTreeMap::from([("a".to_string(), 1u64), ("b".to_string(), 2)]);
        let mut b = encode(&m);
        assert_eq!(decode::<BTreeMap<String, u64>>(&b).unwrap(), m);
        b.swap(8, 21); // "a" ↔ "b"
        assert!(decode::<BTreeMap<String, u64>>(&b).is_err());
    }
}
