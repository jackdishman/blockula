//! `blockula program …` (docs/PROGRAMS-V0.md §8.3): upload, create, call and view WebAssembly
//! programs, read them, and (for a validator) sign `ProgramsPause`. It signs like `blockula name`
//! and `blockula stake` (`contrib_cli`): the same key files, `--network` pinning and confirmation
//! rules, the same node checks before anything is signed, and the same wait for the transaction to
//! be final.
//!
//! | Command | Does |
//! |---|---|
//! | `upload <code.wasm> --interface <iface.json>` | validates the code as the chain will, encodes the interface, prints both hashes, the deposit at today's price and the most the fee can be, asks, signs tag 62 |
//! | `create <wasm_hash> <interface_hash> [--init …]` | waits until the upload is usable, prints the address the program will get, the warning about sending anything there early, the record deposit, the account creation fee it burns and the most the fee can be, asks, signs tag 63 |
//! | `call <address> <method> [--input HEX] [--value FANG]` | reads the record, prints the whole version digest it signs, the most the fee can be and, without `--storage-limit`, that the call can't bond new storage; waits until the program is usable, signs tag 64, prints the receipt |
//! | `view <address> <method> [--input HEX]` | `POST /api/program/{address}/view`: a `view` method's return data, as the program's claim |
//! | `show <address>`, `code <hash>`, `interface <hash>` | the reads |
//! | `interface encode <iface.json>` | the interface's bytes and its hash, offline |
//! | `pause --key validator.key` | for a validator: what a pause does, the live votes, whether this validator may sign, then tag 69 from its own account with its consensus key |
//!
//! **Addresses only**, never names, for programs and bindings (PRG 8.1). A binding without a version
//! digest is filled with the version in force now, which the command prints: the CLI never signs
//! "any version" (64 zeros) unless told to (`ADDR@000…0`). **Fuel**: `--fuel` defaults to
//! `program_max_tx_fuel`; fuel is paid for as reserved and never refunded, so the most a default
//! overpays is `program_max_tx_fuel` × the fuel rate (§8.3). Every command that signs refuses a
//! reservation a block couldn't carry before signing.
//!
//! **One step per block** (§2.1): `create` waits until the code it names is usable (the block after
//! the one that stored it), and `call` until the program is (the block after the one that created
//! it), and says so, instead of being refused by the mempool.
//!
//! **Spends that can't be undone ask first** (`upload`, `create`: `[y/N]`, `--yes` skips it, as `name`
//! and `stake` do), with the whole cost in the question: the deposit v0 never gives back, the account
//! creation fee a create burns (none when FANG sent to the program's address ahead opens it already)
//! and the most the fee can be.
//!
//! **A lying node can't make this panic or sign what the user didn't ask for**: the status height is
//! checked against the clock before any arithmetic on it, and every version digest it reports is 64
//! lowercase hex before it's printed or signed.
//!
//! What a program says (return data, event data) is printed as hex under "the program reports": the
//! chain checks none of it. Method and event names are the deployer's identifiers.

use crate::cli_net::{ChainPin, Remote};
use crate::contrib_cli::{confirm, Rpc, Signer};
use crate::crypto;
use crate::programs::{digest, pause};
use crate::types::{fmt_fang, Action, Binding, HexBytes, InitCall, ProgramCall, ProgramPolicy};
use crate::validator_cli::{fee_ceiling, get, load_seed, parse_fang, rt, submit_with, unsigned_tx, wait_final, SendOpts};
use anyhow::{anyhow, bail, ensure, Context, Result};
use blockula_vm::meter::{Schedule, SCHEDULE_V0};
use clap::Subcommand;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Subcommand)]
pub enum ProgramCmd {
    /// Upload a program's code and interface (tag 62): both are stored by hash, with a deposit
    /// bonded for each that isn't stored yet. The code is checked offline as the chain will check it
    Upload {
        /// The compiled program (WebAssembly)
        code: PathBuf,
        /// The interface, as JSON (docs/PROGRAMS-V0.md §3.4): `blockula program interface encode` shows its bytes
        #[arg(long)]
        interface: PathBuf,
        /// The most deposit to bond, in FANG (default: the deposit at today's price plus 10%)
        #[arg(long)]
        storage_limit: Option<String>,
        /// Don't ask for confirmation (a deposit is bonded for good in v0)
        #[arg(long)]
        yes: bool,
        #[command(flatten)]
        signer: Signer,
    },
    /// Create a program from uploaded code and interface (tag 63), at an address made from your
    /// account and this transaction's nonce
    Create {
        /// The code's hash, as `upload` printed it
        wasm_hash: String,
        /// The interface's hash, as `upload` printed it
        interface_hash: String,
        /// Run this initializer (an `init` method of the interface) as the program is created
        #[arg(long)]
        init: Option<String>,
        /// The initializer's input (lowercase hex)
        #[arg(long, requires = "init")]
        input: Option<String>,
        /// FANG to send the program with its initializer (if it's payable)
        #[arg(long, requires = "init")]
        value: Option<String>,
        /// Fuel for the initializer (default: program_max_tx_fuel)
        #[arg(long, requires = "init")]
        fuel: Option<u64>,
        /// The most deposit to bond, in FANG: the record's, and what the initializer's writes bond
        /// (default: the record's deposit plus 10%, which a program whose initializer writes needs more than)
        #[arg(long)]
        storage_limit: Option<String>,
        /// An account that may upgrade the program once a protocol version adds upgrades (an address)
        #[arg(long)]
        authority: Option<String>,
        /// Blocks' notice the authority gives of an upgrade (at least program_min_upgrade_delay_blocks)
        #[arg(long, requires = "authority")]
        delay: Option<u64>,
        /// What an upgrade may change: `open` or `additive`
        #[arg(long, requires = "authority", value_parser = ["open", "additive"])]
        policy: Option<String>,
        /// The program takes plain transfers (`Transfer`) to its address
        #[arg(long)]
        accepts_transfers: bool,
        /// A program the initializer may reach, `ADDRESS[@VERSION_DIGEST]` (the version in force if left out; repeatable)
        #[arg(long = "bind", requires = "init")]
        bindings: Vec<String>,
        /// Don't ask for confirmation (the record's deposit is bonded for good in v0, and the program's account costs its creation fee)
        #[arg(long)]
        yes: bool,
        #[command(flatten)]
        signer: Signer,
    },
    /// Call a program's method (tag 64): a transaction, charged for the fuel it reserves
    Call {
        /// The program's address (never a name)
        address: String,
        /// The method (a `call` method of its interface; `view` methods are read with `view`)
        method: String,
        /// The method's input (lowercase hex)
        #[arg(long)]
        input: Option<String>,
        /// FANG to send with the call (the method must be payable)
        #[arg(long)]
        value: Option<String>,
        /// Fuel to reserve (default: program_max_tx_fuel); unused fuel isn't refunded
        #[arg(long)]
        fuel: Option<u64>,
        /// The most deposit the call's writes may bond, in FANG. Default 0, which consents to no new
        /// storage: a method that writes needs a limit, and without one the call traps
        /// `storage_limit` and still pays its fee (the CLI says so before it signs)
        #[arg(long)]
        storage_limit: Option<String>,
        /// A program the call may reach, `ADDRESS[@VERSION_DIGEST]` (the version in force if left out; repeatable)
        #[arg(long = "bind")]
        bindings: Vec<String>,
        /// Don't ask for confirmation of a call that sends FANG
        #[arg(long)]
        yes: bool,
        #[command(flatten)]
        signer: Signer,
    },
    /// Run a program's `view` method on the node, as of its newest block: nothing is signed or paid
    View {
        /// The program's address (never a name)
        address: String,
        /// The method (a `view` method of its interface)
        method: String,
        /// The method's input (lowercase hex)
        #[arg(long)]
        input: Option<String>,
        /// A program the view may reach, `ADDRESS[@VERSION_DIGEST]` (repeatable)
        #[arg(long = "bind")]
        bindings: Vec<String>,
        /// The most fuel the view may use (the node caps it)
        #[arg(long)]
        fuel: Option<u64>,
        #[command(flatten)]
        rpc: Rpc,
    },
    /// A program: its record, who may upgrade it, its balance, storage and methods
    Show {
        address: String,
        #[command(flatten)]
        rpc: Rpc,
    },
    /// A stored code: its size, methods, uploader, deposit and references
    Code {
        hash: String,
        #[command(flatten)]
        rpc: Rpc,
    },
    /// A stored interface by its hash, or (`interface encode <iface.json>`, offline) an interface's
    /// bytes and hash
    Interface {
        /// An interface hash, or `encode`
        what: String,
        /// With `encode`: the interface's JSON file
        file: Option<PathBuf>,
        #[command(flatten)]
        rpc: Rpc,
    },
    /// For a validator: pause programs with the others (tag 69), from the validator's own account
    /// with its consensus key. Prints what a pause does, the live votes and whether this validator
    /// may sign now, and asks
    Pause {
        /// The validator's key file (its home's validator.key)
        #[arg(long)]
        key: PathBuf,
        /// Don't ask for confirmation
        #[arg(long)]
        yes: bool,
        #[arg(long, default_value = "http://localhost:7070")]
        rpc: String,
        #[command(flatten)]
        pin: ChainPin,
    },
}

