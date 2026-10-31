//! `POST /api/program/{address}/view` (docs/PROGRAMS-V0.md §8.2): a program's read-only method,
//! run on this node for whoever asks, against the state of the newest block it executed. The
//! request is `{ "method": "balance_of", "input": "<hex>", "bindings": [{ "program": "boo1…",
//! "version_digest": "<hex64>" }], "fuel": 1000000 }`, with `input`, `bindings` and `fuel` optional;
//! the answer is `{ "ok", "outcome", "return_data", "fuel_used", "height" }`.
//!
//! **What a view is.** A `View` method can't write, transfer, emit or call a method that isn't a
//! `View` (W1's `exec`: `ReadOnly`, `NotCallable`), so nothing it does changes anything, and this
//! route never touches the chain lock, the mempool or any state: it runs on the [`ReadView`] the
//! chain published after its last block (an immutable `Arc`, so the state it reads can't move
//! under it), at height = executed height + 1, the height admission judges a transaction submitted
//! now at (so `height_v1` answers that). The caller is 32 zero bytes, nothing is sent, no deposit
//! is bonded. It runs as a transaction does (§6.1): on `tokio::task::spawn_blocking`, and inside
//! it on the same [`PROGRAM_STACK_BYTES`](super::exec::PROGRAM_STACK_BYTES) scoped thread with a
//! fresh engine, the declared code loaded in hash order, then W1's `execute` (`exec::run_batch`).
//!
//! **What it refuses**, before running anything, and in this order:
//! - 404 while programs are dormant ("no programs exist"), and for a program that doesn't exist; 400
//!   for an address that isn't one (a program is named by its address, never a name);
//! - 403 where views are off: a validator serves them only with `--program-views`, because its HTTP
//!   is private only by convention and a view runs a VM for the caller (§8.2); full nodes always do
//!   (and `blockula dev`'s validator, which is its own RPC);
//! - 429 beyond [`VIEWS_PER_IP_PER_MINUTE`] a minute from one client IP (through `Node::limits`, as
//!   the writes' windows are: `limit_reads` covers GET and HEAD only; loopback callers have no
//!   budget; taken before the body is parsed, so a malformed one costs a hit too), and when
//!   [`MAX_CONCURRENT_VIEWS`] are running on the node already and no slot frees within
//!   [`VIEW_SLOT_WAIT`], or [`MAX_WAITING_VIEWS`] others are waiting for one already (`Retry-After:
//!   1`; the per-IP hit is given back, since the node refused for its own reasons);
//! - 422 for a body that isn't the request, 400 for one that asks for something a view can't do: a
//!   method that isn't there or isn't a `View` (a `Call` or an `Init` method is never run from
//!   here), a binding that isn't a program or names a version that isn't in force, or declared code
//!   past what one transaction may load.
//!
//! **Fuel.** `min(fuel, --view-max-fuel, program_max_tx_fuel)`: the flag is at most what a
//! transaction may declare (`MAX_TX_FUEL`) and defaults to it, and the chain's current
//! `program_max_tx_fuel` always bounds it, so no view can do more work than a transaction can pay for
//! and a governance cut of the parameter applies to views at once. A request without `fuel` gets the
//! whole cap. The load of the declared code isn't metered, as a transaction's isn't: it's bounded by
//! [`MAX_TX_CODE_BYTES`], the same bound admission holds a transaction to.
//!
//! **What it returns.** Status 200 for every run, the program's failure included: `ok: false` with
//! the receipt's own `outcome` (`{ "ok": false, "number": 1, "code": "trapped", … }`, numbers and
//! the program's sanitized abort message, never engine text) and `null` return data. Return data is
//! the program's claim (PRG 8.4). A node fault (a panic, an engine that won't build, stored code
//! that won't load, a thread the OS won't start) is a 500 and a count in
//! `blockula_program_views_total{outcome="fault"}`, nothing else: the view has no state to leave
//! faulted and no block to halt (§6.2, "`/view`").
//!
//! **Permits.** A running view holds one of the node's slots until its blocking task ends, which
//! the request timeout doesn't cut short: a client that gives up, or a request the timeout layer
//! drops, leaves the slot held until the VM returns, so at most [`MAX_CONCURRENT_VIEWS`] threads
//! ever run views, whatever the clients do. A view takes a slot only once nothing else can refuse
//! it, and a request that finds none free waits for one a moment ([`ViewGate::slot`]): a view takes
//! about a millisecond, so a client's own parallel views (a wallet reading a dozen balances) queue
//! behind each other instead of being refused, while the waiters are few and the wait short enough
//! that a flood can't pile tasks up.

