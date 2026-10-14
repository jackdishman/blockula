//! Circuit tests that need the trace internals: completeness over every dummy pattern, the
//! negative soundness list (each constraint violated by a prover who keeps everything else
//! consistent), and the cell-perturbation sweep for under-constrained columns.

use p3_air::{Air, BaseAir, DebugConstraintBuilder, check_all_constraints};
use p3_field::{PrimeCharacteristicRing, PrimeField32};
use p3_matrix::dense::{RowMajorMatrix, RowMajorMatrixView};
use p3_matrix::stack::ViewPair;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use super::layout::*;
use super::trace::{Built, RawAction, build};
use super::*;
use crate::field::P;
use crate::hash::permute;
use crate::tree::FullTree;

/// An action spending `ins` (`None` = dummy) from one wallet, in a tree with other people's
/// notes, and creating `outs` for a random recipient.
pub(crate) fn action(rng: &mut StdRng, ins: [Option<u64>; 2], outs: [u64; 2]) -> ActionWitness {
    let key = SpendingKey::from_seed(random_bytes(rng));
    let owner = key.full_viewing_key().owner();
    let mut tree = FullTree::new();
    let mut real = Vec::new();
    for value in ins.iter().flatten() {
        for _ in 0..(rng.next_u32() % 4) {
            let other = Note { value: rng.next_u64(), owner: xof_to_digest("t", &random_bytes::<_, 8>(rng)), rseed: random_bytes(rng) };
            tree.append(other.commitment()).unwrap();
        }
        let note = Note { value: *value, owner, rseed: random_bytes(rng) };
        real.push((note, tree.append(note.commitment()).unwrap()));
    }
    let mut real = real.into_iter();
    let spends = ins.map(|v| match v {
        Some(_) => {
            let (note, pos) = real.next().unwrap();
            SpendWitness::new(&key, note, tree.path(pos).unwrap())
        }
        None => SpendWitness::dummy(rng),
    });
    let recipient = SpendingKey::from_seed(random_bytes(rng)).full_viewing_key().owner();
    let outputs = outs.map(|value| Note { value, owner: recipient, rseed: random_bytes(rng) });
    ActionWitness { spends, outputs, anchor: tree.root(), sighash: random_bytes(rng) }
}

fn raw(w: &ActionWitness) -> RawAction {
    RawAction::from_witness(w, ValueBalance::new(trace::balance(w)).unwrap())
}

fn satisfied(b: &Built) -> bool {
    check_all_constraints(action_air(), &b.trace, &b.public, Some(1)).is_ok()
}

/// A cheating prover's best attempt must fail twice over: the constraints reject the trace, and
/// a proof forced out of it anyway doesn't verify.
fn assert_rejected(b: Built, what: &str) {
    assert!(!satisfied(&b), "{what}: the trace satisfies every constraint");
    match prove_trace(b.trace, &b.public, [3; 32]) {
        Ok(proof) => assert!(verify_raw(&b.public, &proof).is_err(), "{what}: forged proof verified"),
        Err(_) => {} // the prover refused: also fine
    }
}

fn rng(seed: u64) -> StdRng {
    StdRng::seed_from_u64(seed)
}

// ---- Completeness -------------------------------------------------------------------------

#[test]
fn honest_action_proves_and_verifies() {
    let w = action(&mut rng(1), [Some(1000), Some(234)], [1200, 30]);
    let public = check_witness(&w).expect("satisfied");
    assert_eq!(public.value_balance, ValueBalance::new(4).unwrap());
    let (public2, proof) = prove_with_seed(&w, [9; 32]).expect("proves");
    assert_eq!(public, public2);
    verify(&public, &proof).expect("verifies");
    assert!(proof.len() <= MAX_PROOF_BYTES);
}

