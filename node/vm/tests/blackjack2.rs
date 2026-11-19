//! The `blackjack2` guest (rules v2: docs/proposals/blackjack-dealer-outage.md,
//! `guests/blackjack2`) on `MemHost`, under its own interface's kinds and payability, on every
//! engine (`EngineKind::ALL`), each call run twice. `tests/blackjack.rs` holds the v1 program to
//! everything the two share; this file holds v2 to what it changes, and to the money again:
//!
//! * **A hand's new shape**: the bet, the dealer's opening links (which deal no card), the seed
//!   (which deals three), the play, with every card recomputed here with the `blake3` crate.
//! * **A dealer that never opens a hand refunds it**, to the drop, however much of the opening
//!   it revealed; a late opening link is refused; the house neither pays nor keeps anything.
//! * **A dealer that stalls after the seed pays what the cards still allow**, and that trips the
//!   breaker: no new bet until the owner allows them, while open hands play on.
//! * **An outage**: thirty bets against a dealer that is down cost the house nothing; with hands
//!   in flight it costs at most their stakes, once.
//! * **Solvency**: a full table of naturals is paid in full after the owner takes out every free
//!   drop. **Overflow**: the largest hand a chain can hold. **A settled hand** takes no call.
//! * **The invariant**, after every successful call: the program's balance is exactly
//!   `bankroll + reserved + stakes + table`, and each counter is the sum of its records.
//! * **A soak** with every ending (1 to 9), and the most fuel each method used.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use bj2_core::{CHAIN_LEN, Hand, Outcome, Phase, Step, exposure, natural, payout, total};
use blockula_vm::abi::FIRST_VERSION;
use blockula_vm::engine::{Address, EngineKind, TrapKind};
use blockula_vm::exec::{Call, Event, FailCode, Receipt, TxLimits, execute};
use blockula_vm::host::{MethodKind, View};
use blockula_vm::interface::{Interface, decode};
use blockula_vm::meter::SCHEDULE_V0;
use blockula_vm::testing::{MemHost, addr};

const FANG: u64 = 1_000_000_000;
const PROGRAM: &str = "blackjack2";
/// `program_max_tx_fuel`'s default: every call runs under it.
const MAX_TX_FUEL: u64 = 10_000_000;

// Record layouts, as the guest's doc gives them.
const HOUSE: usize = 114;
const GAME: usize = 157;
const PLAYER: usize = 195;
const LIMITS: usize = 112;

// The default deadlines.
const OPEN_BLOCKS: u64 = 150;
const SEED_BLOCKS: u64 = 300;
const REVEAL_BLOCKS: u64 = 150;
const IDLE_BLOCKS: u64 = 1_800;

// The pause's values.
const TRIPPED: u8 = 2;

fn guest() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/programs/blackjack2.wasm");
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e} (run guests/build.sh)", path.display()))
}

fn interface() -> Interface {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/programs/blackjack2.iface");
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

/// What `who`'s bet commits to for `seed`, from the documented preimage with the `blake3` crate.
fn seed_hash(who: &str, seed: &[u8; 32]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"blockula/blackjack/seed").update(&addr(who)).update(seed);
    *h.finalize().as_bytes()
}

/// The cards game `id` would deal under `seed`, all 32 of them.
fn deck(id: u64, seed: &[u8; 32]) -> Vec<u8> {
    let links = chain_of(id);
    (1..=CHAIN_LEN).map(|j| card(&links[j], seed, id, j)).collect()
}

/// A hand the dealer opened and the player's seed dealt: its state, and its outcome when the
/// three cards settled it.
fn opened(deck: &[u8]) -> (Hand, Option<Outcome>) {
    let mut hand = Hand::committed();
    for _ in 0..3 {
        hand.open_link();
    }
    match hand.seeded([deck[0], deck[1], deck[2]]).unwrap() {
        Step::Settled(o) => (hand, Some(o)),
        _ => (hand, None),
    }
}

/// A hand played by `wants` (hit?) from `deck`, `start`ing as an account's (opened) or a relayed
/// one (dealt): the links used, the outcome and the hand.
fn play_from(start: (Hand, Option<Outcome>), deck: &[u8], mut wants: impl FnMut(&Hand) -> bool) -> (usize, Outcome, Hand) {
    let (mut hand, mut settled) = start;
    loop {
        if let Some(o) = settled {
            return (hand.dealt as usize, o, hand);
        }
        if hand.phase == Phase::Player {
            if wants(&hand) {
                hand.hit();
            } else {
                hand.stand();
            }
        }
        if let Some(Step::Settled(o)) = hand.deal(deck[hand.dealt as usize]) {
            settled = Some(o);
        }
    }
}

fn play(deck: &[u8], wants: impl FnMut(&Hand) -> bool) -> (usize, Outcome, Hand) {
    play_from(opened(deck), deck, wants)
}

/// A client seed under which game `id`, bet from an account and played by `wants`, ends as
/// `accept` says.
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
    total(hand.player_cards()).0 < 17
}

/// A hand that isn't over when its first three cards are out.
fn plain(_: Outcome, hand: &Hand) -> bool {
    !natural(hand.player_cards())
}

struct Table {
    host: MemHost,
    kinds: BTreeMap<String, MethodKind>,
    /// The storage limit player and owner calls run under; the dealer's reveals, a seed's reveal
    /// and anyone's timeouts always run under 0.
    storage_limit: u64,
}

