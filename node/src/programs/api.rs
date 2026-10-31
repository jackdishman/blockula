//! Programs' read API (docs/PROGRAMS-V0.md §8.2): the routes that serve program state, and the
//! sections they add to the node's existing endpoints. Every handler reads the view the chain
//! published after its last block (`n.live.view()`, a `ReadView`), never the chain lock, like the
//! rest of `api.rs`, and sits behind its per-client read limit and timeout.
//!
//! | Route | Returns |
//! |---|---|
//! | `GET /api/program/{address}` | the record, its code and version digest, who may upgrade it once upgrades exist, its balance and storage totals, its methods, the proof path of its record |
//! | `GET /api/program/{address}/storage?after=&limit=` | its storage entries in key order, a bounded page at a time |
//! | `GET /api/code/{hash}` | a stored code's size, method names, uploader, deposit and references; `?wasm=1` adds the code |
//! | `GET /api/interface/{hash}` | a stored interface's bytes and its JSON form (§3.4) |
//!
//! and, in existing endpoints: `/api/fees`' `fuel`, `/api/params`' `programs`, `/api/account`'s
//! `program` (for a program's account), `/api/search`'s `program` and `code` hits, and the
//! program lines of `/metrics`.
//!
//! **While programs are dormant nothing here exists.** Below v2, or before programs first switch
//! on (no META `programs`, so no program record either), every program route answers 404 and none
//! of the sections is added: each existing endpoint answers byte for byte as it did before
//! programs were built, on a v1 chain (testnet-2) and on a chain born at v2 with programs off
//! (`blockula-1` at launch), so v1 clients and the explorer's recorded vectors see no change. The
//! gate is [`status_now`], the status admission would judge the next block's transactions under.
//!
//! **Chain text.** Everything a deployer wrote is a claim (PRG 8.4, 9.7): method, argument and
//! export names come out as `untrusted_*` (an interface's whole JSON form as
//! `untrusted_interface`), and storage keys and values, code and interface bytes as hex. Accounts
//! are labelled as everywhere in the API (`api::label`).

use super::interface_json::{InterfaceJson, KindJson, TypeJson};
use super::params::{self, Param};
use super::state::{blob_key, storage_key, BlobRef, ProgramRecord, BLOB_CODE, BLOB_INTERFACE};
use super::{digest, fuel, status, Status};
use crate::api::{label, ApiError, ApiResult, Node};
use crate::chain::State as ChainState;
use crate::crypto;
use crate::state::ns;
use crate::types::fmt_fang;
use crate::view::ReadView;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use blockula_vm::interface;
use serde::Deserialize;
use serde_json::{json, Value};
use std::ops::Bound;

/// Programs' status for the next block (`econ::now`: the height admission judges a transaction
/// submitted now at), or `None` while they're `Dormant`. Every route and section below asks it
/// first: `None` means no program exists, so routes answer 404 and sections are left out.
pub fn status_now(st: &ChainState) -> Option<Status> {
    match status(st, crate::econ::now(st)) {
        Status::Dormant => None,
        s => Some(s),
    }
}

/// A status's name in JSON.
fn status_name(s: Status) -> &'static str {
    match s {
        Status::Dormant => "dormant",
        Status::On => "on",
        Status::ExitOnly => "exit_only",
        Status::Paused { .. } => "paused",
    }
}

/// The 404 of every program route while programs are dormant.
pub(super) fn dormant() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "no programs exist on this chain: programs have never been switched on".into())
}

/// What a program route names: a program by its address (never a name: PRG 8.1), with its record.
pub(super) fn find_program<'a>(st: &'a ChainState, address: &str) -> Result<&'a ProgramRecord, ApiError> {
    status_now(st).ok_or_else(dormant)?;
    if !crypto::is_address(address) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "a program is named by its address, never by a name".into()));
    }
    st.programs.records.get(address).map(|r| &r.0).ok_or_else(|| crate::api::not_found("program"))
}

/// A code or interface hash from a route: 64 lowercase hex characters.
fn parse_hash(hash: &str) -> Result<[u8; 32], ApiError> {
    let bytes = (hash.len() == 64 && crypto::is_lower_hex(hash)).then(|| hex::decode(hash).ok()).flatten();
    bytes.and_then(|b| b.try_into().ok()).ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "a hash is 64 lowercase hex characters".into()))
}

/// A record's `policy` byte as `CreateProgram`'s JSON names it (`types::ProgramPolicy`).
fn policy_name(policy: u8) -> &'static str {
    match policy {
        0 => "open",
        1 => "additive",
        _ => "unknown",
    }
}