#[test]
fn completeness_every_dummy_pattern_and_extreme_values() {
    let max = u64::MAX;
    let cases: &[([Option<u64>; 2], [u64; 2])] = &[
        ([Some(10), Some(20)], [25, 5]),             // transfer, zero fee
        ([Some(10), None], [3, 0]),                  // one real spend, fee 7
        ([None, Some(10)], [0, 10]),                 // dummy first
        ([None, None], [40, 2]),                     // pure shield: vb = -42
        ([Some(max), Some(max)], [max, max]),        // max values balance
        ([Some(max), None], [0, 0]),                 // unshield u64::MAX
        ([None, None], [max, 0]),                    // shield u64::MAX
        ([Some(0), Some(0)], [0, 0]),                // real zero-value notes
        ([Some(1 << 16), Some((1 << 32) - 1)], [1 << 47, 0]), // limb carries, shield the rest
    ];
    for (i, (ins, outs)) in cases.iter().enumerate() {
        let w = action(&mut rng(100 + i as u64), *ins, *outs);
        let expected = ins.iter().flatten().map(|v| i128::from(*v)).sum::<i128>()
            - outs.iter().map(|v| i128::from(*v)).sum::<i128>();
        let public = check_witness(&w).unwrap_or_else(|e| panic!("case {i}: {e:?}"));
        assert_eq!(public.value_balance.value(), expected, "case {i}");
    }
    // Full proofs for a few patterns (the rest are covered by the constraint check above).
    for (i, &(ins, outs)) in [cases[1], cases[3], cases[4]].iter().enumerate() {
        let w = action(&mut rng(200 + i as u64), ins, outs);
        let (public, proof) = prove(&w).expect("proves");
        verify(&public, &proof).expect("verifies");
    }
}

#[test]
fn random_actions_satisfy_the_circuit() {
    let mut r = rng(7);
    for _ in 0..24 {
        let pick = |r: &mut StdRng| match r.next_u32() % 3 {
            0 => None,
            1 => Some(r.next_u64() >> (r.next_u32() % 64)),
            _ => Some(r.next_u64()),
        };
        let ins = [pick(&mut r), pick(&mut r)];
        let total: u128 = ins.iter().flatten().map(|v| u128::from(*v)).sum();
        // Outputs take a random share of the inputs, or (shield) a random amount.
        let a = ((total * u128::from(r.next_u32())) >> 32).min(u128::from(u64::MAX));
        let outs = [a as u64, ((total - a) / 2).min(u128::from(u64::MAX)) as u64];
        let w = action(&mut r, ins, outs);
        check_witness(&w).unwrap_or_else(|e| panic!("{ins:?} -> {outs:?}: {e:?}"));
    }
}

#[test]
fn poseidon2_columns_match_the_native_permutation() {
    let w = action(&mut rng(3), [Some(5), None], [5, 0]);
    let b = build(&raw(&w));
    for r in 0..HEIGHT {
        let row = &b.trace.values[r * WIDTH..(r + 1) * WIDTH];
        let input: [F; 16] = row[IN..IN + 16].try_into().unwrap();
        assert_eq!(&row[OUT..OUT + 16], &permute(input), "row {r}");
    }
}

// ---- Soundness: the negative list ---------------------------------------------------------

#[test]
fn inflated_output_is_rejected() {
    let w = action(&mut rng(10), [Some(100), Some(50)], [100, 50]);
    let mut a = raw(&w);
    a.outputs[0].limbs = u64_to_limbs(101);
    assert_rejected(build(&a), "output one drop larger than the inputs");
    let mut a = raw(&w);
    a.outputs[1].limbs = u64_to_limbs(50 + (1 << 40));
    assert_rejected(build(&a), "output 2^40 larger");
}

#[test]
fn value_balance_must_match_exactly() {
    let w = action(&mut rng(11), [Some(1000), None], [990, 0]);
    let honest = raw(&w);
    assert!(satisfied(&build(&honest)));
    for (neg, vb) in [(0, 9), (0, 11), (1, 10), (0, 10 + (1 << 16)), (0, 0)] {
        let mut a = honest.clone();
        a.vb_neg = F::from_u32(neg);
        a.vb = u64_to_limbs(vb);
        assert_rejected(build(&a), &format!("claimed value balance {}{vb}", if neg == 1 { "-" } else { "" }));
    }
    let mut a = honest.clone();
    a.vb_neg = F::TWO;
    assert_rejected(build(&a), "non-boolean sign");
}

#[test]
fn limbs_must_be_16_bit() {
    // Same value, non-canonical limbs: [65536, 0, 0, 0] instead of [0, 1, 0, 0]. The balance
    // holds; only the range check stops it.
    let w = action(&mut rng(12), [Some(1 << 16), None], [1 << 16, 0]);
    let mut a = raw(&w);
    a.outputs[0].limbs = [F::from_u32(1 << 16), F::ZERO, F::ZERO, F::ZERO];
    assert_rejected(build(&a), "limb of 2^16");
}