impl Table {
    /// Created and initialized, with the default limits, nothing funded or committed.
    fn bare() -> Table {
        let mut host = MemHost::new();
        // Free storage, so balances move by stakes and payouts alone.
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
        let storage_limit = if matches!(method, "reveal" | "timeout" | "reveal_seed") { 0 } else { self.storage_limit };
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
    /// records it counts. And a hand's record is in a shape its phase allows.
    fn check(&self) {
        let h = self.house();
        assert_eq!(self.balance(PROGRAM), h.bankroll + h.reserved + h.stakes + h.table, "balance = bankroll + reserved + stakes + table: {h:?}");
        assert!(h.paused <= TRIPPED);
        let (mut reserved, mut stakes, mut table, mut open, mut commits) = (0, 0, 0, vec![], 0);
        for ((program, key), entry) in &self.host.storage {
            assert_eq!(*program, addr(PROGRAM));
            match key[0] {
                b'g' => {
                    let g = &entry.value;
                    assert_eq!(g.len(), GAME);
                    let stake = u64_at(g, 32);
                    stakes += stake;
                    reserved += exposure(stake);
                    let id = u64_at(key, 1);
                    open.push(id);
                    // Its player's record names it.
                    let player: Address = g[..32].try_into().unwrap();
                    let p = self.host.storage(&addr(PROGRAM), &[&b"p"[..], &player].concat()).unwrap().value;
                    assert_eq!(u64_at(&p, 24), id + 1, "the player's open hand");
                    let (phase, dealt, n_player, n_dealer) = (g[120], g[122], g[123], g[124]);
                    match phase {
                        // Being opened, or waiting for its seed: links and no cards.
                        6 => assert!(dealt < 3 && n_player == 0 && n_dealer == 0, "a hand being opened: {dealt} links"),
                        0 => assert!(dealt == 3 && n_player == 0 && n_dealer == 0, "a hand waiting for its seed"),
                        1..=4 => assert_eq!(dealt, n_player + n_dealer, "every link is a card"),
                        other => panic!("an open hand in phase {other}"),
                    }
                    assert_eq!(g[125..].iter().filter(|c| **c >= 52).count(), 0);
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
                b'V' => assert_eq!(entry.value, [2]),
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

    /// The bet: `stake` from `who`'s account and the hash of `seed`. The dealer owes the opening.
    fn commit_bet(&mut self, who: &str, stake: u64, seed: &[u8; 32]) -> Receipt {
        self.tx(who, "bet", seed_hash(who, seed).to_vec(), stake)
    }

    /// Reveals links `from..from + n` of game `id`'s chain.
    fn reveal(&mut self, id: u64, from: usize, n: usize) -> Receipt {
        self.tx("dealer", "reveal", reveal_input(id, &chain_of(id)[from..from + n]), 0)
    }

    /// The dealer opens hand `id`: its chain's first three links.
    fn open(&mut self, id: u64) {
        let r = self.reveal(id, 1, 3);
        assert_eq!(r.returns.first().map(Vec::as_slice), Some(&[Phase::Seed as u8, 0][..]), "open: {:?}", r.fail);
    }

    fn reveal_seed(&mut self, who: &str, id: u64, seed: &[u8; 32]) -> Receipt {
        self.tx(who, "reveal_seed", [&le(id)[..], seed].concat(), 0)
    }

    /// Bets `stake` from `who`'s account, has the dealer open the hand and reveals the seed: the
    /// hand's id and the seed's receipt (the first three cards are out).
    fn bet(&mut self, who: &str, stake: u64, seed: &[u8; 32]) -> (u64, Receipt) {
        let r = self.commit_bet(who, stake, seed);
        assert!(r.ok(), "bet: {:?}", r.fail);
        let id = u64_at(&r.returns[0], 0);
        self.open(id);
        let r = self.reveal_seed(who, id, seed);
        assert!(r.ok(), "reveal_seed: {:?}", r.fail);
        (id, r)
    }

    /// Plays hand `id` on from `hand` (its cards are out and didn't settle it) as the dealer
    /// daemon and `who` would. Returns the settling receipt.
    fn finish_from(&mut self, who: &str, id: u64, seed: &[u8; 32], mut hand: Hand, wants: impl Fn(&Hand) -> bool) -> Receipt {
        let deck = deck(id, seed);
        loop {
            if hand.phase == Phase::Player {
                let hit = wants(&hand);
                let r = self.tx(who, if hit { "hit" } else { "stand" }, le(id), 0);
                assert!(r.ok(), "move: {:?}", r.fail);
                if hit {
                    hand.hit();
                } else {
                    hand.stand();
                }
            }
            // The dealer reveals everything it owes in one call.
            let from = hand.dealt as usize + 1;
            let mut settled = false;
            while hand.phase.dealers() {
                if let Some(Step::Settled(_)) = hand.deal(deck[hand.dealt as usize]) {
                    settled = true;
                }
            }
            let r = self.reveal(id, from, hand.dealt as usize + 1 - from);
            assert!(r.ok(), "reveal: {:?}", r.fail);
            if settled {
                assert_eq!(r.returns[0][0], Phase::Done as u8);
                return r;
            }
            assert_eq!(r.returns[0], [Phase::Player as u8, 0]);
        }
    }

    /// A whole hand from `who`'s account: the id and the settling receipt (the seed's own, when
    /// the first three cards settle it).
    fn play_out(&mut self, who: &str, stake: u64, seed: &[u8; 32], wants: impl Fn(&Hand) -> bool) -> (u64, Receipt) {
        let (id, r) = self.bet(who, stake, seed);
        let (hand, settled) = opened(&deck(id, seed));
        if settled.is_some() {
            return (id, r);
        }
        (id, self.finish_from(who, id, seed, hand, wants))
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
    let mut l = [FANG, 50 * FANG, 500 * FANG, 43_200, 10, 20, 750 * FANG, REVEAL_BLOCKS, IDLE_BLOCKS, 200 * FANG, 25 * FANG, 43_200, SEED_BLOCKS, OPEN_BLOCKS];
    for (i, v) in changes {
        l[*i] = *v;
    }
    l.iter().flat_map(|n| n.to_le_bytes()).collect()
}

// Indexes into the limits.
const MAX_BET: usize = 1;
const DAILY_CAP: usize = 2;
const DAY_BLOCKS: usize = 3;
const MAX_OPEN: usize = 4;
const DIVISOR: usize = 5;
const MAX_EXPOSURE: usize = 6;
const REVEAL: usize = 7;
const DEPOSIT_CAP: usize = 9;
const GRANT_BLOCKS: usize = 11;
const OPEN: usize = 13;

// A `settled` event's fields.
const S_OUTCOME: usize = 40;
const S_PAYOUT: usize = 49;
const S_TO_TABLE: usize = 57;

// ---------------------------------------------------------------------------------- the program

#[test]
fn it_uploads_under_protocol_v2_and_says_it_is_rules_v2() {
    let mut host = MemHost::new();
    host.protocol_version = FIRST_VERSION;
    host.upload(&guest()).expect("no node fault").expect("uploads under protocol v2");
    let wasm = guest();
    for name in ["blake3_v1", "epoch_v1", "beacon_v1"] {
        assert!(!wasm.windows(name.len()).any(|w| w == name.as_bytes()), "imports {name}");
    }
    assert!(wasm.len() < 64 * 1024, "{} bytes", wasm.len());
    // A different program from v1's, which this branch leaves as it was.
    let v1 = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/programs/blackjack.wasm")).unwrap();
    assert_ne!(wasm, v1);

    let mut t = Table::bare();
    assert_eq!(t.view("version", vec![]), [2]);
    assert_eq!(t.stored(b"V").unwrap(), [2]);
    assert_eq!(abort_code(&t.tx("stranger", "version", vec![0], 0)), Some(1));
    // The defaults: v1's thirteen limits and 150 blocks to open a hand.
    assert_eq!(t.stored(b"C").unwrap(), limits_input(&[]));
    assert_eq!(t.stored(b"C").unwrap().len(), LIMITS);
    let limits = t.view("limits", vec![]);
    assert_eq!((&limits[..4], &limits[4..]), (&(LIMITS as u32).to_le_bytes()[..], &limits_input(&[])[..]));
    let h = t.stored(b"H").unwrap();
    assert_eq!((&h[..32], &h[32..64], &h[64..]), (&addr("house")[..], &addr("dealer")[..], &[0u8; 50][..]));
}

#[test]
fn the_time_to_open_a_hand_is_a_limit_the_owner_sets_within_bounds() {
    let mut t = Table::new();
    assert_eq!(abort_code(&t.tx("alice", "set_limits", limits_input(&[]), 0)), Some(42));
    // v1's thirteen limits alone aren't this program's input.
    assert_eq!(abort_code(&t.tx("house", "set_limits", limits_input(&[])[..104].to_vec(), 0)), Some(1));
    for (blocks, ok) in [(0, false), (1, true), (86_400, true), (86_401, false), (1 << 41, false)] {
        let r = t.tx("house", "set_limits", limits_input(&[(OPEN, blocks)]), 0);
        if ok {
            assert_eq!(event(&r, "limits"), limits_input(&[(OPEN, blocks)]), "{blocks} blocks");
        } else {
            assert_eq!(abort_code(&r), Some(45), "{blocks} blocks: a stake never waits for the dealer without end");
        }
    }
    // The new limit is the bet's deadline.
    assert!(t.tx("house", "set_limits", limits_input(&[(OPEN, 7)]), 0).ok());
    t.host.height = 500;
    let r = t.commit_bet("alice", FANG, &[1; 32]);
    assert_eq!(u64_at(&t.game(u64_at(&r.returns[0], 0)).unwrap(), 104), 507);
}

// ---------------------------------------------------------------------------------- a hand

#[test]
fn a_hand_from_bet_to_settlement() {
    let mut t = Table::new();
    t.host.height = 1_000;
    let stake = 10 * FANG;
    let seed = seed_for(0, below_17, |o, h| o == Outcome::Win && h.n_player >= 3 && h.n_dealer >= 2);
    let (alice, house) = (t.balance("alice"), t.house());
    let links = chain_of(0);
    let cards = deck(0, &seed);

    // The bet: the stake, the seed's hash, the next commitment. The dealer owes the opening.
    let r = t.commit_bet("alice", stake, &seed);
    assert_eq!(r.returns[0], le(0));
    assert_eq!(event(&r, "bet"), [&le(0)[..], &addr("alice"), &le(stake), &seed_hash("alice", &seed), &links[0], &le(1_000 + OPEN_BLOCKS), &[0], &le(0)].concat());
    let g = t.game(0).unwrap();
    assert_eq!((g[120], g[122], g[123], g[124]), (Phase::Open as u8, 0, 0, 0));
    assert_eq!((&g[40..72], &g[72..104], u64_at(&g, 104)), (&seed_hash("alice", &seed)[..], &links[0][..], 1_150));
    assert_eq!(t.house(), House { bankroll: 1_985 * FANG, reserved: 15 * FANG, stakes: stake, next_game: 1, open: 1, ..house });

    // Nothing but the opening moves the hand: no seed (the dealer could read the cards and then
    // choose whether to open), no move, no stranger's link, no timeout yet.
    assert_eq!(abort_code(&t.reveal_seed("alice", 0, &seed)), Some(70), "a seed before the hand is opened");
    assert_eq!(abort_code(&t.tx("alice", "hit", le(0), 0)), Some(56));
    assert_eq!(abort_code(&t.tx("alice", "stand", le(0), 0)), Some(56));
    assert_eq!(abort_code(&t.tx("stranger", "reveal", reveal_input(0, &[[9; 32]]), 0)), Some(58));
    assert_eq!(abort_code(&t.tx("stranger", "reveal", reveal_input(0, &links[2..3]), 0)), Some(58), "out of order");
    assert_eq!(abort_code(&t.tx("alice", "timeout", le(0), 0)), Some(60));

    // The dealer opens: one link, which deals no card.
    let r = t.reveal(0, 1, 1);
    assert_eq!(r.returns[0], [Phase::Open as u8, 0]);
    assert_eq!(event_names(&r), ["opened"]);
    assert_eq!(event(&r, "opened"), [&le(0)[..], &addr("alice"), &[1, 1], &links[1], &[Phase::Open as u8], &le(1_150)].concat());
    let g = t.game(0).unwrap();
    assert_eq!((g[120], g[122], g[123], g[124], &g[72..104]), (Phase::Open as u8, 1, 0, 0, &links[1][..]));
    assert_eq!(g[125..], [0u8; 32], "no card is stored: none is known");
    // The same link again changes nothing.
    let r = t.reveal(0, 1, 1);
    assert_eq!((r.returns[0].as_slice(), r.events.len(), r.diff.storage.len()), (&[Phase::Open as u8, 0][..], 0, 0));

    // The rest, in a call that repeats the first: it is passed over. The player now owes the
    // seed, and has `seed_blocks` past the opening's deadline, however early the dealer opened.
    t.host.height = 1_010;
    let r = t.reveal(0, 1, 3);
    assert_eq!(r.returns[0], [Phase::Seed as u8, 0]);
    assert_eq!(event(&r, "opened"), [&le(0)[..], &addr("alice"), &[2, 2], &links[2], &links[3], &[Phase::Seed as u8], &le(1_150 + SEED_BLOCKS)].concat());
    let g = t.game(0).unwrap();
    assert_eq!((g[120], g[122], g[123], g[124], &g[72..104], u64_at(&g, 104)), (Phase::Seed as u8, 3, 0, 0, &links[3][..], 1_450));
    // A fourth link would be the player's hit card: the dealer owes none, and none is taken.
    assert_eq!(abort_code(&t.reveal(0, 4, 1)), Some(57));
    assert_eq!(abort_code(&t.reveal(0, 3, 2)), Some(57), "nor behind one the hand holds");
    assert_eq!(abort_code(&t.tx("alice", "hit", le(0), 0)), Some(56));

    // The seed: only the player's, only the one the bet committed to. It deals the three cards.
    assert_eq!(abort_code(&t.reveal_seed("bob", 0, &seed)), Some(55));
    assert_eq!(abort_code(&t.reveal_seed("alice", 0, &[7; 32])), Some(71));
    assert_eq!(abort_code(&t.tx("alice", "reveal_seed", le(0), 0)), Some(1));
    t.host.height = 1_020;
    let r = t.reveal_seed("alice", 0, &seed);
    assert_eq!(r.returns[0], [Phase::Player as u8, 0]);
    assert_eq!(event_names(&r), ["seed", "cards"]);
    assert_eq!(event(&r, "seed"), [&le(0)[..], &addr("alice"), &seed, &le(1_020 + IDLE_BLOCKS)].concat());
    let (player_total, dealer_total) = (total(&[cards[0], cards[2]]).0, total(&[cards[1]]).0);
    let dealt: Vec<u8> = (1..=3).flat_map(|j| [&links[j][..], &[cards[j - 1]]].concat()).collect();
    assert_eq!(event(&r, "cards"), [&le(0)[..], &addr("alice"), &[1, 3], &dealt, &[Phase::Player as u8, player_total, dealer_total], &le(1_020 + IDLE_BLOCKS)].concat());
    let g = t.game(0).unwrap();
    assert_eq!((g[120], g[122], g[123], g[124]), (Phase::Player as u8, 3, 2, 1));
    assert_eq!((&g[40..72], &g[125..127], g[145]), (&seed[..], &[cards[0], cards[2]][..], cards[1]));
    assert_eq!(abort_code(&t.reveal_seed("alice", 0, &seed)), Some(70), "once");

    // The rest is v1's hand.
    let (hand, _) = opened(&cards);
    let r = t.finish_from("alice", 0, &seed, hand, below_17);
    assert_eq!(r.returns[0], [Phase::Done as u8, Outcome::Win as u8]);
    assert_eq!(u64_at(event(&r, "settled"), S_PAYOUT), 2 * stake);
    assert!(!event_names(&r).contains(&"tripped"));
    assert_eq!(t.balance("alice"), alice + stake);
    assert_eq!(t.house(), House { bankroll: 1_990 * FANG, next_game: 1, ..house });
    assert_eq!(t.player("alice")[112], Outcome::Win as u8);
    assert!(t.game(0).is_none());
}

#[test]
fn every_ending_of_a_played_hand_pays_what_it_should() {
    type Shape = fn(Outcome, &Hand) -> bool;
    let cases: [(&str, Shape, Outcome); 8] = [
        ("a natural the upcard can't match, settled by the seed itself", |o, h| o == Outcome::Blackjack && h.n_dealer == 1, Outcome::Blackjack),
        ("a natural the hole card doesn't match", |o, h| o == Outcome::Blackjack && h.n_dealer == 2, Outcome::Blackjack),
        ("two naturals", |o, h| o == Outcome::Push && natural(h.player_cards()), Outcome::Push),
        ("a win", |o, _| o == Outcome::Win, Outcome::Win),
        ("a push", |o, h| o == Outcome::Push && !natural(h.player_cards()), Outcome::Push),
        ("a loss", |o, _| o == Outcome::Lose, Outcome::Lose),
        ("a bust", |o, _| o == Outcome::Bust, Outcome::Bust),
        ("21 in three cards against a natural", |o, h| o == Outcome::Lose && total(h.player_cards()).0 == 21, Outcome::Lose),
    ];
    // An odd stake: the half drop of a 3:2 rounds down, to the house.
    let stake = 7 * FANG + 1;
    for (what, shape, outcome) in cases {
        let mut t = Table::new();
        let seed = seed_for(0, below_17, shape);
        let (alice, house, everyone) = (t.balance("alice"), t.house(), t.host.total());
        let (id, r) = t.play_out("alice", stake, &seed, below_17);
        assert_eq!(r.returns[0], [Phase::Done as u8, outcome as u8], "{what}");
        let pays = payout(stake, outcome);
        let settled = event(&r, "settled");
        assert_eq!((settled[S_OUTCOME], u64_at(settled, S_PAYOUT), settled[S_TO_TABLE]), (outcome as u8, pays, 0), "{what}");
        assert_eq!(t.balance("alice"), alice - stake + pays, "{what}");
        assert_eq!(t.house(), House { bankroll: house.bankroll + stake - pays, next_game: 1, ..house }, "{what}: no hand ever trips the breaker by its cards");
        assert_eq!(t.host.total(), everyone, "{what}: no FANG made or lost");
        // A settled hand takes no call.
        for (method, input) in [("timeout", le(id)), ("hit", le(id)), ("stand", le(id)), ("reveal", reveal_input(id, &chain_of(id)[1..2])), ("reveal_seed", [&le(id)[..], &seed].concat())] {
            let signer = if method == "reveal" { "dealer" } else { "alice" };
            assert_eq!(abort_code(&t.tx(signer, method, input, 0)), Some(54), "{what}: {method} on a settled hand");
        }
        assert_eq!(t.balance("alice"), alice - stake + pays);
    }
}

// ---------------------------------------------------------------------------------- a dealer that never opens

#[test]
fn a_hand_the_dealer_never_opens_is_refunded_to_the_drop() {
    for revealed in 0..3usize {
        let mut t = Table::new();
        t.host.height = 1_000;
        let stake = 10 * FANG + 1;
        let seed = [3; 32];
        let (alice, house, everyone) = (t.balance("alice"), t.house(), t.host.total());
        let r = t.commit_bet("alice", stake, &seed);
        assert!(r.ok());
        if revealed > 0 {
            assert_eq!(t.reveal(0, 1, revealed).returns[0], [Phase::Open as u8, 0]);
        }
        assert_eq!(u64_at(&t.game(0).unwrap(), 104), 1_150, "what it revealed doesn't move its deadline");
        // On the deadline's own block it isn't late.
        t.host.height = 1_150;
        assert_eq!(abort_code(&t.tx("alice", "timeout", le(0), 0)), Some(60));
        t.host.height = 1_151;
        // Anyone calls it. The stake goes back and that is all: nothing won, nothing kept.
        let r = t.tx("stranger", "timeout", le(0), 0);
        assert_eq!(r.returns[0], [Phase::Done as u8, Outcome::Refund as u8], "{revealed} links");
        assert_eq!(event_names(&r), ["settled"], "a refund doesn't trip the breaker");
        let settled = event(&r, "settled");
        assert_eq!((settled[S_OUTCOME], u64_at(settled, 41), u64_at(settled, S_PAYOUT), settled[58], settled[59]), (9, stake, stake, revealed as u8, 0));
        assert_eq!(&settled[60..92], &seed_hash("alice", &seed), "the seed was never public: its hash");
        assert_eq!(&settled[92..124], &chain_of(0)[revealed], "the newest link revealed (the commitment when none was)");
        assert_eq!(t.balance("alice"), alice, "{revealed} links: every drop back");
        assert_eq!(t.house(), House { next_game: 1, ..house }, "the bankroll is what it was: the whole reserve returned");
        assert_eq!(t.host.total(), everyone);
        let p = t.player("alice");
        assert_eq!((u64_at(&p, 24), p[112], p[113], u64_at(&p, 115), u64_at(&p, 123)), (0, 9, revealed as u8, stake, stake));
        // Once; and the links that come too late find no hand.
        assert_eq!(abort_code(&t.tx("stranger", "timeout", le(0), 0)), Some(54));
        assert_eq!(abort_code(&t.reveal(0, 1, 3)), Some(54));
        assert_eq!(abort_code(&t.reveal_seed("alice", 0, &seed)), Some(54));
        // A refunded bet was never a wager: it is off the day's count.
        assert_eq!(u64_at(&p, 16), 0, "wagered today");
        // The table takes the next bet.
        assert!(t.commit_bet("alice", stake, &seed).ok());
    }
}

/// The dealer chooses when to open, so the player's time for the seed must not depend on it: it
/// is `seed_blocks` past the opening's deadline, whether the dealer opened at once or at the last
/// block. A player who comes back when the refund would have been due always has all of it.
#[test]
fn the_seeds_deadline_does_not_depend_on_when_the_dealer_opened() {
    for opened_at in [1_001, 1_075, 1_150] {
        let mut t = Table::new();
        t.host.height = 1_000;
        let seed = seed_for(0, never, plain);
        assert!(t.commit_bet("alice", FANG, &seed).ok());
        t.host.height = opened_at;
        t.open(0);
        assert_eq!(u64_at(&t.game(0).unwrap(), 104), 1_150 + SEED_BLOCKS, "opened at {opened_at}");
        // The player returns at the block the refund would have been claimable from.
        t.host.height = 1_151;
        assert_eq!(abort_code(&t.tx("stranger", "timeout", le(0), 0)), Some(60), "not the house's yet");
        t.host.height = 1_150 + SEED_BLOCKS;
        assert_eq!(t.reveal_seed("alice", 0, &seed).returns[0], [Phase::Player as u8, 0]);
    }
}

/// During an outage every bet is refunded, and none of them uses up the player's day.
#[test]
fn refunded_bets_do_not_use_up_the_daily_cap() {
    let mut t = Table::new();
    t.commit(16, 16);
    // 500 FANG a day at 50 a bet: ten wagers. Twelve refunds leave all ten.
    for id in 0..12u64 {
        assert!(t.commit_bet("alice", 50 * FANG, &[id as u8; 32]).ok(), "bet {id}");
        t.host.height += OPEN_BLOCKS + 1;
        assert_eq!(t.tx("alice", "timeout", le(id), 0).returns[0][1], Outcome::Refund as u8);
        assert_eq!(u64_at(&t.player("alice"), 16), 0);
    }
    // A hand that is opened counts, whatever becomes of it.
    assert!(t.commit_bet("alice", 50 * FANG, &[99; 32]).ok());
    t.open(12);
    t.host.height += OPEN_BLOCKS + SEED_BLOCKS + 1;
    assert_eq!(t.tx("alice", "timeout", le(12), 0).returns[0][1], Outcome::NoSeed as u8);
    assert_eq!(u64_at(&t.player("alice"), 16), 50 * FANG);
    // A refund on a later day leaves that day's count alone (and never underflows).
    assert!(t.commit_bet("alice", 50 * FANG, &[98; 32]).ok());
    t.host.height += 43_200;
    assert_eq!(t.tx("alice", "timeout", le(13), 0).returns[0][1], Outcome::Refund as u8);
    assert_eq!(u64_at(&t.player("alice"), 16), 100 * FANG, "the earlier day's count, untouched");
    // The owner renumbers the days so that the bet's day number comes round again with less counted.
    assert!(t.commit_bet("alice", FANG, &[97; 32]).ok());
    assert!(t.tx("house", "set_limits", limits_input(&[(DAY_BLOCKS, 1 << 30)]), 0).ok());
    t.host.height += OPEN_BLOCKS + 1;
    assert_eq!(t.tx("alice", "timeout", le(14), 0).returns[0][1], Outcome::Refund as u8);
}

#[test]
fn an_opening_link_past_the_deadline_is_refused() {
    let mut t = Table::new();
    t.host.height = 1_000;
    let seed = [5; 32];
    assert!(t.commit_bet("alice", FANG, &seed).ok());
    // In time, on the deadline's own block: taken.
    t.host.height = 1_150;
    assert_eq!(t.reveal(0, 1, 2).returns[0], [Phase::Open as u8, 0]);
    // A block later the stake is the player's to take back, whoever gets there first. The link
    // is public by now: were it let in, a player who read the cards it deals could choose
    // between playing them and calling `timeout`.
    t.host.height = 1_151;
    let before = t.game(0).unwrap();
    assert_eq!(abort_code(&t.reveal(0, 3, 1)), Some(73));
    assert_eq!(abort_code(&t.reveal(0, 1, 3)), Some(73), "nor behind links the hand holds");
    assert_eq!(t.game(0).unwrap(), before);
    // Links the hand already holds are still passed over: nothing changes.
    let r = t.reveal(0, 1, 2);
    assert_eq!((r.returns[0].as_slice(), r.events.len(), r.diff.storage.len()), (&[Phase::Open as u8, 0][..], 0, 0));
    // The seed isn't owed, so it isn't taken, and the hand ends as a refund.
    assert_eq!(abort_code(&t.reveal_seed("alice", 0, &seed)), Some(70));
    assert_eq!(t.tx("alice", "timeout", le(0), 0).returns[0], [Phase::Done as u8, Outcome::Refund as u8]);

    // A hand never opened at all: the same.
    assert!(t.commit_bet("bob", FANG, &seed).ok());
    t.host.height += OPEN_BLOCKS + 1;
    assert_eq!(abort_code(&t.reveal(1, 1, 3)), Some(73));
}

#[test]
fn a_seed_that_never_comes_still_forfeits_the_stake() {
    let mut t = Table::new();
    t.host.height = 2_000;
    let stake = 4 * FANG;
    let seed = [8; 32];
    let (alice, house) = (t.balance("alice"), t.house());
    assert!(t.commit_bet("alice", stake, &seed).ok());
    t.host.height = 2_030;
    t.open(0);
    // The player has seen the three cards, and has `seed_blocks` past the opening's deadline.
    assert_eq!(u64_at(&t.game(0).unwrap(), 104), 2_000 + OPEN_BLOCKS + SEED_BLOCKS);
    t.host.height = 2_450;
    assert_eq!(abort_code(&t.tx("dealer", "timeout", le(0), 0)), Some(60));
    t.host.height = 2_451;
    assert_eq!(abort_code(&t.reveal_seed("alice", 0, &seed)), Some(72), "a late seed is refused, as in v1");
    let r = t.tx("dealer", "timeout", le(0), 0);
    assert_eq!(r.returns[0], [Phase::Done as u8, Outcome::NoSeed as u8]);
    assert_eq!(event_names(&r), ["settled"]);
    let settled = event(&r, "settled");
    assert_eq!((u64_at(settled, S_PAYOUT), settled[58], settled[59]), (0, 3, 0), "three links, no cards");
    assert_eq!((&settled[60..92], &settled[92..124]), (&seed_hash("alice", &seed)[..], &chain_of(0)[3][..]));
    assert_eq!(t.balance("alice"), alice - stake);
    assert_eq!(t.house(), House { bankroll: house.bankroll + stake, next_game: 1, ..house }, "the stake is the house's; nothing trips");
}

// ---------------------------------------------------------------------------------- a dealer that stalls after the seed

#[test]
fn a_stall_after_the_seed_pays_what_the_cards_allow_and_trips_the_breaker() {
    type Shape = fn(Outcome, &Hand) -> bool;
    // (what, the seed's shape, the player's move before the stall, the outcome)
    let cases: [(&str, Shape, Option<&str>, Outcome); 3] = [
        ("a natural waiting on the hole card", |_, h| natural(h.player_cards()) && h.n_dealer == 2, None, Outcome::ForfeitDeal),
        ("a hit never answered", plain, Some("hit"), Outcome::Forfeit),
        ("a stand never answered", plain, Some("stand"), Outcome::Forfeit),
    ];
    let stake = 10 * FANG;
    for (what, shape, moved, outcome) in cases {
        let mut t = Table::new();
        t.host.height = 1_000;
        // Two more hands are open when the dealer stalls: bob's is being played, carol's waits
        // to be opened.
        let bob_seed = seed_for(0, never, plain);
        let (bob_id, _) = t.bet("bob", FANG, &bob_seed);
        let seed = seed_for(1, never, shape);
        let (alice, house) = (t.balance("alice"), t.house());
        let (id, r) = t.bet("alice", stake, &seed);
        assert_eq!(r.returns[0][1], 0, "{what}: not settled by the seed");
        if let Some(method) = moved {
            t.host.height += 3;
            assert!(t.tx("alice", method, le(id), 0).ok());
        }
        let deadline = u64_at(&t.game(id).unwrap(), 104);
        assert_eq!(deadline, t.host.height + REVEAL_BLOCKS, "{what}");
        let carol_seed = seed_for(2, never, plain);
        let carol = t.commit_bet("carol", FANG, &carol_seed);
        let carol_id = u64_at(&carol.returns[0], 0);

        t.host.height = deadline;
        assert_eq!(abort_code(&t.tx("alice", "timeout", le(id), 0)), Some(60), "{what}");
        t.host.height = deadline + 1;
        let r = t.tx("stranger", "timeout", le(id), 0);
        assert_eq!(r.returns[0], [Phase::Done as u8, outcome as u8], "{what}");
        assert_eq!(event_names(&r), ["settled", "tripped"], "{what}");
        assert_eq!(event(&r, "tripped"), [&le(id)[..], &[outcome as u8]].concat());
        let pays = if outcome == Outcome::ForfeitDeal { 25 * FANG } else { 20 * FANG };
        assert_eq!(u64_at(event(&r, "settled"), S_PAYOUT), pays, "{what}");
        assert_eq!(t.balance("alice"), alice - stake + pays, "{what}");
        let h = t.house();
        assert_eq!((h.bankroll + h.reserved, h.paused), (house.bankroll + house.reserved + stake - pays, TRIPPED), "{what}: paid from the hand's own reserve");

        // No new bet, from an account or through a relayer, by anyone.
        assert_eq!(abort_code(&t.commit_bet("alice", FANG, &seed)), Some(46), "{what}");
        assert_eq!(abort_code(&t.commit_bet("stranger", FANG, &seed)), Some(46), "{what}");
        // Only the owner allows bets again.
        for who in ["alice", "dealer", "stranger"] {
            assert_eq!(abort_code(&t.tx(who, "pause", vec![0], 0)), Some(42), "{what}: {who} can't clear a trip");
        }
        // Open hands play on: carol's is opened, dealt and played, bob's is finished. (The clock
        // is put back: this is a dealer that returned, not one that is still away.)
        t.host.height = 1_010;
        t.open(carol_id);
        assert!(t.reveal_seed("carol", carol_id, &carol_seed).ok());
        let r = t.finish_from("carol", carol_id, &carol_seed, opened(&deck(carol_id, &carol_seed)).0, never);
        assert_eq!(r.returns[0][0], Phase::Done as u8);
        let r = t.finish_from("bob", bob_id, &bob_seed, opened(&deck(bob_id, &bob_seed)).0, never);
        assert_eq!(r.returns[0][0], Phase::Done as u8);
        // Deposits, withdrawals, the bankroll and commitments still move.
        assert!(t.tx("bob", "deposit", vec![], FANG).ok());
        assert!(t.tx("bob", "withdraw", le(FANG), 0).ok());
        assert!(t.tx("house", "fund", vec![], FANG).ok());
        assert!(t.tx("house", "defund", le(FANG), 0).ok());
        t.commit(16, 4);
        assert_eq!(t.house().paused, TRIPPED, "{what}: nothing but the owner clears it");

        // The owner's pause over a trip is a pause; allowing bets clears both.
        assert_eq!(event(&t.tx("house", "pause", vec![1], 0), "paused"), [1]);
        assert_eq!(t.house().paused, 1);
        assert_eq!(abort_code(&t.commit_bet("alice", FANG, &seed)), Some(46));
        assert_eq!(event(&t.tx("house", "pause", vec![0], 0), "paused"), [0]);
        assert_eq!(t.house().paused, 0);
        assert!(t.commit_bet("alice", FANG, &seed).ok(), "{what}: bets are taken again");
    }
}

#[test]
fn a_forfeit_while_the_owner_has_paused_still_trips() {
    let mut t = Table::new();
    let seed = seed_for(0, never, plain);
    let (id, _) = t.bet("alice", FANG, &seed);
    assert!(t.tx("alice", "stand", le(id), 0).ok());
    assert!(t.tx("house", "pause", vec![1], 0).ok());
    t.host.height += REVEAL_BLOCKS + 1;
    let r = t.tx("stranger", "timeout", le(id), 0);
    assert_eq!(r.returns[0], [Phase::Done as u8, Outcome::Forfeit as u8]);
    assert_eq!(t.house().paused, TRIPPED);
    assert!(t.tx("house", "pause", vec![0], 0).ok());
    assert_eq!(t.house().paused, 0);
}

#[test]
fn an_idle_player_stands_and_a_dealer_that_then_stalls_pays_one_to_one() {
    let mut t = Table::new();
    t.host.height = 2_000;
    let seed = seed_for(0, never, plain);
    let alice = t.balance("alice");
    let (id, _) = t.bet("alice", 5 * FANG, &seed);
    assert_eq!(u64_at(&t.game(id).unwrap(), 104), 2_000 + IDLE_BLOCKS, "the player's clock starts with the cards");
    t.host.height = 2_000 + IDLE_BLOCKS;
    assert_eq!(abort_code(&t.tx("dealer", "timeout", le(id), 0)), Some(60));
    t.host.height += 1;
    let r = t.tx("dealer", "timeout", le(id), 0);
    assert_eq!(r.returns[0], [Phase::Dealer as u8, 0]);
    assert_eq!(event(&r, "stand"), [&le(id)[..], &addr("alice"), &le(t.host.height + REVEAL_BLOCKS), &[1]].concat());
    assert_eq!(t.house().paused, 0, "an idle player trips nothing");
    t.host.height += REVEAL_BLOCKS + 1;
    let r = t.tx("stranger", "timeout", le(id), 0);
    assert_eq!(r.returns[0], [Phase::Done as u8, Outcome::Forfeit as u8]);
    assert_eq!(t.balance("alice"), alice + 5 * FANG);
}

// ---------------------------------------------------------------------------------- relayed hands

fn approve(t: &mut Table, relayer: &str) {
    assert!(t.tx("house", "approve_relayer", [&addr(relayer)[..], &[1]].concat(), 0).ok());
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

/// A table where bob plays through the relayer `bot`.
fn relayed_table() -> Table {
    let mut t = Table::new();
    approve(&mut t, "bot");
    assert!(t.tx("bob", "deposit", vec![], 100 * FANG).ok());
    let expires = t.host.height + 40_000;
    assert!(t.tx("bob", "set_relayer", grant_input("bot", 25 * FANG, 100 * FANG, expires), 0).ok());
    t
}

#[test]
fn a_relayed_hand_is_dealt_as_in_v1_and_a_stall_pays_by_the_cards_on_the_table() {
    let ten_or_ace = |c: u8| matches!(c % 13, 0 | 9..=12);
    // (links revealed before the stall, whether the player's first card can start a natural)
    for (revealed, first_can) in [(0usize, false), (0, true), (1, false), (1, true), (2, false), (2, true)] {
        let mut t = relayed_table();
        t.host.height = 1_000;
        let stake = 10 * FANG;
        let post = (1u64..).find(|p| ten_or_ace(deck(0, &relay_seed("bob", *p))[0]) == first_can).unwrap();
        let (bob, table, house) = (t.balance("bob"), t.table_balance("bob"), t.house());
        let r = relay_bet(&mut t, "bot", "bob", stake, post);
        assert_eq!(r.returns[0], le(0));
        let g = t.game(0).unwrap();
        // Its seed is known from the bet, so the dealer can't open it blind: it owes the deal.
        assert_eq!((g[120], &g[40..72], u64_at(&g, 104)), (Phase::Deal as u8, &relay_seed("bob", post)[..], 1_000 + REVEAL_BLOCKS));
        if revealed > 0 {
            let r = t.reveal(0, 1, revealed);
            assert_eq!((r.returns[0].as_slice(), event_names(&r)), (&[Phase::Deal as u8, 0][..], vec!["cards"]));
        }
        // 3:2 only while a natural can still be dealt: no card out, or a first card that can
        // start one. v1 paid 3:2 in all six.
        let outcome = if revealed == 0 || first_can { Outcome::ForfeitDeal } else { Outcome::Forfeit };
        t.host.height = 1_000 + REVEAL_BLOCKS + 1;
        let r = t.tx("stranger", "timeout", le(0), 0);
        assert_eq!(r.returns[0], [Phase::Done as u8, outcome as u8], "{revealed} links, first card can start a natural: {first_can}");
        assert_eq!(event_names(&r), ["settled", "tripped"]);
        let pays = payout(stake, outcome);
        assert_eq!((u64_at(event(&r, "settled"), S_PAYOUT), event(&r, "settled")[S_TO_TABLE]), (pays, 1));
        assert_eq!((t.balance("bob"), t.table_balance("bob")), (bob, table - stake + pays), "paid to the table balance");
        assert_eq!(t.house(), House { bankroll: house.bankroll + stake - pays, table: house.table - stake + pays, next_game: 1, paused: TRIPPED, ..house });
        // The breaker stops the relayer too.
        assert_eq!(abort_code(&relay_bet(&mut t, "bot", "bob", FANG, post + 1)), Some(46));
    }
}

#[test]
fn a_relayed_hand_plays_out_and_its_seed_is_never_owed() {
    let mut t = relayed_table();
    let post = (1u64..).find(|p| !natural(&[deck(0, &relay_seed("bob", *p))[0], deck(0, &relay_seed("bob", *p))[2]])).unwrap();
    let seed = relay_seed("bob", post);
    let table = t.table_balance("bob");
    assert!(relay_bet(&mut t, "bot", "bob", 5 * FANG, post).ok());
    assert_eq!(abort_code(&t.reveal_seed("bob", 0, &seed)), Some(70), "a relayed hand waits for no seed");
    let cards = deck(0, &seed);
    let mut hand = Hand::new();
    for c in &cards[..3] {
        hand.deal(*c);
    }
    assert_eq!(t.reveal(0, 1, 3).returns[0], [Phase::Player as u8, 0]);
    assert!(t.tx("bot", "relay_stand", [le(0), le(post + 1)].concat(), 0).ok());
    hand.stand();
    let (used, outcome, _) = play_from((hand, None), &cards, never);
    let r = t.reveal(0, 4, used - 3);
    assert_eq!(r.returns[0], [Phase::Done as u8, outcome as u8]);
    assert_eq!(t.table_balance("bob"), table - 5 * FANG + payout(5 * FANG, outcome));
    assert_eq!(t.house().paused, 0);
}

// ---------------------------------------------------------------------------------- an outage

/// The audit's M-3, on v2: the dealer daemon is down, its commitments are on the chain, and
/// players bet against them. Under v1 every one of these bets was paid 3:2 at its deadline.
#[test]
fn bets_against_a_dealer_that_is_down_win_nothing() {
    let mut t = Table::bare();
    let players: Vec<String> = (0..10).map(|i| format!("player{i}")).collect();
    for who in &players {
        t.host.fund(addr(who), 1_000 * FANG);
    }
    assert!(t.tx("house", "fund", vec![], 2_000 * FANG).ok());
    t.commit(0, 30);
    let (house, everyone) = (t.house(), t.host.total());
    let stake = 50 * FANG;
    let mut v1_would_have_paid = 0;
    for round in 0..3u64 {
        // A full table at the largest bet, against a dealer that never answers.
        for who in &players {
            let r = t.commit_bet(who, stake, &[round as u8; 32]);
            assert!(r.ok(), "round {round}: {:?}", r.fail);
        }
        assert_eq!((t.house().open, t.house().reserved), (10, 750 * FANG));
        t.host.height += OPEN_BLOCKS + 1;
        for (i, who) in players.iter().enumerate() {
            let id = round * 10 + i as u64;
            let r = t.tx(who, "timeout", le(id), 0);
            assert_eq!(r.returns[0], [Phase::Done as u8, Outcome::Refund as u8]);
            v1_would_have_paid += bj_core::payout(stake, bj_core::Outcome::ForfeitDeal) - stake;
        }
        // Not a drop moved, and the table still takes bets: nothing was lost that a pause
        // would have saved.
        assert_eq!(t.house(), House { next_game: (round + 1) * 10, ..house });
    }
    for who in &players {
        assert_eq!(t.balance(who), 1_000 * FANG, "{who} won nothing and lost nothing");
    }
    assert_eq!(t.host.total(), everyone);
    assert_eq!(v1_would_have_paid, 2_250 * FANG, "what the same thirty bets took from a v1 table: more than its bankroll");
    assert!(v1_would_have_paid > house.bankroll);
}

/// What an outage can still cost: the hands the dealer had opened when it stopped, at most their
/// stakes (1:1), once. Then the breaker holds until the owner looks.
#[test]
fn an_outage_costs_at_most_the_hands_in_flight_once() {
    let mut t = Table::new();
    t.host.height = 1_000;
    let stake = 50 * FANG;
    let (house, everyone) = (t.house(), t.host.total());
    // In flight when the dealer stops: alice is playing, bob's hand is opened and waits for his
    // seed, carol's bet was never opened.
    let alice_seed = seed_for(0, never, plain);
    let (alice_id, _) = t.bet("alice", stake, &alice_seed);
    let bob_seed = seed_for(1, never, plain);
    let r = t.commit_bet("bob", stake, &bob_seed);
    let bob_id = u64_at(&r.returns[0], 0);
    t.open(bob_id);
    let r = t.commit_bet("carol", stake, &[1; 32]);
    let carol_id = u64_at(&r.returns[0], 0);
    let in_flight = 2 * stake;

    // The dealer is gone. Everyone makes the most of it: alice stands on whatever she holds,
    // bob reveals his seed (his cards are dealt by it) and stands too.
    assert!(t.tx("alice", "stand", le(alice_id), 0).ok());
    assert_eq!(t.reveal_seed("bob", bob_id, &bob_seed).returns[0], [Phase::Player as u8, 0]);
    assert!(t.tx("bob", "stand", le(bob_id), 0).ok());
    t.host.height += REVEAL_BLOCKS + 1;
    // Carol's bet is refunded, and so is any bet placed in the meantime.
    assert_eq!(t.tx("carol", "timeout", le(carol_id), 0).returns[0][1], Outcome::Refund as u8);
    let r = t.commit_bet("stranger", stake, &[2; 32]);
    let late_id = u64_at(&r.returns[0], 0);
    // The first forfeit trips the breaker.
    assert_eq!(t.tx("alice", "timeout", le(alice_id), 0).returns[0][1], Outcome::Forfeit as u8);
    assert_eq!(t.house().paused, TRIPPED);
    assert_eq!(t.tx("bob", "timeout", le(bob_id), 0).returns[0][1], Outcome::Forfeit as u8);
    // From here nobody gets in, for as long as the outage lasts.
    for who in ["alice", "bob", "carol"] {
        t.host.height += 500;
        assert_eq!(abort_code(&t.commit_bet(who, stake, &[4; 32])), Some(46));
    }
    assert_eq!(t.tx("stranger", "timeout", le(late_id), 0).returns[0][1], Outcome::Refund as u8);

    let h = t.house();
    assert_eq!((h.open, h.reserved, h.stakes), (0, 0, 0));
    assert_eq!(house.bankroll - h.bankroll, in_flight, "the house lost the in-flight stakes, 1:1, and nothing else");
    assert!(house.bankroll - h.bankroll <= u64::from(house.open.max(10)) * stake, "bounded by the seats");
    assert_eq!(t.host.total(), everyone);
}

// ---------------------------------------------------------------------------------- solvency and overflow

#[test]
fn a_full_table_of_naturals_is_paid_after_the_owner_takes_every_free_drop() {
    let mut t = Table::bare();
    let players: Vec<String> = (0..10).map(|i| format!("player{i}")).collect();
    for who in &players {
        t.host.fund(addr(who), 100 * FANG);
    }
    assert!(t.tx("house", "fund", vec![], 2_000 * FANG).ok());
    t.commit(0, 10);
    let everyone = t.host.total();
    let stake = 50 * FANG;
    let seeds: Vec<[u8; 32]> = (0..10).map(|id| seed_for(id, never, |o, _| o == Outcome::Blackjack)).collect();
    for (id, who) in players.iter().enumerate() {
        assert!(t.commit_bet(who, stake, &seeds[id]).ok());
        t.open(id as u64);
    }
    // Ten hands at the largest bet: the whole exposure limit is reserved.
    assert_eq!((t.house().reserved, t.house().stakes, t.house().bankroll), (750 * FANG, 500 * FANG, 1_250 * FANG));
    assert_eq!(abort_code(&t.tx("house", "defund", le(1_250 * FANG + 1), 0)), Some(63), "the reserve isn't the owner's to take");
    assert!(t.tx("house", "defund", le(1_250 * FANG), 0).ok());
    assert_eq!(t.house().bankroll, 0);
    // Every one of them is a natural, and every one is paid in full from what was set aside.
    for (id, who) in players.iter().enumerate() {
        let id = id as u64;
        let r = t.reveal_seed(who, id, &seeds[id as usize]);
        let (hand, settled) = opened(&deck(id, &seeds[id as usize]));
        let r = if settled.is_some() { r } else { t.finish_from(who, id, &seeds[id as usize], hand, never) };
        assert_eq!(r.returns[0], [Phase::Done as u8, Outcome::Blackjack as u8]);
        assert_eq!(t.balance(who), 100 * FANG + 75 * FANG);
    }
    let h = t.house();
    assert_eq!((h.bankroll, h.reserved, h.stakes, h.open), (0, 0, 0, 0));
    assert_eq!(t.balance(PROGRAM), 0);
    assert_eq!(t.host.total(), everyone);
}

#[test]
fn the_largest_hand_a_chain_can_hold_never_overflows() {
    // A stake whose natural pays the whole supply: stake + exposure = 10^18 drops.
    let stake = 400_000_000 * FANG;
    let supply = 1_000_000_000 * FANG;
    type Shape = fn(Outcome, &Hand) -> bool;
    let cases: [(&str, Shape); 4] = [("a natural", |o, _| o == Outcome::Blackjack), ("a loss", |o, _| o == Outcome::Lose), ("a refund", |_, _| true), ("a forfeit", plain)];
    for (what, shape) in cases {
        let mut t = Table::bare();
        t.host.fund(addr("house"), 600_000_000 * FANG + FANG);
        t.host.fund(addr("alice"), stake + FANG);
        assert!(t.tx("house", "fund", vec![], 600_000_000 * FANG).ok());
        assert!(t.tx("house", "set_limits", limits_input(&[(MAX_BET, stake), (DAILY_CAP, supply), (DIVISOR, 1), (MAX_EXPOSURE, supply), (DEPOSIT_CAP, supply)]), 0).ok());
        t.commit(0, 2);
        let everyone = t.host.total();
        let seed = seed_for(0, never, shape);
        let r = t.commit_bet("alice", stake, &seed);
        assert!(r.ok(), "{what}: {:?}", r.fail);
        assert_eq!((t.house().reserved, t.house().bankroll, t.balance(PROGRAM)), (600_000_000 * FANG, 0, supply), "{what}");
        let outcome = match what {
            "a refund" => {
                t.host.height += OPEN_BLOCKS + 1;
                t.tx("alice", "timeout", le(0), 0).returns[0][1]
            }
            "a forfeit" => {
                t.open(0);
                assert!(t.reveal_seed("alice", 0, &seed).ok());
                assert!(t.tx("alice", "stand", le(0), 0).ok());
                t.host.height += REVEAL_BLOCKS + 1;
                t.tx("alice", "timeout", le(0), 0).returns[0][1]
            }
            _ => {
                t.open(0);
                let r = t.reveal_seed("alice", 0, &seed);
                let (hand, settled) = opened(&deck(0, &seed));
                if settled.is_some() { r.returns[0][1] } else { t.finish_from("alice", 0, &seed, hand, never).returns[0][1] }
            }
        };
        let pays = payout(stake, Outcome::from_u8(outcome).unwrap());
        assert_eq!(t.balance("alice"), FANG + pays, "{what}");
        let h = t.house();
        assert_eq!((h.bankroll, h.reserved, h.stakes), (supply - pays, 0, 0), "{what}");
        assert_eq!(t.host.total(), everyone, "{what}");
    }
    // And from a table balance, where the payout is a sum of balances.
    let mut t = Table::bare();
    t.host.fund(addr("house"), 600_000_000 * FANG + FANG);
    t.host.fund(addr("bob"), stake + FANG);
    assert!(t.tx("house", "fund", vec![], 600_000_000 * FANG).ok());
    assert!(t.tx("house", "set_limits", limits_input(&[(MAX_BET, stake), (DAILY_CAP, supply), (DIVISOR, 1), (MAX_EXPOSURE, supply), (DEPOSIT_CAP, supply), (10, stake)]), 0).ok());
    approve(&mut t, "bot");
    t.commit(0, 1);
    assert!(t.tx("bob", "deposit", vec![], stake).ok());
    assert!(t.tx("bob", "set_relayer", grant_input("bot", stake, supply, t.host.height + 100), 0).ok());
    assert!(relay_bet(&mut t, "bot", "bob", stake, 1).ok());
    t.host.height += REVEAL_BLOCKS + 1;
    assert_eq!(t.tx("stranger", "timeout", le(0), 0).returns[0][1], Outcome::ForfeitDeal as u8);
    assert_eq!((t.table_balance("bob"), t.house().table, t.house().bankroll), (supply, supply, 0));
}

#[test]
fn a_refund_that_cant_be_sent_becomes_table_balance() {
    let mut t = Table::new();
    assert!(t.commit_bet("alice", 5 * FANG, &[1; 32]).ok());
    // The account is gone by the time the hand is refunded (emptied and closed).
    t.host.balances.remove(&addr("alice"));
    t.host.height += OPEN_BLOCKS + 1;
    let r = t.tx("stranger", "timeout", le(0), 0);
    assert_eq!(r.returns[0], [Phase::Done as u8, Outcome::Refund as u8]);
    assert_eq!(event(&r, "settled")[S_TO_TABLE], 1);
    assert_eq!((t.table_balance("alice"), t.house().bankroll, t.house().table), (5 * FANG, 2_000 * FANG, 5 * FANG));
    t.host.fund(addr("alice"), FANG);
    assert!(t.tx("alice", "withdraw", le(5 * FANG), 0).ok());
    assert_eq!(t.balance("alice"), 6 * FANG);
}

#[test]
fn a_refund_gives_the_hands_deposit_back_and_a_seed_that_settles_bonds_nothing() {
    let mut t = Table::bare();
    t.host.storage_price = 100_000;
    assert!(t.tx("house", "fund", vec![], 1_000 * FANG).ok());
    t.commit(0, 4);
    // A first hand, so alice's own record exists and stays.
    let before = t.balance("alice");
    assert!(t.commit_bet("alice", 2 * FANG, &[1; 32]).ok());
    t.host.height += OPEN_BLOCKS + 1;
    assert!(t.tx("stranger", "timeout", le(0), 0).ok());
    let kept = before - t.balance("alice");
    assert!(kept > 0, "the player's record stays bonded");
    // A refunded hand costs its bettor nothing: the stake and the hand's deposit both come back.
    let before = t.balance("alice");
    assert!(t.commit_bet("alice", 2 * FANG, &[1; 32]).ok());
    assert!(t.balance("alice") < before - 2 * FANG, "the hand's record is bonded at the bet");
    t.host.height += OPEN_BLOCKS + 1;
    assert!(t.tx("stranger", "timeout", le(1), 0).ok());
    assert_eq!(t.balance("alice"), before);
    // The seed's reveal runs with a storage limit of 0 (`Table::tx`), even when it deals three
    // cards and settles the hand.
    let seed = seed_for(2, never, |o, h| o == Outcome::Blackjack && h.n_dealer == 1);
    let (_, r) = t.bet("alice", 2 * FANG, &seed);
    assert_eq!(r.returns[0], [Phase::Done as u8, Outcome::Blackjack as u8]);
    assert_eq!(t.balance("alice"), before + 3 * FANG);
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

/// 500 hands by four players, some from their accounts and some through the relayer, with
/// dealers that never open, open late or stall, players who never reveal or go idle, and the
/// owner clearing each trip. The invariant is checked after every call (`Table::tx`); at the end
/// every FANG is accounted for.
#[test]
fn a_soak_keeps_the_invariant_and_every_drop() {
    let mut t = Table::bare();
    assert!(t.tx("house", "fund", vec![], 3_000 * FANG).ok());
    assert!(t.tx("house", "set_limits", limits_input(&[(DAILY_CAP, 100_000 * FANG), (DAY_BLOCKS, 5_000), (DEPOSIT_CAP, 400 * FANG), (GRANT_BLOCKS, 1 << 30)]), 0).ok());
    approve(&mut t, "bot");
    let players = ["alice", "bob", "carol", "stranger"];
    let relayed = ["carol", "stranger"];
    for who in relayed {
        assert!(t.tx(who, "deposit", vec![], 300 * FANG).ok());
        assert!(t.tx(who, "set_relayer", grant_input("bot", 25 * FANG, 50_000 * FANG, 1 << 29), 0).ok());
    }
    let everyone: u128 = t.host.total();
    let mut rng = Rng(11);
    let mut outcomes: BTreeMap<u8, u32> = BTreeMap::new();
    let mut committed = 0;
    let (mut wagered, mut paid, mut trips) = (0u128, 0u128, 0);
    for n in 0..500u64 {
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
            let room = 400 * FANG - t.table_balance(who);
            assert!(t.tx(who, "deposit", vec![], room.min(100 * FANG)).ok());
        }
        wagered += u128::from(stake);
        let quirk = rng.below(40);
        let outcome = 'hand: {
            let (id, seed, mut hand) = if via_bot {
                let r = relay_bet(&mut t, "bot", who, stake, post);
                assert!(r.ok(), "hand {n}: {:?}", r.fail);
                (u64_at(&r.returns[0], 0), relay_seed(who, post), Hand::new())
            } else {
                let seed = *blake3::hash(&n.to_le_bytes()).as_bytes();
                let r = t.commit_bet(who, stake, &seed);
                assert!(r.ok(), "hand {n}: {:?}", r.fail);
                let id = u64_at(&r.returns[0], 0);
                if quirk < 4 {
                    // A dealer that is down, or that opens part of a hand and stops: a refund.
                    if quirk >= 2 {
                        assert!(t.reveal(id, 1, quirk as usize - 1).ok());
                    }
                    t.host.height += OPEN_BLOCKS + 1;
                    if quirk == 3 {
                        assert_eq!(abort_code(&t.reveal(id, 1, 3)), Some(73), "too late to open");
                    }
                    break 'hand t.tx("alice", "timeout", le(id), 0).returns[0][1];
                }
                // One link at a time now and then.
                if quirk == 4 {
                    for j in 1..=3 {
                        assert!(t.reveal(id, j, 1).ok());
                    }
                } else {
                    t.open(id);
                }
                if quirk == 5 {
                    // A player who never reveals the seed: the stake is the house's.
                    t.host.height += OPEN_BLOCKS + SEED_BLOCKS + 1;
                    break 'hand t.tx("dealer", "timeout", le(id), 0).returns[0][1];
                }
                t.host.height += rng.below(3);
                let r = t.reveal_seed(who, id, &seed);
                assert!(r.ok(), "{:?}", r.fail);
                let (hand, settled) = opened(&deck(id, &seed));
                if let Some(o) = settled {
                    assert_eq!(r.returns[0], [Phase::Done as u8, o as u8]);
                    break 'hand o as u8;
                }
                (id, seed, hand)
            };
            let deck = deck(id, &seed);
            loop {
                if hand.phase == Phase::Player {
                    // An idle player now and then: the dealer stands them.
                    if quirk == 6 {
                        t.host.height += IDLE_BLOCKS + 1;
                        assert!(t.tx("dealer", "timeout", le(id), 0).ok());
                        hand.stand();
                    } else {
                        let hit = total(hand.player_cards()).0 < 12 + rng.below(7) as u8;
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
                    }
                }
                // A dealer that stalls on a card it owes: the player is paid what the cards on
                // the table allow, and the breaker trips.
                if quirk == 7 || quirk == 8 && hand.dealt >= 3 {
                    let expected = hand.forfeit().unwrap();
                    t.host.height += REVEAL_BLOCKS + 1;
                    let r = t.tx("alice", "timeout", le(id), 0);
                    assert_eq!(r.returns[0], [Phase::Done as u8, expected as u8], "{:?}", r.fail);
                    assert_eq!(t.house().paused, TRIPPED);
                    assert_eq!(abort_code(&t.commit_bet("alice", FANG, &[0; 32])), Some(46));
                    assert!(t.tx("house", "pause", vec![0], 0).ok());
                    trips += 1;
                    break 'hand expected as u8;
                }
                let from = hand.dealt as usize + 1;
                let mut settled = None;
                // A relayed deal one link at a time now and then; otherwise all that is owed.
                let step = if quirk == 9 { 1 } else { usize::MAX };
                let mut n_links = 0;
                while hand.phase.dealers() && n_links < step {
                    if let Some(Step::Settled(o)) = hand.deal(deck[hand.dealt as usize]) {
                        settled = Some(o);
                    }
                    n_links += 1;
                }
                assert!(t.reveal(id, from, n_links).ok());
                if let Some(o) = settled {
                    break 'hand o as u8;
                }
            }
        };
        let outcome_of = Outcome::from_u8(outcome).unwrap();
        *outcomes.entry(outcome).or_default() += 1;
        paid += u128::from(payout(stake, outcome_of));
        assert_eq!(t.player(who)[112], outcome);
        assert_eq!((t.house().open, t.house().paused), (0, 0));
    }
    for who in relayed {
        if t.table_balance(who) > 0 {
            assert!(t.tx("bot", "relay_withdraw", [&addr(who)[..], &le(0)].concat(), 0).ok());
        }
    }
    let h = t.house();
    assert_eq!((h.reserved, h.stakes, h.table, h.open), (0, 0, 0, 0));
    assert_eq!(u128::from(h.bankroll), 3_000 * u128::from(FANG) + wagered - paid, "the bankroll is the stakes less the payouts");
    assert!(t.tx("house", "defund", le(h.bankroll), 0).ok());
    assert_eq!(t.balance(PROGRAM), 0);
    assert_eq!(t.host.total(), everyone, "no FANG made or lost");
    assert!((1..=9).all(|o| outcomes.contains_key(&o)), "every outcome happened: {outcomes:?}");
    assert_eq!(trips, outcomes[&6] + outcomes[&7], "every forfeit tripped the breaker, and nothing else did");
    println!("outcomes over 500 hands: {outcomes:?}; {trips} trips; wagered {} FANG, paid {} FANG", wagered / u128::from(FANG), paid / u128::from(FANG));
}

/// The most fuel each method used, over a run of its own: every one far under
/// `program_max_tx_fuel`. `reveal_seed` now deals three cards (and may settle the hand), which a
/// player pays for.
#[test]
fn every_method_runs_far_under_the_transaction_fuel_limit() {
    let mut t = relayed_table();
    t.commit(16, 64);
    assert!(t.tx("house", "set_limits", limits_input(&[(MAX_OPEN, 32), (REVEAL, 150)]), 0).ok());
    assert!(t.tx("house", "set_dealer", addr("dealer").to_vec(), 0).ok());
    assert!(t.tx("house", "skip_commits", 1u32.to_le_bytes().to_vec(), 0).ok());
    assert!(t.tx("house", "pause", vec![0], 0).ok());
    assert!(t.tx("house", "defund", le(FANG), 0).ok());
    assert!(t.tx("bob", "withdraw", le(FANG), 0).ok());
    // The seed's reveal at its dearest: three cards and a settlement in one call.
    let seed = seed_for(1, never, |o, h| o == Outcome::Blackjack && h.n_dealer == 1);
    let (_, r) = t.bet("alice", FANG, &seed);
    assert_eq!(r.returns[0][0], Phase::Done as u8);
    println!("a seed that deals three cards and settles used {} fuel", r.fuel_used);
    // The longest dealer hand a search finds.
    let mut longest = (0, [0u8; 32]);
    for n in 0u32..3_000 {
        let seed = *blake3::hash(&n.to_le_bytes()).as_bytes();
        let (used, _, hand) = play(&deck(2, &seed), never);
        if !natural(hand.player_cards()) && used > longest.0 {
            longest = (used, seed);
        }
    }
    let (used, seed) = longest;
    assert!(used >= 8, "a dealer hand of {used} cards");
    let (id, _) = t.bet("alice", FANG, &seed);
    assert!(t.tx("alice", "stand", le(id), 0).ok());
    let r = t.reveal(id, 4, used - 3);
    assert!(r.ok());
    let per_link = r.fuel_used / (used as u64 - 3);
    assert!(12 * per_link * 2 < MAX_TX_FUEL / 4, "twelve links, doubled, are under a quarter of the limit");
    // A relayed hand with a hit, a stand and a timeout; the views; a revoke.
    let next = t.house().next_game;
    let takes_a_hit = |post: &u64| {
        let cards = deck(next, &relay_seed("bob", *post));
        let mut hand = Hand::new();
        for c in &cards[..3] {
            hand.deal(*c);
        }
        hand.phase == Phase::Player && hand.hit() && hand.deal(cards[3]) == Some(Step::Player)
    };
    let post = (77u64..).find(takes_a_hit).unwrap();
    let r = relay_bet(&mut t, "bot", "bob", FANG, post);
    let id = u64_at(&r.returns[0], 0);
    assert!(t.reveal(id, 1, 3).ok());
    assert!(t.tx("bot", "relay_hit", [le(id), le(post + 1)].concat(), 0).ok());
    assert!(t.reveal(id, 4, 1).ok());
    assert!(t.tx("bot", "relay_stand", [le(id), le(post + 2)].concat(), 0).ok());
    t.host.height += REVEAL_BLOCKS + 1;
    assert!(t.tx("stranger", "timeout", le(id), 0).ok());
    assert!(t.tx("house", "pause", vec![0], 0).ok());
    let (id, _) = t.bet("carol", FANG, &seed_for(t.house().next_game, never, plain));
    assert!(t.tx("carol", "hit", le(id), 0).ok());
    for (method, input) in [("game", le(id)), ("player", addr("bob").to_vec()), ("house", vec![]), ("limits", vec![]), ("open_games", vec![]), ("commitment", le(t.house().next_game)), ("version", vec![])] {
        t.view(method, input);
    }
    assert!(t.tx("bot", "relay_withdraw", [&addr("bob")[..], &le(80)].concat(), 0).ok());
    assert!(t.tx("bob", "revoke", vec![], 0).ok());

    let fuel = FUEL.lock().unwrap().clone();
    println!("{:<16} {:>9}", "method", "most fuel");
    for m in interface().methods.iter().map(|m| m.name.clone()) {
        let used = fuel.get(&m).copied().unwrap_or_else(|| panic!("{m} never ran"));
        println!("{m:<16} {used:>9}");
        assert!(used < MAX_TX_FUEL / 4, "{m} used {used} fuel");
    }
}
