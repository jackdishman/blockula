//! The wrapper against the crate, natively: what JavaScript is handed is what `blockula-ghost`
//! itself derives, and what it proves is what `blockula_ghost::verify` (the node's verifier) accepts.

use super::*;

fn statement(a: &GhostAction, sighash: [u8; 32], balance: i128) -> PublicInputs {
    let d = |b: Vec<u8>| digest_from_bytes(&b.try_into().unwrap()).unwrap();
    PublicInputs {
        anchor: a.anchor,
        nullifiers: [
            d(
                a.part(0, |i| digest_to_bytes(&a.spends[i].nullifier()).to_vec())
                    .unwrap(),
            ),
            d(
                a.part(1, |i| digest_to_bytes(&a.spends[i].nullifier()).to_vec())
                    .unwrap(),
            ),
        ],
        commitments: [a.outputs[0].0.commitment(), a.outputs[1].0.commitment()],
        value_balance: ValueBalance::new(balance).unwrap(),
        sighash,
    }
}

#[test]
fn the_key_is_the_crates_own() {
    let master = [7u8; 32];
    let key = GhostKey::derive(master.to_vec(), 0).unwrap();
    let theirs = SpendingKey::from_master(&master, 0).address();
    assert_eq!(key.address(), theirs.to_bytes().to_vec());
    assert_eq!(key.fingerprint(), theirs.fingerprint());
    assert_ne!(
        GhostKey::derive(master.to_vec(), 1).unwrap().address(),
        key.address()
    );
    assert!(GhostKey::derive(vec![7; 31], 0).is_err());
    assert_eq!(key.viewing("full").unwrap().len(), 161);
    assert_eq!(key.viewing("incoming").unwrap().len(), 97);
    assert_eq!(key.viewing("outgoing").unwrap().len(), 32);
    assert!(key.viewing("spending").is_err());
}

#[test]
fn shield_scan_spend_and_the_nodes_verifier_agrees() {
    let alice = GhostKey::derive([1u8; 32].to_vec(), 0).unwrap();
    let bob = GhostKey::derive([2u8; 32].to_vec(), 0).unwrap();
    let mut tree = GhostTree::new();

    // A shield: nothing spent, two notes for alice. Value balance −(60 + 40).
    let mut shield = GhostAction::start(&tree.root()).unwrap();
    assert!(shield.make_proof(&[9; 32]).is_err(), "not sealed yet");
    shield
        .add_output(&alice, &alice.address(), 60, b"")
        .unwrap();
    shield
        .add_output(&alice, &alice.address(), 40, b"")
        .unwrap();
    assert!(
        shield.add_output(&alice, &alice.address(), 1, b"").is_err(),
        "a third output"
    );
    shield.pad().unwrap();
    assert!(
        shield.add_output(&alice, &alice.address(), 1, b"").is_err(),
        "sealed"
    );
    assert_eq!(shield.value_balance(), "-100");
    let (public, proof) = shield.make_proof(&[9; 32]).unwrap();
    assert_eq!(public, statement(&shield, [9; 32], -100));
    assert_eq!(verify(&public, &proof), Ok(()));
    assert!(proof.len() <= MAX_PROOF_BYTES);
    // Bound to its sighash.
    assert!(verify(&statement(&shield, [8; 32], -100), &proof).is_err());

    // The chain appends both commitments; alice's scanner finds them, bob's doesn't.
    let cms: Vec<u8> = shield
        .outputs
        .iter()
        .flat_map(|(n, _)| digest_to_bytes(&n.commitment()))
        .collect();
    tree.extend(&cms).unwrap();
    assert!(tree.extend(&[0xff; 32]).is_err(), "not a canonical digest");
    assert!(tree.extend(&[0; 31]).is_err());
    assert_eq!(tree.size(), 2.0);
    let (scan_a, scan_b) = (GhostScanner::new(&alice), GhostScanner::new(&bob));
    let encs: Vec<Vec<u8>> = shield.outputs.iter().map(|(_, e)| e.enc.clone()).collect();
    let found = scan_a.try_decrypt(&cms[..32], &encs[0], 0).unwrap();
    assert_eq!((found.value(), found.memo()), (60, vec![]));
    assert!(scan_b.try_decrypt(&cms[..32], &encs[0], 0).is_none());
    assert!(
        scan_a.try_decrypt(&cms[32..], &encs[0], 1).is_none(),
        "the wrong commitment"
    );

    // A private transfer: both notes spent, 70 to bob with a memo, 27 back, fee 3.
    let mut pay = GhostAction::start(&tree.root()).unwrap();
    assert!(
        pay.add_spend(&bob, &tree, 0, &encs[0]).is_err(),
        "not bob's note"
    );
    assert!(
        pay.add_spend(&alice, &tree, 2, &encs[0]).is_err(),
        "not in the tree"
    );
    assert_eq!(pay.add_spend(&alice, &tree, 0, &encs[0]).unwrap(), 60);
    assert_eq!(pay.add_spend(&alice, &tree, 1, &encs[1]).unwrap(), 40);
    assert!(
        pay.add_spend(&alice, &tree, 1, &encs[1]).is_err(),
        "a third spend"
    );
    pay.add_output(&alice, &bob.address(), 70, b"for the garlic")
        .unwrap();
    pay.add_output(&alice, &alice.address(), 27, b"").unwrap();
    pay.pad().unwrap();
    assert_eq!(pay.value_balance(), "3");
    let (public, proof) = pay.make_proof(&[5; 32]).unwrap();
    assert_eq!(public, statement(&pay, [5; 32], 3));
    assert_eq!(verify(&public, &proof), Ok(()));
    // The nullifiers are the ones the scanner said to watch for.
    assert_eq!(
        digest_to_bytes(&public.nullifiers[0]).to_vec(),
        found.nullifier()
    );
    // Bob finds his note and its memo.
    let theirs = scan_b
        .try_decrypt(
            &digest_to_bytes(&public.commitments[0]),
            &pay.outputs[0].1.enc,
            2,
        )
        .unwrap();
    assert_eq!(
        (theirs.value(), theirs.memo()),
        (70, b"for the garlic".to_vec())
    );

    // The same note twice is refused before any proof.
    let mut twice = GhostAction::start(&tree.root()).unwrap();
    twice.add_spend(&alice, &tree, 0, &encs[0]).unwrap();
    twice.add_spend(&alice, &tree, 0, &encs[0]).unwrap();
    twice.pad().unwrap();
    assert!(twice.make_proof(&[5; 32]).is_err());

    // A stale anchor is refused when the spend is added.
    let stale = tree.root();
    tree.extend(&digest_to_bytes(&public.commitments[0]))
        .unwrap();
    let mut old = GhostAction::start(&stale).unwrap();
    assert!(old.add_spend(&alice, &tree, 0, &encs[0]).is_err());
}