#[test]
fn field_wrap_around_is_rejected() {
    // An output limb of p − 1 (= −1) adds one drop to the pool's balance mod p, so claiming an
    // unshield of 1 from nothing balances in the field. The range check stops it.
    let w = action(&mut rng(13), [None, None], [0, 0]);
    let mut a = raw(&w);
    a.outputs[0].limbs[0] = F::NEG_ONE;
    a.vb = u64_to_limbs(1);
    assert_rejected(build(&a), "output limb p - 1");
    // Spend side: a dummy-free spend of a note whose limb is p − 1 cannot exist either.
    let w = action(&mut rng(14), [Some(0), None], [0, 0]);
    let mut a = raw(&w);
    a.spends[0].limbs[3] = F::from_u32(P - 1);
    assert_rejected(build(&a), "spent limb p - 1");
}

#[test]
fn u64_overflow_is_rejected() {
    // 2^63 + 2^63 = 2^64 ≡ 0 (mod 2^64): a wrapping implementation would accept zero outputs.
    let half = 1u64 << 63;
    let w = action(&mut rng(15), [Some(half), Some(half)], [0, 0]);
    assert_eq!(check_witness(&w), Err(ProveError::ValueBalanceOutOfRange));
    let mut a = RawAction::from_witness(&w, ValueBalance::ZERO);
    a.vb = [F::ZERO; 4];
    assert_rejected(build(&a), "u64 sum wraps to zero");
}

#[test]
fn wrong_merkle_path_is_rejected() {
    let w = action(&mut rng(16), [Some(77), Some(3)], [80, 0]);
    for level in [0, 7, 31] {
        let mut a = raw(&w);
        a.spends[0].siblings[level] = xof_to_digest("other", &[level as u8]);
        assert_rejected(build(&a), &format!("sibling at level {level}"));
    }
    let mut a = raw(&w);
    a.spends[1].position ^= 1 << 5; // claims another position with the same siblings
    assert_rejected(build(&a), "position bit flipped");
}

#[test]
fn wrong_anchor_is_rejected() {
    let w = action(&mut rng(17), [Some(1), None], [1, 0]);
    let mut a = raw(&w);
    a.anchor = crate::tree::empty_roots()[DEPTH];
    assert_rejected(build(&a), "anchor of the empty tree");
}

#[test]
fn wrong_nullifier_is_rejected() {
    let w = action(&mut rng(18), [Some(9), Some(1)], [10, 0]);
    let a = raw(&w);
    // A double spend needs a second nullifier for the same note: the same note at another
    // position, or simply a fresh value.
    let nk = sponge(Domain::NullifierKey, &a.spends[0].sk);
    let mut b = build(&a);
    let cm = b.trace.values[(spend_base(0) + CM_C) * WIDTH + OUT..][..8].try_into().unwrap();
    let other = nullifier(&nk, &cm, a.spends[0].position.wrapping_add(1));
    b.public[pv::NF..pv::NF + 8].copy_from_slice(&other);
    assert_rejected(b, "nullifier for another position");
    let mut b = build(&a);
    b.public[pv::NF + 8] += F::ONE;
    assert_rejected(b, "nullifier off by one");
}

#[test]
fn wrong_output_commitment_is_rejected() {
    let w = action(&mut rng(19), [Some(9), None], [9, 0]);
    let mut b = build(&raw(&w));
    b.public[pv::CM + 3] += F::ONE;
    assert_rejected(b, "output commitment changed");
}

#[test]
fn foreign_note_is_rejected() {
    // Spending someone else's note with your own key: the circuit commits to your owner, so
    // the leaf isn't in the tree.
    let mut r = rng(20);
    let w = action(&mut r, [Some(500), None], [500, 0]);
    let thief = SpendingKey::from_seed(random_bytes(&mut r));
    let mut stolen = w.clone();
    stolen.spends[0].sk = thief.sk();
    assert!(matches!(check_witness(&stolen), Err(ProveError::Unsatisfied(_))));
    assert_rejected(build(&raw(&stolen)), "someone else's note");
}

#[test]
fn dummy_cannot_carry_value() {
    // A dummy skips the membership check, so it must spend exactly zero.
    let w = action(&mut rng(21), [Some(5), None], [5, 0]);
    let mut stolen = w.clone();
    stolen.spends[1].note.value = 1_000_000;
    stolen.outputs[1].value = 1_000_000;
    assert!(matches!(check_witness(&stolen), Err(ProveError::Unsatisfied(_))));
    assert_rejected(build(&raw(&stolen)), "dummy with value");
    // And a real note can't be relabelled dummy to skip the tree.
    let mut a = raw(&w);
    a.spends[0].dummy = true;
    a.anchor = xof_to_digest("fake", b"root");
    assert_rejected(build(&a), "real value marked dummy");
}

