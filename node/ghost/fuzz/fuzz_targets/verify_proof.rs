//! Fuzzes `verify`: it must return an error, never panic or abort, for anything but the one
//! honest proof. The first input byte picks the mode: raw bytes as a proof, or a list of
//! (offset, xor) edits applied to an honest proof (structured mutations that get past the
//! decoder into the verifier).

#![no_main]

use std::sync::OnceLock;

use blockula_ghost::*;
use libfuzzer_sys::fuzz_target;
use rand::SeedableRng;
use rand::rngs::StdRng;

fn honest() -> &'static (PublicInputs, Vec<u8>) {
    static P: OnceLock<(PublicInputs, Vec<u8>)> = OnceLock::new();
    P.get_or_init(|| {
        let key = SpendingKey::from_seed([1; 32]);
        let owner = key.full_viewing_key().owner();
        let note = Note { value: 10, owner, rseed: [2; 32] };
        let mut tree = FullTree::new();
        let pos = tree.append(note.commitment()).unwrap();
        let w = ActionWitness {
            spends: [SpendWitness::new(&key, note, tree.path(pos).unwrap()), SpendWitness::dummy(&mut StdRng::seed_from_u64(3))],
            outputs: [Note { value: 9, owner, rseed: [4; 32] }, Note { value: 0, owner, rseed: [5; 32] }],
            anchor: tree.root(),
            sighash: [6; 32],
        };
        blockula_ghost::circuit::prove_with_seed(&w, [7; 32]).unwrap()
    })
}

fuzz_target!(|data: &[u8]| {
    let (public, proof) = honest();
    let Some((&mode, rest)) = data.split_first() else { return };
    let candidate = if mode & 1 == 0 {
        rest.to_vec()
    } else {
        let mut p = proof.clone();
        for edit in rest.chunks_exact(4) {
            let at = u32::from_le_bytes([edit[0], edit[1], edit[2], 0]) as usize % p.len();
            p[at] ^= edit[3].max(1);
        }
        p
    };
    if candidate != *proof {
        assert!(verify(public, &candidate).is_err());
    }
});
