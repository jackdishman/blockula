//! Plain-English summaries of program transactions (`State::describe_with`'s programs arm),
//! built from the transaction's structure only (docs/PROGRAMS.md 9.7): who acted, which programs
//! and code by address and hash, how much FANG went in, what the program may become. A method's
//! name, an interface's identifiers and a call's input are the deployer's or the caller's words,
//! and no summary quotes them: wallets and the MCP show those apart, as the program's own claims
//! (`untrusted_*`). So a summary can't be made to say what the transaction doesn't do.

use super::digest;
use crate::chain::{short, State};
use crate::crypto;
use crate::types::{fmt_fang, Action, ProgramPolicy, Tx};

/// The summary of a program transaction (tags 62–64 and 69; empty for any other). `from` is the
/// sender as `describe_with` names it.
pub fn describe(state: &State, tx: &Tx, from: &str, primary_name: &dyn Fn(&str) -> Option<String>) -> String {
    let who = |a: &str| primary_name(a).unwrap_or_else(|| short(a));
    match &tx.action {
        Action::UploadCode { wasm, interface, storage_limit } => {
            let methods = blockula_vm::interface::decode(&interface.0).map(|i| format!(" ({} methods)", i.methods.len())).unwrap_or_default();
            format!(
                "{from} uploaded program code {} ({} bytes) with interface {}{methods}, bonding at most {}",
                hash(&digest::wasm_hash(&wasm.0)),
                wasm.0.len(),
                hash(&digest::interface_hash(&interface.0)),
                fmt_fang(*storage_limit)
            )
        }
        Action::CreateProgram { wasm_hash, authority, upgrade_delay_blocks, policy, accepts_transfers, init, .. } => {
            // The address it creates: from the sender and this transaction's nonce (§2.1).
            let program = crypto::address_bytes(&tx.from).map(|c| format!(" {}", short(&digest::program_address(state.params.prefix(), &c, tx.nonce)))).unwrap_or_default();
            let upgrades = match authority {
                None => "immutable".to_string(),
                Some(a) => {
                    let policy = match policy {
                        ProgramPolicy::Open => "any code",
                        ProgramPolicy::Additive => "code keeping its methods",
                    };
                    format!("upgradable by {} to {policy} with {upgrade_delay_blocks} blocks' notice, once program upgrades exist", who(a))
                }
            };
            let transfers = if *accepts_transfers { ", accepting transfers" } else { "" };
            let init = match init {
                None => String::new(),
                Some(i) if i.value > 0 => format!(", running its initializer with {} attached", fmt_fang(i.value)),
                Some(_) => ", running its initializer".into(),
            };
            format!("{from} created program{program} ({upgrades}{transfers}) from code {}{init}", short_hex(wasm_hash))
        }
        Action::CallProgram { calls, .. } => {
            let value = calls.iter().fold(0u64, |v, c| v.saturating_add(c.value));
            let attached = if value > 0 { format!(" with {} attached", fmt_fang(value)) } else { String::new() };
            // The programs called, each once, in the order the calls first name them.
            let mut programs: Vec<&str> = vec![];
            for c in calls {
                if !programs.contains(&c.program.as_str()) {
                    programs.push(&c.program);
                }
            }
            match (calls.len(), programs.as_slice()) {
                (1, [p]) => format!("{from} called program {}{attached}", who(p)),
                (n, [p]) => format!("{from} made {n} calls to program {}{attached}", who(p)),
                (n, ps) => format!("{from} called {} programs ({}) in {n} calls{attached}", ps.len(), ps.iter().map(|p| who(p)).collect::<Vec<_>>().join(", ")),
            }
        }
        Action::ProgramsPause {} => format!("{from} signed a pause of programs (they pause once f + 1 of the epoch's validators have)"),
        _ => String::new(),
    }
}

/// A digest as summaries show it: its first 10 hex characters.
fn hash(h: &[u8; 32]) -> String {
    short_hex(&hex::encode(h))
}

/// A hex digest's first 10 characters (by character, so a malformed one can't split one).
fn short_hex(h: &str) -> String {
    format!("{}…", h.chars().take(10).collect::<String>())
}