/// Who could change the program's code, in a sentence (§8.2). Nothing can in v0: upgrades are a
/// later protocol version's (tag 65), and an authority is stored with no power until then (A19).
fn upgradability(r: &ProgramRecord) -> String {
    match &r.authority {
        None => "immutable".into(),
        Some(a) => format!("no upgrades exist yet; once a protocol version adds them, {a} may upgrade it with {} blocks' notice", r.upgrade_delay_blocks),
    }
}

/// A stored blob's [`BlobRef`] (namespace 72): who uploaded it, its deposit, its references and
/// heights. Absent only if the state breaks its invariants (`state::check_invariants`).
fn blob_json(v: &ReadView, b: Option<&BlobRef>) -> Value {
    match b {
        Some(b) => json!({ "uploader": label(v, &b.uploader), "deposit": b.deposit.to_string(), "refs": b.refs, "uploaded_at": b.uploaded_at, "last_used_at": b.last_used_at }),
        None => json!({ "uploader": null, "deposit": null, "refs": null, "uploaded_at": null, "last_used_at": null }),
    }
}

/// Copies `from`'s keys into `into` (both objects).
fn merge(into: &mut Value, from: Value) {
    if let (Some(into), Value::Object(from)) = (into.as_object_mut(), from) {
        into.extend(from);
    }
}

/// An interface's methods as `/api/program` lists them: what execution enforces (kind, payable)
/// and each method's selector, beside the deployer's names (`untrusted_name`) and types.
fn methods_json(i: &interface::Interface) -> Vec<Value> {
    i.methods
        .iter()
        .map(|m| {
            let args: Vec<Value> = m.args.iter().map(|a| json!({ "untrusted_name": a.name, "type": TypeJson::from(&a.ty) })).collect();
            json!({
                "untrusted_name": m.name,
                "selector": hex::encode(digest::selector(&m.name)),
                "kind": KindJson::from(m.kind),
                "payable": m.payable,
                "args": args,
                "returns": m.returns.as_ref().map(TypeJson::from),
            })
        })
        .collect()
}

/// `GET /api/program/{address}`: the program's record (§4.2) and what follows from it. `methods`
/// is its interface, decoded (`null` only if the state breaks its invariants); `proof_path` is
/// where `/api/proof` serves the record with its Merkle proof (namespace 66, keyed by the address
/// as accounts are), for a client that doesn't take this node's word for it.
pub(crate) async fn program(State(n): State<Node>, Path(address): Path<String>) -> ApiResult {
    let v = n.live.view();
    let st = &v.state;
    let r = find_program(st, &address)?;
    let code = st.programs.code.get(&r.wasm_hash);
    let methods = st.programs.interfaces.get(&r.interface_hash).and_then(|i| i.interface()).map(methods_json);
    let balance = st.account(&address).balance;
    Ok(Json(json!({
        "address": address,
        "creator": label(&v, &r.creator),
        "created_at": r.created_at,
        "code": { "wasm_hash": hex::encode(r.wasm_hash), "size": code.map(|c| c.wasm.len()), "interface_hash": hex::encode(r.interface_hash) },
        "version": r.version,
        // What a call to it binds (§2.1): a call runs only while this is the digest in force.
        "version_digest": hex::encode(digest::version_digest(&address, &r.wasm_hash, &r.interface_hash, r.version)),
        "authority": r.authority.as_deref().map(|a| label(&v, a)),
        "upgrade_delay_blocks": r.upgrade_delay_blocks,
        "policy": policy_name(r.policy),
        "upgradability": upgradability(r),
        "accepts_transfers": r.accepts_transfers,
        "balance": balance.to_string(),
        "balance_display": fmt_fang(balance),
        "storage": { "items": r.items, "bytes": r.bytes, "deposit": r.deposit.to_string(), "sponsored": r.sponsored.to_string() },
        "record_deposit": r.record_deposit.to_string(),
        "record_payer": label(&v, &r.record_payer),
        "methods": methods,
        "proof_path": format!("/api/proof/{}/{}", ns::PROGRAMS, hex::encode(address.as_bytes())),
        "height": v.height,
    })))
}

/// The most entries a storage page holds (`api::ACCOUNT_LIST_LIMIT`, every list's page size).
pub const STORAGE_PAGE_ENTRIES: usize = crate::api::ACCOUNT_LIST_LIMIT;
/// The most key and value bytes a storage page holds: a page ends at the entry that reaches it
/// (and always holds one), so a response stays under about twice this in hex whatever the values
/// are (each is at most 16 KiB).
pub const STORAGE_PAGE_BYTES: usize = 256 * 1024;

