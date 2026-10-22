//! `UploadCode`'s checks on new code (docs/PROGRAMS.md 4.2, 4.3, 7.1): upload validation first,
//! then a test translation on a fresh soroban-wasmi engine, dropped after. So every module an
//! engine loads at execution passed [`validate`] before any engine saw it: no engine's translator
//! is trusted with code validation refused. The node and `testing::MemHost::upload` both run
//! [`check`].
//!
//! Two kinds of outcome, as for `exec::execute`:
//! * **A refusal** ([`Refusal`]): the code breaks a rule of 4.2, or the engine won't translate it.
//!   The same on every node, so the upload fails: its receipt records the rule's number
//!   (`Rule::number`), or that the translation was refused, never a message.
//! * **A node fault** ([`NodeFault`]): a panic in validation or in the test translation (caught
//!   here: [`NodeFault::Panic`]), or an engine that refuses its own configuration. **Never a
//!   refusal:** a panic one node hits and another doesn't, or that a fixed release no longer hits,
//!   must not become a deterministic-looking outcome. The node commits nothing of the block and
//!   halts, as for a fault in `execute`.

use crate::engine::{BlockEngine, CodeHash, EngineKind, LoadError, NodeFault, new_block_engine};
use crate::meter::Schedule;
use crate::validate::{ModuleInfo, ValidateError, validate};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Why an upload's new code was refused: the transaction's failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Upload validation (4.2) refused it: no engine saw it.
    Invalid(ValidateError),
    /// It passed validation, but the test translation refused it. The engine's words are for
    /// logs only: a receipt records only that it was refused.
    Translate(LoadError),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Invalid(e) => write!(f, "invalid code: {e}"),
            Refusal::Translate(e) => write!(f, "{}: {e}", EngineKind::Soroban.name()),
        }
    }
}

impl std::error::Error for Refusal {}

/// The hash the test translation loads `wasm` under. An engine only keys loaded code by its hash,
/// and the throwaway engine holds this one code, so any hash will do.
const TRIAL: CodeHash = [0; 32];

/// [`validate`], then a test translation of `wasm` on a fresh soroban-wasmi engine configured
/// from `schedule` (the node's engine: the `ref038` oracle never decides what uploads). Returns
/// what validation learned (`ModuleInfo`: the methods to match with the interface, the entity
/// counts) or why the code is refused, or the node fault that stopped the check. Run it on the
/// transaction's large-stack thread, as `execute`.
pub fn check(wasm: &[u8], schedule: &Schedule) -> Result<Result<ModuleInfo, Refusal>, NodeFault> {
    check_on(wasm, || new_block_engine(EngineKind::Soroban, schedule))
}

/// [`check`], with the engine `fresh` makes: the tests give it engines that fail.
fn check_on(wasm: &[u8], fresh: impl FnOnce() -> Result<Box<dyn BlockEngine>, NodeFault>) -> Result<Result<ModuleInfo, Refusal>, NodeFault> {
    // A panic anywhere in here unwinds through the throwaway engine, which goes with it.
    catch_unwind(AssertUnwindSafe(|| {
        let info = match validate(wasm) {
            Ok(info) => info,
            Err(e) => return Ok(Err(Refusal::Invalid(e))),
        };
        let mut engine = fresh()?;
        Ok(engine.load(TRIAL, wasm).map(|()| info).map_err(Refusal::Translate))
    }))
    .unwrap_or(Err(NodeFault::Panic))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{CodeInfo, Host, Invoked};
    use crate::meter::SCHEDULE_V0;
    use crate::validate::Rule;

    /// A minimal program: one function `[] -> []` doing nothing, and one memory of one page,
    /// exported (sections: type, function, memory, export, code).
    const MINIMAL: &[u8] = b"\0asm\x01\0\0\0\x01\x04\x01\x60\0\0\x03\x02\x01\0\x05\x04\x01\x01\x01\x01\x07\x0a\x01\x06memory\x02\0\x0a\x04\x01\x02\0\x0b";

    /// An engine whose translation does what `load` says: refuse, or panic.
    struct Failing {
        panics: bool,
    }

    impl BlockEngine for Failing {
        fn kind(&self) -> EngineKind {
            EngineKind::Soroban
        }
        fn load(&mut self, _: CodeHash, _: &[u8]) -> Result<(), LoadError> {
            if self.panics {
                panic!("injected translator panic");
            }
            Err(LoadError::Translate("injected refusal".into()))
        }
        fn info(&self, _: &CodeHash) -> Option<CodeInfo> {
            None
        }
        fn invoke(&self, _: &CodeHash, _: &str, _: u64, _: &mut dyn Host) -> Invoked {
            unreachable!("an upload never runs code")
        }
    }

    fn failing(panics: bool) -> impl FnOnce() -> Result<Box<dyn BlockEngine>, NodeFault> {
        move || Ok(Box::new(Failing { panics }))
    }

    #[test]
    fn accepts_valid_code_and_refuses_invalid_code() {
        let info = check(MINIMAL, &SCHEDULE_V0).unwrap().unwrap();
        assert_eq!(info, validate(MINIMAL).unwrap());
        match check(&MINIMAL[..MINIMAL.len() - 1], &SCHEDULE_V0) {
            Ok(Err(Refusal::Invalid(e))) => assert_eq!(e.rule, Rule::Wasm, "{e}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    // The engine refusing code validation passed is the upload's refusal, on every node alike.
    #[test]
    fn a_translation_refused_is_a_refusal() {
        assert_eq!(check_on(MINIMAL, failing(false)), Ok(Err(Refusal::Translate(LoadError::Translate("injected refusal".into())))));
    }

    // A panic in the test translation, or an engine that can't be made, is the node's trouble:
    // never a refused upload.
    #[test]
    fn a_translator_panic_is_a_node_fault() {
        assert_eq!(check_on(MINIMAL, failing(true)), Err(NodeFault::Panic));
        let broken = || Err(NodeFault::Engine("injected".into()));
        assert_eq!(check_on(MINIMAL, broken), Err(NodeFault::Engine("injected".into())));
        // Code validation refuses never reaches the engine, panicking or not.
        assert!(matches!(check_on(b"\0asm", failing(true)), Ok(Err(Refusal::Invalid(_)))));
    }
}
