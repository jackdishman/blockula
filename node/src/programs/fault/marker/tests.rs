//! The trial marker on its own (docs/PROGRAMS-V0.md §6.2): what it writes, what a restart reads
//! back, and that a process that dies keeps it. What the proposer and verification write into it,
//! and what the quarantine does with it, is `fault/safeguards.rs`.

use crate::programs::probe::{bare_call, bare_tx, bare_upload, Dev};
use super::*;

fn home(tag: &str) -> PathBuf {
    let home = Dev::fresh_home(tag);
    std::fs::create_dir_all(&home).unwrap();
    home
}

/// What the file holds now: its records, and its length.
fn file(home: &Path) -> (Vec<Record>, usize) {
    let bytes = std::fs::read(home.join(MARKER_FILE)).unwrap();
    (records_in(&bytes), bytes.len())
}

/// A call from `from` at `nonce`, valid until block 100, and an upload (whose record carries its
/// code's hash).
fn call(from: &str, nonce: u64) -> Tx {
    bare_tx(from, nonce, 100, bare_call())
}

fn upload(from: &str, nonce: u64) -> Tx {
    bare_tx(from, nonce, 100, bare_upload(b"some code"))
}

/// A record is one line, and comes back from it; anything that isn't exactly one doesn't.
#[test]
fn a_record_round_trips_through_its_line_and_anything_else_is_refused() {
    let (c, u) = (Record::of(&call("boo1alice", 1)), Record::of(&upload("boo1bob", 2)));
    assert_eq!((c.kind.as_str(), c.wasm_hash, u.kind.as_str(), u.wasm_hash), ("call_program", None, "upload_code", Some(digest::wasm_hash(b"some code"))));
    for r in [&c, &u] {
        assert_eq!(Record::parse(&r.line()).as_ref(), Some(r));
    }
    let good = c.line();
    for bad in [
        String::new(),
        "garbage".into(),
        good.replace("call_program", "transfer"),
        format!("{good} extra"),
        good.replacen(&c.id, &c.id[1..], 1),
        good.replacen(&c.id, &c.id.to_uppercase(), 1),
        good.replace(" 100 ", " -1 "),
        good.replace(" -", " zz"),
        good.split(' ').take(4).collect::<Vec<_>>().join(" "),
    ] {
        assert_eq!(Record::parse(&bad), None, "{bad:?}");
    }
}

/// The file doesn't exist until a trial names a transaction; it then names what is in flight
/// (every trial's, however they overlap) and, once they end, nothing: the same bytes, all zeros.
#[test]
fn the_marker_names_every_trial_in_flight_and_nothing_once_they_end() {
    let home = home("marker-flight");
    let marker = TrialMarker::new(&home);
    let (mut a, mut b) = (marker.begin(), marker.begin());
    assert!(!home.join(MARKER_FILE).exists(), "a trial that marks nothing writes nothing");
    let (t1, t2, t3) = (call("boo1alice", 1), upload("boo1bob", 1), call("boo1alice", 2));
    a.mark(&t1);
    assert_eq!(file(&home), (vec![Record::of(&t1)], MARKER_BYTES));
    b.mark(&t2);
    assert_eq!(file(&home).0, vec![Record::of(&t1), Record::of(&t2)], "both, in the order they began");
    a.mark(&t3);
    assert_eq!(file(&home).0, vec![Record::of(&t3), Record::of(&t2)], "a trial names one transaction at a time");
    // One ending doesn't take the other's record with it.
    drop(a);
    assert_eq!(file(&home).0, vec![Record::of(&t2)]);
    drop(b);
    let (records, len) = file(&home);
    assert_eq!((records, len), (vec![], MARKER_BYTES));
    assert!(std::fs::read(home.join(MARKER_FILE)).unwrap().iter().all(|b| *b == 0), "emptied in place");
    assert!(marker.in_flight().is_empty());
}

/// More trials in flight than the file has room for leave the later ones out of it, and the file
/// keeps its size.
#[test]
fn the_marker_never_outgrows_its_file() {
    let home = home("marker-full");
    let marker = TrialMarker::new(&home);
    let mut trials: Vec<Trial> = (0..40).map(|_| marker.begin()).collect();
    for (i, t) in trials.iter_mut().enumerate() {
        t.mark(&upload("boo1alice", i as u64));
    }
    let (records, len) = file(&home);
    assert_eq!(len, MARKER_BYTES);
    assert!((16..=40).contains(&records.len()) && records.len() < 40, "{}", records.len());
    assert_eq!(records[0], Record::of(&upload("boo1alice", 0)), "the oldest trials are the ones kept");
}