use super::api::{dormant, find_program, status_now};
use super::exec::{run_batch, Failure, Faulted};
use super::params::{param, Param};
use super::receipt::ReceiptOutcome;
use super::view::ProgramView;
use super::digest;
use crate::api::Node;
use crate::chain::{short, Role, State};
use crate::crypto;
use crate::types::HexBytes;
use crate::upgrade;
use crate::view::ReadView;
use axum::extract::{ConnectInfo, Path, State as AxState};
use axum::http::{header::RETRY_AFTER, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use blockula_vm::config::{MAX_BINDINGS, MAX_INPUT, MAX_TX_CODE_BYTES};
use blockula_vm::engine::Address;
use blockula_vm::exec::{Binding, Call, TxLimits, ANY_VERSION};
use blockula_vm::host::MethodKind;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// The most views one client IP may start in [`VIEW_WINDOW`] (§8.2's per-IP budget). A wallet that
/// reads a dozen balances makes a dozen views at once; a scraper doesn't get a VM for free.
pub const VIEWS_PER_IP_PER_MINUTE: usize = 60;
/// The window of [`VIEWS_PER_IP_PER_MINUTE`].
pub const VIEW_WINDOW: Duration = Duration::from_secs(60);
/// The most views running on a node at once (§8.2): each is a 64 MiB thread and an engine, so the
/// node's cost is this many at most, whoever asks and however fast.
pub const MAX_CONCURRENT_VIEWS: usize = 2;
/// How long a view that finds every slot taken waits for one before it's refused (§8.2).
pub const VIEW_SLOT_WAIT: Duration = Duration::from_millis(250);
/// The most views waiting for a slot at once: past it a view is refused at once, so a flood can't
/// pile up tasks (each holds its parsed request) however long the running views take.
pub const MAX_WAITING_VIEWS: usize = 16;

/// Whether this node serves views, with how much fuel at most, and the slots that bound how many
/// run at once and how many wait (the module docs). One per `Node`, shared by its clones.
pub struct ViewGate {
    enabled: bool,
    max_fuel: Option<u64>,
    slots: Arc<Semaphore>,
    /// One permit per view waiting for a slot: [`MAX_WAITING_VIEWS`] of them.
    waiting: Arc<Semaphore>,
    /// How long a view waits for a slot: [`VIEW_SLOT_WAIT`] (longer in tests that need the waiters
    /// to stay put).
    slot_wait: Duration,
    /// Test builds: closed, it holds every VM inside its blocking task, slot taken, until it opens.
    #[cfg(test)]
    latch: Arc<Latch>,
}

/// A latch the tests close to hold a view's VM where it is (inside its blocking task, holding its
/// slot) for as long as they need, so that what they check about a running view doesn't depend on
/// how long the VM takes or on how busy the machine is.
#[cfg(test)]
#[derive(Default)]
pub(super) struct Latch {
    closed: std::sync::Mutex<bool>,
    opened: std::sync::Condvar,
}

#[cfg(test)]
impl Latch {
    pub(super) fn close(&self) {
        *self.closed.lock().unwrap() = true;
    }

    pub(super) fn open(&self) {
        *self.closed.lock().unwrap() = false;
        self.opened.notify_all();
    }

    fn wait(&self) {
        let mut closed = self.closed.lock().unwrap();
        while *closed {
            closed = self.opened.wait(closed).unwrap();
        }
    }
}

impl ViewGate {
    pub fn new(enabled: bool, max_fuel: Option<u64>) -> Arc<ViewGate> {
        ViewGate::with_slot_wait(enabled, max_fuel, VIEW_SLOT_WAIT)
    }

    fn with_slot_wait(enabled: bool, max_fuel: Option<u64>, slot_wait: Duration) -> Arc<ViewGate> {
        Arc::new(ViewGate { enabled, max_fuel, slots: Arc::new(Semaphore::new(MAX_CONCURRENT_VIEWS)), waiting: Arc::new(Semaphore::new(MAX_WAITING_VIEWS)), slot_wait, #[cfg(test)] latch: Arc::default() })
    }

    /// The default for a node of `role` (§8.2): on for a full node, off for a validator.
    pub fn for_role(role: Role) -> Arc<ViewGate> {
        ViewGate::new(role == Role::FullNode, None)
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The most fuel a view may use here on `state`: `--view-max-fuel` if it's lower than
    /// `program_max_tx_fuel`, which is read from `state` each time so that a cut of the parameter
    /// bounds the views of a node started with a higher flag.
    pub fn max_fuel(&self, state: &State) -> u64 {
        let chain = param(state, Param::MaxTxFuel);
        self.max_fuel.map_or(chain, |flag| flag.min(chain))
    }

    /// A slot to run a view in: one that's free now, or else one that frees within
    /// [`VIEW_SLOT_WAIT`] (`slot_wait`), if fewer than [`MAX_WAITING_VIEWS`] others are waiting
    /// already. `None` is a view to refuse (429). The slot is returned to the node when it's
    /// dropped: the caller moves it into the VM's blocking task (the module docs).
    async fn slot(&self) -> Option<OwnedSemaphorePermit> {
        if let Ok(slot) = self.slots.clone().try_acquire_owned() {
            return Some(slot);
        }
        // Waiting takes a place in the queue, given back when the wait ends, however it ends.
        let _place = self.waiting.clone().try_acquire_owned().ok()?;
        tokio::time::timeout(self.slot_wait, self.slots.clone().acquire_owned()).await.ok()?.ok()
    }
}

/// How a view request ended, for `blockula_program_views_total{outcome}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The method returned.
    Ok,
    /// The program trapped, aborted or ran out of fuel: a result, status 200.
    Failed,
    /// The request asked for something a view can't do (4xx).
    Refused,
    /// The per-IP budget or the node's running views were full (429).
    Limited,
    /// The node faulted (500).
    Fault,
}

impl Outcome {
    const ALL: [Outcome; 5] = [Outcome::Ok, Outcome::Failed, Outcome::Refused, Outcome::Limited, Outcome::Fault];

    fn label(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Failed => "failed",
            Outcome::Refused => "refused",
            Outcome::Limited => "limited",
            Outcome::Fault => "fault",
        }
    }
}

/// This process's views by outcome (a node runs one chain). Node-local, never state.
static VIEWS: [AtomicU64; Outcome::ALL.len()] = [const { AtomicU64::new(0) }; Outcome::ALL.len()];

fn counter(outcome: Outcome) -> &'static AtomicU64 {
    &VIEWS[Outcome::ALL.iter().position(|o| *o == outcome).expect("every outcome is listed")]
}

/// How many views of this process ended in `outcome`.
pub fn count(outcome: Outcome) -> u64 {
    counter(outcome).load(Relaxed)
}

/// `/metrics`' view series, `blockula_program_views_total{outcome}` (`api::metrics` prints them
/// once programs exist: empty while they're dormant).
pub fn metrics(v: &ReadView) -> String {
    use std::fmt::Write as _;
    if status_now(&v.state).is_none() {
        return String::new();
    }
    let mut out = String::from("# HELP blockula_program_views_total Program views served over HTTP, by outcome\n# TYPE blockula_program_views_total counter\n");
    for outcome in Outcome::ALL {
        let _ = writeln!(out, "blockula_program_views_total{{outcome=\"{}\"}} {}", outcome.label(), count(outcome));
    }
    out
}

/// A view request's refusal: its status, words, `Retry-After` for a 429, and how it counts.
pub struct Refusal {
    status: StatusCode,
    message: String,
    retry_after: Option<u64>,
    outcome: Outcome,
}

impl Refusal {
    fn refused(status: StatusCode, message: impl Into<String>) -> Refusal {
        Refusal { status, message: message.into(), retry_after: None, outcome: Outcome::Refused }
    }

    fn limited(message: impl Into<String>, retry_after: u64) -> Refusal {
        Refusal { status: StatusCode::TOO_MANY_REQUESTS, message: message.into(), retry_after: Some(retry_after.max(1)), outcome: Outcome::Limited }
    }

    fn fault(message: impl Into<String>) -> Refusal {
        Refusal { status: StatusCode::INTERNAL_SERVER_ERROR, message: message.into(), retry_after: None, outcome: Outcome::Fault }
    }
}

impl From<crate::api::ApiError> for Refusal {
    /// A program route's own 404 or 400 (`find_program`): the request asked for something that
    /// isn't there.
    fn from(e: crate::api::ApiError) -> Refusal {
        Refusal::refused(e.0, e.1)
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        let mut res = (self.status, Json(json!({ "error": self.message }))).into_response();
        if let Some(secs) = self.retry_after {
            res.headers_mut().insert(RETRY_AFTER, secs.into());
        }
        res
    }
}

/// The body of a view request.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewRequest {
    /// The `View` method to run: an identifier, the program's interface names it.
    method: String,
    /// What the method reads as its input, at most [`MAX_INPUT`] bytes.
    #[serde(default)]
    input: HexBytes,
    /// Programs the method may reach beyond the one called, as a transaction's `bindings`.
    #[serde(default)]
    bindings: Vec<BindingRequest>,
    /// The most fuel to run with (capped: the module docs). Absent: the cap.
    fuel: Option<u64>,
}

