//! Runs every fuzz target's harness over its seeds on stable Rust (no fuzzer needed):
//! `cargo test --manifest-path node/fuzz/Cargo.toml`.
//!
//! Seeds must not just avoid panics but be *accepted*: a seed the decoder rejects (say, after a
//! format change) leaves the fuzzer mutating dead input. Regenerate them with
//! `cargo test --manifest-path node/fuzz/Cargo.toml -- --ignored write_seeds`.

use blockula_fuzz::harness;
use std::path::PathBuf;

fn seeds_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("seeds")
}

#[test]
fn fresh_seeds_are_accepted() {
    for (target, name, bytes) in harness::seeds() {
        assert!(harness::run(target, &bytes), "{target}/{name}: the harness rejects the seed it built");
    }
}

#[test]
fn committed_seeds_are_accepted() {
    for target in harness::TARGETS {
        let mut files: Vec<PathBuf> = std::fs::read_dir(seeds_dir().join(target)).unwrap_or_else(|e| panic!("seeds/{target}: {e}")).map(|e| e.unwrap().path()).collect();
        files.sort();
        assert!(!files.is_empty(), "seeds/{target} is empty");
        for f in files {
            let bytes = std::fs::read(&f).unwrap();
            assert!(harness::run(target, &bytes), "{} is stale: regenerate with `cargo test --manifest-path node/fuzz/Cargo.toml -- --ignored write_seeds`", f.display());
        }
    }
}

/// Open findings (README.md "Findings") must still reproduce. They're undisclosed vulnerabilities,
/// so their inputs never enter this public tree: they live in `private/<target>/` (git-ignored) on
/// a maintainer's machine, or wherever `BLOCKULA_FUZZ_FINDINGS` points (the advisory's private
/// fork). Without that directory, as in public CI, this checks nothing. When one is fixed this
/// fails: once the fix is public, move its input to `harness::seeds`, where it then guards the fix.
#[test]
fn open_findings_still_reproduce() {
    let dir = findings_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!("no open findings here ({} doesn't exist): nothing to reproduce", dir.display());
        return;
    };
    let mut inputs = Vec::new();
    for entry in entries {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let target = harness::TARGETS.iter().find(|t| **t == name).unwrap_or_else(|| panic!("{}: not a fuzz target (one directory per target: {:?})", path.display(), harness::TARGETS));
        for f in std::fs::read_dir(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())) {
            let f = f.unwrap().path();
            if !f.file_name().unwrap().to_string_lossy().starts_with('.') {
                inputs.push((*target, f));
            }
        }
    }
    inputs.sort();
    for (target, f) in &inputs {
        let bytes = std::fs::read(f).unwrap();
        let outcome = std::panic::catch_unwind(|| harness::run(target, &bytes));
        assert!(outcome.is_err(), "{} no longer panics: the finding is fixed; once the fix is public, make it a seed (README.md \"Findings\")", f.display());
    }
    eprintln!("{} open finding(s) in {} still reproduce", inputs.len(), dir.display());
}

fn findings_dir() -> PathBuf {
    match std::env::var_os("BLOCKULA_FUZZ_FINDINGS") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("private"),
    }
}

#[test]
#[ignore = "generator: rewrites seeds/"]
fn write_seeds() {
    let _ = std::fs::remove_dir_all(seeds_dir());
    for (target, name, bytes) in harness::seeds() {
        std::fs::create_dir_all(seeds_dir().join(target)).unwrap();
        std::fs::write(seeds_dir().join(target).join(name), bytes).unwrap();
    }
}