#[derive(Deserialize)]
pub struct StorageQuery {
    /// The previous page's `next`: the key (hex) its last entry had.
    after: Option<String>,
    limit: Option<usize>,
}

/// `GET /api/program/{address}/storage?after=&limit=`: the program's entries (namespace 69, keyed
/// by its 32 address bytes ‖ the key, so they're contiguous) in key order from just past `after`,
/// at most `limit` (1 to [`STORAGE_PAGE_ENTRIES`]) and [`STORAGE_PAGE_BYTES`] at a time. Keys and
/// values are the program's bytes, as hex; `payer` is whose deposit it is (refunded to exactly
/// that account). `next` is the next page's `after`, `null` at the end. A page reads only the
/// entries it returns, and one more.
pub(crate) async fn storage(State(n): State<Node>, Path(address): Path<String>, Query(q): Query<StorageQuery>) -> ApiResult {
    let v = n.live.view();
    let st = &v.state;
    find_program(st, &address)?;
    let program = crypto::address_bytes(&address).expect("a program's address is an address");
    let after = match q.after.as_deref().filter(|a| !a.is_empty()) {
        Some(a) if crypto::is_lower_hex(a) && a.len() <= 2 * blockula_vm::config::MAX_KEY_BYTES => Some(hex::decode(a).expect("lowercase hex")),
        Some(_) => return Err(ApiError(StatusCode::BAD_REQUEST, format!("`after` is a storage key: lowercase hex, at most {} bytes", blockula_vm::config::MAX_KEY_BYTES))),
        None => None,
    };
    let limit = q.limit.unwrap_or(STORAGE_PAGE_ENTRIES).clamp(1, STORAGE_PAGE_ENTRIES);
    let start = match after {
        Some(key) => Bound::Excluded(storage_key(&program, &key)),
        None => Bound::Included(storage_key(&program, &[])),
    };
    let prefix = st.params.prefix();
    let (mut entries, mut bytes, mut next) = (vec![], 0usize, None);
    let mut range = st.programs.storage.range((start, Bound::Unbounded)).take_while(|(k, _)| k.0.starts_with(&program));
    for (k, e) in range.by_ref() {
        let key = &k.0[32..];
        entries.push(json!({ "key": hex::encode(key), "value": hex::encode(&e.value), "payer": label(&v, &crypto::render_address(prefix, &e.payer)), "deposit": e.deposit.to_string(), "capacity": e.capacity }));
        bytes += key.len() + e.value.len();
        if entries.len() == limit || bytes >= STORAGE_PAGE_BYTES {
            next = Some(hex::encode(key));
            break;
        }
    }
    // A full page says where the next one starts only if there's one.
    if next.is_some() && range.next().is_none() {
        next = None;
    }
    Ok(Json(json!({ "program": address, "entries": entries, "next": next, "height": v.height })))
}

#[derive(Deserialize)]
pub struct CodeQuery {
    /// `1`: include the code itself (at most 256 KiB, as hex).
    wasm: Option<u8>,
}

/// `GET /api/code/{hash}`: stored code (namespace 67) by its `wasm_hash`: its size, its method
/// names (its `bk_` exports without the prefix, as upload validation found them), and who
/// uploaded it, for what deposit, how many programs cite it, and when it was stored and last
/// re-uploaded (its `BlobRef`). `?wasm=1` adds the code as hex.
pub(crate) async fn code(State(n): State<Node>, Path(hash): Path<String>, Query(q): Query<CodeQuery>) -> ApiResult {
    let v = n.live.view();
    let st = &v.state;
    status_now(st).ok_or_else(dormant)?;
    let h = parse_hash(&hash)?;
    let c = st.programs.code.get(&h).ok_or_else(|| crate::api::not_found("code"))?;
    let mut out = json!({ "wasm_hash": hash, "size": c.wasm.len(), "untrusted_exports": c.exports, "height": v.height });
    merge(&mut out, blob_json(&v, st.programs.blob_refs.get(&blob_key(BLOB_CODE, &h)).map(|b| &b.0)));
    if q.wasm == Some(1) {
        out["wasm"] = hex::encode(&c.wasm).into();
    }
    Ok(Json(out))
}

