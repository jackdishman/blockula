//! The trial marker (docs/PROGRAMS-V0.md §6.2): a node-local hint that survives a crash.
//!
//! A program transaction that kills the process outright (an abort, a stack overflow past the
//! guard page, an out-of-memory kill: nothing `catch_unwind` sees) would loop a validator through
//! its own mempool and its peers' gossip. So before it tries one, a proposer writes the
//! transaction's id, sender, kind, `valid_until` and (for an upload) code hash to
//! `<home>/program-trial`, and empties the file when packing ends; verification does the same
//! around a block's program transactions. A process that dies keeps what it wrote (the bytes are
//! in the kernel's page cache: one `pwrite`, no `fsync`; a host that dies loses only this hint).
//! At startup whatever the file still names goes into the quarantine under the rule a fault gets
//! ([`recover`]), so such a transaction stops a validator at most once.
//!
//! A panic that escapes the program thread's `catch_unwind` (node-side code in `apply`, commit or
//! the receipts) ends the process too: the runtime doesn't catch task panics. The guard that
//! unwinds with it must not empty the file on its way out, so a [`Trial`] dropped while its thread
//! is panicking leaves the file as it was (`TrialMarker::end`).
//!
//! Proposals and verifications of different rounds can overlap, so the file holds every trial in
//! flight, not the last one: [`TrialMarker`] keeps them in memory and rewrites the whole file, in
//! one `pwrite` of a fixed size at offset 0, whenever one starts or ends, under one lock. The file
//! is `MARKER_BYTES` long: the records, one text line each, then zeros.
//!
//! The marker is best effort. A write that fails is logged (once) and the node goes on; a file
//! that doesn't parse is ignored.

use crate::programs::digest;
use crate::types::{Action, Tx};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The marker's file name, under the node's home.
pub const MARKER_FILE: &str = "program-trial";
/// The marker's size: room for 16 records of at most 256 bytes (a 64-character id, a sender of at
/// most 90, a kind, a height and a code hash). More trials in flight than fit leave the later ones
/// out of the file.
pub const MARKER_BYTES: usize = 4096;

/// One program transaction under trial.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub id: String,
    pub sender: String,
    /// Its action's kind (`upload_code`, `create_program` or `call_program`: tags 62–64).
    pub kind: String,
    /// The last block that can carry it.
    pub valid_until: u64,
    /// For an upload, the hash of its code.
    pub wasm_hash: Option<[u8; 32]>,
}

impl Record {
    pub fn of(tx: &Tx) -> Record {
        Record::with_id(tx, tx.id())
    }

    /// `of(tx)`, for a caller that has `tx`'s id (`id`: [`Tx::id`]) already.
    pub fn with_id(tx: &Tx, id: String) -> Record {
        // Test builds hold every caller to it, as `programs::apply` does: a wrong id would quarantine
        // another transaction after a crash.
        #[cfg(test)]
        assert_eq!(id, tx.id(), "a caller handed Record::with_id another transaction's id");
        let wasm_hash = match &tx.action {
            Action::UploadCode { wasm, .. } => Some(digest::wasm_hash(&wasm.0)),
            _ => None,
        };
        Record { id, sender: tx.from.clone(), kind: tx.action.kind().to_string(), valid_until: tx.valid_until, wasm_hash }
    }

    /// `id sender kind valid_until wasm_hash`, the hash `-` for any but an upload.
    fn line(&self) -> String {
        format!("{} {} {} {} {}", self.id, self.sender, self.kind, self.valid_until, self.wasm_hash.map_or("-".to_string(), hex::encode))
    }

    /// A record from its line, `None` for anything that isn't exactly one.
    fn parse(line: &str) -> Option<Record> {
        let mut fields = line.split(' ');
        let (id, sender, kind, valid_until, hash) = (fields.next()?, fields.next()?, fields.next()?, fields.next()?, fields.next()?);
        if fields.next().is_some() || !is_hex_32(id) || sender.is_empty() || !matches!(kind, "upload_code" | "create_program" | "call_program") {
            return None;
        }
        let wasm_hash = match hash {
            "-" => None,
            h if is_hex_32(h) => Some(hex::decode(h).ok()?.try_into().ok()?),
            _ => return None,
        };
        Some(Record { id: id.to_string(), sender: sender.to_string(), kind: kind.to_string(), valid_until: valid_until.parse().ok()?, wasm_hash })
    }
}

