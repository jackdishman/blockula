//! The replay gate on recordings: green on the committed corpus, and red, at the height where it
//! starts, on a binary that diverges from it.

use super::corpus::*;
use super::*;
use crate::chain::Genesis;
use crate::types::Action;
use std::collections::BTreeMap;

/// A recorder for a two-validator chain (`tag`'s), its accounts `alice` and `bob`.
fn smoke(tag: &str) -> Recorder {
    let names = ["v0", "v1", "o0", "o1", "alice", "bob"];
    let k = keys_of(&names);
    let p = "tboo";
    let a = |n: &str| k[n].address_with(p);
    let (v0, op0) = validator("replay-smoke", "v0", &a("o0"), 2_000);
    let (v1, op1) = validator("replay-smoke", "v1", &a("o1"), 2_000);
    let g: Genesis = serde_json::from_value(serde_json::json!({
        "chain_id": "replay-smoke", "token": "FANG", "decimals": 9, "block_time_ms": 1000, "faucet": false, "created_at_ms": 1_790_000_000_000u64,
        "validators": [v0, v1], "operators": [op0, op1], "epoch_blocks": 10, "address_prefix": p,
        "params": { "min_self_bond": (1_000 * FANG).to_string() },
        "allocations": [alloc(a("o0"), 5_000, None, "o0"), alloc(a("o1"), 5_000, None, "o1"), alloc(a("alice"), 100, None, "alice")],
    }))
    .unwrap();
    Recorder::new(tag, &g, keys_of(&names), &[seed("v0"), seed("v1")])
}

/// Replays the archive in `dir` (homes in `dir` too).
fn replay_in(dir: &Path) -> Result<Report> {
    run(&Opts { archive: dir.join("archive"), home: Some(dir.join("homes")), to: None, prune_blocks: None, checkpoints: vec![] })
}

/// The smoke chain with a transfer, a failed transfer, a junk tx and a few empty epochs: what the
/// recorder writes (gzipped, as the archive script closes its files) replays, block for block,
/// with both paths.
#[test]
fn a_recording_replays() {
    let mut r = smoke("smoke");
    let (alice, bob) = (r.addr("alice"), r.addr("bob"));
    let pay = r.tx("alice", Action::Transfer { to: bob.clone(), amount: 5 * FANG });
    let too_much = r.tx("alice", Action::Transfer { to: bob.clone(), amount: 1_000 * FANG });
    let junk = r.tx_with("alice", &alice, Action::Transfer { to: bob, amount: FANG }, |t| t.nonce = 7);
    r.block(vec![(pay, Want::Applied), (too_much, Want::Failed("insufficient")), (junk, Want::Skipped)]);
    r.skip(2);
    r.until(25);
    let dir = scratch("smoke");
    r.write(&dir.join("archive"), "a_recording_replays", true);
    let report = replay_in(&dir).unwrap();
    assert_eq!((report.height, report.recorded, report.startup), (25, 25, Some(25)));
    assert_eq!((report.txs(), report.tags.len()), ([1, 1, 1], 1));
    std::fs::remove_dir_all(dir).ok();
}

/// A finalized block `consensus::verify` refuses fails the replay, whatever its certificate: here
/// one that comes a millisecond sooner than a block time after its parent. Execution never reads a
/// block's time; the replay runs `verify`'s header checks (`consensus::check_header`).
#[test]
fn a_block_verify_refuses_fails_the_gate() {
    let mut r = smoke("early");
    r.until(5);
    r.rush(1);
    r.until(12);
    let dir = scratch("early");
    r.write(&dir.join("archive"), "a_block_verify_refuses_fails_the_gate", false);
    let err = format!("{:#}", replay_in(&dir).unwrap_err());
    assert!(err.contains("block 6: block too early"), "{err}");
    std::fs::remove_dir_all(dir).ok();
}

/// The open chain's genesis is one a node accepts.
#[test]
fn the_open_genesis_loads() {
    let g = open_genesis();
    g.check().unwrap();
}

