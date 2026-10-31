//! The mempool's side of programs (WBS S3's acceptance), on a running dev chain with programs on
//! at genesis: a sender's queue judged against its program transactions' worst case, the cap on
//! pending uploads and on an upload's size, and a create or call queued behind its sender's own
//! pending upload or create refused with the block it's usable from. The mempool never counts the
//! block's fuel.

use super::*;
use crate::chain::Chain;
use crate::programs::fixtures::*;
use crate::programs::state::BlockRun;
use crate::types::{Binding, HexBytes, InitCall, ProgramPolicy, DROPS_PER_FANG, WASM_HEADER};

/// What every test's txs pay at most: a hundredth of a FANG covers the bytes and the fuel of the
/// small uploads, creates and calls here (only the charge is debited).
const FEE: u64 = DROPS_PER_FANG / 100;

/// A dev chain (programs on at genesis, `params` overriding) with each of `funds`' keys funded with
/// its FANG in block 1.
fn funded(tag: &str, params: &[(&str, u64)], funds: &[(&crate::crypto::Keys, u64)]) -> (std::path::PathBuf, Chain) {
    let (home, mut c) = dev_chain(tag, params);
    for (k, fang) in funds {
        c.faucet(&k.address(), fang * FANG).unwrap();
    }
    c.produce_block().unwrap();
    assert_eq!(super::super::status(&c.state, c.height() + 1), super::super::Status::On);
    (home, c)
}

/// The worst case of each tag (§6.4): an upload's `storage_limit`; a create's `init.value` and
/// `storage_limit`, and the account creation fee when its address is new (not when a balance was
/// sent ahead to it); a call's values and `storage_limit`; nothing for any other tx; an overflow
/// is refused rather than wrapped.
#[test]
fn the_worst_case_of_each_tag() {
    let mut st = state_on();
    let creation = st.chain_params.account_creation_fee;
    let up = tx(&st, ALICE, upload(COUNTER.0, COUNTER.1, 7), 3);
    assert_eq!(worst_case(&st, &up).unwrap(), 7);
    let init = |value: u64| Action::CreateProgram {
        wasm_hash: "ab".repeat(32),
        interface_hash: "cd".repeat(32),
        authority: None,
        upgrade_delay_blocks: 0,
        policy: ProgramPolicy::Open,
        accepts_transfers: false,
        init: Some(InitCall { method: "init".into(), input: HexBytes(vec![]), value }),
        fuel_limit: 1,
        storage_limit: 11,
        bindings: vec![],
    };
    let made = tx(&st, ALICE, init(5), 3);
    assert_eq!(worst_case(&st, &made).unwrap(), 5 + 11 + creation);
    let bare = tx(&st, ALICE, create(&[1; 32], &[2; 32]), 3);
    assert_eq!(worst_case(&st, &bare).unwrap(), FANG + creation);
    // A balance sent ahead to the address: no account to open.
    st.accounts.insert(program_of(ALICE, made.nonce), crate::chain::Account { balance: 1, ..Default::default() });
    assert_eq!(worst_case(&st, &made).unwrap(), 5 + 11);
    let digest = "d2".repeat(32);
    let batch = tx(&st, ALICE, Action::CallProgram { calls: vec![call(&program_of(BOB, 0), &digest, "a", 3), call(&program_of(BOB, 0), &digest, "b", 4)], fuel_limit: 1, storage_limit: 13, bindings: vec![] }, 3);
    assert_eq!(worst_case(&st, &batch).unwrap(), 3 + 4 + 13);
    let transfer = tx(&st, ALICE, Action::Transfer { to: addr(BOB), amount: 9 }, 3);
    assert_eq!(worst_case(&st, &transfer).unwrap(), 0);
    let huge = tx(&st, ALICE, Action::CallProgram { calls: vec![call(&program_of(BOB, 0), &digest, "a", u64::MAX)], fuel_limit: 1, storage_limit: 1, bindings: vec![] }, 3);
    assert_eq!(worst_case(&st, &huge).unwrap_err().to_string(), "the FANG this transaction could move overflows");
}

