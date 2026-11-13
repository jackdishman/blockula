//! The `blackjack` guest (docs/BLACKJACK.md, `guests/blackjack`) on `MemHost`, under its own
//! interface's kinds and payability, on every engine (`EngineKind::ALL`), each call run twice:
//!
//! * **Protocol v2.** The module uploads under v2 and imports none of v3's host functions.
//! * **Every transition** of a hand (bet, deal, hit, stand, the dealer's play, each outcome) with
//!   the cards recomputed here from the chain with the `blake3` crate.
//! * **Every abort**, cap and limit; the pause, which stops new bets and nothing else.
//! * **Timeouts**: a dealer that misses a reveal pays the player; an idle player stands.
//! * **Table balances and relayers**: a relayer plays a player's balance within the grant and can
//!   send it nowhere but back to the player.
//! * **The invariant**, after every successful call: the program's balance is exactly
//!   `bankroll + reserved + stakes + table`, and each counter is the sum of the records it counts
//!   (the open hands' exposure and stakes, the players' table balances).
//! * **Fuel**: the most each method used, printed by the soak (`-- --nocapture`), far under
//!   `program_max_tx_fuel`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use bj_core::{CHAIN_LEN, Hand, Outcome, Phase, Step, exposure, payout};
use blockula_vm::abi::FIRST_VERSION;
use blockula_vm::engine::{Address, EngineKind, TrapKind};
use blockula_vm::exec::{Call, Event, FailCode, Receipt, TxLimits, execute};
use blockula_vm::host::{MethodKind, View};
use blockula_vm::interface::{Interface, decode};
use blockula_vm::meter::SCHEDULE_V0;
use blockula_vm::testing::{MemHost, addr};

const FANG: u64 = 1_000_000_000;
const PROGRAM: &str = "blackjack";
/// `program_max_tx_fuel`'s default: every call runs under it.
const MAX_TX_FUEL: u64 = 10_000_000;

// Record layouts, as the guest's doc gives them.
const HOUSE: usize = 114;
const GAME: usize = 157;
const PLAYER: usize = 195;

fn guest() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/programs/blackjack.wasm");
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e} (run guests/build.sh)", path.display()))
}

fn interface() -> Interface {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/programs/blackjack.iface");
    decode(&std::fs::read(path).unwrap()).unwrap()
}

fn on_program_thread<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    std::thread::scope(|s| std::thread::Builder::new().stack_size(64 << 20).spawn_scoped(s, f).unwrap().join().unwrap())
}

/// The most fuel each method used in any test of this file.
static FUEL: Mutex<BTreeMap<String, u64>> = Mutex::new(BTreeMap::new());

fn le(n: u64) -> Vec<u8> {
    n.to_le_bytes().to_vec()
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

fn abort_code(r: &Receipt) -> Option<u32> {
    match &r.fail {
        Some(FailCode::Aborted { code, .. }) => Some(*code),
        _ => None,
    }
}

fn trap(r: &Receipt) -> Option<TrapKind> {
    match &r.fail {
        Some(FailCode::Trapped { kind, .. }) => Some(*kind),
        _ => None,
    }
}

fn event<'a>(r: &'a Receipt, name: &str) -> &'a [u8] {
    let found: Vec<&Event> = r.events.iter().filter(|e| e.name == name).collect();
    assert_eq!(found.len(), 1, "one `{name}` event in {:?}", r.events.iter().map(|e| &e.name).collect::<Vec<_>>());
    &found[0].data
}

fn event_names(r: &Receipt) -> Vec<&str> {
    r.events.iter().map(|e| e.name.as_str()).collect()
}

/// A dealer's chain from its top link: `links[0]` is the commitment, `links[j]` deals card `j`.
fn chain(top: [u8; 32]) -> Vec<[u8; 32]> {
    let mut links = vec![top];
    for _ in 0..CHAIN_LEN {
        links.push(*blake3::hash(links.last().unwrap()).as_bytes());
    }
    links.reverse();
    links
}

/// Game `k`'s chain in these tests.
fn chain_of(k: u64) -> Vec<[u8; 32]> {
    chain(*blake3::hash(format!("dealer secret {k}").as_bytes()).as_bytes())
}

/// Card `j` of game `id`, from the documented formula with the `blake3` crate.
fn card(link: &[u8; 32], seed: &[u8; 32], id: u64, j: usize) -> u8 {
    let mut h = blake3::Hasher::new();
    h.update(link).update(seed).update(&id.to_le_bytes()).update(&[j as u8]);
    (u64::from_le_bytes(h.finalize().as_bytes()[..8].try_into().unwrap()) % 52) as u8
}

/// The cards game `id` would deal under `seed`, all 32 of them.
fn deck(id: u64, seed: &[u8; 32]) -> Vec<u8> {
    let links = chain_of(id);
    (1..=CHAIN_LEN).map(|j| card(&links[j], seed, id, j)).collect()
}

/// A hand played by `wants` (hit?) from `deck`: the cards used, the outcome and the hand.
fn play(deck: &[u8], mut wants: impl FnMut(&Hand) -> bool) -> (usize, Outcome, Hand) {
    let mut hand = Hand::new();
    loop {
        if hand.phase == Phase::Player {
            if wants(&hand) {
                hand.hit();
            } else {
                hand.stand();
            }
        }
        let c = deck[hand.dealt as usize];
        if let Some(Step::Settled(o)) = hand.deal(c) {
            return (hand.dealt as usize, o, hand);
        }
    }
}

/// A client seed under which game `id`, played by `wants`, ends as `accept` says.
fn seed_for(id: u64, wants: impl Fn(&Hand) -> bool + Copy, accept: impl Fn(Outcome, &Hand) -> bool) -> [u8; 32] {
    for n in 0u32..200_000 {
        let seed = *blake3::hash(&[&id.to_le_bytes()[..], &n.to_le_bytes()].concat()).as_bytes();
        let (_, outcome, hand) = play(&deck(id, &seed), wants);
        if accept(outcome, &hand) {
            return seed;
        }
    }
    panic!("no seed found for game {id}");
}

fn never(_: &Hand) -> bool {
    false
}

fn below_17(hand: &Hand) -> bool {
    bj_core::total(hand.player_cards()).0 < 17
}

/// A chain with the program created and initialized by `house` (the owner), `dealer` named, and
/// accounts funded.
struct Table {
    host: MemHost,
    kinds: BTreeMap<String, MethodKind>,
    /// The storage limit player and owner calls run under; the dealer's reveals and anyone's
    /// timeouts always run under 0.
    storage_limit: u64,
}

impl Table {
    /// Created and initialized, with the default limits, nothing funded or committed.
    fn bare() -> Table {
        let mut host = MemHost::new();
        // Free storage, so balances move by stakes and payouts alone; `deposits_are_bonded…` runs
        // with a price.
        host.storage_price = 0;
        for who in ["house", "dealer", "alice", "bob", "carol", "bot", "stranger"] {
            host.fund(addr(who), 10_000 * FANG);
        }
        host.deploy(addr(PROGRAM), &guest());
        let mut kinds = BTreeMap::new();
        for m in &interface().methods {
            host.set_method(addr(PROGRAM), &m.name, m.kind, m.payable);
            kinds.insert(m.name.clone(), m.kind);
        }
        let mut t = Table { host, kinds, storage_limit: FANG };
        assert!(t.tx("house", "init", addr("dealer").to_vec(), 0).ok());
        t
    }

    /// A funded table: 2,000 FANG of bankroll and 16 commitments.
    fn new() -> Table {
        let mut t = Table::bare();
        assert!(t.tx("house", "fund", vec![], 2_000 * FANG).ok());
        t.commit(0, 16);
        t
    }

    /// One call from `signer`, on every engine, twice on each: every receipt must be the same.
    /// A successful one is applied, and the invariant checked.
    fn tx(&mut self, signer: &str, method: &str, input: Vec<u8>, value: u64) -> Receipt {
        let kind = *self.kinds.get(method).unwrap_or_else(|| panic!("no method {method}"));
        let storage_limit = if matches!(method, "reveal" | "timeout") { 0 } else { self.storage_limit };
        let limits = TxLimits { fuel_limit: MAX_TX_FUEL, storage_limit };
        let call = self.host.bind(Call { program: addr(PROGRAM), version_digest: [0; 32], method: method.into(), input, value, kind });
        let h = &self.host;
        let receipts: Vec<Receipt> = EngineKind::ALL
            .iter()
            .map(|&kind| {
                on_program_thread(|| {
                    let (engine, _) = h.engine_for(kind, &SCHEDULE_V0, &[call.program]).unwrap();
                    let cold = execute(engine.as_ref(), h, addr(signer), std::slice::from_ref(&call), &[], limits, &SCHEDULE_V0).expect("no node fault");
                    let warm = execute(engine.as_ref(), h, addr(signer), std::slice::from_ref(&call), &[], limits, &SCHEDULE_V0).expect("no node fault");
                    assert_eq!(cold, warm, "{method}: a second run differs on {}", kind.name());
                    cold
                })
            })
            .collect();
        for r in &receipts[1..] {
            assert_eq!(Receipt { fuel_used: receipts[0].fuel_used, ..r.clone() }, receipts[0], "{method}: the engines disagree");
        }
        let r = receipts[0].clone();
        assert_ne!(trap(&r), Some(TrapKind::OutOfFuel), "{method} ran out of fuel");
        let mut fuel = FUEL.lock().unwrap();
        let most = fuel.entry(method.into()).or_default();
        *most = (*most).max(r.fuel_used);
        drop(fuel);
        if r.ok() {
            self.host.apply(&r.diff);
            if kind != MethodKind::View {
                self.check();
            }
        }
        r
    }

    fn view(&mut self, method: &str, input: Vec<u8>) -> Vec<u8> {
        let r = self.tx("stranger", method, input, 0);
        assert!(r.ok(), "{method}: {:?}", r.fail);
        assert!(r.events.is_empty() && r.diff.storage.is_empty() && r.diff.balances.is_empty(), "a view changes nothing");
        r.returns[0].clone()
    }

    /// A view that returns `bytes`: the `u32` length is the rest's.
    fn view_bytes(&mut self, method: &str, input: Vec<u8>) -> Vec<u8> {
        let out = self.view(method, input);
        assert_eq!(u32::from_le_bytes(out[..4].try_into().unwrap()) as usize, out.len() - 4, "{method}");
        out[4..].to_vec()
    }

    fn stored(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.host.storage(&addr(PROGRAM), key).map(|e| e.value)
    }

    fn house(&self) -> House {
        let h = self.stored(b"H").expect("initialized");
        assert_eq!(h.len(), HOUSE);
        House {
            bankroll: u64_at(&h, 64),
            reserved: u64_at(&h, 72),
            stakes: u64_at(&h, 80),
            table: u64_at(&h, 88),
            next_commit: u64_at(&h, 96),
            next_game: u64_at(&h, 104),
            paused: h[112],
            open: h[113],
        }
    }

    fn game(&self, id: u64) -> Option<Vec<u8>> {
        self.stored(&[&b"g"[..], &le(id)].concat())
    }