/// The committed recordings (`node/testdata/replay/`).
fn corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/replay").join(name)
}

/// A directory of its own for each call (tests run in parallel, in one process).
fn scratch(tag: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("blockula-replay-{tag}-{}-{}-{n}", std::process::id(), crate::chain::now_ms()))
}

/// Replays `archive` into a scratch directory.
fn replay(archive: &Path, tag: &str) -> Result<Report> {
    let dir = scratch(tag);
    let r = run(&Opts { archive: archive.to_path_buf(), home: Some(dir.clone()), to: None, prune_blocks: None, checkpoints: vec![] });
    std::fs::remove_dir_all(dir).ok();
    r
}

/// The tags a v1 binary can put in a block: 0–54 and 60–61 (55–59 are reserved: a block
/// carrying one doesn't decode, and `verify` rejects it).
fn v1_tags() -> Vec<u8> {
    (0..=54).chain(60..=61).collect()
}

/// Replays the committed corpus: every recording, through sync and startup, block for block.
/// Returns their txs by tag (applied, failed, skipped).
fn replay_corpus() -> BTreeMap<u8, [u64; 3]> {
    let mut tags = BTreeMap::<u8, [u64; 3]>::new();
    for (name, height) in [("v1-open", 1_999), ("v1-season0", 200)] {
        let r = replay(&corpus(name), name).unwrap_or_else(|e| panic!("{name}: {e:#}"));
        assert_eq!((r.height, r.recorded, r.startup), (height, height, Some(height)), "{name}");
        for (tag, n) in r.tags {
            let e = tags.entry(tag).or_default();
            (0..3).for_each(|i| e[i] += n[i]);
        }
    }
    tags
}

/// Every tag a block can carry applied in `tags`, but Unbond (7), which has no state it applies
/// in: v1 always fails it.
fn assert_covers_v1(tags: &BTreeMap<u8, [u64; 3]>) {
    for tag in v1_tags() {
        assert!(tags.get(&tag).is_some_and(|n| n[0] > 0 || (tag == 7 && n[1] > 0)), "the corpus has no {} tx with tag {tag}", if tag == 7 { "failed" } else { "applied" });
    }
    assert!(tags.keys().all(|t| v1_tags().contains(t)));
}

/// Writes the corpus: `cargo test --release -- --ignored write_replay_corpus`. Only on purpose,
/// and only with v1's rules unchanged: a recording that a binary with other rules made would
/// gate on its rules. Recording again gives the same bytes until a v1 rule or a recorded
/// scenario changes (docs/REPLAY.md).
#[test]
#[ignore]
fn write_replay_corpus() {
    let source = "node/src/replay/corpus.rs (cargo test --release -- --ignored write_replay_corpus)";
    // As text: git stores it compressed anyway, and reviews and tools see a text file.
    record_open().write(&corpus("v1-open"), source, false);
    record_season0().write(&corpus("v1-season0"), source, false);
    let tags = replay_corpus();
    eprintln!("tag: applied/failed/skipped, open + season0\n{}", tags.iter().map(|(t, n)| format!("{t}: {}/{}/{}", n[0], n[1], n[2])).collect::<Vec<_>>().join("\n"));
    assert_covers_v1(&tags);
}

/// The gate itself: every recording replays with this binary, through sync and startup, block
/// for block, and between them every tag a block can carry applied (or, Unbond, failed).
#[test]
fn the_v1_corpus_replays() {
    assert_covers_v1(&replay_corpus());
}

/// What a binary that diverges by `d` makes of the open recording: the sync replay's error, and
/// the startup replay's (`Chain::open` on the recorded block log) on its own.
fn diverging(d: diverge::Divergence) -> (String, String) {
    let archive = Archive::open(&corpus("v1-open")).unwrap();
    let last: Block = serde_json::from_slice(&archive.recorded().last().unwrap().unwrap()).unwrap();
    diverge::set(Some(d));
    let sync = replay(&corpus("v1-open"), "diverging").map(|_| ()).unwrap_err();
    let home = scratch("diverging-startup");
    let startup = startup(&archive, &home, last.height, &last.state_root).map(|_| ()).unwrap_err();
    diverge::set(None);
    std::fs::remove_dir_all(home).ok();
    (format!("{sync:#}"), format!("{startup:#}"))
}