/// `GET /api/interface/{hash}`: a stored interface (namespace 68) by its `interface_hash`: its
/// format-1 bytes as hex, the same interface in §3.4's JSON form (`untrusted_interface`: every
/// name in it is the deployer's), and its `BlobRef` as for code.
pub(crate) async fn interface(State(n): State<Node>, Path(hash): Path<String>) -> ApiResult {
    let v = n.live.view();
    let st = &v.state;
    status_now(st).ok_or_else(dormant)?;
    let h = parse_hash(&hash)?;
    let i = st.programs.interfaces.get(&h).ok_or_else(|| crate::api::not_found("interface"))?;
    // A stored interface decoded at upload (`check_format`) and when its record was made or loaded
    // (`InterfaceRecord::new`); `null` only if the state is broken.
    let decoded = i.interface().map(InterfaceJson::from);
    let mut out = json!({ "interface_hash": hash, "size": i.bytes.len(), "hex": hex::encode(&i.bytes), "untrusted_interface": decoded, "height": v.height });
    merge(&mut out, blob_json(&v, st.programs.blob_refs.get(&blob_key(BLOB_INTERFACE, &h)).map(|b| &b.0)));
    Ok(Json(out))
}

/// `/api/fees`' `fuel` section (§5.5): what a program transaction's fuel costs and what bounds it,
/// or `None` while programs are dormant (`/api/fees` then has no `fuel` key at all, which a client
/// reads as `null`). Rates are in milli-drops per fuel; `rate` is what the next block charges (the
/// stored rate, never below the floor parameter, §5.6: [`fuel::rate`]), `suggested_rate` the most
/// it can reach in `econ::FEE_LAG` blocks of full fuel ([`fuel::suggested_rate`], as `suggested`
/// is for bytes), so a fee priced with it covers any of them; `block_fuel` is the fuel the last
/// executed block counted (§5.4). `storage_price` is the price of a byte at today's program state
/// (`null` past its ceiling, §4.6); `value_headroom` is what the value cap still allows programs
/// to take in (§7.1). The schedule's numbers are the ones the next block runs (W1's `meter`).
pub fn fees_fuel(st: &ChainState) -> Option<Value> {
    let s = status_now(st)?;
    let m = st.programs.meta.as_ref()?;
    let p = |k: Param| params::param(st, k);
    let now = crate::econ::now(st);
    let schedule = blockula_vm::meter::schedule(crate::upgrade::version_at(st, now));
    let control = u128::from(m.value_held) + u128::from(m.sponsored);
    let headroom = u128::from(p(Param::ValueCap)).saturating_sub(control);
    Some(json!({
        "status": status_name(s),
        "rate": fuel::rate(st),
        "floor_rate": p(Param::FuelFloorRate),
        "suggested_rate": fuel::suggested_rate(st),
        "rate_unit": "milli-drops per fuel",
        "block_fuel": st.programs.block.fuel_at(st.econ.height),
        "max_tx_fuel": p(Param::MaxTxFuel),
        "max_block_fuel": p(Param::MaxBlockFuel),
        "schedule": schedule.map(|s| s.version),
        "load_base": schedule.map(|s| s.load.load_base),
        "load_per_byte": schedule.map(|s| s.load.load_per_byte),
        "upload_base": schedule.map(|s| s.load.upload_base),
        "upload_per_byte": schedule.map(|s| s.load.upload_per_byte),
        "max_tx_code_bytes": blockula_vm::config::MAX_TX_CODE_BYTES,
        "storage_price": blockula_vm::storage::price(p(Param::StoragePrice), p(Param::StateSoftBytes), m.state_bytes).map(|d| d.to_string()),
        "state_bytes": m.state_bytes,
        "state_soft_bytes": p(Param::StateSoftBytes),
        "value_cap": p(Param::ValueCap).to_string(),
        "value_held": m.value_held.to_string(),
        "sponsored": m.sponsored.to_string(),
        "value_headroom": headroom.to_string(),
        "paused_until": match s { Status::Paused { until } => Some(until), _ => None },
    }))
}

