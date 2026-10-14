//! Circuit tests that need the trace internals: completeness, the negative soundness list,
//! the cell-perturbation sweep.

use p3_field::PrimeCharacteristicRing;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use super::*;
use crate::tree::FullTree;

/// A wallet with notes in a tree full of other people's notes.
pub(crate) struct Fixture {
    pub key: SpendingKey,
    pub tree: FullTree,
    pub notes: Vec<(Note, u32)>,
}

pub(crate) fn fixture(rng: &mut StdRng, values: &[u64]) -> Fixture {
    let key = SpendingKey::from_seed(random_bytes(rng));
    let owner = key.full_viewing_key().owner();
    let mut tree = FullTree::new();
    let mut notes = Vec::new();
    for &value in values {
        for _ in 0..(rng.next_u32() % 3) {
            let other = Note { value: rng.next_u64(), owner: xof_to_digest("t", &random_bytes::<_, 8>(rng)), rseed: random_bytes(rng) };
            tree.append(other.commitment()).unwrap();
        }
        let note = Note { value, owner, rseed: random_bytes(rng) };
        let pos = tree.append(note.commitment()).unwrap();
        notes.push((note, pos));
    }
    Fixture { key, tree, notes }
}

/// An action spending `ins` (None = dummy) and creating `outs` to a random recipient.
pub(crate) fn action(rng: &mut StdRng, ins: [Option<u64>; 2], outs: [u64; 2]) -> ActionWitness {
    let real: Vec<u64> = ins.iter().flatten().copied().collect();
    let fx = fixture(rng, &real);
    let mut real_notes = fx.notes.iter();
    let spends = ins.map(|v| match v {
        Some(_) => {
            let (note, pos) = real_notes.next().unwrap();
            SpendWitness::new(&fx.key, *note, fx.tree.path(*pos).unwrap())
        }
        None => SpendWitness::dummy(rng),
    });
    let recipient = SpendingKey::from_seed(random_bytes(rng)).full_viewing_key().owner();
    let outputs = outs.map(|value| Note { value, owner: recipient, rseed: random_bytes(rng) });
    ActionWitness { spends, outputs, anchor: fx.tree.root(), sighash: random_bytes(rng) }
}

#[test]
fn honest_action_proves_and_verifies() {
    let mut rng = StdRng::seed_from_u64(1);
    let w = action(&mut rng, [Some(1000), Some(234)], [1200, 30]);
    let public = check_witness(&w).expect("satisfied");
    assert_eq!(public.value_balance, ValueBalance::new(4).unwrap());
    let t = std::time::Instant::now();
    let (public2, proof) = prove_with_seed(&w, [9; 32]).expect("proves");
    eprintln!("prove {:?}, {} bytes", t.elapsed(), proof.len());
    assert_eq!(public, public2);
    let t = std::time::Instant::now();
    verify(&public, &proof).expect("verifies");
    eprintln!("verify {:?}", t.elapsed());
}