#[test]
fn same_note_twice_is_rejected() {
    let mut w = action(&mut rng(22), [Some(5), Some(6)], [11, 0]);
    w.spends[1] = w.spends[0].clone();
    w.outputs[0].value = 10;
    assert_eq!(check_witness(&w), Err(ProveError::DuplicateNullifier));
    // Two identical dummies are the same note too.
    let mut w = action(&mut rng(23), [None, None], [0, 0]);
    w.spends[1] = w.spends[0].clone();
    assert_eq!(check_witness(&w), Err(ProveError::DuplicateNullifier));
    // The verifier refuses equal nullifiers whatever the proof.
    let w = action(&mut rng(24), [Some(5), None], [5, 0]);
    let (mut public, proof) = prove_with_seed(&w, [1; 32]).unwrap();
    public.nullifiers[1] = public.nullifiers[0];
    assert_eq!(verify(&public, &proof), Err(VerifyError::DuplicateNullifier));
}

#[test]
fn wrong_circuit_id_is_rejected() {
    let w = action(&mut rng(25), [None, None], [0, 0]);
    let mut a = raw(&w);
    a.circuit_id = F::from_u32(CIRCUIT_ID + 1);
    assert_rejected(build(&a), "circuit id");
}

#[test]
fn trace_level_cheats_are_rejected() {
    let w = action(&mut rng(26), [Some(5), Some(8)], [13, 0]);
    let honest = build(&raw(&w));
    let at = |row: usize, col: usize| row * WIDTH + col;
    // (what, cells to change, new value or None to add one)
    let cheats: Vec<(&str, Vec<usize>, Option<F>)> = vec![
        ("path bit 2", vec![at(spend_base(0) + MERKLE + 3, BIT)], Some(F::TWO)),
        ("bit outside the path", vec![at(spend_base(0) + NF_A, BIT)], Some(F::ONE)),
        ("dummy flag 2", (0..SPEND_ROWS).map(|r| at(spend_base(1) + r, DUMMY)).collect(), Some(F::TWO)),
        ("dummy flag set mid-block", (MERKLE..SPEND_ROWS).map(|r| at(spend_base(0) + r, DUMMY)).collect(), Some(F::ONE)),
        ("register outside spends", vec![at(PADDING + 3, REG_NK + 2)], Some(F::ONE)),
        ("acc starts nonzero", (0..HEIGHT).map(|r| at(r, ACC)).collect(), None),
        ("value bits on a non-value row", vec![at(spend_base(0) + CM_A, VBITS + 5)], Some(F::ONE)),
        ("carry bits off the balance row", vec![at(PADDING, CBITS)], Some(F::ONE)),
        ("padding input", vec![at(PADDING + 1, IN + 15)], Some(F::ONE)),
    ];
    for (what, cells, value) in cheats {
        let mut trace = honest.trace.clone();
        for c in cells {
            trace.values[c] = value.unwrap_or(trace.values[c] + F::ONE);
        }
        let b = Built { trace, nullifiers: honest.nullifiers, commitments: honest.commitments, public: honest.public.clone() };
        assert!(!satisfied(&b), "{what}: accepted");
    }
}

#[test]
fn mismatched_sighash_or_public_inputs_fail() {
    let w = action(&mut rng(27), [Some(70), Some(30)], [60, 35]);
    let (public, proof) = prove_with_seed(&w, [5; 32]).unwrap();
    verify(&public, &proof).unwrap();
    let mut variants = Vec::new();
    let mut p = public.clone();
    p.sighash[0] ^= 1;
    variants.push(("sighash", p));
    let mut p = public.clone();
    p.nullifiers.swap(0, 1);
    variants.push(("nullifiers swapped", p));
    let mut p = public.clone();
    p.commitments.swap(0, 1);
    variants.push(("commitments swapped", p));
    let mut p = public.clone();
    p.anchor = crate::tree::empty_roots()[DEPTH];
    variants.push(("anchor", p));
    let mut p = public.clone();
    p.value_balance = ValueBalance::new(-5).unwrap();
    variants.push(("value balance sign", p));
    let mut p = public.clone();
    p.value_balance = ValueBalance::new(6).unwrap();
    variants.push(("value balance", p));
    for (what, p) in variants {
        assert!(verify(&p, &proof).is_err(), "{what}");
    }
}

