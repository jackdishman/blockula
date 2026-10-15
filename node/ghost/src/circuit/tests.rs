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
use super::trace::{Built, Cheat, RawAction, build, build_with};
use super::*;
use crate::field::P;
use crate::hash::permute;
use crate::tree::{DEPTH, FullTree};

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
    // The prover may also refuse (an error), which is fine.
    if let Ok(proof) = prove_trace(b.trace, &b.public, [3; 32]) {
        assert!(verify_raw(&b.public, &proof).is_err(), "{what}: forged proof verified");
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
fn trace_matches_the_native_derivations() {
    // The row machine and note.rs / tree.rs compute the same things independently.
    let mut r = rng(8);
    for ins in [[Some(3), Some(4)], [None, Some(9)], [None, None]] {
        let w = action(&mut r, ins, [1, 2]);
        let public = check_witness(&w).unwrap();
        for i in 0..2 {
            assert_eq!(public.nullifiers[i], w.spends[i].nullifier());
            assert_eq!(public.commitments[i], w.outputs[i].commitment());
            let s = &w.spends[i];
            if !s.dummy {
                assert_eq!(s.note.owner, SpendingKey::owner_of(&s.sk));
                assert_eq!(s.path.root(&s.note.commitment()), w.anchor);
            }
        }
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
fn viewing_key_cannot_spend() {
    // A full viewing key holder knows ak and nk but not sk. Feeding them in directly (a
    // random sk, then ak into the owner hash and nk into the register) must fail.
    let mut r = rng(45);
    let w = action(&mut r, [Some(500), None], [500, 0]);
    let fvk_ak = sponge(Domain::SpendAuth, &w.spends[0].sk);
    let fvk_nk = sponge(Domain::NullifierKey, &w.spends[0].sk);
    let mut a = raw(&w);
    a.spends[0].sk = SpendingKey::from_seed(random_bytes(&mut r)).sk();
    let b = spend_base(0);
    let mut cheats: Vec<Cheat> = (0..8).map(|j| Cheat { row: b + OWN_A, col: IN + j, value: fvk_ak[j] }).collect();
    cheats.extend((0..8).map(|j| Cheat { row: b + NK, col: REG_NK + j, value: fvk_nk[j] }));
    let built = build_with(&a, &cheats);
    assert_eq!(built.nullifiers[0], w.spends[0].nullifier(), "the attacker reproduced the real nullifier");
    assert_rejected(built, "spend with a viewing key");
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
fn duplicate_commitments_give_distinct_nullifiers_and_no_more() {
    // A sender can make two identical notes. Both are spendable, with different nullifiers
    // (position binding), and the path bit can't be stretched to make a third.
    let mut r = rng(40);
    let key = SpendingKey::from_seed(random_bytes(&mut r));
    let note = Note { value: 1000, owner: key.full_viewing_key().owner(), rseed: random_bytes(&mut r) };
    let mut tree = FullTree::new();
    tree.append(note.commitment()).unwrap();
    tree.append(note.commitment()).unwrap();
    let spend = |pos: u32| SpendWitness::new(&key, note, tree.path(pos).unwrap());
    let recipient = xof_to_digest("r", b"r");
    let w = ActionWitness {
        spends: [spend(0), spend(1)],
        outputs: [Note { value: 2000, owner: recipient, rseed: [1; 32] }, Note { value: 0, owner: recipient, rseed: [2; 32] }],
        anchor: tree.root(),
        sighash: [0; 32],
    };
    let public = check_witness(&w).expect("both duplicates spend");
    assert_ne!(public.nullifiers[0], public.nullifiers[1]);
    // At level 0 the node and its sibling are equal, so the arrangement holds for any bit.
    // Only booleanity stops a bit of 2 (position 2, a fresh nullifier for the same note).
    let a = raw(&w);
    for bit in [F::TWO, F::NEG_ONE, F::from_u32(1 << 20)] {
        let b = build_with(&a, &[Cheat { row: spend_base(0) + MERKLE, col: BIT, value: bit }]);
        assert_eq!(b.public[..8], a.anchor[..], "same anchor");
        assert_ne!(b.nullifiers[0], public.nullifiers[0], "a new nullifier");
        assert_rejected(b, "non-boolean path bit at a duplicated leaf");
    }
}

#[test]
fn value_bits_must_be_boolean() {
    // Limb p − 1 (= −1) decomposed as bits [p − 1, 0, …]: the sum is right, the bits aren't.
    let w = action(&mut rng(41), [None, None], [0, 0]);
    let mut a = raw(&w);
    a.outputs[0].limbs[0] = F::NEG_ONE;
    a.vb = u64_to_limbs(1);
    let row = output_base(0) + 1;
    let mut cheats = vec![Cheat { row, col: VBITS, value: F::NEG_ONE }];
    cheats.extend((1..16).map(|i| Cheat { row, col: VBITS + i, value: F::ZERO }));
    assert_rejected(build_with(&a, &cheats), "non-boolean value bits");
}

#[test]
fn balance_starts_at_zero() {
    // Start the accumulator at 7 and mint 7 into an output: the end balance is unchanged.
    let w = action(&mut rng(42), [Some(100), None], [100, 0]);
    let mut a = raw(&w);
    a.outputs[0].limbs = u64_to_limbs(107);
    assert_rejected(build_with(&a, &[Cheat { row: 0, col: ACC, value: F::from_u32(7) }]), "accumulator starts at 7");
}

#[test]
fn balance_holds_over_the_integers_not_mod_p() {
    // Mint an output of exactly p drops from nothing with value balance 0: Σ in − Σ out = −p,
    // which is 0 mod p. Field carries (c0 = p >> 16) would accept it; 3-bit carries can't.
    let w = action(&mut rng(43), [None, None], [0, 0]);
    let mut a = raw(&w);
    a.outputs[0].limbs = u64_to_limbs(u64::from(P));
    a.vb = [F::ZERO; 4];
    let carry = |k: usize, c: u32| -> Vec<Cheat> {
        let v = c + 4;
        if c > 3 {
            // Not representable in 3 bits: put it all in the first "bit".
            return vec![
                Cheat { row: BALANCE_ROW, col: CBITS + 3 * k, value: F::from_u32(v) },
                Cheat { row: BALANCE_ROW, col: CBITS + 3 * k + 1, value: F::ZERO },
                Cheat { row: BALANCE_ROW, col: CBITS + 3 * k + 2, value: F::ZERO },
            ];
        }
        (0..3).map(|i| Cheat { row: BALANCE_ROW, col: CBITS + 3 * k + i, value: F::from_bool((v >> i) & 1 == 1) }).collect()
    };
    let mut cheats = carry(0, P >> 16);
    cheats.extend(carry(1, 0));
    cheats.extend(carry(2, 0));
    assert_rejected(build_with(&a, &cheats), "mint p with field carries");
}

#[test]
fn sign_must_be_boolean() {
    // Honest balance −30. Sign 2 means multiplier −3, so |vb| = 10 balances in the field.
    let w = action(&mut rng(44), [None, None], [30, 0]);
    let mut a = raw(&w);
    a.vb_neg = F::TWO;
    a.vb = u64_to_limbs(10);
    assert_rejected(build(&a), "sign 2");
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

/// The cells a prover may legitimately choose, with the anchor, value balance and sighash
/// fixed (nullifiers and output commitments are outputs). Real spends have none: the note,
/// its position and so its nullifier are determined by the tree. Outputs choose owner and
/// randomness. Dummies choose key, randomness, path and position.
fn degrees_of_freedom(w: &ActionWitness) -> std::collections::HashSet<(usize, usize)> {
    let mut free = std::collections::HashSet::new();
    let mut add = |row: usize, cols: core::ops::Range<usize>| {
        for c in cols {
            free.insert((row, c));
        }
    };
    for j in 0..2 {
        let b = output_base(j);
        add(b, IN..IN + 8);
        add(b + 1, IN + 4..IN + 8);
        add(b + 2, IN..IN + 4);
    }
    for (i, s) in w.spends.iter().enumerate() {
        if !s.dummy {
            continue;
        }
        let b = spend_base(i);
        add(b + NK, IN..IN + 8);
        add(b + CM_B, IN + 4..IN + 8);
        add(b + CM_C, IN..IN + 4);
        for k in 0..32 {
            let sibling = if (s.path.position >> k) & 1 == 1 { IN..IN + 8 } else { IN + 8..IN + 16 };
            add(b + MERKLE + k, sibling);
            add(b + MERKLE + k, BIT..BIT + 1);
        }
    }
    free
}

/// The stronger sweep: change one input or register cell and let everything downstream follow
/// (as a cheating prover would), then check the whole trace. Only the declared degrees of
/// freedom may survive. This is what catches a permutation input or a register that nothing
/// pins, which the single-cell sweep can't see (the permutation's own constraints break first).
fn propagating_sweep(w: &ActionWitness) {
    let a = raw(w);
    let honest = build(&a);
    assert!(satisfied(&honest));
    let allowed = degrees_of_freedom(w);
    let mut free = Vec::new();
    for row in 0..HEIGHT {
        for col in (IN..IN + 16).chain(P2_COLS..WIDTH) {
            let value = honest.trace.values[row * WIDTH + col] + F::ONE;
            let b = build_with(&a, &[Cheat { row, col, value }]);
            if satisfied(&b) && !allowed.contains(&(row, col)) {
                free.push((row, col));
            }
        }
    }
    assert!(free.is_empty(), "cells a prover can change without breaking a constraint: {free:?}");
}

#[test]
fn sweep_two_real_spends() {
    let w = action(&mut rng(30), [Some(123_456_789), Some(42)], [100, 123_456_700]);
    sweep(&w, 31);
    propagating_sweep(&w);
}

#[test]
fn sweep_dummy_spends() {
    let w = action(&mut rng(32), [None, Some(u64::MAX)], [u64::MAX, 0]);
    sweep(&w, 33);
    propagating_sweep(&w);
    let w = action(&mut rng(34), [None, None], [7, 9]);
    sweep(&w, 35);
    propagating_sweep(&w);
}

#[test]
fn declared_freedoms_are_real() {
    // The other direction: each declared degree of freedom is accepted, so the list is exact.
    let w = action(&mut rng(37), [None, Some(5)], [4, 0]);
    let a = raw(&w);
    let honest = build(&a);
    for (row, col) in degrees_of_freedom(&w) {
        let old = honest.trace.values[row * WIDTH + col];
        let value = if col == BIT { F::ONE - old } else { old + F::ONE };
        assert!(satisfied(&build_with(&a, &[Cheat { row, col, value }])), "({row}, {col}) is not free");
    }
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

// ---- Hygiene ------------------------------------------------------------------------------

/// `{:?}` (logs, panics, `dbg!`) must never show a spending key, note contents or which note a
/// spend is (audit findings: the witnesses used to print `sk` verbatim, enough to spend).
#[test]
fn debug_output_redacts_secrets() {
    use crate::encryption::{MEMO_BYTES, TrialDecryptor, encrypt_note};

    let mut r = rng(11);
    let mut w = action(&mut r, [Some(123_456_789_012), None], [100_000_000_000, 23_456_789_012]);
    let key = SpendingKey::from_seed(random_bytes(&mut r));
    let fvk = key.full_viewing_key();
    let note = Note { value: 987_654_321_098, owner: fvk.owner(), rseed: random_bytes(&mut r) };
    w.spends[0] = SpendWitness::new(&key, note, w.spends[0].path.clone());

    // Everything a log line must not contain, in the forms `Debug`/`Display` would print it.
    let mut secrets: Vec<String> = Vec::new();
    for s in &w.spends {
        for x in s.sk.iter().chain(&s.note.rcm()).chain(&s.nullifier()) {
            secrets.extend([format!("{x:?}"), format!("{x}")]);
        }
        secrets.extend([format!("{:?}", s.note.rseed), s.note.value.to_string()]);
        secrets.extend(s.path.siblings.iter().flatten().map(|x| x.to_string()));
    }
    secrets.push(w.spends[1].path.position.to_string()); // the dummy's: random, so distinctive
    for n in &w.outputs {
        secrets.extend([format!("{:?}", n.rseed), n.value.to_string()]);
        secrets.extend(n.owner.iter().map(|x| x.to_string()));
    }
    let seed = key.seed();
    secrets.extend([format!("{seed:?}"), format!("{:?}", fvk.outgoing().0), format!("{:?}", fvk.to_bytes())]);
    secrets.extend(fvk.nk().iter().chain(&fvk.ak()).map(|x| x.to_string()));
    secrets.retain(|s| s.len() >= 6); // short numbers turn up anywhere
    assert!(secrets.len() > 300);

    let enc = encrypt_note(&note, &key.address(), &[7; MEMO_BYTES], Some(&fvk.outgoing()), &mut r).unwrap();
    let scanner = TrialDecryptor::new(&fvk.incoming());
    let decrypted = scanner.try_decrypt(&note.commitment(), &enc.enc).unwrap();

    let printed = [
        format!("{w:?}"),
        format!("{w:#?}"),
        format!("{:?}", w.spends),
        format!("{:#?}", w.outputs),
        format!("{:?}", w.spends[0].path),
        format!("{key:?} {fvk:?} {:?} {:?}", fvk.incoming(), fvk.outgoing()),
        format!("{decrypted:#?} {scanner:?}"),
    ];
    for p in &printed {
        for s in &secrets {
            assert!(!p.contains(s.as_str()), "{s} leaks into {p}");
        }
    }
    assert_eq!(format!("{:?}", w.spends[0]), "SpendWitness(..)");
    assert_eq!(format!("{note:?}"), "Note(..)");
    assert_eq!(format!("{:?}", w.spends[1].path), "MerklePath(..)");
    assert_eq!(format!("{decrypted:?} {scanner:?}"), "DecryptedNote(..) TrialDecryptor(..)");
    // The public parts still print, for debugging.
    assert!(printed[0].starts_with("ActionWitness { anchor: ["));
    assert!(printed[0].contains(&format!("{:?}", w.sighash)));
}