/// What a dead process left is read back once and the file is removed; what's in it that isn't a
/// record (a host that died mid-write may leave anything) is skipped.
#[test]
fn recover_returns_what_the_last_run_left_and_removes_the_file() {
    let home = home("marker-recover");
    assert_eq!(recover(&home), vec![], "no file, no trials");
    let marker = TrialMarker::new(&home);
    let mut trial = marker.begin();
    let t = upload("boo1alice", 3);
    trial.mark(&t);
    let _died = std::mem::ManuallyDrop::new(trial);
    assert_eq!(recover(&home), vec![Record::of(&t)]);
    assert!(!home.join(MARKER_FILE).exists());
    assert_eq!(recover(&home), vec![]);
    // Whole records are kept, garbage and a torn last line aren't.
    let (a, b) = (Record::of(&call("boo1alice", 1)), Record::of(&call("boo1bob", 2)));
    let mut bytes = format!("{}\nnot a record\n\n{}\n{}", a.line(), b.line(), &a.line()[..70]).into_bytes();
    bytes.resize(MARKER_BYTES, 0);
    std::fs::write(home.join(MARKER_FILE), &bytes).unwrap();
    assert_eq!(recover(&home), vec![a, b]);
    // A file with no zeros at all, or invalid UTF-8, is no trouble either.
    std::fs::write(home.join(MARKER_FILE), vec![0xffu8; 100]).unwrap();
    assert_eq!(recover(&home), vec![]);
}

/// A marker that is far larger than the 4 KiB it should be (damaged, or not ours) is never read
/// whole: `recover` takes its first `MARKER_BYTES` and no more, and says the file is damaged. A
/// megabyte of records with no zero in it would otherwise come back as thousands of them.
#[test]
fn recover_reads_at_most_the_markers_size() {
    let home = home("marker-big");
    let record = Record::of(&call("boo1alice", 1));
    let line = format!("{}\n", record.line());
    let mut bytes = line.repeat(1_000_000 / line.len());
    bytes.push_str("trailing garbage");
    std::fs::write(home.join(MARKER_FILE), bytes).unwrap();
    let found = recover(&home);
    assert_eq!(found.len(), MARKER_BYTES / line.len(), "the lines that fit in the first {MARKER_BYTES} bytes (the one cut off by the limit isn't a record)");
    assert!(found.iter().all(|r| *r == record));
    assert!(!home.join(MARKER_FILE).exists(), "and it is removed all the same");
}

/// A marker that can't be written is no reason to stop: the trial goes on, and nothing panics.
#[test]
fn a_marker_that_cant_be_written_is_only_logged() {
    let marker = TrialMarker::new(Path::new("/this/directory/does/not/exist"));
    let mut trial = marker.begin();
    trial.mark(&call("boo1alice", 1));
    trial.mark(&call("boo1alice", 2));
    assert_eq!(marker.in_flight().len(), 1, "it still counts what's in flight");
    drop(trial);
    assert!(marker.0.lock().failed && marker.in_flight().is_empty());
}