/// A v2 admission rule that missed its version gate: the first recorded tx with a memo (block 1's
/// first) can't be admitted any more.
#[test]
fn an_ungated_admission_rule_fails_the_gate() {
    let (sync, startup) = diverging(diverge::Divergence::Admission);
    assert!(sync.contains("block 1: tx 0 (transfer") && sync.contains("is skipped (not admitted), but the recording says it applied"), "{sync}");
    assert!(startup.contains("replaying block 1") && startup.contains("memos are refused"), "{startup}");
}

/// An action that changed without a gate: block 1's second transfer, which opens an account, fails.
#[test]
fn an_ungated_handler_change_fails_the_gate() {
    let (sync, startup) = diverging(diverge::Divergence::Handler);
    assert!(sync.contains("block 1: tx 1 (transfer") && sync.contains("failed (transfers can't open accounts") && sync.contains("but the recording says it applied"), "{sync}");
    assert!(startup.contains("replaying block 1: tx 1 failed (transfers can't open accounts"), "{startup}");
}

/// A change only the state root shows: the first epoch's end (block 29) moves a drop.
#[test]
fn a_state_only_change_fails_the_gate() {
    let (sync, startup) = diverging(diverge::Divergence::EndOfEpoch);
    assert!(sync.contains("block 29: state root") && sync.contains("but the recording has"), "{sync}");
    assert!(startup.contains("state root mismatch at height 29"), "{startup}");
}

/// The replay trusts what a joining node trusts (`node::verify_batch_trusted`): a checkpoint,
/// embedded for the chain or pinned (`--checkpoint`), that the archive's block at its height
/// doesn't match stops it, and so does a validator set other than the one a checkpoint pins.
#[test]
fn a_checkpoint_the_archive_contradicts_fails_the_gate() {
    let archive = Archive::open(&corpus("v1-season0")).unwrap();
    let block50: Block = serde_json::from_slice(&archive.recorded().nth(49).unwrap().unwrap()).unwrap();
    let pinned = |hash: &str, set_hash: Option<&str>| {
        let dir = scratch("checkpoint");
        let checkpoint = crate::checkpoints::Checkpoint { height: 50, hash: hash.into(), set_hash: set_hash.map(Into::into), timestamp_ms: None };
        let r = run(&Opts { archive: corpus("v1-season0"), home: Some(dir.clone()), to: Some(60), prune_blocks: None, checkpoints: vec![checkpoint] });
        std::fs::remove_dir_all(dir).ok();
        r.map(|r| r.height).map_err(|e| format!("{e:#}"))
    };
    assert_eq!(pinned(&block50.hash, None), Ok(60));
    let other = pinned(&"ab".repeat(32), None).unwrap_err();
    assert!(other.contains("block #50 is") && other.contains("but the trusted checkpoint says abab"), "{other}");
    let forged = pinned(&block50.hash, Some(&"cd".repeat(32))).unwrap_err();
    assert!(forged.contains("but checkpoint #50 pins cdcd") && forged.contains("a forged validator-set history"), "{forged}");
}

