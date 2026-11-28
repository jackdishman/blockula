//! `blockula ghost` against a real node over HTTP (a summoned chain with Ghost Mode on, making
//! blocks in the background): the commands as a person runs them, with real proofs.

use super::*;
use crate::chain::SummonOpts;
use crate::types::DROPS_PER_FANG;
use crate::validator_cli::tests::local_node_with;
use crate::validator_cli::{wallet, WalletCmd};

const FANG: u64 = DROPS_PER_FANG;

fn who(rpc: &str) -> Who {
    Who { key: None, rpc: rpc.to_string(), pin: ChainPin::default() }
}

/// A CLI wallet in its own directory, funded from the faucet. Returns the directory and its address.
fn new_wallet(root: &Path, name: &str, rpc: &str) -> (PathBuf, String) {
    let dir = root.join(name);
    wallet(WalletCmd::New { rpc: None, prefix: None }, &dir).unwrap();
    wallet(WalletCmd::Faucet { rpc: rpc.to_string(), to: None }, &dir).unwrap();
    let address = wallet_keys(&dir).unwrap().address();
    (dir, address)
}

fn merges(values: &[u64], need: u64) -> Option<usize> {
    merges_needed(values, need, 4)
}

/// The acceptance flow: shield → scan → a private send to a second wallet (three notes, so a
/// merge first) → the recipient's scan finds it → an unshield to a public account → the wallets'
/// balances, the pool and `/api/ghost` add up, and the chain's invariants hold.
#[test]
fn shield_scan_send_and_unshield_end_to_end() {
    let (root, _homes, chain, _rt, url) = local_node_with("ghostcli", SummonOpts::new("ghostcli", 1).with_ghost(1_000_000 * FANG));
    let (alice, alice_account) = new_wallet(&root, "alice", &url);
    let (bob, bob_account) = new_wallet(&root, "bob", &url);
    let balance = |account: &str| chain.lock().state.account(account).balance;
    let pool_now = || chain.lock().state.ghost.meta.pool;

    // Nothing shielded yet, and nobody to send to.
    let s = scan(&alice, &who(&url)).unwrap();
    assert_eq!((s.balance, s.leaves, s.unspent.len()), (0, 0, 0));
    let e = send(&alice, &who(&url), &bob_account, FANG, "", Some("0000-0000-0000-0000-0000"), FANG, true).unwrap_err().to_string();
    assert!(e.contains("hasn't published a Ghost Mode address"), "{e}");

    // Bob publishes his address (tag 53); the chain files it under his account.
    address(&bob, &who(&url), true, None).unwrap();
    let bobs = ghost_key(&wallet_keys(&bob).unwrap()).address();
    assert_eq!(chain.lock().state.ghost.addresses.get(&bob_account).map(|a| a.to_vec()), Some(bobs.to_bytes().to_vec()));

    // Alice shields 40, 30 and 20 FANG: three notes.
    let before = balance(&alice_account);
    let mut shield_fees = 0;
    for amount in [40, 30, 20] {
        let sent = shield(&alice, &who(&url), amount * FANG, None).unwrap();
        assert!(sent.height.is_some() && sent.bytes > 120_000 && sent.bytes < 262_144, "a real proof: {} bytes", sent.bytes);
        shield_fees += sent.fee;
    }
    assert_eq!(pool_now(), 90 * FANG);
    let paid = before - balance(&alice_account);
    assert!(paid > 90 * FANG && paid <= 90 * FANG + shield_fees + 3 * FANG / 1_000, "the amounts, and at most the fee ceilings and verify fees: {paid}");
    let s = scan(&alice, &who(&url)).unwrap();
    assert_eq!((s.balance, s.unspent.len(), s.leaves, s.new_leaves), (90 * FANG, 3, 6, 0), "the shields scanned as they went");

    // More than she has, in a sentence; nothing is sent.
    let e = send(&alice, &who(&url), &bob_account, 90 * FANG, "", Some(&bobs.fingerprint()), FANG, true).unwrap_err().to_string();
    assert!(e.contains("This wallet has 90 FANG shielded in 3 notes") && e.contains("Nothing was sent"), "{e}");
    // A wrong fingerprint, and a fee above --max-fee, are refused before anything is proved.
    let e = send(&alice, &who(&url), &bob_account, FANG, "", Some("0000-0000-0000-0000-0000"), FANG, true).unwrap_err().to_string();
    assert!(e.contains(&bobs.fingerprint()) && e.contains("Nothing was sent"), "{e}");
    let e = send(&alice, &who(&url), &bob_account, FANG, "", Some(&bobs.fingerprint()), 1, true).unwrap_err().to_string();
    assert!(e.contains("--max-fee"), "{e}");
    // --yes with no fingerprint would pay whatever address the node reports, unchecked: refused.
    let e = send(&alice, &who(&url), &bob_account, FANG, "", None, FANG, true).unwrap_err().to_string();
    assert!(e.contains("--yes needs --fingerprint") && e.contains("Nothing was sent"), "{e}");
    // And with nobody at a terminal to ask (a test's stdin), it's the fingerprint or nothing.
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        let e = send(&alice, &who(&url), &bob_account, FANG, "", None, FANG, false).unwrap_err().to_string();
        assert!(e.contains("--fingerprint") && e.contains("Nothing was sent"), "{e}");
    }
    assert_eq!(scan(&alice, &who(&url)).unwrap().balance, 90 * FANG, "none of those spent a note");
    assert_eq!(pool_now(), 90 * FANG);

    // 80 FANG to bob takes all three notes: one merge (40 + 30), then the payment.
    let sent = send(&alice, &who(&url), &bob_account, 80 * FANG, "for the garlic", Some(&bobs.fingerprint()), FANG, false).unwrap();
    assert_eq!(sent.len(), 2, "a merge, then the payment");
    assert!(sent[0].what.contains("merged 2 notes") && sent[1].what.contains("sent 80 FANG privately"), "{} / {}", sent[0].what, sent[1].what);
    let fee = sent[1].fee;
    assert_eq!(sent[0].fee, fee, "both pay the canonical fee");
    let s = scan(&alice, &who(&url)).unwrap();
    assert_eq!((s.balance, s.unspent.len()), (10 * FANG - 2 * fee, 1), "her change");

    // Bob's scan finds the note, with its memo.
    let s = scan(&bob, &who(&url)).unwrap();
    assert_eq!((s.balance, s.found, s.unspent.len(), s.leaves), (80 * FANG, 1, 1, 10));
    assert_eq!(s.unspent[0].memo, "for the garlic");
    assert_eq!(scan(&bob, &who(&url)).unwrap().found, 0, "a second scan starts where the first stopped");

    // He unshields 25.5 FANG to an account that doesn't exist yet: the fee carries its creation.
    let carol = crate::crypto::Keys::generate().address();
    let sent = unshield(&bob, &who(&url), &carol, 25 * FANG + FANG / 2, FANG).unwrap();
    assert_eq!(sent.len(), 1);
    let fee_new = sent[0].fee;
    assert_eq!(fee_new, fee + chain.lock().state.chain_params.account_creation_fee);
    assert_eq!(balance(&carol), 25 * FANG + FANG / 2);
    // And 4.5 to alice's account, which exists: the plain fee.
    let before = balance(&alice_account);
    let sent = unshield(&bob, &who(&url), &alice_account, 4 * FANG + FANG / 2, FANG).unwrap();
    assert_eq!((sent[0].fee, balance(&alice_account) - before), (fee, 4 * FANG + FANG / 2));

    // It all adds up: what the wallets hold is what the pool holds, and what left it is the two
    // unshields and the four fees.
    let (a, b) = (scan(&alice, &who(&url)).unwrap(), scan(&bob, &who(&url)).unwrap());
    assert_eq!(b.balance, 50 * FANG - fee_new - fee);
    assert_eq!(a.balance + b.balance, pool_now());
    assert_eq!(pool_now(), 90 * FANG - 30 * FANG - 3 * fee - fee_new);
    let api = rt().unwrap().block_on(get(&url, "/ghost")).unwrap();
    assert_eq!(api["pool"].as_str(), Some(pool_now().to_string().as_str()));
    assert_eq!((api["frontier_size"].as_u64(), api["nullifiers"].as_u64()), (Some(14), Some(14)), "seven actions");
    chain.lock().state.check_invariants().unwrap();

    // The note store: where the docs say, private, and a cache (a fresh scan rebuilds it).
    let instance = hex::encode(chain.lock().state.ghost.meta.instance);
    let store = alice.join("ghost").join(format!("{instance}.json"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&store).unwrap().permissions().mode() & 0o777, 0o600);
    }
    std::fs::remove_file(&store).unwrap();
    let again = scan(&alice, &who(&url)).unwrap();
    assert_eq!((again.balance, again.new_leaves, again.unspent.len()), (a.balance, 14, 1), "rebuilt from the chain, spent notes marked");
    // A store that isn't this wallet's is refused in a sentence, not used.
    std::fs::copy(bob.join("ghost").join(format!("{instance}.json")), &store).unwrap();
    let e = scan(&alice, &who(&url)).unwrap_err().to_string();
    assert!(e.contains("belongs to another wallet key") && e.contains("scan` again"), "{e}");
    std::fs::remove_dir_all(root).ok();
}

