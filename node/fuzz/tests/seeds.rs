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

/// Open findings (README.md) must still reproduce. When one is fixed this fails: move its input
/// from `harness::open_findings` and `regressions/` to the seeds, where it then guards the fix.
#[test]
fn open_findings_still_reproduce() {
    for (target, name, bytes) in harness::open_findings() {
        let committed = std::fs::read(regressions_dir().join(target).join(name)).unwrap_or_else(|e| panic!("regressions/{target}/{name}: {e}"));
        assert_eq!(committed, bytes, "regressions/{target}/{name} is out of date: rerun write_seeds");
        let outcome = std::panic::catch_unwind(|| harness::run(target, &bytes));
        assert!(outcome.is_err(), "{target}/{name} no longer panics: the finding is fixed, so make it a seed (see README.md)");
    }
}

fn regressions_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("regressions")
}

#[test]
#[ignore = "generator: rewrites seeds/ and regressions/"]
fn write_seeds() {
    for dir in [seeds_dir(), regressions_dir()] {
        let _ = std::fs::remove_dir_all(&dir);
    }
    for (dir, inputs) in [(seeds_dir(), harness::seeds()), (regressions_dir(), harness::open_findings())] {
        for (target, name, bytes) in inputs {
            std::fs::create_dir_all(dir.join(target)).unwrap();
            std::fs::write(dir.join(target).join(name), bytes).unwrap();
        }
    }
}