    fn player(&self, who: &str) -> Vec<u8> {
        self.stored(&[&b"p"[..], &addr(who)].concat()).unwrap_or(vec![0; PLAYER])
    }

    fn table_balance(&self, who: &str) -> u64 {
        u64_at(&self.player(who), 0)
    }

    fn balance(&self, who: &str) -> u64 {
        self.host.balance(&addr(who)).unwrap_or(0)
    }

    /// The invariant: every drop is in exactly one counter, and each counter is the sum of the
    /// records it counts.
    fn check(&self) {
        let h = self.house();
        assert_eq!(self.balance(PROGRAM), h.bankroll + h.reserved + h.stakes + h.table, "balance = bankroll + reserved + stakes + table: {h:?}");
        let (mut reserved, mut stakes, mut table, mut open, mut commits) = (0, 0, 0, vec![], 0);
        for ((program, key), entry) in &self.host.storage {
            assert_eq!(*program, addr(PROGRAM));
            match key[0] {
                b'g' => {
                    assert_eq!(entry.value.len(), GAME);
                    let stake = u64_at(&entry.value, 32);
                    stakes += stake;
                    reserved += exposure(stake);
                    let id = u64_at(key, 1);
                    open.push(id);
                    // Its player's record names it.
                    let player: Address = entry.value[..32].try_into().unwrap();
                    let p = self.host.storage(&addr(PROGRAM), &[&b"p"[..], &player].concat()).unwrap().value;
                    assert_eq!(u64_at(&p, 24), id + 1, "the player's open hand");
                    assert!((1..=4).contains(&entry.value[120]), "an open hand's phase");
                }
                b'p' => {
                    assert_eq!(entry.value.len(), PLAYER);
                    table += u64_at(&entry.value, 0);
                    let open_id = u64_at(&entry.value, 24);
                    if open_id != 0 {
                        assert!(self.game(open_id - 1).is_some(), "a player's open hand exists");
                    }
                }
                b'c' => {
                    let k = u64_at(key, 1);
                    assert!(k >= h.next_game && k < h.next_commit, "only unused commitments are stored");
                    commits += 1;
                }
                b'H' | b'C' | b'O' | b'r' => {}
                other => panic!("a stray key {other}"),
            }
        }
        assert_eq!((reserved, stakes, table), (h.reserved, h.stakes, h.table), "the counters are their records' sums");
        assert_eq!(commits, h.next_commit - h.next_game, "every unused commitment is stored");
        assert_eq!(open.len(), h.open as usize);
        let ids = self.stored(b"O").unwrap();
        let mut listed: Vec<u64> = (0..h.open as usize).map(|i| u64_at(&ids, 8 * i)).collect();
        listed.sort();
        open.sort();
        assert_eq!(listed, open, "the open list");
    }

    fn commit(&mut self, first: u64, n: u64) {
        let r = self.tx("dealer", "commit_batch", commit_input(first, n), 0);
        assert!(r.ok(), "{:?}", r.fail);
    }

    /// Bets `stake` from `who`'s account; the game's id.
    fn bet(&mut self, who: &str, stake: u64, seed: &[u8; 32]) -> u64 {
        let r = self.tx(who, "bet", seed.to_vec(), stake);
        assert!(r.ok(), "bet: {:?}", r.fail);
        u64_at(&r.returns[0], 0)
    }

    /// Reveals links `from..from + n` of game `id`'s chain.
    fn reveal(&mut self, id: u64, from: usize, n: usize) -> Receipt {
        self.tx("dealer", "reveal", reveal_input(id, &chain_of(id)[from..from + n]), 0)
    }

