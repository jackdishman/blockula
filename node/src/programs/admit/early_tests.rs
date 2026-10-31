//! `submit_inner`'s early refusal (WBS S6b's acceptance, docs/PROGRAMS-V0.md §8.2) against stage T's
//! admission matrix (§10.1; `fixtures`' verdict functions):
//!
//! - `refuse_early` on every cell of the matrix (every v0 tag × the six signers × the five statuses
//!   of `the_admission_matrix`): it refuses exactly what the status refuses (`spec_refusal`, whoever
//!   signs), and so never a cell `State::admit` admits (`state_refusal`);
//! - `POST /api/tx` through the router on real chains, one for each status, signed for real: the
//!   whole of `submit_inner` refuses exactly the cells `state_refusal` refuses and admits the rest,
//!   a value-free call in exit-only and a pause while paused among them; and with a corrupted
//!   signature the status refusals still come first, as Ghost Mode's do (the others reach the
//!   signature check).

use super::*;
use crate::api::{router, ClientIpPolicy, Node};
use crate::chain::{Chain, SummonOpts};
use crate::crypto::{self, Keys};
use crate::programs::fixtures::*;
use crate::programs::probe::{calls, create_probe, keys as probe_keys, probe_interface, upload as probe_upload, Dev, ALICE as P_ALICE, FANG as P_FANG, FUEL_MARGIN, PROBE_WASM};
use crate::programs::state::Pause;
use crate::types::{KeyRole, ProgramCall};
use crate::upgrade::{ScheduledUpgrade, PROGRAMS_FEATURE, PROGRAMS_VERSION};
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode};
use parking_lot::Mutex;
use serde_json::Value;
use std::net::SocketAddr;
use std::sync::Arc;
use tower::ServiceExt as _;

/// The early refusal on every cell of §10.1's matrix, at the height `H`: it refuses the cell iff the
/// status does (`spec_refusal`), in the status's words, whoever the signer is (it decides nothing
/// that depends on the signer: what a sender's own queue may change stays with admission); and
/// whatever it refuses `State::admit` refuses too. A tx that isn't a program's is never refused.
#[test]
fn the_early_refusal_is_the_status_gate_on_every_cell_of_the_matrix() {
    let w = world();
    let actions = matrix_actions(&w);
    let mut cells = 0;
    for column in COLUMNS {
        let mut base = column_state(&w, column);
        // The next block is the one at `H`: `refuse_early` judges at `econ::now`.
        base.econ.height = H - 1;
        assert_eq!(crate::econ::now(&base), H);
        for action in &actions {
            let mut st = base.clone();
            for (signer, t) in signers(&mut st, action, H) {
                let what = format!("{column:?}, a {} by {signer:?}", describe(action));
                expect_verdict(&format!("{what}, refuse_early"), refuse_early(&st, &t).map_err(|e| e.to_string()), spec_refusal(action, column));
                if refuse_early(&st, &t).is_err() {
                    assert!(state_refusal(signer, action, column).is_some(), "{what}: early refused a cell State::admit admits");
                }
                // The status refusals are `admit`'s own, word for word.
                if let (Err(early), Err(admission)) = (refuse_early(&st, &t), admit(&st, &t, H, ApplyMode::Mempool))
                    && signer != Signer::Program
                    && !matches!((signer, action), (Signer::MultisigOwner, Action::ProgramsPause {}))
                {
                    assert_eq!(early.to_string(), admission.to_string(), "{what}");
                }
                cells += 1;
            }
        }
    }
    assert_eq!(cells, 5 * 6 * 6);
    // Not a program's: never refused, in the dormant columns least of all.
    let dormant = column_state(&w, Column::DormantAtV2);
    let transfer = tx(&dormant, ALICE, Action::Transfer { to: addr(BOB), amount: FANG }, H);
    assert!(refuse_early(&dormant, &transfer).is_ok());
}