/// A binding as a transaction's JSON has it, except that the version may be left out.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingRequest {
    program: String,
    /// The version digest (hex64) the program must be at: absent, or 64 zeros, for any version.
    version_digest: Option<String>,
}

/// A request that passed every check that needs no VM: everything `execute` is given.
struct Checked {
    /// The program's address as the chain renders it.
    address: String,
    method: String,
    call: Call,
    bindings: Vec<Binding>,
    /// The program and its bindings, each once.
    declared: Vec<Address>,
    fuel: u64,
}

/// What the tests arm a fault for (`view::inject`): the run's name.
pub(super) fn label(address: &str, method: &str) -> String {
    format!("view {address} {method}")
}

/// A hex64 digest as its 32 bytes.
fn parse_digest(hex64: &str) -> Option<[u8; 32]> {
    (hex64.len() == 64 && crypto::is_lower_hex(hex64)).then(|| hex::decode(hex64).ok()).flatten()?.try_into().ok()
}

/// Checks `req` against `st` for the program at `address` (the module docs' 404s and 400s), giving
/// what to run. `max_fuel` is this node's cap.
fn check(st: &State, address: &str, req: ViewRequest, max_fuel: u64) -> Result<Checked, Refusal> {
    let bad = |what: String| Refusal::refused(StatusCode::BAD_REQUEST, what);
    let record = find_program(st, address)?;
    let program = crypto::address_bytes(address).ok_or_else(|| bad("a program is named by its address".into()))?;
    if !blockula_vm::abi::is_ident(&req.method) {
        return Err(bad("`method` is a method's name: a lowercase letter, then lowercase letters, digits and underscores, at most 32".into()));
    }
    if req.input.0.len() > MAX_INPUT {
        return Err(bad(format!("`input` is {} bytes, past the {MAX_INPUT} a call may carry", req.input.0.len())));
    }
    let interface = st.programs.interfaces.get(&record.interface_hash).and_then(|i| i.interface());
    let Some(interface) = interface else {
        // A program whose interface isn't stored, or doesn't decode, is a broken state, never a request's fault.
        tracing::error!(program = %address, "a program's stored interface is missing or doesn't decode");
        return Err(Refusal::fault("this node couldn't read the program's interface (a node fault, logged)"));
    };
    let Some(method) = interface.method(&req.method) else { return Err(bad("the program's interface has no method by that name".into())) };
    match method.kind {
        MethodKind::View => {}
        MethodKind::Call => return Err(bad("that method is a call, which only a transaction runs: a view runs the program's `view` methods".into())),
        MethodKind::Init => return Err(bad("that method is the program's initializer, which runs only when it's created".into())),
    }
    if req.bindings.len() > MAX_BINDINGS {
        return Err(bad(format!("{} bindings, past the {MAX_BINDINGS} a run may declare", req.bindings.len())));
    }
    let mut declared = vec![program];
    let mut codes: BTreeSet<[u8; 32]> = BTreeSet::from([record.wasm_hash]);
    let mut bindings = vec![];
    for b in &req.bindings {
        // A client's string: an address before anything echoes it (`short`) or looks it up.
        if !crypto::is_address(&b.program) {
            return Err(bad("a binding is a program's address".into()));
        }
        let Some(bound) = st.programs.records.get(&b.program) else { return Err(bad(format!("a binding names {}, which isn't a program here", short(&b.program)))) };
        let bytes = crypto::address_bytes(&b.program).ok_or_else(|| bad("a binding is a program's address".into()))?;
        if declared.contains(&bytes) {
            return Err(bad(format!("{} is bound twice, or is the program being called", short(&b.program))));
        }
        let in_force = digest::version_digest(&b.program, &bound.wasm_hash, &bound.interface_hash, bound.version);
        let version = match b.version_digest.as_deref() {
            None => ANY_VERSION,
            Some(d) => parse_digest(d).ok_or_else(|| bad("a binding's `version_digest` is 64 lowercase hex characters".into()))?,
        };
        if version != ANY_VERSION && version != in_force {
            return Err(bad(format!("a binding names version {}… of {}, whose version in force is {}…", &hex::encode(version)[..10], short(&b.program), &hex::encode(in_force)[..10])));
        }
        declared.push(bytes);
        codes.insert(bound.wasm_hash);
        bindings.push(Binding { program: bytes, version_digest: version });
    }
    // What the run loads, as a transaction's declared code is bounded (§5.1): the work of loading
    // isn't metered, so the bound is what keeps it the work a transaction can pay for.
    let code: usize = codes.iter().filter_map(|h| st.programs.code.get(h)).map(|c| c.wasm.len()).sum();
    if code > MAX_TX_CODE_BYTES {
        return Err(bad(format!("the code this run loads is {code} bytes, past the {MAX_TX_CODE_BYTES} one run may load")));
    }
    let fuel = match req.fuel {
        Some(0) => return Err(bad("`fuel` is at least 1".into())),
        Some(f) => f.min(max_fuel),
        None => max_fuel,
    };
    let in_force = digest::version_digest(address, &record.wasm_hash, &record.interface_hash, record.version);
    let call = Call { program, version_digest: in_force, method: req.method.clone(), input: req.input.0, value: 0, kind: MethodKind::View };
    Ok(Checked { address: address.to_string(), method: req.method, call, bindings, declared, fuel })
}