// ---- Under-constraint sweep ---------------------------------------------------------------

/// Evaluates every constraint on one row (with its successor). Returns true if all hold.
fn row_holds(trace: &RowMajorMatrix<F>, public: &[F], row: usize) -> bool {
    let air = action_air();
    let next = (row + 1) % HEIGHT;
    let local = &trace.values[row * WIDTH..(row + 1) * WIDTH];
    let nxt = &trace.values[next * WIDTH..(next + 1) * WIDTH];
    let periodic = air.periodic_values(row);
    let empty: [F; 0] = [];
    let mut builder = DebugConstraintBuilder::new(
        row,
        ViewPair::new(RowMajorMatrixView::new_row(local), RowMajorMatrixView::new_row(nxt)),
        ViewPair::new(RowMajorMatrixView::new(&empty, 0), RowMajorMatrixView::new(&empty, 0)),
        public,
        F::from_bool(row == 0),
        F::from_bool(row == HEIGHT - 1),
        F::from_bool(row != HEIGHT - 1),
        &periodic,
    );
    air.eval(&mut builder);
    !builder.has_failures()
}

/// Every cell of a valid trace is pinned: changing any one of them (by +1 and by a random
/// amount) breaks a constraint on its row or the row before. A free cell would mean an
/// under-constrained column, the Orchard bug class.
fn sweep(w: &ActionWitness, seed: u64) {
    let b = build(&raw(w));
    assert!(satisfied(&b));
    let mut trace = b.trace.clone();
    let mut r = rng(seed);
    let mut free = Vec::new();
    for row in 0..HEIGHT {
        let prev = (row + HEIGHT - 1) % HEIGHT;
        for col in 0..WIDTH {
            for delta in [F::ONE, F::from_u32(1 + r.next_u32() % (P - 1))] {
                let idx = row * WIDTH + col;
                let orig = trace.values[idx];
                trace.values[idx] = orig + delta;
                let holds = row_holds(&trace, &b.public, row) && row_holds(&trace, &b.public, prev);
                trace.values[idx] = orig;
                if holds {
                    free.push((row, col, delta.as_canonical_u32()));
                }
            }
        }
    }
    assert!(free.is_empty(), "cells accept a change with every constraint holding: {free:?}");
}

#[test]
fn sweep_two_real_spends() {
    sweep(&action(&mut rng(30), [Some(123_456_789), Some(42)], [100, 123_456_700]), 31);
}

#[test]
fn sweep_dummy_spends() {
    sweep(&action(&mut rng(32), [None, Some(u64::MAX)], [u64::MAX, 0]), 33);
    sweep(&action(&mut rng(34), [None, None], [7, 9]), 35);
}

#[test]
fn public_values_are_each_pinned() {
    // Same idea for the public values: changing any one must break the trace.
    let w = action(&mut rng(36), [Some(9), Some(10)], [15, 1]);
    let b = build(&raw(&w));
    let mut free = Vec::new();
    for i in 0..pv::COUNT {
        if (pv::SIGHASH..pv::SIGHASH + 8).contains(&i) {
            continue; // bound by Fiat-Shamir, not by a constraint (see the sighash test)
        }
        let mut public = b.public.clone();
        public[i] += F::ONE;
        if check_all_constraints(action_air(), &b.trace, &public, Some(1)).is_ok() {
            free.push(i);
        }
    }
    assert!(free.is_empty(), "public values not pinned by any constraint: {free:?}");
}

#[test]
fn security_meets_the_target() {
    let r = config::security_report();
    assert!(r.proven_bits >= 100, "{r:?}");
    assert_eq!(r.max_constraint_degree, 3);
}

#[test]
#[ignore = "parameter exploration; run with --release --ignored --nocapture"]
fn explore_security_parameters() {
    for log_blowup in [2, 3, 4, 5] {
        for queries in [32, 40, 48, 56, 64, 80, 100] {
            for pow in [0, 16, 20] {
                let r = config::security_report_for(log_blowup, queries, pow, 0, pow, pow);
                eprintln!(
                    "blowup 2^{log_blowup} q {queries:3} pow {pow:2}: proven {:3} (udr {:3} ldr {:3}) conj {:3}",
                    r.proven_bits, r.proven_unique_decoding_bits, r.proven_list_decoding_bits, r.conjectured_bits
                );
            }
        }
    }
    eprintln!("{:?}", config::security_report());
}