/// It judges the status at the next block, not the executed one: a pause that starts at `H` refuses
/// an upload for the block at `H` and not for the one before; a pause running at `H` that lapses at
/// `until` admits from `until` on.
#[test]
fn the_early_refusal_judges_the_next_height() {
    let w = world();
    let mut st = column_state(&w, Column::Paused);
    let t = tx(&st, ALICE, upload(COUNTER.0, COUNTER.1, FANG), H);
    for (executed, refused) in [(H - 2, false), (H - 1, true), (H + 8, true), (H + 9, false)] {
        st.econ.height = executed;
        assert_eq!(refuse_early(&st, &t).is_err(), refused, "executed height {executed}: the next block is {}", executed + 1);
    }
}

/// A pause running in exit-only (§10.1's named cell): programs switched off by governance while a pause
/// is still running. The pause wins: a value-free call, which exit-only alone admits, is refused with a
/// pause's words, as is every other program transaction but the pause itself, which both statuses admit.
/// Once the pause lapses exit-only's own rules are what's left (the value-free call goes through again,
/// the upload doesn't).
#[test]
fn a_pause_running_in_exit_only_is_paused_first() {
    let w = world();
    let mut st = column_state(&w, Column::ExitOnly);
    st.econ.height = H - 1;
    st.programs.meta.as_mut().unwrap().pause = Some(Pause { from: H, until: H + 10, started: H, signers: vec![addr(VALIDATOR)] });
    assert_eq!(status(&st, H), Status::Paused { until: H + 10 }, "Paused is the status while a pause runs, whatever else is off");
    assert!(st.upgrade.features.get(PROGRAMS_FEATURE).is_none(), "and programs are switched off");
    let mut cells = 0;
    for action in &matrix_actions(&w) {
        for (signer, t) in signers(&mut st, action, H) {
            // The same verdict as the Paused column's, whoever signs: not the exit-only column's.
            expect_verdict(&format!("a {} by {signer:?} in exit-only under a pause, refuse_early", describe(action)), refuse_early(&st, &t).map_err(|e| e.to_string()), spec_refusal(action, Column::Paused));
            cells += 1;
        }
    }
    assert_eq!(cells, 6 * 6);
    // The value-free call, which exit-only alone admits, is the cell that tells the two apart: refused
    // here, and in the words of the pause, not of exit-only.
    let free = matrix_actions(&w).remove(2);
    assert!(spec_refusal(&free, Column::ExitOnly).is_none() && spec_refusal(&free, Column::Paused).is_some(), "the matrix's value-free call tells the statuses apart");
    let owner = tx(&st, ALICE, free, H);
    let refusal = refuse_early(&st, &owner).unwrap_err().to_string();
    assert!(refusal.contains("programs are paused until block") && !refusal.contains("exit-only"), "{refusal}");
    // Past the pause: exit-only again.
    st.econ.height = H + 9;
    assert_eq!(status(&st, H + 10), Status::ExitOnly);
    for action in &matrix_actions(&w) {
        for (signer, t) in signers(&mut st, action, H + 10) {
            expect_verdict(&format!("a {} by {signer:?} in exit-only once the pause lapsed, refuse_early", describe(action)), refuse_early(&st, &t).map_err(|e| e.to_string()), spec_refusal(action, Column::ExitOnly));
        }
    }
}