/// A sender's queue is judged against the worst its program transactions could cost (§6.4): with
/// an upload that may bond 95 of its 100 FANG queued, a transfer of 5 FANG behind it is refused
/// (it couldn't pay once the upload bonds all it may), while 4 FANG fits; and an upload whose worst
/// case its sender can't cover never enters the mempool. The mempool never counts the block's fuel.
#[test]
fn a_senders_queue_is_judged_against_the_worst_case() {
    let (alice, carol) = (keys(61), keys(63));
    let (home, mut c) = funded("worst", &[], &[(&alice, 100), (&carol, 10)]);
    let me = c.me().unwrap();
    c.submit(signed(&c, &alice, 0, FEE, upload(COUNTER.0, COUNTER.1, 95 * FANG))).unwrap();
    let pay = |c: &Chain, fang: u64| signed(c, &alice, 1, FEE, Action::Transfer { to: me.clone(), amount: fang * FANG });
    let e = format!("{:#}", c.submit(pay(&c, 5)).unwrap_err());
    assert!(e.contains("insufficient balance"), "{e}");
    c.submit(pay(&c, 4)).unwrap();
    // Carol's upload may bond twice what she has.
    let e = format!("{:#}", c.submit(signed(&c, &carol, 0, FEE, upload(COUNTER.0, COUNTER.1, 20 * FANG))).unwrap_err());
    assert!(e.starts_with("its worst case is 20 FANG beyond the fee: insufficient balance"), "{e}");
    // Without the upload ahead of it, Alice's 5 FANG would have gone (Carol's 5 FANG does).
    c.submit(signed(&c, &carol, 0, FEE, Action::Transfer { to: me.clone(), amount: 5 * FANG })).unwrap();
    assert_eq!(c.state.programs.block, BlockRun::default(), "the mempool's simulations count no fuel");
    drop(c);
    std::fs::remove_dir_all(home).ok();
}

/// At most 16 uploads wait in the mempool (§2.2, A38): the 17th is refused, from any sender, while
/// other transactions still get in; once a block has taken them, uploads are admitted again.
#[test]
fn the_seventeenth_pending_upload_is_refused() {
    let (alice, bob) = (keys(61), keys(62));
    let (home, mut c) = funded("uploads", &[], &[(&alice, 100), (&bob, 100)]);
    for n in 0..MAX_MEMPOOL_UPLOADS as u64 {
        c.submit(signed(&c, &alice, n, FEE, upload(COUNTER.0, COUNTER.1, 0))).unwrap();
    }
    for (k, nonce) in [(&alice, MAX_MEMPOOL_UPLOADS as u64), (&bob, 0)] {
        assert_eq!(format!("{:#}", c.submit(signed(&c, k, nonce, FEE, upload(COUNTER.0, COUNTER.1, 0))).unwrap_err()), "too many pending uploads; try again shortly");
    }
    let me = c.me().unwrap();
    c.submit(signed(&c, &bob, 0, FEE, Action::Transfer { to: me, amount: FANG })).unwrap();
    // A block takes them all (each upload charged, and failing: a `storage_limit` of 0 can't bond
    // its deposits).
    c.produce_block().unwrap();
    assert!(c.mempool.is_empty());
    let b = c.last_block().unwrap();
    assert_eq!((b.txs.len(), b.skipped.len(), b.failed.len()), (MAX_MEMPOOL_UPLOADS + 1, 0, MAX_MEMPOOL_UPLOADS));
    c.submit(signed(&c, &bob, 1, FEE, upload(COUNTER.0, COUNTER.1, 0))).unwrap();
    drop(c);
    std::fs::remove_dir_all(home).ok();
}

/// An upload larger than the next block may carry (`max_block_bytes`, a parameter whose floor of
/// 64 KiB is below an upload's own bound) is refused at the door: no proposer could include it.
#[test]
fn an_upload_larger_than_a_block_is_refused() {
    let alice = keys(61);
    let (home, mut c) = funded("large", &[("max_block_bytes", 64 * 1024)], &[(&alice, 100)]);
    let mut wasm = WASM_HEADER.to_vec();
    wasm.resize(100 * 1024, 0);
    let large = signed(&c, &alice, 0, FEE, upload(&wasm, COUNTER.1, 0));
    let next = c.height() + 1;
    assert_eq!(format!("{:#}", c.submit(large.clone()).unwrap_err()), format!("this upload is {} bytes, more than block {next} may carry (65536 bytes)", large.size()));
    c.submit(signed(&c, &alice, 0, FEE, upload(COUNTER.0, COUNTER.1, 0))).unwrap();
    drop(c);
    std::fs::remove_dir_all(home).ok();
}