fn is_hex_32(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

struct Inner {
    path: PathBuf,
    /// Opened by the first record written: a node that never tries a program transaction never
    /// creates the file.
    file: Option<File>,
    next_ticket: u64,
    /// The trials in flight, by the ticket of the [`Trial`] that started each.
    live: BTreeMap<u64, Record>,
    /// A write failed (logged once).
    failed: bool,
}

impl Inner {
    /// Writes the whole file for what's in flight: one `pwrite` at offset 0.
    fn flush(&mut self) {
        let mut buf = vec![0u8; MARKER_BYTES];
        let mut at = 0;
        for record in self.live.values() {
            let line = record.line();
            if at + line.len() + 1 > MARKER_BYTES {
                break;
            }
            buf[at..at + line.len()].copy_from_slice(line.as_bytes());
            buf[at + line.len()] = b'\n';
            at += line.len() + 1;
        }
        let written = match &self.file {
            Some(file) => file.write_all_at(&buf, 0),
            None => OpenOptions::new().create(true).truncate(false).write(true).open(&self.path).and_then(|file| {
                let r = file.write_all_at(&buf, 0);
                self.file = Some(file);
                r
            }),
        };
        if let Err(e) = written
            && !std::mem::replace(&mut self.failed, true)
        {
            tracing::error!(path = %self.path.display(), error = %e, "can't write the program trial marker: a crash while trying a program transaction won't be remembered");
        }
    }
}

/// The node's trial marker, shared by everything that tries program transactions (a proposer's
/// packing and verifications, which can overlap): [`TrialMarker::begin`] starts a [`Trial`].
#[derive(Clone)]
pub struct TrialMarker(Arc<parking_lot::Mutex<Inner>>);

impl TrialMarker {
    /// The marker of the node whose files are in `home`. Writes nothing yet.
    pub fn new(home: &Path) -> TrialMarker {
        TrialMarker(Arc::new(parking_lot::Mutex::new(Inner { path: home.join(MARKER_FILE), file: None, next_ticket: 0, live: BTreeMap::new(), failed: false })))
    }

    /// A trial that names nothing until [`Trial::mark`], and names nothing again once dropped.
    pub fn begin(&self) -> Trial {
        let mut inner = self.0.lock();
        inner.next_ticket += 1;
        Trial { marker: self.clone(), ticket: inner.next_ticket }
    }

    /// The records the file would hold now: what's in flight, in order.
    #[cfg(test)]
    pub fn in_flight(&self) -> Vec<Record> {
        self.0.lock().live.values().cloned().collect()
    }

    /// `ticket`'s trial now names `record`: the file is rewritten.
    fn set(&self, ticket: u64, record: Record) {
        let mut inner = self.0.lock();
        inner.live.insert(ticket, record);
        inner.flush();
    }

    /// `ticket`'s trial ends: it names nothing, and the file is rewritten without it. Except while
    /// the thread is unwinding from a panic: the record leaves what's in flight, but the file is
    /// left as it was, because the panic probably ends the process (nothing here catches it) and
    /// the file is then the only thing that remembers what it was trying. A process that survives
    /// the panic settles the file at the next write, which rewrites it whole from what's in flight.
    fn end(&self, ticket: u64) {
        let mut inner = self.0.lock();
        if inner.live.remove(&ticket).is_some() && !std::thread::panicking() {
            inner.flush();
        }
    }
}

/// One proposal's packing or one verification: names the program transaction it is trying, one at
/// a time, and nothing once it ends (it is dropped on every way out of the function that holds it,
/// including a cancelled future; but see `TrialMarker::end` for a panic).
pub struct Trial {
    marker: TrialMarker,
    ticket: u64,
}

impl Trial {
    /// `tx` is about to be tried: the file names it (instead of this trial's previous one).
    #[cfg_attr(not(test), allow(dead_code))] // the callers that have the id use `mark_with_id`
    pub fn mark(&mut self, tx: &Tx) {
        self.mark_with_id(tx, &tx.id());
    }

    /// `mark(tx)`, for a caller that has `tx`'s id (`id`: [`Tx::id`]) already, which it hands on
    /// to the run (`State::apply_j_with_id`).
    pub fn mark_with_id(&mut self, tx: &Tx, id: &str) {
        self.marker.set(self.ticket, Record::with_id(tx, id.to_string()));
    }
}

impl Drop for Trial {
    fn drop(&mut self) {
        self.marker.end(self.ticket);
    }
}

/// What the last run left in `home`'s marker, which is then removed: the trials a process that
/// stopped in the middle of one was in the middle of. At most [`MARKER_BYTES`] are read, however
/// large the file is. Lines that aren't records are skipped (a host that died mid-write may leave
/// anything), with a warning: a damaged marker is never fatal.
pub fn recover(home: &Path) -> Vec<Record> {
    let path = home.join(MARKER_FILE);
    let Ok(file) = File::open(&path) else { return vec![] };
    let mut bytes = Vec::with_capacity(MARKER_BYTES);
    let read = file.take(MARKER_BYTES as u64).read_to_end(&mut bytes);
    let too_long = std::fs::metadata(&path).is_ok_and(|m| m.len() > MARKER_BYTES as u64);
    if let Err(e) = std::fs::remove_file(&path) {
        tracing::warn!(path = %path.display(), error = %e, "can't remove the program trial marker after reading it");
    }
    let (records, skipped) = parse(&bytes);
    if let Err(e) = &read {
        tracing::warn!(path = %path.display(), error = %e, "can't read all of the program trial marker");
    }
    if skipped > 0 || too_long {
        tracing::warn!(path = %path.display(), skipped, too_long, "the program trial marker is damaged: keeping the records that parse");
    }
    records
}

/// The records in a marker file's bytes: its lines up to the zeros that pad it, each that is one.
#[cfg(test)]
pub(super) fn records_in(bytes: &[u8]) -> Vec<Record> {
    parse(bytes).0
}

/// [`records_in`], and how many of those lines weren't records.
fn parse(bytes: &[u8]) -> (Vec<Record>, usize) {
    let text = &bytes[..bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len())];
    let (mut records, mut skipped) = (vec![], 0);
    for line in String::from_utf8_lossy(text).lines().filter(|l| !l.is_empty()) {
        match Record::parse(line) {
            Some(record) => records.push(record),
            None => skipped += 1,
        }
    }
    (records, skipped)
}

#[cfg(test)]
mod tests;