/// `/api/params`' `programs` section (§8.2, §4.5), or `None` while programs are dormant: the
/// status, each of programs' parameters with its value, default, bounds (inclusive) and class
/// (`council`: the bootstrap council may pass it too; `tokens_only`: only the token
/// supermajority, as `params::tokens_only` says, §4.5), META's totals and fuel rate (as stored:
/// readers take it at least at the floor), and the pause: the last run (kept after it lapses,
/// §7.2), its signers, the pending votes and each validator's last pause.
pub fn params_section(v: &ReadView) -> Option<Value> {
    let st = &v.state;
    let s = status_now(st)?;
    let m = st.programs.meta.as_ref()?;
    let entries: serde_json::Map<String, Value> = Param::ALL
        .into_iter()
        .map(|k| {
            let (min, max) = k.bounds(&st.params);
            let class = if crate::params::tokens_only(k.key()) { "tokens_only" } else { "council" };
            let p = json!({ "value": params::param(st, k).to_string(), "default": k.default(&st.params).to_string(), "min": min.to_string(), "max": max.to_string(), "class": class });
            (k.key().to_string(), p)
        })
        .collect();
    let at = |map: &std::collections::BTreeMap<String, u64>| map.iter().map(|(validator, h)| json!({ "validator": label(v, validator), "at": h })).collect::<Vec<_>>();
    Some(json!({
        "status": status_name(s),
        "paused_until": match s { Status::Paused { until } => Some(until), _ => None },
        "params": entries,
        "fuel_rate": m.fuel_rate,
        "value_held": m.value_held.to_string(),
        "sponsored": m.sponsored.to_string(),
        "escrow": m.escrow.to_string(),
        "state_bytes": m.state_bytes,
        "programs": m.programs,
        "pause": m.pause.as_ref().map(|p| json!({ "from": p.from, "until": p.until, "started": p.started, "signers": p.signers.iter().map(|a| label(v, a)).collect::<Vec<_>>() })),
        "votes": at(&m.votes),
        "pause_signed": at(&m.pause_signed),
        // Whether a fix is pending (§7.2), which waives a validator's window between pauses.
        "fix_pending": super::pause::fix_pending(st, crate::econ::now(st)),
    }))
}

/// `/api/account`'s `program` section, for a program's account only (`None` for every other
/// account, and while programs are dormant): what it is, and where its record is served.
pub fn account_section(v: &ReadView, address: &str) -> Option<Value> {
    let st = &v.state;
    status_now(st)?;
    let r = st.programs.records.get(address)?;
    Some(json!({
        "creator": label(v, &r.creator),
        "created_at": r.created_at,
        "version_digest": hex::encode(digest::version_digest(address, &r.wasm_hash, &r.interface_hash, r.version)),
        "upgradability": upgradability(r),
        "accepts_transfers": r.accepts_transfers,
        "link": format!("/api/program/{address}"),
    }))
}

/// `/api/search`'s program hits (§8.2), asked after blocks and transactions and before accounts:
/// a program's address is a `program`, a stored code's hash a `code`. `None` for anything else,
/// and while programs are dormant.
pub fn search(st: &ChainState, q: &str) -> Option<Value> {
    status_now(st)?;
    if st.programs.records.contains_key(q) {
        return Some(json!({ "type": "program", "id": q }));
    }
    let code = parse_hash(q).ok().filter(|h| st.programs.code.contains_key(h));
    code.map(|_| json!({ "type": "code", "id": q }))
}

/// `/metrics`' program lines (§8.2), from the published state, and this process's safeguards
/// (`fault::metrics`: node faults by path and class, the quarantine, strikes, the totals check and
/// the replay cache); empty while programs are dormant. The views' series come from
/// `view_api::metrics`. The per-outcome transaction counter, the reserved-fuel counter and the
/// VM-commit-fault counter that an earlier draft of §8.2 listed are not emitted (§8.2 says so).
pub fn metrics(v: &ReadView) -> String {
    use std::fmt::Write as _;
    let st = &v.state;
    let (Some(s), Some(m)) = (status_now(st), st.programs.meta.as_ref()) else { return String::new() };
    let code = match s {
        Status::Dormant => 0,
        Status::On => 1,
        Status::ExitOnly => 2,
        Status::Paused { .. } => 3,
    };
    let mut out = String::new();
    for (name, help, val) in [
        ("blockula_programs_status", "Programs at the next height: 1 on, 2 exit-only, 3 paused (absent before programs first switch on)", code as f64),
        ("blockula_program_block_fuel", "Program fuel the last executed block counted", st.programs.block.fuel_at(st.econ.height) as f64),
        ("blockula_program_fuel_rate", "Fuel rate the next block charges, milli-drops per fuel", fuel::rate(st) as f64),
        ("blockula_program_value_held_drops", "FANG program accounts hold, in drops", m.value_held as f64),
        ("blockula_program_sponsored_drops", "Storage deposits programs paid themselves, in drops", m.sponsored as f64),
        ("blockula_program_state_bytes", "Program state's accounted bytes (what the storage price curve reads)", m.state_bytes as f64),
    ] {
        let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {val}");
    }
    out.push_str(&super::fault::metrics());
    out
}

#[cfg(test)]
mod tests;