pub fn program(cmd: ProgramCmd, dir: &Path) -> Result<()> {
    match cmd {
        ProgramCmd::Upload { code, interface, storage_limit, yes, signer } => upload(dir, &code, &interface, storage_limit, yes, &signer),
        ProgramCmd::Create { wasm_hash, interface_hash, init, input, value, fuel, storage_limit, authority, delay, policy, accepts_transfers, bindings, yes, signer } => {
            let args = CreateArgs { wasm_hash, interface_hash, init, input, value, fuel, storage_limit, authority, delay, policy, accepts_transfers, bindings, yes };
            create(dir, args, &signer)
        }
        ProgramCmd::Call { address, method, input, value, fuel, storage_limit, bindings, yes, signer } => call(dir, CallArgs { address, method, input, value, fuel, storage_limit, bindings, yes }, &signer),
        ProgramCmd::View { address, method, input, bindings, fuel, rpc } => view(&address, &method, input, bindings, fuel, &rpc.rpc),
        ProgramCmd::Show { address, rpc } => {
            let address = address_arg(&address)?;
            let v = rt()?.block_on(get(&rpc.rpc, &format!("/program/{address}")))?;
            print!("{}", show_lines(&v).join("\n"));
            println!();
            Ok(())
        }
        ProgramCmd::Code { hash, rpc } => {
            let hash = hash_arg(&hash)?;
            let v = rt()?.block_on(get(&rpc.rpc, &format!("/code/{hash}")))?;
            println!("code {hash}: {} bytes, uploaded by {} at block {} for a deposit of {} drops ({} program{} use it)", v["size"], v["uploader"]["address"].as_str().unwrap_or("?"), v["uploaded_at"], v["deposit"].as_str().unwrap_or("0"), v["refs"], if v["refs"] == 1 { "" } else { "s" });
            println!("   methods (named by the deployer): {}", names(&v["untrusted_exports"]));
            Ok(())
        }
        ProgramCmd::Interface { what, file, rpc } => interface(&what, file.as_deref(), &rpc.rpc),
        ProgramCmd::Pause { key, yes, rpc, pin } => pause_cmd(dir, &key, yes, &rpc, &pin),
    }
}

// ------------------------------------------------------------------------------- arguments

/// A code or interface hash: 64 lowercase hex characters.
pub fn hash_arg(s: &str) -> Result<String> {
    let s = s.trim();
    ensure!(s.len() == 64 && crypto::is_lower_hex(s), "{s:?} isn't a hash: 64 lowercase hex characters, as `blockula program upload` prints them");
    Ok(s.to_string())
}

/// A program's address. Never a name: a name moves with its owner, and what a transaction binds is
/// the program (PRG 8.1).
pub fn address_arg(s: &str) -> Result<String> {
    let s = s.trim();
    ensure!(crypto::is_address(s), "{s:?} isn't an address: programs and bindings are named by their address (boo1… or tboo1…), never by a .boo name");
    Ok(s.to_string())
}

/// Input as the transaction carries it: lowercase hex of whole bytes, up to `MAX_INPUT`.
pub fn input_arg(s: Option<&str>) -> Result<Vec<u8>> {
    let s = s.unwrap_or_default().trim();
    ensure!(s.len().is_multiple_of(2) && crypto::is_lower_hex(s), "--input is lowercase hex of whole bytes (what the method reads), not {s:?}");
    let bytes = hex::decode(s)?;
    ensure!(bytes.len() <= blockula_vm::config::MAX_INPUT, "--input is {} bytes, past the {} a call may carry", bytes.len(), blockula_vm::config::MAX_INPUT);
    Ok(bytes)
}

/// A `--bind ADDRESS[@VERSION_DIGEST]`: the program's address and the version it must be at, if said.
pub fn binding_arg(s: &str) -> Result<(String, Option<String>)> {
    let (address, digest) = match s.split_once('@') {
        Some((a, d)) => (a, Some(d.trim())),
        None => (s, None),
    };
    let digest = digest.map(|d| hash_arg(d).context("a binding's version digest after the @")).transpose()?;
    Ok((address_arg(address)?, digest))
}

/// A version digest as a node reported it (`what`'s, in `record`), refused unless it's 64 lowercase hex:
/// it's printed and put in the transaction as it is, so a node that lies about it gets a refusal, not a
/// slice of a short string or a signature over text (what it says is echoed cut short and escaped).
fn node_digest(rpc: &str, what: &str, reported: &Value) -> Result<String> {
    let d = reported.as_str().ok_or_else(|| anyhow!("{rpc} doesn't report {what}'s version digest"))?;
    ensure!(d.len() == 64 && crypto::is_lower_hex(d), "{rpc} reports {what}'s version digest as {:?}, which isn't 64 lowercase hex characters. Nothing was signed: use another node", d.chars().take(40).collect::<String>());
    Ok(d.to_string())
}

/// The most a transaction of `action`, reserving `reserved_fuel`, can be charged in fees: what the CLI
/// signs as its fee (`validator_cli::fee_ceiling`), asked before it's signed so the question can say it.
async fn fee_at_most(remote: &Remote, keys: &crate::crypto::Keys, from: &str, action: &Action, reserved_fuel: u64) -> Result<u64> {
    let tx = unsigned_tx(remote, keys, from, 0, 0, action.clone(), String::new());
    fee_ceiling(&remote.rpc, &tx, reserved_fuel).await
}