/// On a chain without Ghost Mode every command says so in a sentence, before it signs anything.
#[test]
fn a_chain_without_ghost_mode_says_so() {
    let (root, _homes, chain, _rt, url) = local_node_with("ghostoff", SummonOpts::new("ghostoff", 1));
    let (dir, _) = new_wallet(&root, "w", &url);
    let off = |e: anyhow::Error| assert!(e.to_string().contains("Ghost Mode is off") && e.to_string().contains("blockula dev --ghost"), "{e}");
    off(scan(&dir, &who(&url)).err().unwrap());
    off(shield(&dir, &who(&url), FANG, None).err().unwrap());
    off(send(&dir, &who(&url), "vlad.boo", FANG, "", Some("0000-0000-0000-0000-0000"), FANG, true).err().unwrap());
    off(unshield(&dir, &who(&url), "vlad.boo", FANG, FANG).err().unwrap());
    off(address(&dir, &who(&url), true, None).err().unwrap());
    // The fingerprint needs no chain.
    address(&dir, &who(&url), false, None).unwrap();
    assert_eq!(chain.lock().state.ghost.meta.pool, 0);
    std::fs::remove_dir_all(root).ok();
}

/// The ghost key is the wallet seed's account 0, apart from its signing key, and the same every time.
#[test]
fn the_ghost_key_is_account_0_of_the_wallet_seed() {
    let keys = Keys::from_seed([9; 32]);
    let key = ghost_key(&keys);
    assert_eq!(key.address(), SpendingKey::from_master(&[9; 32], 0).address());
    assert_eq!(key.address(), ghost_key(&Keys::from_seed([9; 32])).address());
    assert_ne!(key.address(), SpendingKey::from_master(&[9; 32], 1).address());
    assert_ne!(key.address(), SpendingKey::from_seed([9; 32]).address(), "derived, never the seed itself");
    assert_ne!(key.address(), ghost_key(&Keys::from_seed([8; 32])).address());
    // Nothing that holds it prints it.
    assert_eq!(format!("{key:?}"), "SpendingKey(..)");
}