/// Signs `action` as `k` from `from`, at `nonce`, on `c`'s chain, paying the highest base fee already
/// fixed and a margin for fuel.
fn signed(c: &Chain, k: &Keys, from: &str, nonce: u64, action: Action) -> Tx {
    let mut tx = Tx { chain_id: c.genesis.chain_id.clone(), genesis_hash: c.genesis_hash.clone(), nonce, from: from.into(), fee: 0, tip: 0, valid_until: c.height() + 100, action, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
    tx.fee = crate::econ::max_fee(&c.state, tx.size()) + FUEL_MARGIN;
    tx.sign(k)
}

/// One chain for a status of §10.1's table, with the node over it, and what the cells need: the
/// probe program, a session key on Alice's account, and the keys that sign. The dormant chains hold
/// nothing (their cells are refused for the status alone, whoever signs them).
struct Net {
    n: Node,
    _home: Option<Dev>,
    homes: Vec<std::path::PathBuf>,
    program: String,
    version: String,
    alice: Keys,
    validator: Keys,
    session: Keys,
}

impl Drop for Net {
    fn drop(&mut self) {
        for h in &self.homes {
            std::fs::remove_dir_all(h).ok();
        }
    }
}

fn dormant_chain(tag: &str, born_v2: bool) -> (std::path::PathBuf, Arc<Mutex<Chain>>) {
    let home = Dev::fresh_home(tag);
    Chain::summon(&home, &SummonOpts { address_prefix: "boo".into(), ..SummonOpts::new("early-dormant", 1) }).unwrap();
    if born_v2 {
        let path = home.join("genesis.json");
        let mut g: crate::chain::Genesis = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        g.upgrades = vec![ScheduledUpgrade { version: PROGRAMS_VERSION, activation_height: 1 }];
        std::fs::write(&path, serde_json::to_string_pretty(&g).unwrap()).unwrap();
    }
    let chain = Arc::new(Mutex::new(Chain::open(&home).unwrap()));
    chain.lock().produce_block_with(vec![]).unwrap();
    (home, chain)
}

fn net(column: Column) -> Net {
    let (alice, session) = (probe_keys(P_ALICE), Keys::from_seed([77; 32]));
    let mut homes = vec![];
    let mut keep = None;
    let (chain, program, version, validator) = match column {
        Column::DormantBelowV2 | Column::DormantAtV2 => {
            let (home, chain) = dormant_chain("early-dormant", column == Column::DormantAtV2);
            let validator = Keys::from_seed(hex::decode(std::fs::read_to_string(home.join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap());
            homes.push(home);
            // Nothing exists to name: a program that isn't there, at a version nobody holds.
            (chain, probe_keys(2).address_with("boo"), "11".repeat(32), validator)
        }
        Column::On | Column::ExitOnly | Column::Paused => {
            let mut d = Dev::new("early-net");
            let program = d.deploy(1).remove(0);
            let version = d.version(&program);
            // Alice's session key, from a block.
            let c = &mut d.c;
            let add = signed(c, &alice, &alice.address_with("boo"), c.state.account(&alice.address_with("boo")).nonce, crate::types::add_key(&session, &c.genesis.chain_id, &c.genesis_hash, &alice.address_with("boo"), "session", KeyRole::Session { limit: 100 * P_FANG, period_blocks: 100_000, expires_at: 10_000_000 }));
            d.block(vec![add]);
            let validator = Keys::from_seed(hex::decode(std::fs::read_to_string(d.home.join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap());
            let mut chain = d.take();
            match column {
                Column::ExitOnly => {
                    chain.state.upgrade.features.remove(PROGRAMS_FEATURE);
                }
                Column::Paused => {
                    let from = chain.height() + 1;
                    let validator = validator.address_with("boo");
                    chain.state.programs.meta.as_mut().unwrap().pause = Some(Pause { from, until: from + 100_000, started: from, signers: vec![validator] });
                }
                _ => {}
            }
            chain.produce_block_with(vec![]).unwrap();
            let status = crate::programs::status(&chain.state, crate::econ::now(&chain.state));
            assert_eq!(std::mem::discriminant(&status), std::mem::discriminant(&column.status()), "{column:?}: {status:?}");
            keep = Some(d);
            (Arc::new(Mutex::new(chain)), program, version, validator)
        }
    };
    let n = Node::new(chain, None, vec![], ClientIpPolicy::default());
    Net { n, _home: keep, homes, program, version, alice, validator, session }
}

/// POSTs `tx` to `/api/tx` as a client, on loopback (its limits don't apply).
async fn submit(n: &Node, tx: &Tx) -> Result<String, String> {
    let app = router(n.clone()).layer(MockConnectInfo("127.0.0.1:1".parse::<SocketAddr>().unwrap()));
    let req = Request::builder().method("POST").uri("/api/tx").header("content-type", "application/json").body(Body::from(serde_json::to_vec(tx).unwrap())).unwrap();
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let j: Value = serde_json::from_slice(&axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap()).unwrap();
    match status {
        StatusCode::OK => Ok(j["id"].as_str().unwrap().to_string()),
        _ => Err(j["error"].as_str().unwrap_or("").to_string()),
    }
}

impl Net {
    /// The matrix's actions on this chain's probe program: an upload, a create, a value-free call, a
    /// call carrying FANG, a batch whose only later call carries FANG, and the pause.
    fn actions(&self) -> Vec<Action> {
        let call = |method: &str, value: u64| ProgramCall { program: self.program.clone(), version_digest: self.version.clone(), method: method.into(), input: crate::types::HexBytes(vec![]), value };
        vec![
            probe_upload(PROBE_WASM, &probe_interface()),
            create_probe(),
            calls(vec![call("ret", 0)], vec![]),
            calls(vec![call("fund", P_FANG)], vec![]),
            calls(vec![call("ret", 0), call("fund", P_FANG)], vec![]),
            Action::ProgramsPause {},
        ]
    }

    /// `action` signed as `signer`, valid on this chain's account state.
    fn tx(&self, signer: Signer, action: Action) -> Tx {
        let c = self.n.chain.lock();
        let (k, from) = match signer {
            Signer::Owner => (&self.alice, self.alice.address_with("boo")),
            Signer::Session => (&self.session, self.alice.address_with("boo")),
            Signer::ConsensusOnOwnAccount => (&self.validator, self.validator.address_with("boo")),
            Signer::Program => (&self.alice, self.program.clone()),
            other => panic!("{other:?} isn't built on a real chain (the state-level matrix runs it)"),
        };
        let nonce = c.state.account(&from).nonce;
        signed(&c, k, &from, nonce, action)
    }

    /// Submits `tx`, and takes it out of the mempool again if it was admitted, so the next cell is
    /// judged on the same queue.
    async fn try_submit(&self, tx: &Tx) -> Result<(), String> {
        let id = submit(&self.n, tx).await?;
        self.n.chain.lock().drop_pending(&id);
        Ok(())
    }
}

/// `POST /api/tx` on a real chain for each status: the owner key signs the first five actions and
/// the validator's own key (the consensus key on its own account) the pause (Alice isn't a
/// validator), and the whole of `submit_inner` refuses exactly the cells `state_refusal` says
/// (§10.1). The cells the early refusal lets by are refused later, by admission, or admitted: a
/// value-free call in exit-only, the pause while paused.
#[tokio::test]
async fn submit_refuses_exactly_the_cells_of_the_matrix_and_admits_the_rest() {
    let mut admitted = 0;
    for column in COLUMNS {
        let net = net(column);
        for action in net.actions() {
            let signer = if matches!(action, Action::ProgramsPause {}) { Signer::ConsensusOnOwnAccount } else { Signer::Owner };
            let t = net.tx(signer, action.clone());
            let got = net.try_submit(&t).await;
            let verdict = state_refusal(signer, &action, column);
            admitted += usize::from(verdict.is_none());
            expect_verdict(&format!("{column:?}, POST /api/tx of a {} by {signer:?}", describe(&action)), got, verdict);
        }
    }
    // On: all six; exit-only: the value-free call and the pause; paused: the pause.
    assert_eq!(admitted, 6 + 2 + 1);
}

/// The other signers on the chains where the status lets a cell by: a session key, a consensus key
/// on the validator's own account (an ordinary account's original key: it signs what an owner does)
/// and a program's address as `from` (a program has no keys), over HTTP, each as `state_refusal` says.
/// Where the status itself refuses, it refuses first, in its own words.
#[tokio::test]
async fn the_other_signers_are_refused_after_the_status_gate() {
    for column in [Column::On, Column::ExitOnly, Column::Paused] {
        let net = net(column);
        for action in net.actions() {
            for signer in [Signer::Session, Signer::ConsensusOnOwnAccount, Signer::Program] {
                // The validator's key doesn't own Alice's account; its own account signs the pause alone
                // and the rest from itself: still the matrix's cell.
                let t = net.tx(signer, action.clone());
                let got = net.try_submit(&t).await.map_err(|e| e.to_string());
                let what = format!("{column:?}, POST /api/tx of a {} by {signer:?}", describe(&action));
                let (state, spec) = (state_refusal(signer, &action, column), spec_refusal(&action, column));
                match (&got, state) {
                    (Ok(()), None) => {}
                    (Err(e), Some(phrase)) => assert!(e.contains(phrase) || spec.is_some_and(|s| e.contains(s)), "{what}: refused in other words than {phrase:?} (or the status's {spec:?}): {e}"),
                    (Ok(()), Some(phrase)) => panic!("{what}: admitted, but the matrix refuses it ({phrase:?})"),
                    (Err(e), None) => panic!("{what}: refused ({e}), but the matrix admits it"),
                }
            }
        }
    }
}

/// Before the signature check: with a corrupted signature a cell the status refuses is refused for
/// the status, in its words, and a cell it lets by reaches the signature check and is refused for
/// that. And before the fee: a fee too low to pay for the transaction doesn't change the answer for a
/// status-refused cell.
#[tokio::test]
async fn the_status_refuses_before_the_signature_and_the_fee_are_checked() {
    for column in COLUMNS {
        let net = net(column);
        for action in net.actions() {
            let signer = if matches!(action, Action::ProgramsPause {}) { Signer::ConsensusOnOwnAccount } else { Signer::Owner };
            let mut t = net.tx(signer, action.clone());
            let mut sig = t.signature.clone().into_bytes();
            sig[0] = if sig[0] == b'0' { b'1' } else { b'0' };
            t.signature = String::from_utf8(sig).unwrap();
            let err = net.try_submit(&t).await.expect_err("a corrupted signature is never admitted");
            let what = format!("{column:?}, a {} with a bad signature", describe(&action));
            match spec_refusal(&action, column) {
                Some(phrase) => assert!(err.contains(phrase), "{what}: the status refuses first: {err}"),
                None => assert!(err.contains("signature"), "{what}: it reaches the signature check: {err}"),
            }
            // A fee of 1 drop (below what any tx owes): the status still answers first where it refuses.
            let poor = Tx { fee: 1, ..t.clone() };
            let err = net.try_submit(&poor).await.expect_err("a fee of one drop never pays");
            match spec_refusal(&action, column) {
                Some(phrase) => assert!(err.contains(phrase), "{what}, fee 1: {err}"),
                None => assert!(err.contains("fee"), "{what}, fee 1: {err}"),
            }
        }
    }
}

/// A transfer is judged as it always was: admitted in every status, dormant included.
#[tokio::test]
async fn a_transfer_is_admitted_in_every_status() {
    for column in COLUMNS {
        let net = net(column);
        let from = net.alice.address_with("boo");
        // The dormant chains' Alice has no funds: the question is whether the status gate stops it
        // (it doesn't), so the answer is a refusal for funds there, never for programs.
        let t = {
            let c = net.n.chain.lock();
            signed(&c, &net.alice, &from, c.state.account(&from).nonce, Action::Transfer { to: probe_keys(2).address_with("boo"), amount: P_FANG })
        };
        match net.try_submit(&t).await {
            Ok(()) => {}
            Err(e) => assert!(!e.contains("programs") && e.contains("balance"), "{column:?}: {e}"),
        }
    }
}