    /// Plays game `id` to its end as the dealer daemon and `who` would: `wants` decides each move.
    /// Returns the settling receipt.
    fn play_out(&mut self, who: &str, id: u64, seed: &[u8; 32], wants: impl Fn(&Hand) -> bool) -> Receipt {
        let deck = deck(id, seed);
        let mut hand = Hand::new();
        let mut next = 1;
        loop {
            // The dealer reveals everything it owes in one call.
            let mut n = 0;
            let mut probe = hand;
            let mut settled = false;
            while probe.phase.dealers() {
                if let Some(Step::Settled(_)) = probe.deal(deck[next - 1 + n]) {
                    settled = true;
                }
                n += 1;
            }
            let r = self.reveal(id, next, n);
            assert!(r.ok(), "reveal: {:?}", r.fail);
            next += n;
            hand = probe;
            if settled {
                assert_eq!(r.returns[0][0], Phase::Done as u8);
                return r;
            }
            assert_eq!(r.returns[0], [Phase::Player as u8, 0]);
            let hit = wants(&hand);
            let r = self.tx(who, if hit { "hit" } else { "stand" }, le(id), 0);
            assert!(r.ok(), "move: {:?}", r.fail);
            if hit {
                hand.hit();
            } else {
                hand.stand();
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct House {
    bankroll: u64,
    reserved: u64,
    stakes: u64,
    table: u64,
    next_commit: u64,
    next_game: u64,
    paused: u8,
    open: u8,
}

fn commit_input(first: u64, n: u64) -> Vec<u8> {
    let mut input = le(first);
    input.extend_from_slice(&(n as u32).to_le_bytes());
    for k in first..first + n {
        input.extend_from_slice(&chain_of(k)[0]);
    }
    input
}

fn reveal_input(id: u64, links: &[[u8; 32]]) -> Vec<u8> {
    let mut input = le(id);
    input.extend_from_slice(&(links.len() as u32).to_le_bytes());
    for l in links {
        input.extend_from_slice(l);
    }
    input
}

/// `set_limits`' input: the defaults with `changes` (index, value) applied.
fn limits_input(changes: &[(usize, u64)]) -> Vec<u8> {
    let mut l = [FANG, 50 * FANG, 500 * FANG, 43_200, 10, 20, 750 * FANG, 150, 1_800, 200 * FANG, 25 * FANG, 43_200];
    for (i, v) in changes {
        l[*i] = *v;
    }
    l.iter().flat_map(|n| n.to_le_bytes()).collect()
}

// Indexes into the limits.
const MIN_BET: usize = 0;
const MAX_BET: usize = 1;
const DAILY_CAP: usize = 2;
const DAY_BLOCKS: usize = 3;
const MAX_OPEN: usize = 4;
const DIVISOR: usize = 5;
const MAX_EXPOSURE: usize = 6;
const REVEAL_BLOCKS: usize = 7;
const IDLE_BLOCKS: usize = 8;
const DEPOSIT_CAP: usize = 9;
#[allow(dead_code)]
const RELAY_MAX_BET: usize = 10;
const GRANT_BLOCKS: usize = 11;

// ---------------------------------------------------------------------------------- protocol v2

#[test]
fn it_uploads_under_protocol_v2_with_v2s_host_functions_only() {
    let mut host = MemHost::new();
    host.protocol_version = FIRST_VERSION;
    assert_eq!(FIRST_VERSION, 2);
    host.upload(&guest()).expect("no node fault").expect("uploads under protocol v2");
    // And by name: none of the functions protocol v3 adds.
    let wasm = guest();
    for name in ["blake3_v1", "epoch_v1", "beacon_v1"] {
        assert!(!wasm.windows(name.len()).any(|w| w == name.as_bytes()), "imports {name}");
    }
    assert!(wasm.len() < 64 * 1024, "{} bytes", wasm.len());
}

// ---------------------------------------------------------------------------------- the owner

#[test]
fn init_sets_the_owner_the_dealer_and_the_default_limits_once() {
    let mut t = Table::bare();
    let h = t.stored(b"H").unwrap();
    assert_eq!(h[..32], addr("house"));
    assert_eq!(h[32..64], addr("dealer"));
    assert_eq!(h[64..], [0u8; 50], "nothing held, committed or open");
    assert_eq!(t.stored(b"C").unwrap(), limits_input(&[]));
    assert_eq!(t.view_bytes("limits", vec![]), limits_input(&[]));
    assert_eq!(t.stored(b"O").unwrap(), vec![0; 256]);
    assert_eq!(abort_code(&t.tx("house", "init", addr("dealer").to_vec(), 0)), Some(40));
    assert_eq!(abort_code(&t.tx("house", "init", vec![0; 31], 0)), Some(1));

    // Before `init` nothing else runs.
    let mut host = MemHost::new();
    host.fund(addr("alice"), 100 * FANG);
    host.deploy(addr(PROGRAM), &guest());
    let mut fresh = Table { host, kinds: t.kinds.clone(), storage_limit: FANG };
    for m in &interface().methods {
        fresh.host.set_method(addr(PROGRAM), &m.name, m.kind, m.payable);
    }
    for (method, input, value) in [("fund", vec![], FANG), ("bet", vec![0; 32], FANG), ("deposit", vec![], FANG), ("pause", vec![1], 0), ("house", vec![], 0), ("limits", vec![], 0)] {
        assert_eq!(abort_code(&fresh.tx("alice", method, input, value)), Some(41), "{method} before init");
    }
}

#[test]
fn the_bankroll_is_funded_by_anyone_and_taken_out_by_the_owner_only() {
    let mut t = Table::bare();
    let r = t.tx("house", "fund", vec![], 100 * FANG);
    assert_eq!(event(&r, "fund"), [&addr("house")[..], &le(100 * FANG), &le(100 * FANG)].concat());
    let r = t.tx("alice", "fund", vec![], FANG);
    assert_eq!(event(&r, "fund"), [&addr("alice")[..], &le(FANG), &le(101 * FANG)].concat());
    assert_eq!(abort_code(&t.tx("house", "fund", vec![], 0)), Some(44));
    assert_eq!(abort_code(&t.tx("house", "fund", vec![1], FANG)), Some(1));

    assert_eq!(abort_code(&t.tx("alice", "defund", le(FANG), 0)), Some(42));
    assert_eq!(abort_code(&t.tx("dealer", "defund", le(FANG), 0)), Some(42));
    assert_eq!(abort_code(&t.tx("house", "defund", le(101 * FANG + 1), 0)), Some(63));
    assert_eq!(abort_code(&t.tx("house", "defund", le(0), 0)), Some(63));
    assert_eq!(trap(&t.tx("house", "defund", le(FANG), 1)), Some(TrapKind::NotPayable));
    let before = t.balance("house");
    let r = t.tx("house", "defund", le(40 * FANG), 0);
    assert_eq!(event(&r, "defund"), [le(40 * FANG), le(61 * FANG)].concat());
    assert_eq!(t.balance("house"), before + 40 * FANG);
    assert_eq!(t.house().bankroll, 61 * FANG);

    // What open hands reserve isn't the owner's to take.
    t.commit(0, 1);
    let id = t.bet("alice", 2 * FANG, &[1; 32]);
    assert_eq!(t.house(), House { bankroll: 58 * FANG, reserved: 3 * FANG, stakes: 2 * FANG, table: 0, next_commit: 1, next_game: 1, paused: 0, open: 1 });
    assert_eq!(abort_code(&t.tx("house", "defund", le(58 * FANG + 1), 0)), Some(63));
    assert!(t.tx("house", "defund", le(58 * FANG), 0).ok());
    assert_eq!(t.balance(PROGRAM), 5 * FANG, "the stake and its reserve stay");
    assert!(t.game(id).is_some());
}

#[test]
fn limits_are_the_owners_and_checked() {
    let mut t = Table::bare();
    assert_eq!(abort_code(&t.tx("alice", "set_limits", limits_input(&[]), 0)), Some(42));
    assert_eq!(abort_code(&t.tx("house", "set_limits", vec![0; 95], 0)), Some(1));
    let new = limits_input(&[(MIN_BET, 2 * FANG), (MAX_BET, 10 * FANG), (MAX_OPEN, 32), (REVEAL_BLOCKS, 20)]);
    let r = t.tx("house", "set_limits", new.clone(), 0);
    assert_eq!(event(&r, "limits"), new);
    assert_eq!(t.view_bytes("limits", vec![]), new);
    for (bad, why) in [
        (vec![(MIN_BET, 0)], "a zero minimum"),
        (vec![(MIN_BET, 51 * FANG)], "a minimum above the maximum"),
        (vec![(MAX_BET, u64::MAX)], "an amount past the supply"),
        (vec![(DAILY_CAP, u64::MAX)], "an amount past the supply"),
        (vec![(DAY_BLOCKS, 0)], "a day of no blocks"),
        (vec![(MAX_OPEN, 0)], "no open hands"),
        (vec![(MAX_OPEN, 33)], "more open hands than the list holds"),
        (vec![(DIVISOR, 0)], "a zero divisor"),
        (vec![(REVEAL_BLOCKS, 0)], "no time to reveal"),
        (vec![(IDLE_BLOCKS, 0)], "no time to move"),
        (vec![(IDLE_BLOCKS, u64::MAX)], "a deadline that overflows"),
        (vec![(GRANT_BLOCKS, 0)], "grants of no blocks"),
        (vec![(DEPOSIT_CAP, u64::MAX)], "an amount past the supply"),
    ] {
        assert_eq!(abort_code(&t.tx("house", "set_limits", limits_input(&bad), 0)), Some(45), "{why}");
    }
    assert_eq!(t.view_bytes("limits", vec![]), new, "a refused change changes nothing");

    // The dealer and the relayers are the owner's to name.
    assert_eq!(abort_code(&t.tx("dealer", "set_dealer", addr("bob").to_vec(), 0)), Some(42));
    let r = t.tx("house", "set_dealer", addr("bob").to_vec(), 0);
    assert_eq!(event(&r, "dealer"), addr("bob"));
    assert_eq!(abort_code(&t.tx("dealer", "commit_batch", commit_input(0, 1), 0)), Some(43), "the old dealer");
    assert!(t.tx("bob", "commit_batch", commit_input(0, 1), 0).ok());
    assert_eq!(abort_code(&t.tx("alice", "approve_relayer", [&addr("bot")[..], &[1]].concat(), 0)), Some(42));
    assert_eq!(abort_code(&t.tx("house", "approve_relayer", [&addr("bot")[..], &[2]].concat(), 0)), Some(1));
    assert_eq!(abort_code(&t.tx("alice", "pause", vec![1], 0)), Some(42));
    assert_eq!(abort_code(&t.tx("house", "pause", vec![2], 0)), Some(1));
}

// ---------------------------------------------------------------------------------- commitments

#[test]
fn the_dealer_commits_in_order_and_a_repeated_batch_is_refused() {
    let mut t = Table::bare();
    assert_eq!(abort_code(&t.tx("alice", "commit_batch", commit_input(0, 2), 0)), Some(43));
    assert_eq!(abort_code(&t.tx("house", "commit_batch", commit_input(0, 2), 0)), Some(43), "the owner isn't the dealer");
    let r = t.tx("dealer", "commit_batch", commit_input(0, 3), 0);
    assert_eq!(event(&r, "commit"), [&le(0)[..], &chain_of(0)[0], &chain_of(1)[0], &chain_of(2)[0]].concat());
    for k in 0..3 {
        assert_eq!(t.view("commitment", le(k)), chain_of(k)[0]);
    }
    assert_eq!(abort_code(&t.tx("stranger", "commitment", le(3), 0)), Some(53));
    // The same batch again (a retry), a gap, an overlap: each names the wrong start.
    for first in [0, 1, 2, 4] {
        assert_eq!(abort_code(&t.tx("dealer", "commit_batch", commit_input(first, 2), 0)), Some(61), "first = {first}");
    }
    // None, too many, a short list.
    assert_eq!(abort_code(&t.tx("dealer", "commit_batch", commit_input(3, 0), 0)), Some(1));
    assert_eq!(abort_code(&t.tx("dealer", "commit_batch", commit_input(3, 65), 0)), Some(1));
    let mut short = commit_input(3, 2);
    short.pop();
    assert_eq!(abort_code(&t.tx("dealer", "commit_batch", short, 0)), Some(1));
    assert_eq!(abort_code(&t.tx("dealer", "commit_batch", le(3), 0)), Some(1));
    t.commit(3, 64);
    assert_eq!(t.house().next_commit, 67);

    // A bet takes them in order, and a taken one is gone.
    assert!(t.tx("house", "fund", vec![], 1_000 * FANG).ok());
    for (who, k) in [("alice", 0), ("bob", 1)] {
        let r = t.tx(who, "bet", vec![9; 32], FANG);
        assert_eq!(r.returns[0], le(k));
        assert_eq!(event(&r, "bet")[80..112], chain_of(k)[0], "the bet's commitment");
        assert_eq!(abort_code(&t.tx("stranger", "commitment", le(k), 0)), Some(53));
    }

    // The owner drops unused ones (a dealer that lost its secrets): the next bet takes the one after.
    assert_eq!(abort_code(&t.tx("dealer", "skip_commits", 2u32.to_le_bytes().to_vec(), 0)), Some(42));
    assert_eq!(abort_code(&t.tx("house", "skip_commits", 0u32.to_le_bytes().to_vec(), 0)), Some(61));
    assert_eq!(abort_code(&t.tx("house", "skip_commits", 65u32.to_le_bytes().to_vec(), 0)), Some(61));
    let r = t.tx("house", "skip_commits", 3u32.to_le_bytes().to_vec(), 0);
    assert_eq!(event(&r, "skipped"), [&le(2)[..], &3u32.to_le_bytes()].concat());
    assert_eq!(t.house().next_game, 5);
    let r = t.tx("carol", "bet", vec![9; 32], FANG);
    assert_eq!(r.returns[0], le(5));
    // Skipping past the end stops at it.
    let r = t.tx("house", "skip_commits", 64u32.to_le_bytes().to_vec(), 0);
    assert_eq!(event(&r, "skipped"), [&le(6)[..], &61u32.to_le_bytes()].concat());
    assert_eq!((t.house().next_game, t.house().next_commit), (67, 67));
}

// ---------------------------------------------------------------------------------- a hand

#[test]
fn a_hand_from_bet_to_settlement() {
    let mut t = Table::new();
    // A hand alice wins after one hit: hit below 17, stand from it.
    let seed = seed_for(0, below_17, |o, h| o == Outcome::Win && h.n_player == 3 && h.n_dealer >= 3);
    let deck = deck(0, &seed);
    let links = chain_of(0);
    let stake = 4 * FANG;
    let (alice, program) = (t.balance("alice"), t.balance(PROGRAM));

    t.host.height = 100;
    let r = t.tx("alice", "bet", seed.to_vec(), stake);
    assert_eq!(r.returns[0], le(0));
    assert_eq!(event(&r, "bet"), [&le(0)[..], &addr("alice"), &le(stake), &seed, &links[0], &le(250), &[0], &le(0)].concat());
    assert_eq!(t.balance("alice"), alice - stake);
    assert_eq!(t.house(), House { bankroll: 1_994 * FANG, reserved: 6 * FANG, stakes: stake, table: 0, next_commit: 16, next_game: 1, paused: 0, open: 1 });
    let g = t.game(0).unwrap();
    assert_eq!(g[..125], [&addr("alice")[..], &le(stake), &seed, &links[0], &le(250), &le(0), &[1, 0, 0, 0, 0]].concat());
    assert_eq!(t.view("open_games", vec![]), [&1u32.to_le_bytes()[..], &le(0)].concat());

    // The deal: three links, three cards, the player's turn with its own deadline.
    t.host.height = 103;
    let r = t.reveal(0, 1, 3);
    assert_eq!(r.returns[0], [Phase::Player as u8, 0]);
    let p2 = bj_core::total(&[deck[0], deck[2]]).0;
    let up = bj_core::total(&[deck[1]]).0;
    let dealt: Vec<u8> = (0..3).flat_map(|i| [&links[i + 1][..], &[deck[i]]].concat()).collect();
    assert_eq!(event(&r, "cards"), [&le(0)[..], &addr("alice"), &[1, 3], &dealt, &[2, p2, up], &le(103 + 1_800)].concat());
    let view = t.view_bytes("game", le(0));
    assert_eq!(view.len(), GAME + 2);
    assert_eq!(view[72..104], links[3], "the newest link");
    assert_eq!(view[120..125], [2, 0, 3, 2, 1], "phase, flags, dealt, player's cards, dealer's cards");
    assert_eq!(view[125..127], [deck[0], deck[2]]);
    assert_eq!(view[145], deck[1]);
    assert_eq!(view[GAME..], [p2, up]);

    // A hit: the dealer owes one card, within `reveal_blocks`.
    t.host.height = 110;
    let r = t.tx("alice", "hit", le(0), 0);
    assert_eq!(event(&r, "hit"), [&le(0)[..], &addr("alice"), &le(260)].concat());
    assert_eq!(t.game(0).unwrap()[120], Phase::Hit as u8);
    let r = t.reveal(0, 4, 1);
    assert_eq!(r.returns[0], [Phase::Player as u8, 0]);
    assert_eq!(event(&r, "cards")[40..42], [4, 1]);

    // The stand, then the dealer's whole hand in one reveal: settled.
    t.host.height = 120;
    let r = t.tx("alice", "stand", le(0), 0);
    assert_eq!(event(&r, "stand"), [&le(0)[..], &addr("alice"), &le(270), &[0]].concat());
    let (used, outcome, hand) = play(&deck, below_17);
    assert_eq!(outcome, Outcome::Win);
    let r = t.reveal(0, 5, used - 4);
    assert_eq!(r.returns[0], [Phase::Done as u8, Outcome::Win as u8]);
    assert_eq!(event_names(&r), ["cards", "settled"]);
    let (pt, dt) = (bj_core::total(hand.player_cards()).0, bj_core::total(hand.dealer_cards()).0);
    assert_eq!(
        event(&r, "settled"),
        [&le(0)[..], &addr("alice"), &[2], &le(stake), &le(2 * stake), &[0, used as u8, 3], &seed, &links[used], &[pt, dt]].concat()
    );
    assert_eq!(t.balance("alice"), alice + stake, "the stake back and as much again");
    assert_eq!(t.balance(PROGRAM), program - stake);
    assert_eq!(t.house(), House { bankroll: 1_996 * FANG, reserved: 0, stakes: 0, table: 0, next_commit: 16, next_game: 1, paused: 0, open: 0 });
    assert_eq!(t.game(0), None);
    assert_eq!(t.view("open_games", vec![]), 0u32.to_le_bytes());
    // The player's record keeps the hand: enough to recompute every card.
    let p = t.view_bytes("player", addr("alice").to_vec());
    assert_eq!(p.len(), PLAYER + 8);
    assert_eq!(p[..32], [le(0), le(0), le(stake), le(0)].concat(), "no table balance, the day's wager, no open hand");
    assert_eq!(p[104..PLAYER], [&le(0)[..], &[2, used as u8, 3], &le(stake), &le(2 * stake), &seed, &links[used]].concat());
    assert_eq!(u64_at(&p, PLAYER), t.host.height);
    // Hashing the newest link down gives every link, and the last hash is the commitment.
    let mut link = links[used];
    for _ in 0..used {
        link = *blake3::hash(&link).as_bytes();
    }
    assert_eq!(link, links[0]);
    // It's over: no more moves, reveals or timeouts.
    for (who, method, input) in [("alice", "hit", le(0)), ("alice", "stand", le(0)), ("dealer", "timeout", le(0)), ("stranger", "game", le(0))] {
        assert_eq!(abort_code(&t.tx(who, method, input, 0)), Some(54), "{method} on a settled hand");
    }
    assert_eq!(abort_code(&t.reveal(0, used + 1, 1)), Some(54));
}

#[test]
fn every_outcome_pays_what_it_should() {
    let stake = 3 * FANG + 1;
    type Pick = fn(&Hand) -> bool;
    let cases: [(&str, Outcome, Pick, Pick); 9] = [
        ("a natural against a low card: paid at once", Outcome::Blackjack, never, |h| h.n_dealer == 1),
        ("a natural the hole card doesn't match", Outcome::Blackjack, never, |h| h.n_dealer == 2),
        ("two naturals", Outcome::Push, never, |h| h.n_player == 2 && bj_core::natural(h.player_cards())),
        ("the dealer goes over", Outcome::Win, never, |h| bj_core::total(h.dealer_cards()).0 > 21),
        ("a higher total", Outcome::Win, below_17, |h| bj_core::total(h.dealer_cards()).0 <= 21 && h.n_player >= 3),
        ("equal totals", Outcome::Push, below_17, |h| !bj_core::natural(h.player_cards())),
        ("a lower total", Outcome::Lose, never, |h| !bj_core::natural(h.dealer_cards())),
        ("the dealer's natural", Outcome::Lose, never, |h| bj_core::natural(h.dealer_cards())),
        ("over 21", Outcome::Bust, below_17, |h| h.n_dealer == 1),
    ];
    for (what, outcome, wants, shape) in cases {
        let mut t = Table::new();
        let seed = seed_for(0, wants, |o, h| o == outcome && shape(h));
        let (alice, house) = (t.balance("alice"), t.house());
        let id = t.bet("alice", stake, &seed);
        let r = t.play_out("alice", id, &seed, wants);
        assert_eq!(r.returns[0][1], outcome as u8, "{what}");
        let pays = match outcome {
            Outcome::Blackjack => stake + stake + stake / 2,
            Outcome::Win => 2 * stake,
            Outcome::Push => stake,
            _ => 0,
        };
        assert_eq!(pays, payout(stake, outcome));
        assert_eq!(u64_at(event(&r, "settled"), 49), pays, "{what}");
        assert_eq!(t.balance("alice"), alice - stake + pays, "{what}");
        assert_eq!(t.house(), House { bankroll: house.bankroll + stake - pays, next_game: 1, ..house }, "{what}");
    }
}

#[test]
fn reveals_are_the_chains_next_links_and_no_more() {
    let mut t = Table::new();
    let seed = seed_for(0, below_17, |o, h| o == Outcome::Lose && h.n_player >= 4 && h.n_dealer >= 3);
    let links = chain_of(0);
    let id = t.bet("alice", FANG, &seed);
    assert_eq!(abort_code(&t.reveal(7, 1, 3)), Some(54), "no such hand");
    // Not the next link: the second, the commitment itself, another chain's first, noise.
    for wrong in [links[2], links[0], chain_of(1)[1], [0; 32]] {
        assert_eq!(abort_code(&t.tx("dealer", "reveal", reveal_input(id, &[wrong]), 0)), Some(58));
    }
    // The right first link followed by a wrong one: nothing of the call applies.
    assert_eq!(abort_code(&t.tx("dealer", "reveal", reveal_input(id, &[links[1], links[3]]), 0)), Some(58));
    assert_eq!(t.game(id).unwrap()[122], 0, "nothing dealt");
    // No links, too many, a length that isn't the count's.
    assert_eq!(abort_code(&t.tx("dealer", "reveal", reveal_input(id, &[]), 0)), Some(1));
    assert_eq!(abort_code(&t.tx("dealer", "reveal", reveal_input(id, &links[1..14]), 0)), Some(1));
    let mut short = reveal_input(id, &links[1..3]);
    short.truncate(short.len() - 1);
    assert_eq!(abort_code(&t.tx("dealer", "reveal", short, 0)), Some(1));
    assert_eq!(trap(&t.tx("dealer", "reveal", reveal_input(id, &links[1..4]), 1)), Some(TrapKind::NotPayable));
    // A link past the deal: the hand is the player's to move.
    assert_eq!(abort_code(&t.reveal(id, 1, 4)), Some(59));
    // The deal in two calls: the first leaves it the dealer's, with the bet's deadline.
    let deadline = u64_at(&t.game(id).unwrap(), 104);
    t.host.height += 5;
    let r = t.reveal(id, 1, 2);
    assert_eq!(r.returns[0], [Phase::Deal as u8, 0]);
    assert_eq!(u64_at(&t.game(id).unwrap(), 104), deadline, "a partial reveal doesn't extend the deadline");
    assert_eq!(abort_code(&t.reveal(id, 1, 1)), Some(58), "a link already revealed");
    // Anyone who knows the link may reveal it.
    let r = t.tx("stranger", "reveal", reveal_input(id, &links[3..4]), 0);
    assert_eq!(r.returns[0], [Phase::Player as u8, 0]);
    // On the player's turn the dealer owes nothing.
    assert_eq!(abort_code(&t.reveal(id, 4, 1)), Some(57));
    // A hit takes one card and no more.
    assert!(t.tx("alice", "hit", le(id), 0).ok());
    assert_eq!(abort_code(&t.reveal(id, 4, 2)), Some(59));
    assert!(t.reveal(id, 4, 1).ok());
    // The dealer's play takes exactly its cards.
    let (used, _, _) = play(&deck(0, &seed), below_17);
    let mut hits = 1;
    while t.game(id).unwrap()[123] as usize + t.game(id).unwrap()[124] as usize + 1 < used && bj_core::total(&t.game(id).unwrap()[125..125 + t.game(id).unwrap()[123] as usize]).0 < 17 {
        assert!(t.tx("alice", "hit", le(id), 0).ok());
        assert!(t.reveal(id, 4 + hits, 1).ok());
        hits += 1;
    }
    assert!(t.tx("alice", "stand", le(id), 0).ok());
    let dealt = t.game(id).unwrap()[122] as usize;
    assert_eq!(abort_code(&t.reveal(id, dealt + 1, used - dealt + 1)), Some(59), "one link past the dealer's last card");
    let r = t.reveal(id, dealt + 1, used - dealt);
    assert_eq!(r.returns[0], [Phase::Done as u8, Outcome::Lose as u8]);
}

#[test]
fn moves_are_the_players_on_their_turn() {
    let mut t = Table::new();
    let seed = seed_for(0, never, |_, h| !bj_core::natural(h.player_cards()));
    let id = t.bet("alice", FANG, &seed);
    // Before the deal there's no move.
    for method in ["hit", "stand"] {
        assert_eq!(abort_code(&t.tx("alice", method, le(id), 0)), Some(56), "{method} before the deal");
    }
    assert!(t.reveal(id, 1, 3).ok());
    for method in ["hit", "stand"] {
        assert_eq!(abort_code(&t.tx("bob", method, le(id), 0)), Some(55), "{method} by someone else");
        assert_eq!(abort_code(&t.tx("dealer", method, le(id), 0)), Some(55));
        assert_eq!(abort_code(&t.tx("house", method, le(id), 0)), Some(55));
        assert_eq!(abort_code(&t.tx("alice", method, le(id + 1), 0)), Some(54));
        assert_eq!(abort_code(&t.tx("alice", method, vec![0; 7], 0)), Some(1));
        assert_eq!(trap(&t.tx("alice", method, le(id), 1)), Some(TrapKind::NotPayable));
    }
    assert!(t.tx("alice", "stand", le(id), 0).ok());
    for method in ["hit", "stand"] {
        assert_eq!(abort_code(&t.tx("alice", method, le(id), 0)), Some(56), "{method} after standing");
    }
}

// ---------------------------------------------------------------------------------- caps

#[test]
fn bets_keep_every_cap() {
    let mut t = Table::new();
    let seed = [5; 32];
    assert_eq!(abort_code(&t.tx("alice", "bet", seed.to_vec(), 0)), Some(44));
    assert_eq!(abort_code(&t.tx("alice", "bet", vec![5; 31], FANG)), Some(1));
    assert_eq!(abort_code(&t.tx("alice", "bet", seed.to_vec(), FANG - 1)), Some(47), "below the minimum");
    assert_eq!(abort_code(&t.tx("alice", "bet", seed.to_vec(), 50 * FANG + 1)), Some(48), "above the maximum");
    assert!(t.tx("alice", "bet", seed.to_vec(), 50 * FANG).ok(), "the maximum itself");
    // One hand at a time.
    assert_eq!(abort_code(&t.tx("alice", "bet", seed.to_vec(), FANG)), Some(52));

    // A twentieth of the free bankroll: 400 FANG of it leaves 20 a bet.
    let mut t = Table::bare();
    assert!(t.tx("house", "fund", vec![], 400 * FANG).ok());
    t.commit(0, 8);
    assert_eq!(abort_code(&t.tx("alice", "bet", seed.to_vec(), 20 * FANG + 1)), Some(48));
    assert!(t.tx("alice", "bet", seed.to_vec(), 20 * FANG).ok());
    // The free bankroll is now 370: 18.5 a bet.
    assert_eq!(abort_code(&t.tx("bob", "bet", seed.to_vec(), 18 * FANG + FANG / 2 + 1)), Some(48));
    assert!(t.tx("bob", "bet", seed.to_vec(), 18 * FANG + FANG / 2).ok());

    // With a divisor of 1 the bankroll itself is the bound: a bet's 1.5 × must fit it.
    let mut t = Table::bare();
    assert!(t.tx("house", "set_limits", limits_input(&[(DIVISOR, 1)]), 0).ok());
    assert!(t.tx("house", "fund", vec![], 15 * FANG).ok());
    t.commit(0, 8);
    assert_eq!(abort_code(&t.tx("alice", "bet", seed.to_vec(), 10 * FANG + 2)), Some(49), "15 FANG can't cover 1.5 × this");
    assert!(t.tx("alice", "bet", seed.to_vec(), 10 * FANG).ok(), "exactly covered");
    assert_eq!(t.house().bankroll, 0);
    assert_eq!(abort_code(&t.tx("bob", "bet", seed.to_vec(), FANG)), Some(48), "nothing free");

    // The house's exposure limit, over every open hand.
    let mut t = Table::new();
    assert!(t.tx("house", "set_limits", limits_input(&[(MAX_EXPOSURE, 100 * FANG)]), 0).ok());
    assert!(t.tx("alice", "bet", seed.to_vec(), 50 * FANG).ok(), "75 of 100");
    assert_eq!(abort_code(&t.tx("bob", "bet", seed.to_vec(), 17 * FANG)), Some(69), "75 + 25.5");
    assert!(t.tx("bob", "bet", seed.to_vec(), 16 * FANG).ok(), "75 + 24");

    // The table's open hands.
    let mut t = Table::new();
    assert!(t.tx("house", "set_limits", limits_input(&[(MAX_OPEN, 2)]), 0).ok());
    assert!(t.tx("alice", "bet", seed.to_vec(), FANG).ok());
    assert!(t.tx("bob", "bet", seed.to_vec(), FANG).ok());
    assert_eq!(abort_code(&t.tx("carol", "bet", seed.to_vec(), FANG)), Some(51));
    // A settled hand frees its seat.
    t.play_out("alice", 0, &seed, never);
    assert!(t.tx("carol", "bet", seed.to_vec(), FANG).ok());

    // No commitment ready.
    let mut t = Table::bare();
    assert!(t.tx("house", "fund", vec![], 100 * FANG).ok());
    assert_eq!(abort_code(&t.tx("alice", "bet", seed.to_vec(), FANG)), Some(53));
    t.commit(0, 1);
    assert!(t.tx("alice", "bet", seed.to_vec(), FANG).ok());
    assert_eq!(abort_code(&t.tx("bob", "bet", seed.to_vec(), FANG)), Some(53), "the only one is taken");
}

#[test]
fn the_daily_cap_counts_an_accounts_wagers_and_resets_with_the_day() {
    let mut t = Table::new();
    assert!(t.tx("house", "set_limits", limits_input(&[(DAILY_CAP, 120 * FANG), (DAY_BLOCKS, 1_000)]), 0).ok());
    t.commit(16, 48);
    let seed = [6; 32];
    t.host.height = 5_100;
    for _ in 0..2 {
        let id = t.bet("alice", 50 * FANG, &seed);
        t.play_out("alice", id, &seed, never);
    }
    assert_eq!(abort_code(&t.tx("alice", "bet", seed.to_vec(), 20 * FANG + 1)), Some(50), "100 wagered, 20 left");
    // Another account has its own day.
    assert!(t.tx("bob", "bet", seed.to_vec(), 50 * FANG).ok());
    let id = t.bet("alice", 20 * FANG, &seed);
    t.play_out("alice", id, &seed, never);
    assert_eq!(abort_code(&t.tx("alice", "bet", seed.to_vec(), FANG)), Some(50), "the cap is reached, win or lose");
    assert_eq!(u64_at(&t.player("alice"), 8), 5, "day 5");
    assert_eq!(u64_at(&t.player("alice"), 16), 120 * FANG);
    // The last block of the day, then the first of the next.
    t.host.height = 5_999;
    assert_eq!(abort_code(&t.tx("alice", "bet", seed.to_vec(), FANG)), Some(50));
    t.host.height = 6_000;
    let id = t.bet("alice", 50 * FANG, &seed);
    assert_eq!(u64_at(&t.player("alice"), 8), 6);
    assert_eq!(u64_at(&t.player("alice"), 16), 50 * FANG);
    t.play_out("alice", id, &seed, never);
}

#[test]
fn a_pause_stops_new_bets_and_nothing_else() {
    let mut t = Table::new();
    let seed = seed_for(0, below_17, |_, h| h.n_player >= 3);
    assert!(t.tx("house", "approve_relayer", [&addr("bot")[..], &[1]].concat(), 0).ok());
    assert!(t.tx("bob", "deposit", vec![], 20 * FANG).ok());
    let id = t.bet("alice", FANG, &seed);
    let r = t.tx("house", "pause", vec![1], 0);
    assert_eq!(event(&r, "paused"), [1]);
    assert_eq!(t.house().paused, 1);
    assert_eq!(abort_code(&t.tx("carol", "bet", seed.to_vec(), FANG)), Some(46));
    let grant = [&addr("bot")[..], &le(5 * FANG), &le(10 * FANG), &le(t.host.height + 100)].concat();
    assert!(t.tx("bob", "set_relayer", grant, 0).ok(), "a grant isn't a bet");
    assert_eq!(abort_code(&t.tx("bot", "relay_bet", [&addr("bob")[..], &le(FANG), &le(1)].concat(), 0)), Some(46));
    // The open hand plays on to its settlement and its payout.
    let r = t.play_out("alice", id, &seed, below_17);
    assert_eq!(r.returns[0][0], Phase::Done as u8);
    // Deposits, withdrawals, the bankroll and commitments still move.
    assert!(t.tx("bob", "deposit", vec![], FANG).ok());
    assert!(t.tx("bob", "withdraw", le(21 * FANG), 0).ok());
    assert!(t.tx("house", "fund", vec![], FANG).ok());
    assert!(t.tx("house", "defund", le(FANG), 0).ok());
    t.commit(16, 4);
    // A hand open at the pause can still time out.
    assert!(t.tx("house", "pause", vec![0], 0).ok());
    let id = t.bet("carol", FANG, &seed);
    assert!(t.tx("house", "pause", vec![1], 0).ok());
    t.host.height += 151;
    let r = t.tx("stranger", "timeout", le(id), 0);
    assert_eq!(r.returns[0], [Phase::Done as u8, Outcome::ForfeitDeal as u8]);
    assert!(t.tx("house", "pause", vec![0], 0).ok());
    assert!(t.tx("carol", "bet", seed.to_vec(), FANG).ok());
}

// ---------------------------------------------------------------------------------- timeouts

#[test]
fn a_dealer_that_misses_a_reveal_pays_the_player() {
    let stake = 10 * FANG;
    // (what, the seed's shape, hits before the stall, links revealed before it, the outcome)
    type Shape = fn(Outcome, &Hand) -> bool;
    let cases: [(&str, Shape, bool, usize, bool, Outcome); 5] = [
        ("never dealt", |_, _| true, false, 0, false, Outcome::ForfeitDeal),
        ("dealt two of three", |_, _| true, false, 2, false, Outcome::ForfeitDeal),
        ("a natural waiting on the hole card", |_, h| bj_core::natural(h.player_cards()) && h.n_dealer == 2, false, 3, false, Outcome::ForfeitDeal),
        ("a hit never answered", |_, h| !bj_core::natural(h.player_cards()), true, 3, false, Outcome::Forfeit),
        ("a stand never answered", |_, h| !bj_core::natural(h.player_cards()), false, 3, true, Outcome::Forfeit),
    ];
    for (what, shape, hit, revealed, stand, outcome) in cases {
        let mut t = Table::new();
        let seed = seed_for(0, never, shape);
        t.host.height = 1_000;
        let (alice, house) = (t.balance("alice"), t.house());
        let id = t.bet("alice", stake, &seed);
        if revealed > 0 {
            assert!(t.reveal(id, 1, revealed).ok());
        }
        if hit {
            t.host.height += 3;
            assert!(t.tx("alice", "hit", le(id), 0).ok());
        }
        if stand {
            t.host.height += 3;
            assert!(t.tx("alice", "stand", le(id), 0).ok());
        }
        let deadline = u64_at(&t.game(id).unwrap(), 104);
        assert_eq!(deadline, t.host.height + 150, "{what}");
        // On the deadline's own block it isn't late.
        t.host.height = deadline;
        assert_eq!(abort_code(&t.tx("alice", "timeout", le(id), 0)), Some(60), "{what}");
        t.host.height = deadline + 1;
        // Anyone calls it; the player is paid.
        let r = t.tx("stranger", "timeout", le(id), 0);
        assert_eq!(r.returns[0], [Phase::Done as u8, outcome as u8], "{what}");
        assert_eq!(event_names(&r), ["settled"]);
        let pays = if outcome == Outcome::ForfeitDeal { 25 * FANG } else { 20 * FANG };
        assert_eq!(u64_at(event(&r, "settled"), 49), pays, "{what}");
        assert_eq!(t.balance("alice"), alice - stake + pays, "{what}");
        assert_eq!(t.house(), House { bankroll: house.bankroll + stake - pays, next_game: 1, ..house }, "{what}: paid from the reserve");
        assert_eq!(abort_code(&t.tx("stranger", "timeout", le(id), 0)), Some(54), "once");
        assert_eq!(t.player("alice")[112], outcome as u8);
    }
}

#[test]
fn a_late_reveal_still_counts_until_someone_calls_the_timeout() {
    let mut t = Table::new();
    let seed = seed_for(0, never, |_, h| !bj_core::natural(h.player_cards()));
    let id = t.bet("alice", FANG, &seed);
    t.host.height += 500;
    let r = t.reveal(id, 1, 3);
    assert_eq!(r.returns[0], [Phase::Player as u8, 0]);
    assert_eq!(abort_code(&t.tx("alice", "timeout", le(id), 0)), Some(60), "the player's own clock started at the deal");
}

#[test]
fn an_idle_player_stands_and_the_dealer_finishes() {
    let mut t = Table::new();
    let seed = seed_for(0, never, |_, h| !bj_core::natural(h.player_cards()));
    t.host.height = 2_000;
    let id = t.bet("alice", 5 * FANG, &seed);
    t.host.height = 2_010;
    assert!(t.reveal(id, 1, 3).ok());
    assert_eq!(u64_at(&t.game(id).unwrap(), 104), 2_010 + 1_800);
    t.host.height = 3_810;
    assert_eq!(abort_code(&t.tx("dealer", "timeout", le(id), 0)), Some(60), "1,800 blocks aren't over");
    t.host.height = 3_811;
    let r = t.tx("dealer", "timeout", le(id), 0);
    assert_eq!(r.returns[0], [Phase::Dealer as u8, 0]);
    assert_eq!(event(&r, "stand"), [&le(id)[..], &addr("alice"), &le(3_811 + 150), &[1]].concat());
    assert_eq!(abort_code(&t.tx("alice", "hit", le(id), 0)), Some(56), "the hand stood");
    assert_eq!(abort_code(&t.tx("dealer", "timeout", le(id), 0)), Some(60), "now the dealer has its 150 blocks");
    // The dealer plays it out as after any stand.
    let (used, outcome, _) = play(&deck(id, &seed), never);
    let r = t.reveal(id, 4, used - 3);
    assert_eq!(r.returns[0], [Phase::Done as u8, outcome as u8]);

    // And if the dealer then stalls too, the player wins.
    let seed = seed_for(1, never, |_, h| !bj_core::natural(h.player_cards()));
    let alice = t.balance("alice");
    let id = t.bet("alice", 5 * FANG, &seed);
    assert!(t.reveal(id, 1, 3).ok());
    t.host.height += 1_801;
    assert!(t.tx("stranger", "timeout", le(id), 0).ok());
    t.host.height += 151;
    let r = t.tx("stranger", "timeout", le(id), 0);
    assert_eq!(r.returns[0], [Phase::Done as u8, Outcome::Forfeit as u8]);
    assert_eq!(t.balance("alice"), alice + 5 * FANG);
    assert_eq!(abort_code(&t.tx("stranger", "timeout", vec![0; 9], 0)), Some(1));
    assert_eq!(trap(&t.tx("stranger", "timeout", le(9), 1)), Some(TrapKind::NotPayable));
}

// ---------------------------------------------------------------------------------- table balances

fn approve(t: &mut Table, relayer: &str, on: bool) {
    let r = t.tx("house", "approve_relayer", [&addr(relayer)[..], &[on as u8]].concat(), 0);
    assert_eq!(event(&r, "relayer"), [&addr(relayer)[..], &[on as u8]].concat());
}

fn grant_input(relayer: &str, max_bet: u64, daily: u64, expires: u64) -> Vec<u8> {
    [&addr(relayer)[..], &le(max_bet), &le(daily), &le(expires)].concat()
}

fn relay_bet(t: &mut Table, relayer: &str, player: &str, stake: u64, post: u64) -> Receipt {
    t.tx(relayer, "relay_bet", [&addr(player)[..], &le(stake), &le(post)].concat(), 0)
}

/// What a relayed bet's seed must be.
fn relay_seed(player: &str, post: u64) -> [u8; 32] {
    *blake3::hash(&[&b"blockula/blackjack/relay"[..], &addr(player), &le(post)].concat()).as_bytes()
}

#[test]
fn a_table_balance_is_its_depositors() {
    let mut t = Table::new();
    let alice = t.balance("alice");
    assert_eq!(abort_code(&t.tx("alice", "deposit", vec![], 0)), Some(44));
    assert_eq!(abort_code(&t.tx("alice", "deposit", vec![1], FANG)), Some(1));
    let r = t.tx("alice", "deposit", vec![], 150 * FANG);
    assert_eq!(event(&r, "deposit"), [&addr("alice")[..], &le(150 * FANG), &le(150 * FANG)].concat());
    assert_eq!(abort_code(&t.tx("alice", "deposit", vec![], 50 * FANG + 1)), Some(62), "past the 200 FANG cap");
    assert!(t.tx("alice", "deposit", vec![], 50 * FANG).ok(), "the cap itself");
    assert_eq!((t.table_balance("alice"), t.house().table, t.balance("alice")), (200 * FANG, 200 * FANG, alice - 200 * FANG));
    // The bankroll isn't touched, and a table balance isn't the owner's to take.
    assert_eq!(t.house().bankroll, 2_000 * FANG);
    assert_eq!(abort_code(&t.tx("house", "defund", le(2_000 * FANG + 1), 0)), Some(63));
    // Nobody else withdraws it: a withdrawal is from the caller's own balance, to the caller.
    for who in ["bob", "house", "dealer", "bot"] {
        assert_eq!(abort_code(&t.tx(who, "withdraw", le(FANG), 0)), Some(63), "{who} has no table balance");
    }
    assert_eq!(abort_code(&t.tx("alice", "withdraw", le(200 * FANG + 1), 0)), Some(63));
    assert_eq!(abort_code(&t.tx("alice", "withdraw", le(0), 0)), Some(63));
    assert_eq!(trap(&t.tx("alice", "withdraw", le(FANG), 1)), Some(TrapKind::NotPayable));
    let r = t.tx("alice", "withdraw", le(120 * FANG), 0);
    assert_eq!(event(&r, "withdraw"), [&addr("alice")[..], &le(120 * FANG), &le(80 * FANG)].concat());
    assert_eq!((t.table_balance("alice"), t.house().table, t.balance("alice")), (80 * FANG, 80 * FANG, alice - 80 * FANG));
    assert!(t.tx("alice", "withdraw", le(80 * FANG), 0).ok());
    assert_eq!((t.table_balance("alice"), t.balance("alice"), t.balance(PROGRAM)), (0, alice, 2_000 * FANG));
}

#[test]
fn a_grant_names_an_approved_relayer_within_the_tables_limits() {
    let mut t = Table::new();
    t.host.height = 500;
    let ok = grant_input("bot", 25 * FANG, 100 * FANG, 500 + 43_200);
    assert_eq!(abort_code(&t.tx("alice", "set_relayer", ok.clone(), 0)), Some(64), "not approved yet");
    approve(&mut t, "bot", true);
    for (bad, why) in [
        (grant_input("bot", 25 * FANG + 1, 100 * FANG, 600), "above the relayed maximum"),
        (grant_input("bot", 0, 100 * FANG, 600), "a zero maximum"),
        (grant_input("bot", 10 * FANG, 10 * FANG - 1, 600), "a daily cap below one bet"),
        (grant_input("bot", 10 * FANG, u64::MAX, 600), "a daily cap past the supply"),
        (grant_input("bot", 10 * FANG, 100 * FANG, 500), "expiring now"),
        (grant_input("bot", 10 * FANG, 100 * FANG, 499), "expired"),
        (grant_input("bot", 10 * FANG, 100 * FANG, 500 + 43_201), "longer than 43,200 blocks"),
    ] {
        assert_eq!(abort_code(&t.tx("alice", "set_relayer", bad, 0)), Some(67), "{why}");
    }
    assert_eq!(abort_code(&t.tx("alice", "set_relayer", grant_input("stranger", FANG, FANG, 600), 0)), Some(64));
    assert_eq!(abort_code(&t.tx("alice", "set_relayer", vec![0; 55], 0)), Some(1));
    let r = t.tx("alice", "set_relayer", ok.clone(), 0);
    assert_eq!(event(&r, "grant"), [&addr("alice")[..], &ok].concat());
    assert_eq!(t.player("alice")[32..104], [&ok[..], &le(0), &le(0)].concat());
    // A grant needs no deposit, and a new one replaces the old.
    let smaller = grant_input("bot", 5 * FANG, 5 * FANG, 600);
    assert!(t.tx("alice", "set_relayer", smaller.clone(), 0).ok());
    assert_eq!(t.player("alice")[32..88], smaller);
    let r = t.tx("alice", "revoke", vec![], 0);
    assert_eq!(event(&r, "revoke"), [addr("alice"), addr("bot")].concat());
    assert_eq!(t.player("alice")[32..104], [0u8; 72]);
    assert_eq!(abort_code(&t.tx("alice", "revoke", vec![1], 0)), Some(1));
}

#[test]
fn a_relayer_plays_a_players_table_balance_within_the_grant() {
    let mut t = Table::new();
    approve(&mut t, "bot", true);
    t.host.height = 1_000;
    assert!(t.tx("alice", "deposit", vec![], 60 * FANG).ok());
    let post = 1_859_000_000_000_000_001u64;

    // Without a grant, nobody relays: not the approved relayer, not the player, not the owner.
    for who in ["bot", "alice", "house", "dealer", "stranger"] {
        assert_eq!(abort_code(&relay_bet(&mut t, who, "alice", FANG, post)), Some(64), "{who} without a grant");
    }
    assert!(t.tx("alice", "set_relayer", grant_input("bot", 10 * FANG, 25 * FANG, 1_500), 0).ok());
    for who in ["alice", "house", "dealer", "stranger"] {
        assert_eq!(abort_code(&relay_bet(&mut t, who, "alice", FANG, post)), Some(64), "{who} isn't the relayer");
    }
    assert_eq!(abort_code(&relay_bet(&mut t, "bot", "bob", FANG, post)), Some(64), "bob granted nothing");
    assert_eq!(abort_code(&relay_bet(&mut t, "bot", "alice", 0, post)), Some(44));
    assert_eq!(abort_code(&relay_bet(&mut t, "bot", "alice", 10 * FANG + 1, post)), Some(66), "above the grant's bet");
    assert_eq!(abort_code(&relay_bet(&mut t, "bot", "alice", FANG - 1, post)), Some(47), "the table's minimum still holds");
    assert_eq!(trap(&t.tx("bot", "relay_bet", [&addr("alice")[..], &le(FANG), &le(post)].concat(), 1)), Some(TrapKind::NotPayable));

    // A relayed bet: the stake leaves the table balance, the seed is the post's.
    let (alice, bot) = (t.balance("alice"), t.balance("bot"));
    let r = relay_bet(&mut t, "bot", "alice", 10 * FANG, post);
    assert_eq!(r.returns[0], le(0));
    let seed = relay_seed("alice", post);
    assert_eq!(event(&r, "bet"), [&le(0)[..], &addr("alice"), &le(10 * FANG), &seed, &chain_of(0)[0], &le(1_150), &[1], &le(post)].concat());
    assert_eq!(event(&r, "relayed"), [&addr("alice")[..], &addr("bot"), &le(post), &[1], &le(0)].concat());
    assert_eq!(t.table_balance("alice"), 50 * FANG);
    assert_eq!(t.house(), House { bankroll: 1_985 * FANG, reserved: 15 * FANG, stakes: 10 * FANG, table: 50 * FANG, next_commit: 16, next_game: 1, paused: 0, open: 1 });
    assert_eq!((t.balance("alice"), t.balance("bot")), (alice, bot), "no account moved");
    let g = t.game(0).unwrap();
    assert_eq!((&g[40..72], u64_at(&g, 112), g[121]), (&seed[..], post, 1));

    // The moves: the relayer's with the post id, or the player's own.
    assert!(t.reveal(0, 1, 3).ok());
    let natural = t.game(0).is_none() || t.game(0).unwrap()[120] != Phase::Player as u8;
    assert!(!natural, "pick another post id: this one deals a natural");
    assert_eq!(abort_code(&t.tx("stranger", "relay_hit", [le(0), le(post + 1)].concat(), 0)), Some(64));
    assert_eq!(abort_code(&t.tx("alice", "relay_hit", [le(0), le(post + 1)].concat(), 0)), Some(64));
    assert_eq!(abort_code(&t.tx("bot", "relay_hit", [le(3), le(post + 1)].concat(), 0)), Some(54));
    assert_eq!(abort_code(&t.tx("bot", "hit", le(0), 0)), Some(55), "the plain move is the player's");
    let r = t.tx("bot", "relay_stand", [le(0), le(post + 2)].concat(), 0);
    assert_eq!(event_names(&r), ["stand", "relayed"]);
    assert_eq!(event(&r, "relayed"), [&addr("alice")[..], &addr("bot"), &le(post + 2), &[3], &le(0)].concat());
    assert_eq!(abort_code(&t.tx("bot", "relay_stand", [le(0), le(post + 3)].concat(), 0)), Some(56));
    // The payout goes back to the table balance, never to an account.
    let (used, outcome, _) = play(&deck(0, &seed), never);
    let r = t.reveal(0, 4, used - 3);
    assert_eq!(r.returns[0], [Phase::Done as u8, outcome as u8]);
    let pays = payout(10 * FANG, outcome);
    assert_eq!(event(&r, "settled")[57], (pays > 0) as u8, "paid to the table balance");
    assert_eq!(t.table_balance("alice"), 50 * FANG + pays);
    assert_eq!((t.balance("alice"), t.balance("bot")), (alice, bot));

    // The grant's daily cap: 25 FANG, 10 used.
    let r = relay_bet(&mut t, "bot", "alice", 10 * FANG, post + 10);
    assert!(r.ok(), "{:?}", r.fail);
    // The player moves their own table hand from their account.
    let seed = relay_seed("alice", post + 10);
    assert!(t.reveal(1, 1, 3).ok());
    if t.game(1).is_some_and(|g| g[120] == Phase::Player as u8) {
        assert_eq!(abort_code(&t.tx("bot", "relay_hit", [le(1), le(0)].concat(), 1)).or(Some(0)), Some(0));
        assert!(t.tx("alice", "stand", le(1), 0).ok());
        let (used, _, _) = play(&deck(1, &seed), never);
        assert!(t.reveal(1, 4, used - 3).ok());
    }
    assert_eq!(abort_code(&relay_bet(&mut t, "bot", "alice", 5 * FANG + 1, post + 11)), Some(66), "20 of 25 used today");
    assert_eq!(u64_at(&t.player("alice"), 96), 20 * FANG);
    // More than the balance.
    assert!(t.tx("alice", "withdraw", le(t.table_balance("alice") - 2 * FANG), 0).ok());
    assert_eq!(abort_code(&relay_bet(&mut t, "bot", "alice", 3 * FANG, post + 12)), Some(63));
    // The next day the grant's count starts again; past its expiry it's over.
    t.host.height = 43_200;
    assert_eq!(abort_code(&relay_bet(&mut t, "bot", "alice", FANG, post + 13)), Some(65), "expired at 1,500");
    assert!(t.tx("alice", "set_relayer", grant_input("bot", 10 * FANG, 25 * FANG, 43_300), 0).ok());
    let r = relay_bet(&mut t, "bot", "alice", 2 * FANG, post + 14);
    assert!(r.ok(), "{:?}", r.fail);
    let id = u64_at(&r.returns[0], 0);
    assert_eq!(u64_at(&t.player("alice"), 96), 2 * FANG);
    // An expired or revoked grant moves nothing: the hand is the player's, or stands by timeout.
    assert!(t.reveal(id, 1, 3).ok());
    if t.game(id).is_some_and(|g| g[120] == Phase::Player as u8) {
        t.host.height = 43_301;
        assert_eq!(abort_code(&t.tx("bot", "relay_hit", [le(id), le(1)].concat(), 0)), Some(65));
        assert!(t.tx("alice", "revoke", vec![], 0).ok());
        assert_eq!(abort_code(&t.tx("bot", "relay_stand", [le(id), le(1)].concat(), 0)), Some(64));
        assert!(t.tx("alice", "hit", le(id), 0).ok());
    }
}

#[test]
fn a_relayer_can_send_a_balance_nowhere_but_back_to_its_depositor() {
    let mut t = Table::new();
    approve(&mut t, "bot", true);
    assert!(t.tx("alice", "deposit", vec![], 40 * FANG).ok());
    assert!(t.tx("alice", "set_relayer", grant_input("bot", 5 * FANG, 20 * FANG, t.host.height + 1_000), 0).ok());
    let (alice, bot) = (t.balance("alice"), t.balance("bot"));
    // The relayer has no balance of its own to withdraw, and `withdraw` takes no account.
    assert_eq!(abort_code(&t.tx("bot", "withdraw", le(FANG), 0)), Some(63));
    assert_eq!(abort_code(&t.tx("stranger", "relay_withdraw", [&addr("alice")[..], &le(7)].concat(), 0)), Some(64));
    assert_eq!(abort_code(&t.tx("bot", "relay_withdraw", [&addr("bob")[..], &le(7)].concat(), 0)), Some(64));
    let r = t.tx("bot", "relay_withdraw", [&addr("alice")[..], &le(7)].concat(), 0);
    assert_eq!(event(&r, "withdraw"), [&addr("alice")[..], &le(40 * FANG), &le(0)].concat());
    assert_eq!(event(&r, "relayed"), [&addr("alice")[..], &addr("bot"), &le(7), &[4], &le(0)].concat());
    assert_eq!((t.balance("alice"), t.balance("bot"), t.table_balance("alice")), (alice + 40 * FANG, bot, 0));
    assert_eq!(abort_code(&t.tx("bot", "relay_withdraw", [&addr("alice")[..], &le(8)].concat(), 0)), Some(63), "nothing left");

    // The owner ends every grant to a relayer at once.
    assert!(t.tx("alice", "deposit", vec![], 40 * FANG).ok());
    approve(&mut t, "bot", false);
    assert_eq!(abort_code(&relay_bet(&mut t, "bot", "alice", FANG, 9)), Some(64));
    assert_eq!(abort_code(&t.tx("bot", "relay_withdraw", [&addr("alice")[..], &le(9)].concat(), 0)), Some(64));
    assert!(t.tx("alice", "withdraw", le(40 * FANG), 0).ok(), "the player's own way out never closes");
    // A relayed hand can't be moved by a relayer on a hand bet from an account.
    approve(&mut t, "bot", true);
    let seed = seed_for(0, never, |_, h| !bj_core::natural(h.player_cards()));
    let id = t.bet("alice", FANG, &seed);
    assert!(t.reveal(id, 1, 3).ok());
    assert_eq!(abort_code(&t.tx("bot", "relay_hit", [le(id), le(1)].concat(), 0)), Some(68));
    assert_eq!(abort_code(&t.tx("bot", "relay_stand", [le(id), le(1)].concat(), 0)), Some(68));
}

#[test]
fn a_payout_that_cant_be_sent_becomes_table_balance() {
    let mut t = Table::new();
    let seed = seed_for(0, never, |o, h| o == Outcome::Win && !bj_core::natural(h.player_cards()));
    let id = t.bet("alice", 5 * FANG, &seed);
    // The account is gone by the time the hand settles (emptied and closed).
    t.host.balances.remove(&addr("alice"));
    let r = t.play_out_as_dealer(id, &seed);
    assert_eq!(r.returns[0], [Phase::Done as u8, Outcome::Win as u8]);
    assert_eq!(event(&r, "settled")[57], 1, "paid to the table balance");
    assert_eq!(t.table_balance("alice"), 10 * FANG);
    assert_eq!(t.house(), House { bankroll: 1_995 * FANG, reserved: 0, stakes: 0, table: 10 * FANG, next_commit: 16, next_game: 1, paused: 0, open: 0 });
    // Once the account is back, it withdraws.
    t.host.fund(addr("alice"), FANG);
    assert!(t.tx("alice", "withdraw", le(10 * FANG), 0).ok());
    assert_eq!(t.balance("alice"), 11 * FANG);
}

impl Table {
    /// The dealer's side only, for a hand whose player stands by timeout.
    fn play_out_as_dealer(&mut self, id: u64, seed: &[u8; 32]) -> Receipt {
        assert!(self.reveal(id, 1, 3).ok());
        self.host.height += 1_801;
        assert!(self.tx("stranger", "timeout", le(id), 0).ok());
        let (used, _, _) = play(&deck(id, seed), never);
        self.reveal(id, 4, used - 3)
    }
}

#[test]
fn deposits_are_bonded_by_the_bettor_and_come_back_when_the_hand_settles() {
    let mut t = Table::bare();
    t.host.storage_price = 100_000;
    t.storage_limit = FANG;
    assert!(t.tx("house", "fund", vec![], 1_000 * FANG).ok());
    t.commit(0, 4);
    let seed = seed_for(0, never, |o, h| o == Outcome::Push && !bj_core::natural(h.player_cards()));
    let (alice, dealer, escrow) = (t.balance("alice"), t.balance("dealer"), t.host.escrow);
    let id = t.bet("alice", 2 * FANG, &seed);
    let bonded = alice - 2 * FANG - t.balance("alice");
    assert!(bonded > 0, "the bet bonds the hand's record and the player's");
    // The commitment the bet took was the dealer's deposit: it's back.
    assert!(t.balance("dealer") > dealer);
    // The dealer's reveals and a timeout run with a storage limit of 0 (`Table::tx`): they bond
    // nothing. The hand's deposit goes back to the bettor at settlement.
    let before = t.balance("alice");
    let r = t.play_out_as_dealer(id, &seed);
    assert_eq!(r.returns[0][1], Outcome::Push as u8);
    let refund = t.balance("alice") - before - 2 * FANG;
    assert!(refund > 0 && refund < bonded, "the hand's record is refunded; the player's record stays");
    assert_eq!(t.host.escrow, escrow - (t.balance("dealer") - dealer) as u128 + (bonded - refund) as u128);
    // A second hand bonds only the hand's record, and gets all of it back.
    let seed = seed_for(1, never, |o, h| o == Outcome::Push && !bj_core::natural(h.player_cards()));
    let before = t.balance("alice");
    let id = t.bet("alice", 2 * FANG, &seed);
    t.play_out_as_dealer(id, &seed);
    assert_eq!(t.balance("alice"), before);
    // A bet whose signer consents to no storage fails whole.
    t.storage_limit = 0;
    assert_eq!(trap(&t.tx("bob", "bet", [3; 32].to_vec(), FANG)), Some(TrapKind::StorageLimit));
}

// ---------------------------------------------------------------------------------- views

#[test]
fn views_read_and_change_nothing() {
    let mut t = Table::new();
    let house = t.view_bytes("house", vec![]);
    assert_eq!(house.len(), HOUSE + 16);
    assert_eq!(house[..HOUSE], t.stored(b"H").unwrap()[..]);
    assert_eq!(u64_at(&house, HOUSE), t.balance(PROGRAM));
    assert_eq!(u64_at(&house, HOUSE + 8), t.host.height);
    assert_eq!(t.view_bytes("player", addr("nobody").to_vec())[..PLAYER], [0u8; PLAYER], "an account that never played");
    for id in 0..3 {
        t.bet(["alice", "bob", "carol"][id], FANG, &[id as u8; 32]);
    }
    assert_eq!(t.view("open_games", vec![]), [&3u32.to_le_bytes()[..], &le(0), &le(1), &le(2)].concat());
    t.play_out("bob", 1, &[1; 32], never);
    assert_eq!(t.view("open_games", vec![]), [&2u32.to_le_bytes()[..], &le(0), &le(2)].concat(), "the last takes the freed slot");
    for (method, input) in [("game", vec![0; 7]), ("player", vec![0; 31]), ("house", vec![1]), ("limits", vec![1]), ("open_games", vec![1]), ("commitment", vec![])] {
        assert_eq!(abort_code(&t.tx("stranger", method, input, 0)), Some(1), "{method}");
    }
}

// ---------------------------------------------------------------------------------- the soak

/// A deterministic stream of numbers.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 += 1;
        u64::from_le_bytes(blake3::hash(&self.0.to_le_bytes()).as_bytes()[..8].try_into().unwrap())
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// 400 hands by four players, some from their accounts and some through the relayer, with idle
/// players, stalled dealers, a pause and a limit change along the way. The invariant is checked
/// after every call (`Table::tx`); at the end every FANG is accounted for.
#[test]
fn a_soak_keeps_the_invariant_and_every_drop() {
    let mut t = Table::bare();
    assert!(t.tx("house", "fund", vec![], 3_000 * FANG).ok());
    assert!(t.tx("house", "set_limits", limits_input(&[(DAILY_CAP, 100_000 * FANG), (DAY_BLOCKS, 5_000), (DEPOSIT_CAP, 400 * FANG), (GRANT_BLOCKS, 1 << 30)]), 0).ok());
    approve(&mut t, "bot", true);
    let players = ["alice", "bob", "carol", "stranger"];
    let relayed = ["carol", "stranger"];
    for who in relayed {
        assert!(t.tx(who, "deposit", vec![], 300 * FANG).ok());
        assert!(t.tx(who, "set_relayer", grant_input("bot", 25 * FANG, 50_000 * FANG, 1 << 29), 0).ok());
    }
    let everyone: u128 = t.host.total();
    let mut rng = Rng(7);
    let mut outcomes: BTreeMap<u8, u32> = BTreeMap::new();
    let mut committed = 0;
    let (mut wagered, mut paid) = (0u128, 0u128);
    for n in 0..400u64 {
        // The dealer commits ahead of demand.
        if t.house().next_commit - t.house().next_game < 2 {
            t.commit(committed, 16);
            committed += 16;
        }
        t.host.height += 1 + rng.below(3);
        let who = players[rng.below(4) as usize];
        let stake = FANG + rng.below(24 * FANG);
        let post = 1_000_000 + n;
        let via_bot = relayed.contains(&who);
        if via_bot && t.table_balance(who) < stake {
            // Out of table balance: the player tops up, to the cap at most.
            let room = 400 * FANG - t.table_balance(who);
            assert!(t.tx(who, "deposit", vec![], room.min(100 * FANG)).ok());
        }
        let (id, seed) = if via_bot {
            let r = relay_bet(&mut t, "bot", who, stake, post);
            assert!(r.ok(), "hand {n}: {:?}", r.fail);
            (u64_at(&r.returns[0], 0), relay_seed(who, post))
        } else {
            let seed = *blake3::hash(&n.to_le_bytes()).as_bytes();
            (t.bet(who, stake, &seed), seed)
        };
        wagered += stake as u128;
        let deck = deck(id, &seed);
        let mut hand = Hand::new();
        let mut next = 1;
        let quirk = rng.below(40);
        let outcome = 'hand: loop {
            // A dealer that stalls now and then: the player wins by timeout.
            if quirk == 0 && hand.dealt >= 3 || quirk == 1 && hand.dealt == 0 {
                t.host.height += 151;
                let r = t.tx("alice", "timeout", le(id), 0);
                assert!(r.ok(), "{:?}", r.fail);
                break r.returns[0][1];
            }
            let mut n_links = 0;
            let mut probe = hand;
            let mut settled = None;
            while probe.phase.dealers() {
                if let Some(Step::Settled(o)) = probe.deal(deck[next - 1 + n_links]) {
                    settled = Some(o);
                }
                n_links += 1;
            }
            // Sometimes one link at a time.
            if quirk == 2 {
                for i in 0..n_links {
                    assert!(t.reveal(id, next + i, 1).ok());
                }
            } else {
                assert!(t.reveal(id, next, n_links).ok());
            }
            next += n_links;
            hand = probe;
            if let Some(o) = settled {
                break 'hand o as u8;
            }
            // An idle player now and then: the dealer stands them.
            if quirk == 3 {
                t.host.height += 1_801;
                assert!(t.tx("dealer", "timeout", le(id), 0).ok());
                hand.stand();
                continue;
            }
            let hit = bj_core::total(hand.player_cards()).0 < 12 + rng.below(7) as u8;
            let r = if via_bot && rng.below(4) != 0 {
                t.tx("bot", if hit { "relay_hit" } else { "relay_stand" }, [le(id), le(post)].concat(), 0)
            } else {
                t.tx(who, if hit { "hit" } else { "stand" }, le(id), 0)
            };
            assert!(r.ok(), "{:?}", r.fail);
            if hit {
                hand.hit();
            } else {
                hand.stand();
            }
        };
        *outcomes.entry(outcome).or_default() += 1;
        paid += u128::from(payout(stake, Outcome::from_u8(outcome).unwrap()));
        assert_eq!(t.player(who)[112], outcome);
        assert_eq!(t.house().open, 0);
        // The owner's levers, mid-run.
        if n == 150 {
            assert!(t.tx("house", "pause", vec![1], 0).ok());
            assert_eq!(abort_code(&t.tx("alice", "bet", vec![1; 32], FANG)), Some(46));
            assert!(t.tx("house", "pause", vec![0], 0).ok());
        }
        if n == 250 {
            assert!(t.tx("house", "defund", le(100 * FANG), 0).ok());
            assert!(t.tx("house", "fund", vec![], 100 * FANG).ok());
        }
    }
    // Every player cashes out; the house takes its bankroll. Nothing is left, nothing was made.
    for who in relayed {
        let balance = t.table_balance(who);
        if balance > 0 {
            assert!(t.tx("bot", "relay_withdraw", [&addr(who)[..], &le(0)].concat(), 0).ok());
        }
    }
    let h = t.house();
    assert_eq!((h.reserved, h.stakes, h.table, h.open), (0, 0, 0, 0));
    assert_eq!(u128::from(h.bankroll), 3_000 * u128::from(FANG) + wagered - paid, "the bankroll is the stakes less the payouts");
    assert!(t.tx("house", "defund", le(h.bankroll), 0).ok());
    assert_eq!(t.balance(PROGRAM), 0);
    assert_eq!(t.host.total(), everyone, "no FANG made or lost");
    assert!((1..=7).all(|o| outcomes.contains_key(&o)), "every outcome happened: {outcomes:?}");
    println!("outcomes over 400 hands: {outcomes:?}; wagered {} FANG, paid {} FANG", wagered / u128::from(FANG), paid / u128::from(FANG));
}

/// The most fuel each method used, over a run of its own (the worst cases included: a full batch
/// of commitments, the longest reveal): every one far under `program_max_tx_fuel`.
#[test]
fn every_method_runs_far_under_the_transaction_fuel_limit() {
    let mut t = Table::new();
    approve(&mut t, "bot", true);
    t.commit(16, 64);
    assert!(t.tx("house", "set_limits", limits_input(&[(MAX_OPEN, 32)]), 0).ok());
    assert!(t.tx("house", "set_dealer", addr("dealer").to_vec(), 0).ok());
    assert!(t.tx("house", "skip_commits", 1u32.to_le_bytes().to_vec(), 0).ok());
    assert!(t.tx("house", "pause", vec![0], 0).ok());
    assert!(t.tx("house", "defund", le(FANG), 0).ok());
    assert!(t.tx("bob", "deposit", vec![], 100 * FANG).ok());
    assert!(t.tx("bob", "set_relayer", grant_input("bot", 25 * FANG, 100 * FANG, t.host.height + 10_000), 0).ok());
    assert!(t.tx("bob", "withdraw", le(FANG), 0).ok());
    // The longest reveal there is: a hit to 21 and an eleven-card dealer hand don't come up by
    // chance, so the longest found in a search stands in, and the per-link cost bounds the rest.
    let mut longest = (0, [0u8; 32]);
    for n in 0u32..3_000 {
        let seed = *blake3::hash(&n.to_le_bytes()).as_bytes();
        let (used, _, _) = play(&deck(1, &seed), never);
        if used > longest.0 {
            longest = (used, seed);
        }
    }
    let (used, seed) = longest;
    assert!(used >= 8, "a dealer hand of {used} cards");
    let id = t.bet("alice", FANG, &seed);
    assert_eq!(id, 1);
    assert!(t.reveal(id, 1, 3).ok());
    assert!(t.tx("alice", "stand", le(id), 0).ok());
    let before = FUEL.lock().unwrap().get("reveal").copied().unwrap_or(0);
    let r = t.reveal(id, 4, used - 3);
    assert!(r.ok());
    let per_link = r.fuel_used / (used as u64 - 3);
    println!("a reveal of {} links used {} fuel ({per_link} a link; the most before it {before})", used - 3, r.fuel_used);
    assert!(12 * per_link * 2 < MAX_TX_FUEL / 4, "twelve links, doubled, are under a quarter of the limit");
    // A relayed hand, a hit, a stand, a timeout, the views, a revoke. The post id is one whose
    // hand takes a hit without ending.
    let next = t.house().next_game;
    let takes_a_hit = |post: &u64| {
        let (_, _, hand) = play(&deck(next, &relay_seed("bob", *post)), |h| h.n_player == 2);
        hand.n_player == 3 && bj_core::total(hand.player_cards()).0 < 21
    };
    let post = (77u64..).find(takes_a_hit).unwrap();
    let r = relay_bet(&mut t, "bot", "bob", FANG, post);
    assert!(r.ok(), "{:?}", r.fail);
    let id = u64_at(&r.returns[0], 0);
    assert!(t.reveal(id, 1, 3).ok());
    assert!(t.tx("bot", "relay_hit", [le(id), le(post + 1)].concat(), 0).ok());
    assert!(t.reveal(id, 4, 1).ok());
    assert!(t.tx("bot", "relay_stand", [le(id), le(post + 2)].concat(), 0).ok());
    t.host.height += 151;
    assert!(t.tx("stranger", "timeout", le(id), 0).ok());
    let id = t.bet("carol", FANG, &seed_for(t.house().next_game, never, |_, h| !bj_core::natural(h.player_cards())));
    assert!(t.reveal(id, 1, 3).ok());
    assert!(t.tx("carol", "hit", le(id), 0).ok());
    for (method, input) in [("game", le(id)), ("player", addr("bob").to_vec()), ("house", vec![]), ("limits", vec![]), ("open_games", vec![]), ("commitment", le(t.house().next_game))] {
        t.view(method, input);
    }
    assert!(t.tx("bot", "relay_withdraw", [&addr("bob")[..], &le(80)].concat(), 0).ok());
    assert!(t.tx("bob", "revoke", vec![], 0).ok());

    let fuel = FUEL.lock().unwrap().clone();
    let methods: Vec<String> = interface().methods.iter().map(|m| m.name.clone()).collect();
    println!("{:<16} {:>9}", "method", "most fuel");
    for m in &methods {
        let used = fuel.get(m).copied().unwrap_or_else(|| panic!("{m} never ran"));
        println!("{m:<16} {used:>9}");
        assert!(used < MAX_TX_FUEL / 4, "{m} used {used} fuel");
    }
    // What admission adds: the code's load, and a frame's instantiation is inside the numbers above.
    println!("load fuel (at admission, once per transaction): {}", SCHEDULE_V0.load_fuel(guest().len()));
}