/// One action spends two notes: how many merges come first, and when nothing the wallet holds covers it.
#[test]
fn merges_are_counted_before_anything_is_sent() {
    assert_eq!(merges(&[], 1), None);
    assert_eq!(merges(&[10], 10), Some(0));
    assert_eq!(merges(&[10], 11), None);
    assert_eq!(merges(&[10, 5], 15), Some(0));
    assert_eq!(merges(&[10, 5, 5], 16), Some(1), "10 + 5 − 4 + 5");
    assert_eq!(merges(&[10, 5, 5], 17), None, "the merge's fee comes out of it");
    assert_eq!(merges(&[40, 30, 20, 10], 88), Some(2));
    assert_eq!(merges(&[10, 5, 4], 16), None, "a note worth the fee adds nothing");
    assert_eq!(merges(&[u64::MAX, u64::MAX, u64::MAX], u64::MAX), Some(0), "no overflow");
}

/// A memo over 256 bytes and a zero amount are refused before any node is asked.
#[test]
fn arguments_are_checked_offline() {
    let dir = std::env::temp_dir().join(format!("blockula-ghostargs-{}-{}", crate::chain::now_ms(), std::process::id()));
    let nowhere = who("http://127.0.0.1:9");
    assert!(send(&dir, &nowhere, "vlad.boo", FANG, &"x".repeat(257), None, FANG, false).unwrap_err().to_string().contains("at most 256"));
    assert!(send(&dir, &nowhere, "vlad.boo", 0, "", None, FANG, false).unwrap_err().to_string().contains("above 0"));
    // --yes without --fingerprint is refused before any node is asked for an address.
    let e = send(&dir, &nowhere, "vlad.boo", FANG, "", None, FANG, true).unwrap_err().to_string();
    assert!(e.contains("--yes needs --fingerprint") && e.contains("vlad.boo") && e.contains("Nothing was sent"), "{e}");
    assert!(shield(&dir, &nowhere, 0, None).unwrap_err().to_string().contains("above 0"));
    assert!(unshield(&dir, &nowhere, "vlad.boo", 0, FANG).unwrap_err().to_string().contains("above 0"));
}