/// Runs the checked view on `state` at `height`: W1's `execute` for the one `View` call, on a
/// fresh engine on its own large-stack thread (`exec::run_batch`), as a transaction runs.
/// Nothing is committed: `state` is read-only and the receipt's diff, which a view can't have
/// anything in, is dropped.
fn run(state: &State, height: u64, c: &Checked) -> Result<blockula_vm::exec::Receipt, Faulted> {
    let label = label(&c.address, &c.method);
    let schedule = blockula_vm::meter::schedule(upgrade::version_at(state, height)).ok_or_else(|| Faulted::tx(format!("no fuel schedule for protocol v{}", upgrade::version_at(state, height))))?;
    let view = ProgramView::at(state, height, &label, c.declared.iter().copied(), None)?;
    let zero: Address = [0; 32];
    run_batch(&label, schedule, &view, zero, std::slice::from_ref(&c.call), &c.bindings, TxLimits { fuel_limit: c.fuel, storage_limit: 0 })
}

/// `POST /api/program/{address}/view`: the module docs.
pub(crate) async fn view(AxState(n): AxState<Node>, Path(address): Path<String>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, body: axum::body::Bytes) -> Result<Json<Value>, Refusal> {
    let v = n.live.view();
    // While programs are dormant nothing exists to run, and nothing else is answered first. Neither
    // that nor a node with views off is a view that was asked for: neither is counted.
    status_now(&v.state).ok_or_else(dormant)?;
    if !n.views.enabled() {
        return Err(Refusal::refused(StatusCode::FORBIDDEN, "this node doesn't serve program views: a validator does only when started with --program-views. Ask a full node (or the validator's operator)"));
    }
    match serve(&n, v, &address, n.client_ip(peer, &headers), &body).await {
        Ok((outcome, answer)) => {
            counter(outcome).fetch_add(1, Relaxed);
            Ok(Json(answer))
        }
        Err(refusal) => {
            counter(refusal.outcome).fetch_add(1, Relaxed);
            Err(refusal)
        }
    }
}