/// An optional FANG amount in drops (0 when absent).
fn fang_arg(what: &str, s: Option<&str>) -> Result<u64> {
    s.map_or(Ok(0), |s| parse_fang(s).with_context(|| format!("--{what}")))
}

// -------------------------------------------------------------------------- what the chain says

/// What the node says about programs on its chain, from `/api/fees` and `/api/params`: the bounds
/// every program transaction is held to and what storage costs. `programs are off` is an error for
/// every command that signs.
struct Bounds {
    schedule: &'static Schedule,
    max_tx_fuel: u64,
    max_block_fuel: u64,
    max_tx_code_bytes: usize,
    state_bytes: u64,
    storage_price_base: u64,
    state_soft_bytes: u64,
    min_upgrade_delay: u64,
}

impl Bounds {
    async fn read(rpc: &str) -> Result<Bounds> {
        let fees = get(rpc, "/fees").await?;
        let fuel = &fees["fuel"];
        ensure!(fuel.is_object(), "programs aren't on at {rpc}: this chain hasn't switched the `programs` feature on (a chain made with `blockula dev --programs` or `blockula summon --programs` has them from block 1)");
        let params = get(rpc, "/params").await?;
        let param = |k: &str| params["programs"]["params"][k]["value"].as_str().and_then(|v| v.parse::<u64>().ok()).ok_or_else(|| anyhow!("{rpc}/api/params has no programs parameter {k}"));
        let num = |v: &Value, k: &str| v[k].as_u64().ok_or_else(|| anyhow!("{rpc}/api/fees has no fuel.{k}"));
        // `schedule` is the fuel schedule's own number (0: `SCHEDULE_V0`), which a later protocol version may replace.
        let number = num(fuel, "schedule")? as u32;
        let schedule = Some(&SCHEDULE_V0).filter(|s| s.version == number).ok_or_else(|| anyhow!("{rpc} prices fuel under schedule {number}, which this release doesn't know: update blockula"))?;
        Ok(Bounds {
            schedule,
            max_tx_fuel: num(fuel, "max_tx_fuel")?,
            max_block_fuel: num(fuel, "max_block_fuel")?,
            max_tx_code_bytes: num(fuel, "max_tx_code_bytes")? as usize,
            state_bytes: num(fuel, "state_bytes")?,
            storage_price_base: param("program_storage_price")?,
            state_soft_bytes: param("program_state_soft_bytes")?,
            min_upgrade_delay: param("program_min_upgrade_delay_blocks")?,
        })
    }

    /// What the chain bonds for `blobs` (each its size and whether it's new) as one upload, at
    /// program state's bytes once each lands, code first (§2.2 step 5): `None` past the ceiling.
    fn upload_deposit(&self, blobs: &[(usize, bool)]) -> Option<u64> {
        let mut used = self.state_bytes;
        let mut total = 0u64;
        for &(size, new) in blobs.iter().filter(|(_, new)| *new) {
            debug_assert!(new);
            let size = size as u64;
            used = used.checked_add(size)?;
            let price = blockula_vm::storage::price(self.storage_price_base, self.state_soft_bytes, used)?;
            total = total.checked_add(price.checked_mul(size)?)?;
        }
        Some(total)
    }

    /// The record deposit of a create (§2.3 step 2): `RECORD_BYTES` at the price once it lands.
    fn record_deposit(&self) -> Option<u64> {
        let used = self.state_bytes.checked_add(crate::programs::RECORD_BYTES)?;
        blockula_vm::storage::price(self.storage_price_base, self.state_soft_bytes, used)?.checked_mul(crate::programs::RECORD_BYTES)
    }

    /// What a create or call reserves: its fuel and the load of the distinct code it declares, one
    /// entry per code hash with its size (§5.1), refused as admission refuses it.
    fn reservation(&self, fuel_limit: u64, code: &std::collections::BTreeMap<String, usize>) -> Result<u64> {
        ensure!(fuel_limit <= self.max_tx_fuel, "--fuel {fuel_limit} is above program_max_tx_fuel ({})", self.max_tx_fuel);
        let size: usize = code.values().sum();
        ensure!(size <= self.max_tx_code_bytes, "the code this transaction declares is {size} bytes, past the {} one transaction may load: bind fewer or smaller programs", self.max_tx_code_bytes);
        let load = code.values().fold(0u64, |sum, &s| sum.saturating_add(self.schedule.load_fuel(s)));
        let reserved = fuel_limit.saturating_add(load);
        ensure!(reserved <= self.max_block_fuel, "this transaction would reserve {reserved} fuel ({fuel_limit} to run and {load} to load its code), and a block counts at most {} (program_max_block_fuel): lower --fuel or bind less", self.max_block_fuel);
        Ok(reserved)
    }
}

/// `GET path`, or `None` for a 404 (a stored blob or a program that isn't there yet).
async fn try_get(rpc: &str, path: &str) -> Result<Option<Value>> {
    let res = reqwest::get(format!("{}/api{path}", rpc.trim_end_matches('/'))).await?;
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let ok = res.status().is_success();
    let v: Value = res.json().await?;
    ensure!(ok, "{}", v["error"].as_str().unwrap_or("request failed"));
    Ok(Some(v))
}

/// How many one-second polls `create` and `call` wait for what they name to be usable.
const WAIT_TRIES: u32 = 90;