/// A trial dropped while its thread unwinds from a panic leaves the file as it was (S4C-R1): the
/// panic probably ends the process, and the file is all that remembers what it was trying. The
/// trial is out of what's in flight all the same, and the next write by a process that survived
/// rewrites the file from what is: the dead trial's record goes, a live trial's stays. A trial
/// that ends without a panic, after one was caught, empties the file as ever.
#[test]
fn a_trial_dropped_in_a_panic_leaves_the_file_and_the_next_write_settles_it() {
    let home = home("marker-panic");
    let marker = TrialMarker::new(&home);
    let (dying, live) = (call("boo1alice", 1), upload("boo1bob", 2));
    let mut survivor = marker.begin();
    survivor.mark(&live);
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut trial = marker.begin();
        trial.mark(&dying);
        assert_eq!(file(&home).0, vec![Record::of(&live), Record::of(&dying)]);
        panic!("a panic outside the program thread's catch_unwind (the test's, caught here)");
    }));
    assert!(panicked.is_err());
    assert_eq!(marker.in_flight(), vec![Record::of(&live)], "the dead trial is no longer in flight");
    assert_eq!(file(&home).0, vec![Record::of(&live), Record::of(&dying)], "but the file still names it: unwinding didn't write");
    // A trial that starts, and a trial that ends normally, each rewrite the file from what is in flight.
    let mut next = marker.begin();
    assert_eq!(file(&home).0.len(), 2, "beginning names nothing: no write");
    next.mark(&call("boo1carol", 3));
    assert_eq!(file(&home).0, vec![Record::of(&live), Record::of(&call("boo1carol", 3))], "the dead trial's record is gone at the next write");
    drop(next);
    assert_eq!(file(&home).0, vec![Record::of(&live)]);
    drop(survivor);
    assert_eq!(file(&home), (vec![], MARKER_BYTES), "every normal end empties its record");
    // The same with nothing else in flight: the panic leaves the one record, and the next trial's
    // own start settles it.
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut trial = marker.begin();
        trial.mark(&dying);
        panic!("again");
    }));
    assert!(panicked.is_err() && marker.in_flight().is_empty());
    assert_eq!(file(&home).0, vec![Record::of(&dying)]);
    let mut later = marker.begin();
    later.mark(&live);
    assert_eq!(file(&home).0, vec![Record::of(&live)], "the next write settles it");
    drop(later);
    assert_eq!(file(&home).0, vec![]);
}

/// The child half of [`the_marker_survives_a_process_that_panics`]: writes the marker in `home`
/// (`BLOCKULA_S4C_HOME`) for a transaction and panics with the trial in flight, as node-side code
/// outside the program thread's `catch_unwind` could. The test harness catches the panic at the
/// top of the test (unwinding drops the trial first) and the process exits with 101. Returning is a
/// failure.
#[test]
#[ignore = "the child process of the_marker_survives_a_process_that_panics"]
fn marker_panic_child() {
    let Ok(home) = std::env::var("BLOCKULA_S4C_HOME") else { return };
    let mut trial = TrialMarker::new(Path::new(&home)).begin();
    trial.mark(&upload("boo1alice", 9));
    panic!("a panic with a trial in flight");
}

/// What a process that panics keeps (S4C-R1): the runtime doesn't catch task panics, so a panic in
/// node-side code with a trial in flight ends the process (exit 101 here) after unwinding has
/// dropped the trial's guard; the marker must still name the transaction.
#[test]
fn the_marker_survives_a_process_that_panics() {
    let home = home("marker-panic-process");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "programs::fault::marker::tests::marker_panic_child", "--ignored", "--nocapture", "--test-threads=1"])
        .env("BLOCKULA_S4C_HOME", &home)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(101), "the child panicked and exited (the test harness's code for a failed test): {status:?}");
    assert_eq!(recover(&home), vec![Record::of(&upload("boo1alice", 9))]);
}

/// The child half of [`the_marker_survives_a_process_that_aborts`]: writes the marker in `home`
/// (`BLOCKULA_S4C_HOME`) for a transaction, and aborts the process, as an abort inside a program
/// would. Returning is a failure.
#[test]
#[ignore = "the child process of the_marker_survives_a_process_that_aborts"]
fn marker_abort_child() {
    let Ok(home) = std::env::var("BLOCKULA_S4C_HOME") else { return };
    let mut trial = TrialMarker::new(Path::new(&home)).begin();
    trial.mark(&upload("boo1alice", 9));
    std::process::abort();
}

/// What a process that dies keeps (§6.2): the marker, written by one `pwrite` before the trial and
/// never `fsync`ed, is in the file when the process is gone, whatever way it went. This one aborts
/// (SIGABRT, as a Rust abort does; the kernel has the write already, as it does for a SIGKILL).
#[test]
fn the_marker_survives_a_process_that_aborts() {
    let home = home("marker-abort");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "programs::fault::marker::tests::marker_abort_child", "--ignored", "--nocapture", "--test-threads=1"])
        .env("BLOCKULA_S4C_HOME", &home)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success() && status.code().is_none(), "the child was killed by a signal, not a clean exit: {status:?}");
    assert_eq!(recover(&home), vec![Record::of(&upload("boo1alice", 9))]);
}