/// The view's request, from the budget to the answer.
async fn serve(n: &Node, v: Arc<ReadView>, address: &str, client: Option<std::net::IpAddr>, body: &[u8]) -> Result<(Outcome, Value), Refusal> {
    let hit = n.allow_per_ip("view", client, VIEWS_PER_IP_PER_MINUTE, VIEW_WINDOW).map_err(|wait| Refusal::limited(format!("too many program views from your IP: at most {VIEWS_PER_IP_PER_MINUTE} a minute; try again in {} seconds", wait.as_secs().max(1)), wait.as_secs()))?;
    let req: ViewRequest = serde_json::from_slice(body).map_err(|e| Refusal::refused(StatusCode::UNPROCESSABLE_ENTITY, format!("bad view request JSON: {e}")))?;
    let checked = check(&v.state, address, req, n.views.max_fuel(&v.state))?;
    // A slot for the run, taken after every refusal that needs no VM, so a request that was never
    // going to run doesn't hold one. It moves into the blocking task: only the end of the VM's work
    // gives it back (the module docs). Without one the request is refused for the node's own
    // reasons, not the client's, so its hit on the per-IP budget is given back.
    let Some(permit) = n.views.slot().await else {
        n.refund_per_ip(hit);
        return Err(Refusal::limited(format!("this node is running {MAX_CONCURRENT_VIEWS} program views already: try again in a moment"), 1));
    };
    let height = v.height.saturating_add(1);
    #[cfg(test)]
    let latch = n.views.latch.clone();
    let ran = {
        let v = v.clone();
        tokio::task::spawn_blocking(move || {
            let _slot = permit;
            #[cfg(test)]
            latch.wait();
            run(&v.state, height, &checked)
        })
        .await
    };
    let receipt = match ran {
        Ok(Ok(receipt)) => receipt,
        Ok(Err(fault)) => {
            tracing::error!(address, class = ?fault.class, fault = %fault.what, "a program view faulted on this node: answering 500, nothing else");
            return Err(Refusal::fault("this node couldn't run the view (a node fault, logged): try another node"));
        }
        Err(e) => {
            tracing::error!(address, error = %e, "a program view's thread ended abnormally: answering 500, nothing else");
            return Err(Refusal::fault("this node couldn't run the view (a node fault, logged): try another node"));
        }
    };
    Ok(answer(&v, &receipt))
}

/// The answer for a run's receipt: its outcome, the method's return data (the program's claim; `null`
/// unless it returned), the fuel it used and the height of the state it read.
fn answer(v: &ReadView, receipt: &blockula_vm::exec::Receipt) -> (Outcome, Value) {
    match &receipt.fail {
        None => {
            let data = receipt.returns.first().map(hex::encode).unwrap_or_default();
            (Outcome::Ok, json!({ "ok": true, "outcome": { "ok": true }, "return_data": data, "fuel_used": receipt.fuel_used, "height": v.height }))
        }
        Some(fail) => {
            let outcome = ReceiptOutcome::failed(&Failure::Vm(fail.clone()), v.state.params.prefix());
            (Outcome::Failed, json!({ "ok": false, "outcome": outcome, "return_data": null, "fuel_used": receipt.fuel_used, "height": v.height }))
        }
    }
}

#[cfg(test)]
mod tests;