/// One step per block (§2.1, A5): the mempool's simulations write no records, so a create queued
/// behind its sender's own pending upload of its code, and a call (or a create's binding) behind
/// its sender's own pending create of the program, are refused at the door, saying from which block
/// they can go in. Another sender's create of code nobody stored is refused as unstored, and a
/// create of code stored already isn't held back by its sender's pending upload of the same code
/// (which only refreshes it).
#[test]
fn a_create_behind_its_senders_pending_upload_is_refused_with_the_block_its_usable_from() {
    let (alice, bob, carol) = (keys(61), keys(62), keys(63));
    let (home, mut c) = funded("behind", &[], &[(&alice, 100), (&bob, 100), (&carol, 100)]);
    let next = c.height() + 1;
    c.submit(signed(&c, &alice, 0, FEE, upload(TOKEN.0, TOKEN.1, FANG))).unwrap();
    let (code, interface) = (digest::wasm_hash(TOKEN.0), digest::interface_hash(TOKEN.1));
    let make = |c: &Chain, k: &crate::crypto::Keys, nonce: u64| signed(c, k, nonce, FEE, create(&code, &interface));
    let e = format!("{:#}", c.submit(make(&c, &alice, 1)).unwrap_err());
    assert_eq!(e, format!("your pending upload (nonce 0) stores code this create needs, and code is usable from the block after its upload: from block {} at the earliest. Send the create once the upload is final", next + 1));
    let e = format!("{:#}", c.submit(make(&c, &bob, 0)).unwrap_err());
    assert!(e.starts_with("code ") && e.ends_with(" isn't stored: upload it first"), "{e}");
    // The counter's code stored already (as an upload in block 0 would have left it): Bob's create
    // of it goes in, and a call to the program it makes, or a create binding it, waits.
    let blobs = store(&mut c.state, COUNTER.0, COUNTER.1, 0);
    c.submit(signed(&c, &bob, 0, FEE, create(&blobs.0, &blobs.1))).unwrap();
    let program = super::super::digest::program_address(c.state.params.prefix(), &crate::crypto::address_bytes(&bob.address()).unwrap(), 0);
    let wait = format!("your pending create (nonce 0) makes program {}, and a program is usable from the block after its creation: from block {} at the earliest. Send this once the create is final", crate::chain::short(&program), next + 1);
    let increment = signed(&c, &bob, 1, FEE, calls(vec![call(&program, &"d2".repeat(32), "increment", 0)], 100_000, vec![]));
    assert_eq!(format!("{:#}", c.submit(increment).unwrap_err()), wait);
    let binds = Action::CreateProgram {
        wasm_hash: hex::encode(blobs.0),
        interface_hash: hex::encode(blobs.1),
        authority: None,
        upgrade_delay_blocks: 0,
        policy: ProgramPolicy::Open,
        accepts_transfers: false,
        init: Some(InitCall { method: "init".into(), input: HexBytes(vec![]), value: 0 }),
        fuel_limit: 100_000,
        storage_limit: FANG,
        bindings: vec![Binding { program: program.clone(), version_digest: hex::encode(blockula_vm::exec::ANY_VERSION) }],
    };
    assert_eq!(format!("{:#}", c.submit(signed(&c, &bob, 1, FEE, binds)).unwrap_err()), wait);
    // Carol's pending upload of the counter only refreshes code stored already: her create of it
    // doesn't need it, and goes in behind it.
    c.submit(signed(&c, &carol, 0, FEE, upload(COUNTER.0, COUNTER.1, FANG))).unwrap();
    c.submit(signed(&c, &carol, 1, FEE, create(&blobs.0, &blobs.1))).unwrap();
    // Alice's call to it isn't behind a create of hers: admission judges it (no program there yet).
    let e = format!("{:#}", c.submit(signed(&c, &alice, 1, FEE, calls(vec![call(&program, &"d2".repeat(32), "increment", 0)], 100_000, vec![]))).unwrap_err());
    assert_eq!(e, format!("{} isn't a program", crate::chain::short(&program)));
    drop(c);
    std::fs::remove_dir_all(home).ok();
}