/// A record that differs from the recording only where no tx's outcome or state root shows (here
/// block 3's upgrade signal) fails the gate too: below v2, a block's record is v1's.
#[test]
fn a_record_that_differs_fails_the_gate() {
    let dir = scratch("record");
    let archive = dir.join("archive");
    std::fs::create_dir_all(&archive).unwrap();
    for f in ["genesis.json", "sync-000000000001.jsonl"] {
        std::fs::copy(corpus("v1-season0").join(f), archive.join(f)).unwrap();
    }
    let log = std::fs::read_to_string(corpus("v1-season0").join("blocks-000000000001.jsonl")).unwrap();
    let mut lines: Vec<String> = log.lines().map(String::from).collect();
    let mut block3: serde_json::Value = serde_json::from_str(&lines[2]).unwrap();
    assert_eq!((block3["height"].as_u64(), block3["signal"].as_u64()), (Some(3), Some(1)));
    block3["signal"] = 9.into();
    lines[2] = block3.to_string();
    let log = lines.join("\n") + "\n";
    std::fs::write(archive.join("blocks-000000000001.jsonl"), &log).unwrap();
    let mut meta: serde_json::Value = serde_json::from_slice(&std::fs::read(corpus("v1-season0").join("meta.json")).unwrap()).unwrap();
    meta["blocks"][0]["sha256"] = crate::release_cli::sha256_hex(log.as_bytes()).into();
    std::fs::write(archive.join("meta.json"), meta.to_string()).unwrap();
    let err = run(&Opts { archive, home: Some(dir.join("homes")), to: Some(10), prune_blocks: None, checkpoints: vec![] }).map(|_| ()).unwrap_err();
    let err = format!("{err:#}");
    assert!(err.contains("block 3: its record differs from the recording's in signal"), "{err}");
    std::fs::remove_dir_all(dir).ok();
}

/// `verify`'s header check on a parent no clock check has seen (an archive's): a time past the
/// end of `u64` makes its child too early, not a panic (release builds check overflow).
#[test]
fn a_parent_at_the_end_of_time_is_refused_not_a_panic() {
    use crate::consensus::{check_header, CBlock, PROTOCOL_VERSION};
    use commonware_cryptography::Digestible as _;
    let parent = CBlock::new(commonware_cryptography::blake3::Digest([1; 32]), 5, 5, 4, u64::MAX - 10, "v".into(), 4, [0; 32], vec![]);
    let child = CBlock::new(parent.digest(), 6, 6, 5, u64::MAX, "v".into(), 5, [0; 32], vec![]);
    assert_eq!(check_header(&child, &parent, PROTOCOL_VERSION, 1_000, "v"), Err("block too early"));
    assert_eq!(check_header(&child, &parent, PROTOCOL_VERSION, 10, "v"), Ok(()));
}

/// The nightly's case: an archive of a chain's blocks without a recording (testnet-2's), where
/// only the state roots the blocks cite show a divergence. The same state-only change fails at the
/// first block that cites the diverged root, block 29's: block 30.
#[test]
fn without_a_recording_the_cited_roots_catch_a_divergence() {
    let dir = scratch("unrecorded");
    let archive = dir.join("archive");
    std::fs::create_dir_all(&archive).unwrap();
    let mut meta: serde_json::Value = serde_json::from_slice(&std::fs::read(corpus("v1-open").join("meta.json")).unwrap()).unwrap();
    meta["blocks"] = serde_json::json!([]);
    std::fs::write(archive.join("meta.json"), meta.to_string()).unwrap();
    for f in ["genesis.json", "sync-000000000001.jsonl"] {
        std::fs::copy(corpus("v1-open").join(f), archive.join(f)).unwrap();
    }
    let ok = run(&Opts { archive: archive.clone(), home: Some(dir.join("ok")), to: Some(40), prune_blocks: None, checkpoints: vec![] }).unwrap();
    assert_eq!((ok.height, ok.recorded, ok.startup), (40, 0, None));
    diverge::set(Some(diverge::Divergence::EndOfEpoch));
    let err = run(&Opts { archive, home: Some(dir.join("diverging")), to: None, prune_blocks: None, checkpoints: vec![] }).map(|_| ()).unwrap_err();
    diverge::set(None);
    let err = format!("{err:#}");
    assert!(err.contains("block 30 cites state root") && err.contains("at height 29, where this binary has"), "{err}");
    std::fs::remove_dir_all(dir).ok();
}