/// Waits until what `path` serves is usable: stored, and the block after the one that stored it
/// has been executed (`field` is its `uploaded_at` or `created_at`; §2.1, A5, and the rule the SDK
/// keeps: one block past the block that stored it), saying so once, for at most `tries` seconds. A
/// transaction that needs it is refused by the mempool before then.
async fn wait_usable(remote: &Remote, what: &str, path: &str, field: &str, missing: &str, tries: u32) -> Result<Value> {
    let mut said = false;
    for _ in 0..tries {
        let height = remote.status().await?["height"].as_u64().unwrap_or(0);
        match try_get(&remote.rpc, path).await? {
            Some(v) if v[field].as_u64().is_some_and(|at| at < height) => return Ok(v),
            found => {
                if !said {
                    let how = match found.as_ref().and_then(|v| v[field].as_u64()) {
                        Some(at) => format!("stored in block {at}, and usable from the block after it"),
                        None => "not stored yet (an upload in flight shows up within a few blocks)".to_string(),
                    };
                    eprintln!("⏳ waiting for {what}: {how}");
                    said = true;
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    bail!("{what} isn't there after {tries} seconds: {missing}")
}

// ----------------------------------------------------------------------------------- signing

/// Connects to the node `signer` names (checked to be on the chain the command is for), reads the
/// bounds programs have there, and gives `body` the remote, the keys, the account to sign for and
/// the bounds.
fn with_signer<T>(dir: &Path, signer: &Signer, body: impl AsyncFnOnce(&Remote, &crate::crypto::Keys, &str, &Bounds) -> Result<T>) -> Result<T> {
    let keys = signer.keys(dir)?;
    rt()?.block_on(async {
        let remote = signer.connect(dir).await?;
        let from = signer.acting_for(&remote, &keys).await?;
        let bounds = Bounds::read(&remote.rpc).await?;
        body(&remote, &keys, &from, &bounds).await
    })
}

/// Signs `action` (reserving `reserved_fuel`), submits it, waits for it to be final and prints what
/// it did: its receipt, whether it applied or failed (a failed program transaction was charged and
/// has one). `Err` if it failed.
async fn send(remote: &Remote, keys: &crate::crypto::Keys, from: &str, action: Action, opts: SendOpts) -> Result<Value> {
    let (id, valid_until) = submit_with(remote, keys, from, |_| Ok(action), String::new(), opts).await?;
    let done = wait_final(&remote.rpc, &id, valid_until).await;
    // A transaction that ran has a receipt either way; one the node dropped has none.
    let ran = match &done {
        Ok(tx) => Some(tx.clone()),
        Err(_) => get(&remote.rpc, &format!("/tx/{id}")).await.ok().filter(|t| t["program"].is_object()),
    };
    println!("{}", outcome_lines(&id, ran.as_ref()).join("\n"));
    println!("  {}/#/tx/{id}", remote.rpc.trim_end_matches('/'));
    done
}

/// What `send` prints once it has waited: the receipt of a transaction that ran, or, for one that left
/// none (the node dropped it, or it's still pending after the wait), only that it was submitted, which
/// is all the CLI knows of it.
fn outcome_lines(id: &str, ran: Option<&Value>) -> Vec<String> {
    match ran {
        Some(tx) => receipt_lines(tx),
        None => vec![format!("submitted {id} (not final yet)")],
    }
}

// ------------------------------------------------------------------------------------ upload

fn upload(dir: &Path, code: &Path, interface: &Path, storage_limit: Option<String>, yes: bool, signer: &Signer) -> Result<()> {
    let wasm = std::fs::read(code).with_context(|| format!("reading {}", code.display()))?;
    let iface = interface_bytes(interface)?;
    let (wasm_hash, interface_hash) = (hex::encode(digest::wasm_hash(&wasm)), hex::encode(digest::interface_hash(&iface)));
    // What the chain would refuse, found before a fee is spent on it: a failed upload is charged.
    let info = blockula_vm::validate::validate(&wasm).map_err(|e| anyhow!("the chain would refuse this code (upload validation rule {} {:?}{}): nothing was signed", e.rule.number(), e.rule, e.func.map(|f| format!(" in function {f}")).unwrap_or_default()))?;
    let decoded = blockula_vm::interface::decode(&iface).map_err(|e| anyhow!("the interface doesn't decode: {e}"))?;
    blockula_vm::interface::check_exports(&decoded, &info.exports).map_err(|m| anyhow!("the interface doesn't match the code: {m}"))?;
    let limit = storage_limit.as_deref().map(parse_fang).transpose()?;
    println!("code      {wasm_hash}  ({} bytes)", wasm.len());
    println!("interface {interface_hash}  ({} bytes, {} method{})", iface.len(), decoded.methods.len(), if decoded.methods.len() == 1 { "" } else { "s" });
    with_signer(dir, signer, async |remote, keys, from, bounds| {
        let stored = |path: String| async move { try_get(&remote.rpc, &path).await.map(|v| v.is_some()) };
        let (code_stored, iface_stored) = (stored(format!("/code/{wasm_hash}")).await?, stored(format!("/interface/{interface_hash}")).await?);
        let deposit = bounds.upload_deposit(&[(wasm.len(), !code_stored), (iface.len(), !iface_stored)]).ok_or_else(|| anyhow!("program state is full: nothing more can be stored at any price"))?;
        println!("deposit   {} bonded for what's new ({}), at today's price; v0 has no way to take a deposit back", fmt_fang(deposit), match (code_stored, iface_stored) {
            (false, false) => "the code and the interface",
            (true, false) => "the interface: the code is stored already",
            (false, true) => "the code: the interface is stored already",
            (true, true) => "nothing: both are stored already, so this only refreshes them",
        });
        let storage_limit = match limit {
            Some(l) => {
                ensure!(l >= deposit, "--storage-limit {} is below the deposit {}: the upload would fail and still be charged its fee", fmt_fang(l), fmt_fang(deposit));
                l
            }
            None => deposit.saturating_add(deposit / 10),
        };
        let action = Action::UploadCode { wasm: HexBytes(wasm.clone()), interface: HexBytes(iface.clone()), storage_limit };
        let reserved = bounds.schedule.upload_fuel(wasm.len(), iface.len());
        ensure!(reserved <= bounds.max_block_fuel, "this upload reserves {reserved} fuel, more than a block counts ({})", bounds.max_block_fuel);
        let fee = fee_at_most(remote, keys, from, &action, reserved).await?;
        println!("fees      at most {} ({reserved} fuel for the checks, not refunded)", fmt_fang(fee));
        confirm(&upload_question(&wasm_hash, deposit, fee), yes)?;
        send(remote, keys, from, action, SendOpts { reserved_fuel: reserved, nonce: None }).await?;
        println!("   usable from the block after this one: `blockula program create {wasm_hash} {interface_hash}`");
        Ok(())
    })
}

/// What `upload` asks before it signs: the deposit, which v0 never gives back, and the most the fee can be.
fn upload_question(wasm_hash: &str, deposit: u64, fee: u64) -> String {
    if deposit == 0 {
        return format!("Upload {}…, which only refreshes what's stored (nothing new to bond), for at most {} in fees?", &wasm_hash[..10], fmt_fang(fee));
    }
    format!("Upload {}…, with {} of deposit bonded for good (v0 never gives it back) and at most {} in fees?", &wasm_hash[..10], fmt_fang(deposit), fmt_fang(fee))
}

/// An interface file's bytes: §3.4's JSON through the one codec, refused in its own words.
fn interface_bytes(path: &Path) -> Result<Vec<u8>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    crate::programs::interface_json::encode(&text).with_context(|| format!("{} isn't an interface", path.display()))
}

fn interface(what: &str, file: Option<&Path>, rpc: &str) -> Result<()> {
    if what == "encode" {
        let file = file.ok_or_else(|| anyhow!("`interface encode` takes the interface's JSON file"))?;
        let bytes = interface_bytes(file)?;
        println!("interface_hash {}", hex::encode(digest::interface_hash(&bytes)));
        println!("bytes          {}", hex::encode(&bytes));
        return Ok(());
    }
    ensure!(file.is_none(), "`interface <hash>` takes no file (did you mean `interface encode {}`?)", file.map(|f| f.display().to_string()).unwrap_or_default());
    let hash = hash_arg(what)?;
    let v = rt()?.block_on(get(rpc, &format!("/interface/{hash}")))?;
    println!("interface {hash}: {} bytes, uploaded by {} at block {}", v["size"], v["uploader"]["address"].as_str().unwrap_or("?"), v["uploaded_at"]);
    println!("   methods (named by the deployer):");
    for m in v["untrusted_interface"]["methods"].as_array().into_iter().flatten() {
        println!("   - {} ({}{}) {}", m["name"].as_str().unwrap_or("?"), m["kind"].as_str().unwrap_or("?"), if m["payable"] == true { ", payable" } else { "" }, m["args"]);
    }
    Ok(())
}

// ------------------------------------------------------------------------------------ create

struct CreateArgs {
    wasm_hash: String,
    interface_hash: String,
    init: Option<String>,
    input: Option<String>,
    value: Option<String>,
    fuel: Option<u64>,
    storage_limit: Option<String>,
    authority: Option<String>,
    delay: Option<u64>,
    policy: Option<String>,
    accepts_transfers: bool,
    bindings: Vec<String>,
    yes: bool,
}

/// Binds `specs` (`ADDRESS[@DIGEST]`): each program's record read from the node, a missing digest
/// filled with the version in force (printed), and the distinct code the bound programs add to what
/// the transaction declares (by hash, with its size).
async fn bind(rpc: &str, specs: &[String], code: &mut std::collections::BTreeMap<String, usize>) -> Result<Vec<Binding>> {
    let mut bound: Vec<Binding> = vec![];
    for spec in specs {
        let (address, given) = binding_arg(spec)?;
        ensure!(!bound.iter().any(|b| b.program == address), "{address} is bound twice");
        let record = get(rpc, &format!("/program/{address}")).await.with_context(|| format!("binding {address}"))?;
        let version_digest = match given {
            Some(d) => d,
            None => {
                let in_force = node_digest(rpc, &address, &record["version_digest"])?;
                println!("   binding {address} at its version in force now, {in_force}");
                in_force
            }
        };
        if let (Some(hash), Some(size)) = (record["code"]["wasm_hash"].as_str(), record["code"]["size"].as_u64()) {
            code.insert(hash.to_string(), size as usize);
        }
        bound.push(Binding { program: address, version_digest });
    }
    Ok(bound)
}

fn create(dir: &Path, a: CreateArgs, signer: &Signer) -> Result<()> {
    let (wasm_hash, interface_hash) = (hash_arg(&a.wasm_hash)?, hash_arg(&a.interface_hash)?);
    let input = input_arg(a.input.as_deref())?;
    let value = fang_arg("value", a.value.as_deref())?;
    let authority = a.authority.as_deref().map(address_arg).transpose()?;
    if authority.is_none() {
        ensure!(a.delay.is_none() && a.policy.is_none(), "--delay and --policy are an upgrade authority's: pass --authority (an immutable program has neither)");
    }
    let policy = match a.policy.as_deref() {
        Some("additive") => ProgramPolicy::Additive,
        _ => ProgramPolicy::Open,
    };
    with_signer(dir, signer, async |remote, keys, from, bounds| {
        let rpc = remote.rpc.as_str();
        // Both blobs usable: stored in an earlier block (the upload may be a block away).
        let code = wait_usable(remote, "the code", &format!("/code/{wasm_hash}"), "uploaded_at", "upload it first with `blockula program upload`", WAIT_TRIES).await?;
        wait_usable(remote, "the interface", &format!("/interface/{interface_hash}"), "uploaded_at", "upload it first with `blockula program upload`", WAIT_TRIES).await?;
        let mut declared = std::collections::BTreeMap::new();
        if a.init.is_some() {
            declared.insert(wasm_hash.clone(), code["size"].as_u64().ok_or_else(|| anyhow!("{rpc} doesn't report the code's size"))? as usize);
        }
        let bindings = bind(rpc, &a.bindings, &mut declared).await?;
        let fuel_limit = if a.init.is_some() { a.fuel.unwrap_or(bounds.max_tx_fuel) } else { 0 };
        let reserved = bounds.reservation(fuel_limit, &declared)?;
        let delay = authority.as_ref().map_or(0, |_| a.delay.unwrap_or(bounds.min_upgrade_delay));
        if authority.is_some() {
            ensure!(delay >= bounds.min_upgrade_delay, "--delay {delay} is below program_min_upgrade_delay_blocks ({}): an upgradable program gives at least that much notice", bounds.min_upgrade_delay);
        }
        let record_deposit = bounds.record_deposit().ok_or_else(|| anyhow!("program state is full: nothing more can be stored at any price"))?;
        let storage_limit = match a.storage_limit.as_deref() {
            Some(l) => {
                let l = parse_fang(l).context("--storage-limit")?;
                ensure!(l >= record_deposit, "--storage-limit {} is below the record deposit {}: the create would fail and still be charged its fee", fmt_fang(l), fmt_fang(record_deposit));
                l
            }
            None => record_deposit.saturating_add(record_deposit / 10),
        };
        // The address this transaction makes, from its sender and the nonce it will carry (§2.1).
        let nonce = get(rpc, &format!("/account/{from}")).await?["next_nonce"].as_u64().unwrap_or(0);
        let creator = crypto::address_bytes(from).ok_or_else(|| anyhow!("{from} isn't an address"))?;
        let address = digest::program_address(&remote.prefix, &creator, nonce);
        let opening = program_opening(rpc, &address).await?;
        let init = a.init.as_ref().map(|method| InitCall { method: method.clone(), input: HexBytes(input.clone()), value });
        let action = Action::CreateProgram { wasm_hash: wasm_hash.clone(), interface_hash: interface_hash.clone(), authority: authority.clone(), upgrade_delay_blocks: delay, policy, accepts_transfers: a.accepts_transfers, init, fuel_limit, storage_limit, bindings };
        let cost = CreateCost { address: address.clone(), nonce, record_deposit, opening, fee: fee_at_most(remote, keys, from, &action, reserved).await?, reserved, init: a.init.is_some(), value, authority: authority.clone(), delay };
        println!("{}", create_lines(&cost).join("\n"));
        confirm(&create_question(&cost), a.yes)?;
        send(remote, keys, from, action, SendOpts { reserved_fuel: reserved, nonce: Some(nonce) }).await?;
        println!("   usable from the block after this one: `blockula program call {address} <method>`");
        Ok(())
    })
}

/// How a create opens the program's account (§2.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Opening {
    /// FANG sent to the address ahead opened it already: the create adopts that account, and no
    /// creation fee burns.
    Adopted,
    /// Nothing holds an account there: the create opens it, and burns this creation fee from its
    /// creator (the chain's `account_creation_fee`, which a chain may set to 0).
    Burns(u64),
}

impl Opening {
    /// The creation fee that burns, in drops.
    fn fee(self) -> u64 {
        match self {
            Opening::Adopted => 0,
            Opening::Burns(fee) => fee,
        }
    }
}

/// How a create of the program at `address` opens its account, from the node: `/api/account` says
/// whether anything holds one there, and `/api/fees` the creation fee.
async fn program_opening(rpc: &str, address: &str) -> Result<Opening> {
    match get(rpc, &format!("/account/{address}")).await?["exists"].as_bool() {
        Some(true) => Ok(Opening::Adopted),
        Some(false) => get(rpc, "/fees").await?["account_creation_fee"].as_str().and_then(|f| f.parse::<u64>().ok()).map(Opening::Burns).ok_or_else(|| anyhow!("{rpc}/api/fees has no account_creation_fee")),
        None => bail!("{rpc} doesn't say whether {address} holds an account"),
    }
}

/// Everything a create costs, as `create` says it before it asks: all in drops.
struct CreateCost {
    address: String,
    nonce: u64,
    /// The record's deposit, bonded for good.
    record_deposit: u64,
    /// How the program's account is opened, and what that burns.
    opening: Opening,
    /// The most the fee can be.
    fee: u64,
    /// The fuel the create reserves (0 without `init`).
    reserved: u64,
    init: bool,
    /// What `init` is sent.
    value: u64,
    authority: Option<String>,
    delay: u64,
}

/// What `create` prints before it asks (§8.3): the address it makes, the warning about sending
/// anything there early, the deposit, the account creation fee that opening the account burns (or
/// that there's none) and the most the fee can be.
fn create_lines(c: &CreateCost) -> Vec<String> {
    let mut out = vec![
        format!("program   {}  (what this create makes, at nonce {})", c.address, c.nonce),
        "   ⚠️  send nothing to that address before this create is final: a failed create strands whatever is there, for good (docs/PROGRAMS-V0.md §2.3). Fund the program with --value, or with a transfer once it exists and takes them".to_string(),
        format!("   deposit {} for the record (bonded, and v0 has no way to take it back){}", fmt_fang(c.record_deposit), if c.init { "; the initializer's writes bond more, up to --storage-limit" } else { "" }),
    ];
    out.push(match c.opening {
        Opening::Adopted => "   FANG sent to that address already opened its account, which the create adopts: no creation fee burns".to_string(),
        Opening::Burns(0) => "   this chain charges no account creation fee for opening the program's account".to_string(),
        Opening::Burns(fee) => format!("   opening the program's account burns {} too (the account creation fee: gone for good, not a deposit)", fmt_fang(fee)),
    });
    out.push(format!("   fees    at most {}{}", fmt_fang(c.fee), if c.reserved > 0 { format!(" ({} fuel reserved for the initializer and its code, not refunded)", c.reserved) } else { String::new() }));
    out
}

/// What `create` asks before it signs: the whole cost, the deposit and the fee that don't come back
/// and what's sent.
fn create_question(c: &CreateCost) -> String {
    let writes = if c.init { " (and what the initializer's writes bond)" } else { "" };
    let burned = if c.opening.fee() > 0 { format!(", {} burned to open its account", fmt_fang(c.opening.fee())) } else { String::new() };
    let sent = if c.value > 0 { format!(", and {} sent to it with its initializer", fmt_fang(c.value)) } else { String::new() };
    let authority = c.authority.as_deref().map(|x| format!(", upgradable by {x} with {} blocks' notice once upgrades exist", c.delay)).unwrap_or_default();
    format!("Create {}: {} of deposit bonded for good{writes}{burned}, at most {} in fees{sent}{authority}?", c.address, fmt_fang(c.record_deposit), fmt_fang(c.fee))
}

// -------------------------------------------------------------------------------------- call

struct CallArgs {
    address: String,
    method: String,
    input: Option<String>,
    value: Option<String>,
    fuel: Option<u64>,
    storage_limit: Option<String>,
    bindings: Vec<String>,
    yes: bool,
}

fn call(dir: &Path, a: CallArgs, signer: &Signer) -> Result<()> {
    let address = address_arg(&a.address)?;
    let input = input_arg(a.input.as_deref())?;
    ensure!(blockula_vm::abi::is_ident(&a.method), "{:?} isn't a method name: a lowercase letter, then lowercase letters, digits and underscores, at most 32", a.method);
    let value = fang_arg("value", a.value.as_deref())?;
    let storage_limit = fang_arg("storage-limit", a.storage_limit.as_deref())?;
    with_signer(dir, signer, async |remote, keys, from, bounds| {
        let rpc = remote.rpc.as_str();
        let record = wait_usable(remote, "the program", &format!("/program/{address}"), "created_at", "create it first with `blockula program create`", WAIT_TRIES).await?;
        let version_digest = node_digest(rpc, &address, &record["version_digest"])?;
        // What the method is, from the interface this version names (the deployer's own words).
        let method = record["methods"].as_array().into_iter().flatten().find(|m| m["untrusted_name"] == a.method.as_str()).ok_or_else(|| anyhow!("the program's interface has no method {:?}: `blockula program show {address}` lists them", a.method))?;
        match method["kind"].as_str() {
            Some("call") => {}
            Some("view") => bail!("{:?} is a view: read it with `blockula program view {address} {}` (a transaction can't call it)", a.method, a.method),
            _ => bail!("{:?} is the program's initializer, which runs only when it's created", a.method),
        }
        ensure!(value == 0 || method["payable"] == true, "{:?} isn't payable, so the call can't send it FANG", a.method);
        let mut declared = std::collections::BTreeMap::new();
        if let (Some(hash), Some(size)) = (record["code"]["wasm_hash"].as_str(), record["code"]["size"].as_u64()) {
            declared.insert(hash.to_string(), size as usize);
        }
        let bindings = bind(rpc, &a.bindings, &mut declared).await?;
        let fuel_limit = a.fuel.unwrap_or(bounds.max_tx_fuel);
        ensure!(fuel_limit >= 1, "--fuel is at least 1");
        let reserved = bounds.reservation(fuel_limit, &declared)?;
        let call = ProgramCall { program: address.clone(), version_digest: version_digest.clone(), method: a.method.clone(), input: HexBytes(input.clone()), value };
        let action = Action::CallProgram { calls: vec![call], fuel_limit, storage_limit, bindings };
        let fee = fee_at_most(remote, keys, from, &action, reserved).await?;
        println!("{}", call_lines(&address, &version_digest, fee, reserved, a.storage_limit.is_some()).join("\n"));
        if value > 0 {
            confirm(&format!("Send {} to {address} with this call?", fmt_fang(value)), a.yes)?;
        }
        send(remote, keys, from, action, SendOpts { reserved_fuel: reserved, nonce: None }).await?;
        Ok(())
    })
}

/// What `call` prints before it signs: the program and the whole version digest it names, the most
/// the fee can be and, when no `--storage-limit` was given, that the call can't bond new storage.
fn call_lines(address: &str, version_digest: &str, fee: u64, reserved: u64, storage_limit_given: bool) -> Vec<String> {
    let mut out = vec![format!("calling {address} at version {version_digest}"), format!("   fees at most {} ({reserved} fuel reserved: unused fuel isn't refunded; --fuel lowers it)", fmt_fang(fee))];
    if !storage_limit_given {
        out.push("   storage limit 0: this call can't bond new storage; if the method writes, pass --storage-limit FANG (a call that traps on it still pays its fee)".to_string());
    }
    out
}

// -------------------------------------------------------------------------------------- view

fn view(address: &str, method: &str, input: Option<String>, bindings: Vec<String>, fuel: Option<u64>, rpc: &str) -> Result<()> {
    let address = address_arg(address)?;
    let input = input_arg(input.as_deref())?;
    ensure!(blockula_vm::abi::is_ident(method), "{method:?} isn't a method name");
    let bindings: Vec<Value> = bindings.iter().map(|b| binding_arg(b).map(|(program, version_digest)| json!({ "program": program, "version_digest": version_digest }))).collect::<Result<_>>()?;
    let mut body = json!({ "method": method, "input": hex::encode(input), "bindings": bindings });
    if let Some(fuel) = fuel {
        body["fuel"] = fuel.into();
    }
    let answer = rt()?.block_on(async {
        let res = reqwest::Client::new().post(format!("{}/api/program/{address}/view", rpc.trim_end_matches('/'))).json(&body).send().await?;
        let ok = res.status().is_success();
        let retry = res.headers().get(reqwest::header::RETRY_AFTER).and_then(|v| v.to_str().ok()).map(str::to_string);
        let v: Value = res.json().await?;
        ensure!(ok, "{}{}", v["error"].as_str().unwrap_or("the node refused the view"), retry.map(|s| format!(" (try again in {s} s)")).unwrap_or_default());
        Ok::<_, anyhow::Error>(v)
    })?;
    println!("{}", view_lines(&answer).join("\n"));
    Ok(())
}

/// A view's answer: what it returned (the program's claim) or how it failed, and the fuel it used.
pub fn view_lines(v: &Value) -> Vec<String> {
    let mut out = vec![];
    if v["ok"] == true {
        out.push(format!("the program reports: {}", if v["return_data"].as_str() == Some("") { "(no return data)".to_string() } else { format!("return data {}", v["return_data"].as_str().unwrap_or_default()) }));
    } else {
        out.push(format!("✗ the view failed: {}", outcome_words(&v["outcome"])));
    }
    out.push(format!("   fuel used {} · at block {} (the newest this node executed)", v["fuel_used"], v["height"]));
    out
}

// ------------------------------------------------------------------------------------ reading

fn names(v: &Value) -> String {
    v.as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default()
}

/// A program's record, for people: what it is, who could upgrade it, what it holds and its methods
/// (the deployer's names, marked as such).
pub fn show_lines(v: &Value) -> Vec<String> {
    let s = |x: &Value| x.as_str().unwrap_or("?").to_string();
    let mut out = vec![
        format!("program {}", s(&v["address"])),
        format!("   created by {} at block {}", s(&v["creator"]["address"]), v["created_at"]),
        format!("   code {} ({} bytes), interface {}", s(&v["code"]["wasm_hash"]), v["code"]["size"], s(&v["code"]["interface_hash"])),
        format!("   version {} · digest {} (what a call binds)", v["version"], s(&v["version_digest"])),
        format!("   {}", s(&v["upgradability"])),
        format!("   holds {} · takes plain transfers: {}", s(&v["balance_display"]), if v["accepts_transfers"] == true { "yes" } else { "no" }),
        format!("   storage: {} entries, {} bytes, deposits {} drops ({} paid by the program itself)", v["storage"]["items"], v["storage"]["bytes"], s(&v["storage"]["deposit"]), s(&v["storage"]["sponsored"])),
        format!("   record deposit {} drops, paid by {}", s(&v["record_deposit"]), s(&v["record_payer"]["address"])),
        "   methods (named by the deployer):".to_string(),
    ];
    for m in v["methods"].as_array().into_iter().flatten() {
        let args: Vec<String> = m["args"].as_array().into_iter().flatten().map(|a| format!("{}: {}", s(&a["untrusted_name"]), a["type"])).collect();
        out.push(format!("   - {}({}) {}{}{}", s(&m["untrusted_name"]), args.join(", "), s(&m["kind"]), if m["payable"] == true { ", payable" } else { "" }, if m["returns"].is_null() { String::new() } else { format!(", returns {}", m["returns"]) }));
    }
    out
}

/// A failed outcome in words, from its JSON (§8.1): numbers, and the program's own sanitized message.
fn outcome_words(o: &Value) -> String {
    let program = o["program"].as_str().map(crate::chain::short).unwrap_or_default();
    match o["code"].as_str() {
        Some("trapped") => {
            let hint = if o["trap"]["name"] == "storage_limit" { ": the call's writes bond a deposit, and --storage-limit (0 unless given) was too small for it" } else { "" };
            format!("program {program} trapped: {} (trap {}){hint}", o["trap"]["name"].as_str().unwrap_or("?").replace('_', " "), o["trap"]["number"])
        }
        Some("aborted") => format!("program {program} aborted with code {}{}", o["abort_code"], o["untrusted_message"].as_str().filter(|m| !m.is_empty()).map(|m| format!(" and the message {m:?} (the program's words)")).unwrap_or_default()),
        Some("commit") => format!("the changes failed the chain's own commit checks (commit error {})", o["commit_error"]),
        Some("value_cap") => format!("programs would hold more than the value cap ({} drops)", o["cap"].as_str().unwrap_or("?")),
        Some("exit_only") => "programs are exit-only: a call may not put more FANG under programs' control".into(),
        Some("storage_limit") => "the deposits passed --storage-limit".into(),
        Some("state_full") => "program state is full".into(),
        Some("invalid_code") => format!("the code was refused: validation rule {} ({})", o["rule"]["number"], o["rule"]["name"].as_str().unwrap_or("?")),
        Some("translation_refused") => "the engine refused to translate the code".into(),
        Some("interface_mismatch") => "the interface's methods aren't the code's exports".into(),
        Some("native") => "a native check failed (funds, locks or the creation fee)".into(),
        other => format!("{other:?}"),
    }
}

/// What a finished program transaction did, for people: its outcome, what it cost, what changed
/// (facts, from the chain) apart from what the programs said (claims, which the chain doesn't check).
pub fn receipt_lines(tx: &Value) -> Vec<String> {
    let s = |x: &Value| x.as_str().unwrap_or("?").to_string();
    let p = &tx["program"];
    if !p.is_object() {
        return vec![format!("✓ {}", s(&tx["summary"]))];
    }
    let mut out = vec![];
    let facts = &p["facts"];
    if p["outcome"]["ok"] == true {
        out.push(format!("✓ {}", s(&tx["summary"])));
    } else {
        out.push(format!("✗ it ran and failed: {} (the fee was charged)", outcome_words(&p["outcome"])));
    }
    let fee = &facts["fee"];
    out.push(format!("   fee {} (bytes {} + fuel {} + tip {} drops)", s(&tx["fee_display"]), s(&fee["bytes"]), s(&fee["fuel"]), s(&fee["tip"])));
    let fuel = &facts["fuel"];
    out.push(format!("   fuel: reserved {} ({} to run, {} to load code) at {} milli-drops each, used {}; unused fuel isn't refunded", fuel["reserved"], fuel["limit"], fuel["load"], fuel["rate"], fuel["used"]));
    if let Some(addr) = facts["created"].as_str() {
        out.push(format!("   created program {addr}"));
    }
    if facts["upload"].is_object() {
        let u = &facts["upload"];
        out.push(format!("   stored code {} ({}) and interface {} ({})", s(&u["wasm_hash"]), if u["new_code"] == true { "new" } else { "already stored" }, s(&u["interface_hash"]), if u["new_interface"] == true { "new" } else { "already stored" }));
    }
    let storage = &facts["storage"];
    if storage["bonded"] != "0" || storage["refunded"] != "0" || storage["bytes_delta"] != 0 {
        // What was bonded, in all: informational. Whose balance paid it is in the lines around it.
        out.push(format!("   storage: {:+} bytes, {} drops bonded, {} refunded (all that was bonded, whoever paid it)", storage["bytes_delta"].as_i64().unwrap_or(0), s(&storage["bonded"]), s(&storage["refunded"])));
    }
    // What the signer was debited outside the programs' own balance changes (below): deposits a
    // create or an upload bonds, and the creation fee of the program's account (burned).
    let outside = &facts["outside_vm"];
    if outside["total"].as_str().is_some_and(|t| t != "0") {
        let parts: Vec<String> = [("record_deposit", "record deposit"), ("upload_deposit", "upload deposit"), ("account_creation_fee", "account creation fee (burned)")].iter().filter(|(k, _)| outside[*k] != "0").map(|(k, what)| format!("{what} {}", s(&outside[*k]))).collect();
        out.push(format!("   also debited from the signer: {} = {} drops", parts.join(" + "), s(&outside["total"])));
    }
    for c in facts["balance_changes"].as_array().into_iter().flatten() {
        out.push(format!("   {} {} drops", s(&c["account"]), s(&c["change"])));
    }
    let claims = &p["claims"];
    let (returns, events): (Vec<&Value>, Vec<&Value>) = (claims["return_data"].as_array().into_iter().flatten().filter(|r| r["data"] != "").collect(), claims["events"].as_array().into_iter().flatten().collect());
    if !returns.is_empty() || !events.is_empty() || claims["omitted"].is_object() {
        out.push("   the programs report (claims: the chain checks none of this):".into());
        for r in returns {
            out.push(format!("   - {} returned {}", s(&r["untrusted_method"]), s(&r["data"])));
        }
        for e in events {
            out.push(format!("   - {} emitted {} with {}", crate::chain::short(&s(&e["program"])), s(&e["untrusted_name"]), s(&e["data"])));
        }
        if let Some(o) = claims["omitted"].as_object() {
            out.push(format!("   - {} events and {} bytes of return data were left out of the receipt (the block's claims budget)", o["events"], o["return_bytes"]));
        }
    }
    out
}

// ------------------------------------------------------------------------------------- pause

/// What `/api/params`' `programs` section says about pauses, as `pause::Record`'s parts.
pub struct PauseFacts {
    pub pause: Option<crate::programs::state::Pause>,
    pub votes: std::collections::BTreeMap<String, u64>,
    pub pause_signed: std::collections::BTreeMap<String, u64>,
    pub fix_pending: bool,
}

/// [`PauseFacts`] from the `programs` section of `/api/params` (`programs::api::params_section`).
pub fn pause_facts(section: &Value) -> Result<PauseFacts> {
    let at = |list: &Value| -> Result<std::collections::BTreeMap<String, u64>> {
        list.as_array().into_iter().flatten().map(|e| Ok((e["validator"]["address"].as_str().ok_or_else(|| anyhow!("a pause entry without its validator"))?.to_string(), e["at"].as_u64().ok_or_else(|| anyhow!("a pause entry without its height"))?))).collect()
    };
    let n = |p: &Value, k: &str| p[k].as_u64().ok_or_else(|| anyhow!("the pause has no {k}"));
    let pause = match &section["pause"] {
        Value::Null => None,
        p => Some(crate::programs::state::Pause { from: n(p, "from")?, until: n(p, "until")?, started: n(p, "started")?, signers: vec![] }),
    };
    Ok(PauseFacts { pause, votes: at(&section["votes"])?, pause_signed: at(&section["pause_signed"])?, fix_pending: section["fix_pending"].as_bool().ok_or_else(|| anyhow!("/api/params doesn't say whether a fix is pending: update the node"))? })
}

/// Whether the validator `from` may sign a pause at `height`: the chain's own rules
/// (`pause::may_sign_with`) over what the node serves, the one check `pause` makes before it signs.
fn may_sign_now(facts: &PauseFacts, spans: &pause::Spans, from: &str, height: u64) -> Result<()> {
    let record = pause::Record { pause: facts.pause.as_ref(), votes: &facts.votes, pause_signed: &facts.pause_signed };
    pause::may_sign_with(&record, spans, from, height, || facts.fix_pending)
}

fn pause_cmd(dir: &Path, key: &Path, yes: bool, rpc: &str, pin: &ChainPin) -> Result<()> {
    let keys = load_seed(key)?;
    rt()?.block_on(async {
        let remote = Remote::connect(dir, rpc, pin).await?;
        let from = remote.address(&keys);
        let params = get(&remote.rpc, "/params").await?;
        let section = &params["programs"];
        ensure!(section.is_object(), "programs have never been switched on at {}: there's nothing to pause", remote.rpc);
        let facts = pause_facts(section)?;
        let validators = get(&remote.rpc, "/validators").await?;
        let active: Vec<&str> = validators["active"].as_array().into_iter().flatten().filter_map(|v| v["validator"]["address"].as_str()).collect();
        ensure!(active.contains(&from.as_str()), "{from} isn't a validator of the active set (the key signs from the validator's own account: pass the validator.key of one of {})", active.len());
        let status = remote.status().await?;
        let reported = status["height"].as_u64().ok_or_else(|| anyhow!("{} doesn't report its height", remote.rpc))?;
        // The height the chain can't have reached by the clock is refused before any arithmetic on it.
        remote.check_height(reported).await?;
        let height = reported.checked_add(1).ok_or_else(|| anyhow!("{} reports height {reported}, which no chain reaches. Nothing was signed", remote.rpc))?;
        let genesis = remote.genesis().await?;
        let spans = pause::Spans::of(&genesis.params());
        let needed = crate::ghost::pause_threshold(active.len());
        let live = facts.votes.iter().filter(|(v, at)| active.contains(&v.as_str()) && height < at.saturating_add(spans.vote_blocks)).count();
        let days = |blocks: u64| blocks / crate::params::blocks_per_day(genesis.block_time_ms).max(1);
        println!("A pause stops new program transactions (uploads, creates and calls, the ones that withdraw FANG too) for {} days ({} blocks) from the block after enough validators have signed: {needed} of the {} in the active set. It moves no funds, ends on its own, and has no undo. Views and plain transfers to accounts that aren't programs go on.", days(spans.pause_blocks), spans.pause_blocks, active.len());
        match section["status"].as_str() {
            Some("paused") => println!("Programs are paused now, until block {}: a signature now counts toward renewing it.", section["paused_until"]),
            Some("exit_only") => println!("Programs are exit-only (switched off by governance): a pause is still possible."),
            _ => {}
        }
        println!("Live signatures: {live} of the {needed} needed (a signature counts for {} day{}). {} {}; a fix is {}pending.", days(spans.vote_blocks), if days(spans.vote_blocks) == 1 { "" } else { "s" }, crate::chain::short(&from), facts.pause_signed.get(&from).map_or("has never started or renewed a pause".to_string(), |h| format!("started or renewed a pause at block {h}")), if facts.fix_pending { "" } else { "not " });
        if let Err(why) = may_sign_now(&facts, &spans, &from, height) {
            bail!("{} may not sign a pause now: {why}. Nothing was signed", crate::chain::short(&from));
        }
        println!("{} may sign now.", crate::chain::short(&from));
        confirm(&format!("Sign a pause as {}? One signature that doesn't reach {needed} costs its fee and changes nothing else; the others must sign within the day.", crate::chain::short(&from)), yes)?;
        // Signed from the validator's own account with its consensus key: the duty form, which has a
        // slot in the mempool's lane (§2.5).
        send(&remote, &keys, &from, Action::ProgramsPause {}, SendOpts::default()).await?;
        Ok(())
    })
}

#[cfg(test)]
mod tests;
