//! `blockula-1`'s shape at launch (docs/GENESIS_CEREMONY.md): a small founder-run set the spec
//! waives the independence policy for, on record; born at protocol v2 with programs off; and a
//! liquid operator float on each validator's consensus-key account, so the bootstrap council can
//! propose and vote from block 1. These tests build such a genesis with the strict builder (as
//! `blockula genesis build` does for `blockula-1`), then run its state machine: the council passes
//! an upgrade and switches programs on early in the chain, without the float nobody can do either
//! until the operators' vesting cliff, and Ghost Mode stays off whatever the council votes. The
//! same launch on a dry-run chain shows what's `blockula-1`'s alone, and one test runs the
//! council's first proposals through a node (signed, admitted, executed and replayed).

use super::*;
use crate::chain::{BlockInfo, Chain, Outcome, State};
use crate::crypto::Keys;
use crate::governance::ProposalStatus;
use crate::types::{Action, ProposalKind, Tx};

const FANG: u64 = DROPS_PER_FANG;
const MAIN: &str = MAINNET_CHAIN_ID;
/// The same launch on a throwaway chain.
const DRY_RUN: &str = "blockula-dryrun-1";

/// The founder's four validators (seed, Fly region, country): one legal entity and one provider,
/// unflagged (`founder_or_foundation = false`) and each owned by its own operators-bucket account,
/// so a seat that leaves the set can rejoin it (no rejoin trap), with commission 0. The bootstrap
/// delegation backs them, so the launch caps its commission at 0: its rewards stay the treasury's
/// whatever commission the owners set later.
const FOUNDER_SEATS: [(u8, &str, &str); 4] = [(0xa1, "iad", "US"), (0xa2, "sjc", "US"), (0xa3, "fra", "DE"), (0xa4, "ams", "NL")];

/// A validator's consensus key.
fn validator(n: u8) -> Keys {
    Keys::from_seed([n; 32])
}

/// The cold owner account of the validator with consensus key `n`.
fn owner(n: u8) -> Keys {
    Keys::from_seed([n + 0x10; 32])
}

fn founder() -> Keys {
    Keys::from_seed([0x30; 32])
}

fn grants() -> Keys {
    Keys::from_seed([0x32; 32])
}

/// Seat `i` of [`FOUNDER_SEATS`] on `chain`, owned by `owner`, self-bonding `self_bond` FANG,
/// flagged as the founder's or not.
fn seat(chain: &str, i: usize, owner: &Keys, self_bond: &str, flagged: bool) -> OperatorFile {
    let (seed, region, country) = FOUNDER_SEATS[i];
    let independence = Independence { legal_entity: "Blockula Labs".into(), entity_jurisdiction: "US".into(), hosting_provider: "Fly.io".into(), asn: 40_509, datacenter_country: country.into(), datacenter: format!("fly-{region}"), founder_or_foundation: flagged, statement: "Run by the founder (disclosed).".into(), ..Default::default() };
    let info = OperatorInfo { name: format!("Blockula validator {region}"), contact: vec![] };
    operator_file(&validator(seed), chain, &owner.address_with("boo"), &format!("203.0.113.{}:30300", i + 1), &format!("https://rpc{}.blockula.example", i + 1), 0, self_bond, info, independence).unwrap()
}

/// The founder's set on `chain`, as launched: unflagged, one owner each, 10,000 FANG bonds.
fn founder_seats(chain: &str) -> Vec<OperatorFile> {
    (0..FOUNDER_SEATS.len()).map(|i| seat(chain, i, &owner(FOUNDER_SEATS[i].0), "10000", false)).collect()
}

/// What the founder's set breaks, waived on record.
const WAIVERS: &str = r#"
[[waiver]]
rule = "operator-minimum"
reason = "experimental launch: the founder's 4 validators; seats open to stake after launch"
[[waiver]]
rule = "provider-cap"
reason = "all 4 run on Fly.io (one failure domain), disclosed"
[[waiver]]
rule = "asn-cap"
reason = "all 4 announce from Fly's ASN 40509"
[[waiver]]
rule = "country-cap"
reason = "two US regions (iad, sjc)"
[[waiver]]
rule = "facility-cap"
reason = "with fewer than 7 operators any facility holds more than 15% of the set"
[[waiver]]
rule = "one-seat-per-entity"
reason = "the founder runs all 4 seats, unflagged so each can rejoin the set"
"#;

/// The launch on `chain` (`blockula-1` or the dry run): DEC-5 exactly (the operators bucket is the
/// four owners' allocations, the security grants and, with `float`, the four floats), born at v2,
/// a 10 FANG proposal deposit, governance at its floors (3-day votes, a 1-day timelock), the
/// council for 730 days, downtime sized for 4 validators, no commission on the bootstrap
/// delegation's stake, and the waivers. `extra` goes in at the top level.
fn spec(chain: &str, float: Option<u64>, extra: &str) -> Spec {
    let floats = float.unwrap_or(0) * FOUNDER_SEATS.len() as u64;
    let float_line = float.map(|f| format!("operator_float = \"{f}\"\n")).unwrap_or_default();
    let mut t = format!(
        "chain_id = \"{chain}\"\naddress_prefix = \"boo\"\ngenesis_time_ms = 1793491200000\nblock_time_ms = 1000\nepoch_blocks = 3600\ntotal_supply = \"1000000000\"\nprotocol_version = 2\nfeatures = []\n{float_line}{extra}\n\
         [params]\nproposal_deposit = \"10000000000\"\nvoting_blocks = 259200\ntimelock_blocks = 86400\nbootstrap_end_height = 63072000\ndowntime_window = 1200\ndowntime_max_missed = 600\nbootstrap_commission_cap_bps = 0\n"
    );
    let alloc = |bucket: &str, address: Option<String>, amount: u64| format!("[[allocation]]\nbucket = \"{bucket}\"\n{}amount = \"{amount}\"\n", address.map(|a| format!("address = \"{a}\"\n")).unwrap_or_default());
    for (b, amount) in [("community", 320_000_000), ("treasury", 300_000_000), ("endowment", 150_000_000), ("bootstrap", 100_000_000)] {
        t += &alloc(b, None, amount);
    }
    t += &alloc("founder", Some(founder().address_with("boo")), 100_000_000);
    for (seed, ..) in FOUNDER_SEATS {
        t += &alloc("operators", Some(owner(seed).address_with("boo")), 1_000_000);
    }
    t += &alloc("operators", Some(grants().address_with("boo")), 26_000_000 - floats);
    t += WAIVERS;
    toml::from_str(&t).unwrap()
}

/// The launch genesis on `chain`, with 100 FANG floats.
fn launch(chain: &str) -> Genesis {
    build_from(&spec(chain, Some(100), ""), &founder_seats(chain), false).unwrap().genesis
}

/// The build's error, in full.
fn refused(spec: &Spec, ops: &[OperatorFile]) -> String {
    format!("{:#}", build_from(spec, ops, false).err().expect("refused"))
}

/// The state machine from a built genesis, block by block (no node: txs carry dummy signatures,
/// which admission checks before the state machine).
struct Run {
    st: State,
    chain: String,
    /// The block being built.
    h: u64,
    tx_bytes: u64,
}

impl Run {
    fn new(g: &Genesis) -> Run {
        let st = g.initial_state();
        st.check_invariants().unwrap();
        Run { st, chain: g.chain_id.clone(), h: 1, tx_bytes: 0 }
    }

    /// Applies a tx from `k`'s own account in the current block: `Ok`, or why it wasn't included
    /// or failed.
    fn tx(&mut self, k: &Keys, action: Action) -> Result<()> {
        let from = k.address_with("boo");
        let mut tx = Tx { chain_id: self.chain.clone(), genesis_hash: "00".repeat(32), from: from.clone(), nonce: self.st.account(&from).nonce, fee: 0, tip: 0, valid_until: self.h + 10, action, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
        tx.fee = crate::econ::max_fee(&self.st, tx.size());
        self.tx_bytes += tx.size();
        match self.st.apply(&tx, &self.chain, &validator(FOUNDER_SEATS[0].0).address_with("boo"), self.h)? {
            Outcome::Applied => Ok(()),
            Outcome::Failed(e) => bail!(e),
        }
    }

    /// Closes every block up to and including `height` (end-of-block steps and the commit; the
    /// full checks run once, at the end: hundreds of thousands of blocks go by).
    fn through(&mut self, height: u64) {
        let proposer = validator(FOUNDER_SEATS[0].0).address_with("boo");
        while self.h <= height {
            let hash = format!("block-{}", self.h);
            let tx_bytes = std::mem::take(&mut self.tx_bytes);
            self.st.end_block(&BlockInfo { chain_id: &self.chain, height: self.h, version: crate::upgrade::version_at(&self.st, self.h), hash: &hash, proposer: &proposer, view: self.h, parent_view: self.h - 1, skipped: &[], tx_bytes });
            self.st.commit_block().unwrap();
            self.h += 1;
        }
        self.st.debug_check(self.h - 1);
    }

    fn propose(&mut self, k: &Keys, kind: ProposalKind) -> Result<u64> {
        let id = self.st.governance.next_proposal_id;
        self.tx(k, Action::Propose { kind, deposit: 10 * FANG, title: "a proposal".into(), url: String::new() })?;
        Ok(id)
    }

    /// An aye of one FANG (a council vote needs a drop) on proposal `id` as stored.
    fn vote(&mut self, k: &Keys, id: u64) -> Result<()> {
        let digest = self.st.governance.proposals.get(&id).map(crate::governance::digest_of).unwrap_or_else(|| "00".repeat(32));
        self.tx(k, Action::Vote { proposal_id: id, aye: true, amount: FANG, conviction: 0, proposal_digest: digest })
    }

    fn p(&self, id: u64) -> &crate::governance::Proposal {
        &self.st.governance.proposals[&id]
    }
}

/// The genesis validators' consensus keys, which their council votes are signed with.
fn council() -> Vec<Keys> {
    FOUNDER_SEATS.iter().map(|(seed, ..)| validator(*seed)).collect()
}

/// The founder's set builds as `blockula-1` (always strict) only with its waivers, each printed;
/// the genesis is born at v2 with programs off, and the floats are liquid on the consensus-key
/// accounts, inside the operators bucket's 3%.
#[test]
fn the_founder_set_builds_strict_with_its_waivers_on_record() {
    let ops = founder_seats(MAIN);
    let built = build_from(&spec(MAIN, Some(100), ""), &ops, false).unwrap();
    assert!(built.warnings.is_empty(), "{:?}", built.warnings);
    let waived: Vec<&str> = built.waived.iter().map(|w| w.split(':').nth(1).unwrap().trim()).collect();
    assert_eq!(waived, ["operator-minimum", "provider-cap", "asn-cap", "country-cap", "facility-cap", "facility-cap", "facility-cap", "facility-cap", "one-seat-per-entity"]);
    assert_eq!(built.waived[0], "WAIVED: operator-minimum: 4 operators; a mainnet genesis needs at least 22 (DEC-3) (experimental launch: the founder's 4 validators; seats open to stake after launch)");
    assert_eq!(built.waived[3], "WAIVED: country-cap: data-center country us has 2 of the 4 operators; at most 1 may (two US regions (iad, sjc))");
    let g = &built.genesis;
    assert_eq!(g.upgrades, [ScheduledUpgrade { version: 2, activation_height: 1 }]);
    assert!(g.features.is_empty() && g.bootstrap_excluded == [founder().address_with("boo")], "only the founder's own account is excluded");
    let liquid: Vec<&Allocation> = g.allocations.iter().filter(|a| a.vesting.is_none() && system_account("boo", &a.address).is_none()).collect();
    assert_eq!(liquid.len(), 4);
    for (a, v) in liquid.iter().zip(&g.validators) {
        assert_eq!((&a.address, a.amount, a.label.as_deref()), (&v.address, 100 * FANG, Some(OPERATOR_FLOAT_LABEL)));
    }
    let operators: u64 = g.allocations.iter().filter(|a| a.label.as_deref() == Some("operators") || a.label.as_deref() == Some(OPERATOR_FLOAT_LABEL)).map(|a| a.amount).sum();
    assert_eq!(operators, 30_000_000 * FANG, "the floats are inside the operators bucket's 3%");
    let st = g.initial_state();
    assert_eq!(crate::econ::transferable(&st, &g.validators[0].address, 1), 100 * FANG, "liquid from block 1");
    // Without its waivers, the same set is refused, and the error says what may be waived.
    let mut bare = spec(MAIN, Some(100), "");
    bare.waivers.clear();
    let err = refused(&bare, &ops);
    assert!(err.contains("strict") && err.contains("operator-minimum: 4 operators") && err.contains("may waive asn-cap, country-cap, facility-cap, one-seat-per-entity, operator-minimum, provider-cap"), "{err}");
}

/// (a) Governance is live at block 1. On the strict `blockula-1` genesis above, each of the four
/// validators proposes or votes from its consensus-key account, out of its operator float (the
/// deposit is 10 FANG; a council vote needs only a drop). The bootstrap council (the active set,
/// ⅔ of it voting aye) passes a `ScheduleUpgrade` to v3 and `ActivateFeature { programs }` at
/// governance's floors, with a 3-day vote and a 1-day timelock: both run at block 345,601, four
/// days in. Programs are on, and v3 is scheduled where the proposal said: from its height every
/// block runs v3, so this binary (which runs v2) stops cleanly before it, as every node without
/// the next release does, and nodes on that release go on.
#[test]
fn the_council_upgrades_and_switches_programs_on_from_block_1() {
    let g = launch(MAIN);
    let mut r = Run::new(&g);
    let vals = council();
    let cp = r.st.chain_params.clone();
    assert_eq!((cp.proposal_deposit, cp.voting_blocks, cp.timelock_blocks), (10 * FANG, 259_200, 86_400));
    assert_eq!(crate::upgrade::version_at(&r.st, r.h), 2, "born at v2");
    // The earliest activation a ScheduleUpgrade proposed now may name: after its vote and
    // timelock, then room for a CancelUpgrade's vote and the lead.
    let runs_at = r.h + cp.voting_blocks + cp.timelock_blocks;
    let v3 = crate::upgrade::next_version(&r.st);
    let at = runs_at + cp.voting_blocks + crate::upgrade::MIN_UPGRADE_LEAD + 2;
    assert_eq!((v3, runs_at, at), (3, 345_601, 604_867));
    let upgrade = r.propose(&vals[0], ProposalKind::ScheduleUpgrade { version: v3, activation_height: at }).unwrap();
    let programs = r.propose(&vals[1], ProposalKind::ActivateFeature { feature: crate::upgrade::PROGRAMS_FEATURE.into() }).unwrap();
    for v in &vals {
        r.vote(v, upgrade).unwrap();
        r.vote(v, programs).unwrap();
    }
    let seats: Vec<String> = r.p(upgrade).council.keys().cloned().collect();
    assert_eq!(seats, g.validators.iter().map(|v| v.address.clone()).collect::<Vec<_>>(), "each vote counts for its validator's council seat");
    // The votes close at the end of day 3: passed by the council, deposits back.
    let end = r.p(upgrade).voting_end;
    r.through(end);
    for id in [upgrade, programs] {
        assert_eq!(r.p(id).status, ProposalStatus::Queued, "#{id}: {}", r.p(id).outcome);
        assert!(r.p(id).outcome.starts_with("passed by the bootstrap council"), "{}", r.p(id).outcome);
    }
    assert!(r.st.account(&vals[0].address_with("boo")).balance > 99 * FANG, "the deposit is back");
    // They run after the timelock: programs on, v3 scheduled.
    r.through(runs_at);
    for id in [upgrade, programs] {
        assert_eq!(r.p(id).status, ProposalStatus::Executed, "#{id}: {}", r.p(id).outcome);
    }
    assert_eq!(r.st.upgrade.features.get(crate::upgrade::PROGRAMS_FEATURE), Some(&crate::upgrade::Feature { activated_at: runs_at }));
    assert_eq!(r.st.upgrade.schedule, [ScheduledUpgrade { version: 2, activation_height: 1 }, ScheduledUpgrade { version: v3, activation_height: at }]);
    // The upgrade lands: v3 from its height, which this binary doesn't run, so it stops before it.
    assert_eq!((crate::upgrade::version_at(&r.st, at - 1), crate::upgrade::version_at(&r.st, at)), (2, v3));
    let halt = crate::upgrade::HaltControl::for_genesis(&g);
    assert_eq!(halt.supported, crate::consensus::SUPPORTED_PROTOCOL_VERSION);
    assert_eq!(crate::upgrade::check_next_with(&r.st, at - 1, halt.supported), Ok(()));
    assert_eq!(crate::upgrade::check_next_with(&r.st, at, halt.supported), Err(crate::upgrade::Halt::UpgradeRequired { height: at, version: v3, supported: 2 }));
    assert_eq!(crate::upgrade::check_next_with(&r.st, at, v3), Ok(()), "the next release runs it");
}

/// (b) The freeze the float exists for. Without it, every allocation to an account with a key
/// vests (DEC-5): the founder's from a 12-month cliff, the operators' and the grants' from a
/// 6-month one, and the consensus-key accounts get nothing. Unvested tokens neither vote nor pay a
/// deposit, so at block 1 nobody can propose (the deposit is locked, or there's no FANG at all)
/// and nothing could vote on a proposal: every account's voting balance, the council's included,
/// is zero until the operators' cliff (block 15,778,800, about six months in). No upgrade, no
/// feature switch and no parameter change could pass until then.
#[test]
fn without_the_float_nobody_can_propose_or_vote_until_the_operators_cliff() {
    let g = build_from(&spec(MAIN, None, ""), &founder_seats(MAIN), false).unwrap().genesis;
    let mut r = Run::new(&g);
    assert_eq!(crate::governance::unlocked_supply(&r.st, 1), 0, "nothing can vote");
    let keyed: Vec<String> = r.st.accounts.keys().filter(|a| system_account("boo", a).is_none()).cloned().collect();
    assert_eq!(keyed.len(), 6, "the founder, the four owners and the grants: {keyed:?}");
    for a in &keyed {
        assert_eq!((crate::econ::voting_balance(&r.st, a, 1), crate::econ::transferable(&r.st, a, 1)), (0, 0), "{a}");
    }
    let owners: Vec<Keys> = FOUNDER_SEATS.iter().map(|(seed, ..)| owner(*seed)).collect();
    let text = || ProposalKind::Text {};
    for k in council() {
        let e = r.propose(&k, text()).unwrap_err().to_string();
        assert!(e.contains("insufficient balance for the fee"), "{e}");
    }
    for k in owners.iter().chain([&founder(), &grants()]) {
        let e = r.propose(k, text()).unwrap_err().to_string();
        assert!(e.contains("is locked"), "{e}");
    }
    assert_eq!(r.st.governance.next_proposal_id, 0, "no proposal exists");
    // Nor could anyone vote on one, the council included (from a validator's account or its
    // owner's): here's one written into the state.
    let p = crate::governance::Proposal { id: 0, proposer: founder().address_with("boo"), kind: text(), title: "t".into(), url: String::new(), deposit: 0, created_at: 1, voting_end: 1 + r.st.chain_params.voting_blocks, execute_at: None, status: ProposalStatus::Voting, aye_amount: 0, nay_amount: 0, aye_weight: 0, nay_weight: 0, voters: 0, council: Default::default(), tally: None, outcome: String::new() };
    r.st.governance.proposals.insert(0, p);
    for k in council() {
        let e = r.vote(&k, 0).unwrap_err().to_string();
        assert!(e.contains("insufficient balance for the fee"), "{e}");
    }
    for k in owners.iter().chain([&founder(), &grants()]) {
        let e = r.vote(k, 0).unwrap_err().to_string();
        assert!(e.contains("you can vote with up to 0 FANG"), "{e}");
    }
    r.st.governance.proposals.remove(&0);
    // When the freeze ends: the operators' and the grants' cliff, six months of 1 s blocks.
    let cliff = BUCKETS.iter().find(|b| b.name == "operators").unwrap().curve(1000).unwrap().cliff;
    assert_eq!(cliff, 15_778_800);
    for k in owners.iter().chain([&grants()]) {
        let a = k.address_with("boo");
        assert_eq!(crate::econ::voting_balance(&r.st, &a, cliff - 1), 0);
        assert!(crate::econ::voting_balance(&r.st, &a, cliff) > 0);
    }
    assert_eq!(crate::econ::voting_balance(&r.st, &founder().address_with("boo"), cliff), 0, "the founder's cliff is a year");
    // With the float, the same genesis has 400 FANG that vote from block 1.
    assert_eq!(crate::governance::unlocked_supply(&Run::new(&launch(MAIN)).st, 1), 400 * FANG as u128);
    // The refused txs left the state whole.
    r.through(1);
}

/// Ghost Mode stays off on `blockula-1` by rule: it isn't audited, and the council could pass its
/// activation as easily as the programs one. An `ActivateFeature { ghost }` is refused when it's
/// proposed, and one that got there anyway (written into the state here) passes the council and
/// fails when it runs. The same launch on a dry-run chain switches it on.
#[test]
fn ghost_mode_stays_off_on_mainnet_whatever_the_council_votes() {
    let ghost = || ProposalKind::ActivateFeature { feature: crate::ghost::FEATURE.into() };
    let refusal = "blockula-1 doesn't switch ghost on: Ghost Mode isn't audited (a later protocol version lifts this once it is)";
    let vals = council();
    for chain in [MAIN, DRY_RUN] {
        let mut r = Run::new(&launch(chain));
        let id = if chain == MAIN {
            let e = r.propose(&vals[0], ghost()).unwrap_err().to_string();
            assert!(e.contains(refusal), "{e}");
            // As if it had got past `Propose`: a text proposal that turns into the activation.
            let id = r.propose(&vals[0], ProposalKind::Text {}).unwrap();
            r.st.governance.proposals.get_mut(&id).unwrap().kind = ghost();
            id
        } else {
            r.propose(&vals[0], ghost()).unwrap()
        };
        for v in &vals {
            r.vote(v, id).unwrap();
        }
        let runs_at = r.p(id).voting_end + r.st.chain_params.timelock_blocks;
        r.through(runs_at);
        let p = r.p(id);
        assert!(p.tally.as_ref().is_some_and(|t| t.council_ayes == 4), "{chain}: the council passed it: {}", p.outcome);
        let on = crate::upgrade::is_active(&r.st, crate::ghost::FEATURE);
        if chain == MAIN {
            assert_eq!((p.status, p.outcome.as_str(), on), (ProposalStatus::Failed, format!("failed when it ran: {refusal}").as_str(), false));
        } else {
            assert_eq!((p.status, on), (ProposalStatus::Executed, true), "{}", p.outcome);
        }
    }
}

/// (c) Waivers: each names a waivable rule once, with a one-line reason, and waives every finding
/// of that rule, printed as `WAIVED: <rule>: <finding> (<reason>)`. What's left is still refused
/// on `blockula-1` (and warned about elsewhere). A waiver of a rule the set doesn't break is an
/// error, so the waivers on record are exactly what the set breaks; DEC-5, the independence
/// fields and what the builder refuses outright (the faucet, admin keys, the supply, the prefix,
/// governance's floors, Ghost Mode) are never waived.
#[test]
fn waivers_are_named_reasoned_and_never_stale() {
    let ops = founder_seats(MAIN);
    let with = |waivers: &str| -> Spec {
        let mut s = spec(MAIN, Some(100), "");
        s.waivers = toml::from_str::<Spec>(&format!("{}{waivers}", toml_of_head(&s))).unwrap().waivers;
        s
    };
    let all: Vec<&str> = WAIVERS.split("[[waiver]]").skip(1).collect();
    let waiver = |rule: &str, reason: &str| format!("[[waiver]]\nrule = \"{rule}\"\nreason = {reason:?}\n");
    // One left out: its findings are refused, and only those.
    let without_asn: String = all.iter().filter(|w| !w.contains("asn-cap")).map(|w| format!("[[waiver]]{w}")).collect();
    let err = refused(&with(&without_asn), &ops);
    assert!(err.contains("  - asn-cap: ASN 40509 has 4 of the 4 operators; at most 1 may\n(a [[waiver]] with a reason may waive asn-cap)") && !err.contains("provider-cap"), "{err}");
    let full: String = all.iter().map(|w| format!("[[waiver]]{w}")).collect();
    // Stale: the set has no founder seats to waive.
    let err = refused(&with(&format!("{full}{}", waiver("founder-seats", "the founder runs 4"))), &ops);
    assert!(err.contains("waiver \"founder-seats\" matches no finding: remove it"), "{err}");
    // Never waivable: DEC-5, the fields the caps are counted from, and what's refused outright.
    for rule in ["bucket-shares", "opening-balance", "independence-fields"] {
        let err = refused(&with(&format!("{full}{}", waiver(rule, "why not"))), &ops);
        assert!(err.contains(&format!("waiver \"{rule}\": never waivable")), "{err}");
    }
    for rule in ["faucet", "admin-keys", "total-supply", "prefix", "governance-floors", "ghost"] {
        let err = refused(&with(&format!("{full}{}", waiver(rule, "why not"))), &ops);
        assert!(err.contains(&format!("waiver \"{rule}\": never waivable (the builder refuses it outright)")), "{err}");
    }
    let err = refused(&with(&format!("{full}{}", waiver("speed-limit", "fast"))), &ops);
    assert!(err.contains("unknown rule (waivable: operator-minimum, provider-cap, asn-cap, country-cap, facility-cap, one-seat-per-entity, founder-seats, rejoin-trap, fee-headroom, bootstrap-commission)"), "{err}");
    // Once each, with a reason on one line.
    let err = refused(&with(&format!("{full}{}", waiver("asn-cap", "again"))), &ops);
    assert!(err.contains("waiver \"asn-cap\": listed twice"), "{err}");
    for reason in ["", "  ", "two\nlines"] {
        let edited = full.replace("reason = \"all 4 announce from Fly's ASN 40509\"", &format!("reason = {reason:?}"));
        let err = refused(&with(&edited), &ops);
        assert!(err.contains("waiver \"asn-cap\": needs a reason, in one line"), "{reason:?}: {err}");
    }
    assert!(toml::from_str::<Spec>(&format!("{}[[waiver]]\nrule = \"asn-cap\"\nreason = \"r\"\nuntil = 5\n", toml_of_head(&spec(MAIN, None, "")))).is_err(), "unknown fields are refused");
    // Off mainnet a spec that isn't strict warns about what it doesn't waive, and prints what it does.
    let dry = founder_seats(DRY_RUN);
    let built = build_from(&with_chain(&with(&without_asn), DRY_RUN), &dry, false).unwrap();
    assert_eq!(built.warnings, ["asn-cap: ASN 40509 has 4 of the 4 operators; at most 1 may"]);
    assert_eq!(built.waived.len(), 8);
}

/// `s` on another chain (same everything else).
fn with_chain(s: &Spec, chain: &str) -> Spec {
    Spec { chain_id: chain.into(), ..s.clone() }
}

/// The top-level keys of `s` as TOML, enough to parse sections appended to it.
fn toml_of_head(s: &Spec) -> String {
    format!("chain_id = \"{}\"\naddress_prefix = \"{}\"\ngenesis_time_ms = {}\nblock_time_ms = {}\nepoch_blocks = {}\ntotal_supply = \"{}\"\n", s.chain_id, s.address_prefix, s.genesis_time_ms, s.block_time_ms, s.epoch_blocks, s.total_supply)
}

/// (d) `protocol_version`: `blockula-1` is born at v2 (scheduled from block 1), with programs off,
/// or on when the spec lists them; or at v1 with no schedule. Programs need v2 in force at block
/// 1, Ghost Mode stays refused, and no other version is a public network's to start at: not a
/// preview, not one this binary doesn't know.
#[test]
fn mainnet_is_born_at_v2_with_programs_off_or_on() {
    let ops = founder_seats(MAIN);
    let g = launch(MAIN);
    assert_eq!((g.upgrades.clone(), crate::upgrade::supported_version(&g)), (vec![ScheduledUpgrade { version: 2, activation_height: 1 }], 2));
    let st = g.initial_state();
    assert_eq!((crate::upgrade::version_at(&st, 0), crate::upgrade::version_at(&st, 1)), (1, 2), "the genesis block keeps v1; block 1 on run v2");
    assert!(!crate::upgrade::is_active(&st, crate::upgrade::PROGRAMS_FEATURE));
    // Programs on from genesis (an audit came back clean).
    let on = build_from(&programs_on(MAIN), &ops, false).unwrap();
    assert_eq!(on.genesis.features, ["programs"]);
    let st = on.genesis.initial_state();
    st.check_invariants().unwrap();
    assert_eq!(st.upgrade.features.get(crate::upgrade::PROGRAMS_FEATURE), Some(&crate::upgrade::Feature { activated_at: 0 }));
    // Not without v2 from block 1.
    let mut v1 = programs_on(MAIN);
    v1.protocol_version = None;
    assert!(refused(&v1, &ops).contains("unknown feature programs (known: ghost): block 1 runs v1"));
    // Ghost Mode, never.
    let mut ghost = spec(MAIN, Some(100), "");
    ghost.features = vec!["ghost".into()];
    assert!(refused(&ghost, &ops).contains("blockula-1 starts with Ghost Mode off"));
    // v1 (no schedule) is still a genesis this binary builds and runs.
    for v in [None, Some(1)] {
        let mut s = spec(MAIN, Some(100), "");
        s.protocol_version = v;
        let b = build_from(&s, &ops, false).unwrap();
        assert!(b.genesis.upgrades.is_empty() && !b.json.contains("upgrades"), "{v:?}");
    }
    // Nothing else.
    for v in [crate::upgrade::PREVIEW_PROTOCOL_VERSION, 9, 0] {
        let mut s = spec(MAIN, Some(100), "");
        s.protocol_version = Some(v);
        assert!(refused(&s, &ops).contains(&format!("protocol_version {v}: a public network starts at a released version, v1–v2")), "v{v}");
    }
}

/// The launch spec with `features = ["programs"]`.
fn programs_on(chain: &str) -> Spec {
    Spec { features: vec![crate::upgrade::PROGRAMS_FEATURE.into()], ..spec(chain, Some(100), "") }
}

/// Floats: at most `MAX_OPERATOR_FLOAT` each, more than zero, inside the operators bucket's exact
/// share (a spec that forgets to make room for them breaks DEC-5, which nothing waives), and on
/// `blockula-1` the only liquid allocations to an account with a key.
#[test]
fn operator_floats_are_capped_and_inside_the_operators_bucket() {
    let ops = founder_seats(MAIN);
    let g = build_from(&spec(MAIN, Some(1_000), ""), &ops, false).unwrap().genesis;
    assert!(g.allocations.iter().filter(|a| a.label.as_deref() == Some(OPERATOR_FLOAT_LABEL)).all(|a| a.amount == MAX_OPERATOR_FLOAT));
    let mut over = spec(MAIN, Some(1_000), "");
    over.operator_float = Some("1000.000000001".into());
    assert!(refused(&over, &ops).contains("operator_float must be more than 0 and at most 1000 FANG"));
    let mut zero = spec(MAIN, None, "");
    zero.operator_float = Some("0".into());
    assert!(refused(&zero, &ops).contains("operator_float must be more than 0"));
    // Floats on top of a full operators bucket: the supply and the bucket are off.
    let mut extra = spec(MAIN, None, "");
    extra.operator_float = Some("100".into());
    extra.total_supply = "1000000400".into();
    let err = refused(&extra, &ops);
    assert!(err.contains("supply is 1000000000 FANG"), "{err}");
    extra.allocations[0].amount = "319999600".into();
    extra.total_supply = "1000000000".into();
    let err = refused(&extra, &ops);
    assert!(err.contains("bucket-shares: bucket community holds") && err.contains("bucket-shares: bucket operators holds 30000400000000000 drops, operator floats included; DEC-5 gives it 30000000000000000 (3% of the supply)"), "{err}");
    let mut waived = extra.clone();
    waived.waivers.push(SpecWaiver { rule: "bucket-shares".into(), reason: "floats".into() });
    assert!(refused(&waived, &ops).contains("waiver \"bucket-shares\": never waivable"));
}

/// Whether seat `v` comes back into `g`'s set at the first boundary once it's out of it (left, as
/// a jailed seat does, and eligible again: its pool as genesis left it), the other seats holding
/// theirs: what `staking::select_next` does with it.
fn rejoins(g: &Genesis, v: &str) -> bool {
    let mut st = g.initial_state();
    let rest: Vec<String> = st.active_sets[&0].iter().filter(|x| *x != v).cloned().collect();
    st.active_sets.insert(0, rest);
    assert!(crate::staking::eligible(&st, v));
    crate::staking::select_next(&st, 0).joining == [v.to_string()]
}

/// The staking rules a genesis set can trip over from block 1 are findings, with the staking
/// code's own verdict on each shape. A seat that leaves the set (jailed) comes back as a
/// newcomer: the launch's seats, unflagged with an owner each, do; seats flagged as the
/// founder's in a set of 4 never do (no such seat comes back below 7 members, and 4 of them at no
/// size), and nor does a seat whose vesting owner holds another seat. An owner whose self-bond
/// leaves nothing unstaked can't pay its validator's beacon duties. All of them are waivable.
#[test]
fn seats_that_couldnt_rejoin_or_pay_their_duties_are_findings() {
    let launch_g = launch(DRY_RUN);
    for v in &launch_g.validators {
        assert!(rejoins(&launch_g, &v.address), "a launch seat rejoins");
    }
    let findings = |ops: &[OperatorFile]| -> (Genesis, Vec<String>) {
        let mut s = spec(DRY_RUN, Some(100), "");
        s.waivers.clear();
        let b = build_from(&s, ops, false).unwrap();
        (b.genesis, b.warnings.into_iter().filter(|w| w.starts_with("rejoin-trap") || w.starts_with("fee-headroom")).collect())
    };
    assert!(findings(&founder_seats(DRY_RUN)).1.is_empty());
    // Flagged as the founder's: excluded from the bootstrap delegation, and capped as newcomers.
    let flagged: Vec<OperatorFile> = (0..4).map(|i| seat(DRY_RUN, i, &owner(FOUNDER_SEATS[i].0), "10000", true)).collect();
    let (g, f) = findings(&flagged);
    assert_eq!(g.bootstrap_excluded.len(), 5);
    assert!(g.validators.iter().all(|v| !rejoins(&g, &v.address)));
    assert_eq!(f.len(), 1, "{f:?}");
    assert!(f[0].starts_with("rejoin-trap: 4 of the 4 seats (Blockula validator ") && f[0].contains("a set of 4 holds at most 0 of them once one is a newcomer (staking.rs excluded_seats_cap): one that leaves the set (jailed for downtime, or for a beacon commitment it didn't reveal) can't rejoin while the others hold theirs, at any set size (at most 2 such seats ever come back). Leave founder-run seats unflagged, each with its own operators-bucket owner"), "{}", f[0]);
    // One flagged seat: it comes back once the set has 7 members.
    let mut one = founder_seats(DRY_RUN);
    one[2] = seat(DRY_RUN, 2, &owner(FOUNDER_SEATS[2].0), "10000", true);
    let (g, f) = findings(&one);
    let v2 = validator(FOUNDER_SEATS[2].0).address_with("boo");
    assert!(!rejoins(&g, &v2) && f.len() == 1 && f[0].starts_with("rejoin-trap: 1 of the 4 seats (Blockula validator fra)") && f[0].contains("can't rejoin until the set has 7 members"), "{f:?}");
    // Two seats, one vesting owner: neither comes back while the other holds its seat.
    let mut shared = founder_seats(DRY_RUN);
    shared[1] = seat(DRY_RUN, 1, &owner(FOUNDER_SEATS[0].0), "10000", false);
    let (g, f) = findings(&shared);
    let (v0, v1) = (validator(FOUNDER_SEATS[0].0).address_with("boo"), validator(FOUNDER_SEATS[1].0).address_with("boo"));
    assert!(!rejoins(&g, &v0) && !rejoins(&g, &v1));
    assert_eq!(f.len(), 1, "{f:?}");
    let both = f[0].starts_with("rejoin-trap: 2 seats (") && f[0].contains("Blockula validator iad") && f[0].contains("Blockula validator sjc");
    assert!(both && f[0].contains(&format!("share the owner {}, whose allocation vests until block 63115200: an owner with unvested tokens brings back one seat", owner(FOUNDER_SEATS[0].0).address_with("boo"))) && f[0].ends_with("Give each seat its own owner"), "{}", f[0]);
    // Everything bonded: nothing left for the beacon's fees.
    let mut bonded = founder_seats(DRY_RUN);
    bonded[3] = seat(DRY_RUN, 3, &owner(FOUNDER_SEATS[3].0), "1000000", false);
    let (_, f) = findings(&bonded);
    assert_eq!(f, [format!("fee-headroom: the owner {} of Blockula validator ams keeps 0 FANG unstaked after its self-bond: its node pays the validator's system txs (beacon commitments and reveals, attestations) from the owner, and a commitment it can't follow with a reveal jails and slashes the validator (beacon.rs end_epoch). Leave at least 10 FANG unbonded", owner(FOUNDER_SEATS[3].0).address_with("boo"))]);
    // Waivable, on record, on blockula-1 too.
    let main: Vec<OperatorFile> = (0..4).map(|i| seat(MAIN, i, &owner(FOUNDER_SEATS[i].0), if i == 3 { "1000000" } else { "10000" }, false)).collect();
    let mut s = spec(MAIN, Some(100), "");
    assert!(refused(&s, &main).contains("  - fee-headroom: the owner"));
    s.waivers.push(SpecWaiver { rule: "fee-headroom".into(), reason: "its node pays from the consensus key's float (BLOCKULA_SYSTEM_FEES=self)".into() });
    let b = build_from(&s, &main, false).unwrap();
    assert!(b.waived.last().unwrap().starts_with("WAIVED: fee-headroom: the owner ") && b.waived.last().unwrap().ends_with("(its node pays from the consensus key's float (BLOCKULA_SYSTEM_FEES=self))"));
}

/// DEC-5 gives the bootstrap delegation's rewards to the treasury, but the delegation backs the
/// founder's seats (unflagged, so each can rejoin the set), and commission is each owner's to set
/// without a vote: one `UpdateValidator` an epoch, `commission_max_change_bps` (1%) at a time. On
/// the delegation's stake an owner keeps at most `bootstrap_commission_cap_bps` of it, so at the
/// default 10% ten such txs would hand the founder's owners a tenth of the delegation's yield
/// (about 4M FANG in year 1). The launch sets the cap to 0, so the treasury gets all of it
/// whatever they charge. A spec that leaves the cap above 0 while the delegation backs several
/// seats of one entity is a finding, which `blockula-1` refuses unless it's waived on record.
#[test]
fn the_founder_seats_take_no_commission_on_the_bootstrap_delegation() {
    let ops = founder_seats(MAIN);
    let mut capped_at_10 = spec(MAIN, Some(100), "");
    capped_at_10.params.remove("bootstrap_commission_cap_bps");
    let err = refused(&capped_at_10, &ops);
    let finding = "bootstrap-commission: the bootstrap delegation backs 4 seats of blockula labs (Blockula validator iad, Blockula validator sjc, Blockula validator fra, Blockula validator ams), and bootstrap_commission_cap_bps is 10%: each seat's owner may raise its commission without a vote (UpdateValidator, 1% an epoch) and keep up to 10% of what its share of the delegation earns, which DEC-5 gives the treasury, one share per seat. Set params.bootstrap_commission_cap_bps = 0 (a validators' parameter: only a token supermajority changes it)";
    assert!(err.contains(&format!("  - {finding}\n")) && err.contains("may waive bootstrap-commission"), "{err}");
    capped_at_10.waivers.push(SpecWaiver { rule: "bootstrap-commission".into(), reason: "the founder's seats keep an operator's pay".into() });
    let built = build_from(&capped_at_10, &ops, false).unwrap();
    assert_eq!(built.waived.last().unwrap(), &format!("WAIVED: {finding} (the founder's seats keep an operator's pay)"));
    // Seats flagged as the founder's aren't backed at all, and one seat per entity is DEC-5's
    // operator pay: neither is a finding.
    let mut s = spec(DRY_RUN, Some(100), "");
    s.params.remove("bootstrap_commission_cap_bps");
    s.waivers.clear();
    let warned = |ops: &[OperatorFile]| -> Vec<String> { build_from(&s, ops, false).unwrap().warnings.into_iter().filter(|w| w.starts_with("bootstrap-commission")).collect() };
    let flagged: Vec<OperatorFile> = (0..4).map(|i| seat(DRY_RUN, i, &owner(FOUNDER_SEATS[i].0), "10000", true)).collect();
    assert!(warned(&flagged).is_empty());
    let independent: Vec<OperatorFile> = (0..4).map(|i| OperatorFile { independence: Independence { legal_entity: format!("Operator {i} GmbH"), ..founder_seats(DRY_RUN)[i].independence.clone() }, ..founder_seats(DRY_RUN)[i].clone() }).collect();
    assert!(warned(&independent).is_empty());
    assert_eq!(warned(&founder_seats(DRY_RUN)).len(), 1);
    // What it's worth: seat iad's owner raises its commission to 1% in block 1, with no vote (sjc's
    // stays at 0). The delegation is staked with the four seats at the first boundary and earns
    // from the next one on. Under the default cap iad's owner keeps that 1% of its share's
    // rewards; at 0 it all goes to the treasury, and the owner keeps what its own stake earns.
    let run = |g: &Genesis| -> (i128, u64) {
        let mut r = Run::new(g);
        let (raised, kept) = (FOUNDER_SEATS[0].0, FOUNDER_SEATS[1].0);
        r.tx(&owner(raised), Action::UpdateValidator { commission_bps: 100, p2p_addr: String::new(), http_addr: String::new() }).unwrap();
        let treasury = r.st.treasury();
        let before = r.st.account(&treasury).balance;
        r.through(3 * g.epoch_blocks);
        let owns = |n: u8| crate::staking::delegation_value(&r.st, &owner(n).address_with("boo"), &validator(n).address_with("boo"));
        (owns(raised) as i128 - owns(kept) as i128, r.st.account(&treasury).balance - before)
    };
    let (at_0, treasury_at_0) = run(&launch(MAIN));
    let (at_10, treasury_at_10) = run(&built.genesis);
    // Two epochs of the delegation's rewards, about 1,200 FANG each in iad's pool: 1% is ~24 FANG,
    // the treasury's at 0, the owner's at 10%.
    assert!(at_0.unsigned_abs() < (FANG / 100) as u128, "at 0 the owner keeps what its own stake earns, to the drop: {at_0}");
    assert!(at_10 > (20 * FANG) as i128, "under the default cap the owner takes 1% of its share's rewards: {at_10}");
    assert!(treasury_at_0 > treasury_at_10 && ((treasury_at_0 - treasury_at_10) as i128 - (at_10 - at_0)).abs() < (FANG / 100) as i128, "what the owner takes is the treasury's: {treasury_at_0} {treasury_at_10}");
}

/// On `blockula-1` the operator floats are the only liquid allocations to accounts with a key
/// (DEC-5). No spec can make another today (the faucet is refused, every keyed bucket vests and
/// the pool's opening balance goes to a keyless account), so the builder's guard is fed here what
/// a spec can't give it.
#[test]
fn only_the_floats_are_liquid_on_mainnet() {
    assert!(BUCKETS.iter().all(|b| b.system.is_some() || b.vesting.is_some()), "every keyed bucket vests");
    let g = launch(MAIN);
    check_only_floats_liquid("boo", &g.allocations, &g.validators).unwrap();
    let with = |address: String, label: &str| {
        let mut all = g.allocations.clone();
        all.push(Allocation { address, amount: FANG, vesting: None, label: Some(label.into()) });
        check_only_floats_liquid("boo", &all, &g.validators)
    };
    // A liquid allocation to a keyed account: refused, whatever its label (a float is a
    // validator's consensus-key account's).
    let f = founder().address_with("boo");
    assert_eq!(with(f.clone(), "founder").unwrap_err().to_string(), format!("blockula-1: {f} gets a liquid allocation; only the operator floats are liquid (DEC-5)"));
    assert!(with(f, OPERATOR_FLOAT_LABEL).is_err());
    assert!(with(owner(FOUNDER_SEATS[0].0).address_with("boo"), OPERATOR_FLOAT_LABEL).is_err(), "an owner's account isn't the validator's");
    // A keyless system account may hold liquid FANG (the community bucket does).
    with(community("boo"), "community").unwrap();
}

/// What `Genesis::check` refuses on `blockula-1`, so `Chain::open` (and `blockula validator init`
/// and `join`, which load the genesis the same way) for a genesis edited by hand after the
/// builder: any schedule but v2 from block 1, and programs on from genesis unless block 1 runs v2.
#[test]
fn a_hand_edited_mainnet_genesis_is_refused() {
    let g = launch(MAIN);
    g.check().unwrap();
    let at = |version, activation_height| ScheduledUpgrade { version, activation_height };
    for upgrades in [vec![at(3, 1)], vec![at(2, 500)], vec![at(2, 1), at(3, 9)], vec![at(1, 1)]] {
        let e = format!("{:#}", Genesis { upgrades: upgrades.clone(), ..g.clone() }.check().unwrap_err());
        assert!(e.contains("blockula-1 starts at protocol v1, or is born at v2 (upgrades: exactly v2 from block 1)"), "{upgrades:?}: {e}");
    }
    let e = format!("{:#}", Genesis { upgrades: vec![], features: vec![crate::upgrade::PROGRAMS_FEATURE.into()], ..g.clone() }.check().unwrap_err());
    assert_eq!(e, "unknown feature programs (known: ghost): block 1 runs v1");
    Genesis { features: vec![crate::upgrade::PROGRAMS_FEATURE.into()], ..g.clone() }.check().unwrap();
    // A node won't open on one.
    let home = std::env::temp_dir().join(format!("blockula-mainnet-bad-schedule-{}-{}", crate::chain::now_ms(), std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("genesis.json"), serde_json::to_string_pretty(&Genesis { upgrades: vec![at(2, 500)], ..g }).unwrap()).unwrap();
    let e = format!("{:#}", Chain::open(&home).err().expect("refused"));
    assert!(e.contains("is born at v2"), "{e}");
    std::fs::remove_dir_all(home).ok();
}

/// A home on `g`, opened as the first seat's validator (the node signs blocks with its key).
fn node(tag: &str, g: &Genesis) -> (std::path::PathBuf, Chain) {
    let home = std::env::temp_dir().join(format!("blockula-mainnet-{tag}-{}-{}", crate::chain::now_ms(), std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("genesis.json"), serde_json::to_string_pretty(g).unwrap()).unwrap();
    crate::chain::write_secret(&home.join("validator.key"), &hex::encode([FOUNDER_SEATS[0].0; 32])).unwrap();
    let c = Chain::open(&home).unwrap();
    (home, c)
}

/// A tx `k` signs (ML-DSA-65) from its own account against the node's head, as a wallet or the
/// CLI builds one.
fn signed(c: &Chain, k: &Keys, action: Action) -> Tx {
    let from = k.address_with("boo");
    let mut tx = Tx { chain_id: c.genesis.chain_id.clone(), genesis_hash: c.genesis_hash.clone(), nonce: c.next_nonce(&from), from, fee: 0, tip: 0, valid_until: c.height() + 100, action, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
    tx.fee = crate::econ::max_fee(&c.state, tx.size());
    tx.sign(k)
}

/// (a) through a node. The council's members sign `Propose` and `Vote` from their consensus-key
/// accounts with their real keys; the node checks the signatures, admits the txs to its mempool
/// and runs them in blocks (`Chain::execute`, `State::run_block`), each vote taking its
/// validator's council seat, and a rebuild from the block logs replays them to the same state. A
/// Ghost Mode activation is refused at admission, and one that reached execution anyway (written
/// into the state here, as if it had got past `Propose`) fails in the block that runs it: the node
/// hands governance `blockula-1`'s chain id. The two proposals then pass and run as in (a).
#[test]
fn the_council_proposes_and_votes_through_a_node() {
    let g = launch(MAIN);
    let (home, mut c) = node("council", &g);
    let vals = council();
    let propose = |kind| Action::Propose { kind, deposit: 10 * FANG, title: "a proposal".into(), url: String::new() };
    let ghost = || ProposalKind::ActivateFeature { feature: crate::ghost::FEATURE.into() };
    let refusal = "blockula-1 doesn't switch ghost on: Ghost Mode isn't audited (a later protocol version lifts this once it is)";
    let e = c.submit(signed(&c, &vals[0], propose(ghost()))).unwrap_err().to_string();
    assert!(e.contains(refusal), "{e}");
    assert!(c.mempool.is_empty());
    // Block 1: the next version and programs, proposed by two members.
    let cp = c.state.chain_params.clone();
    let runs_at = 1 + cp.voting_blocks + cp.timelock_blocks;
    let (v3, at) = (crate::upgrade::next_version(&c.state), runs_at + cp.voting_blocks + crate::upgrade::MIN_UPGRADE_LEAD + 2);
    c.submit(signed(&c, &vals[0], propose(ProposalKind::ScheduleUpgrade { version: v3, activation_height: at }))).unwrap();
    c.submit(signed(&c, &vals[1], propose(ProposalKind::ActivateFeature { feature: crate::upgrade::PROGRAMS_FEATURE.into() }))).unwrap();
    c.produce_block().unwrap();
    assert_eq!((c.block(1).unwrap().version, c.block(1).unwrap().txs.len()), (2, 2), "block 1 runs v2 and carries both");
    let (upgrade, programs) = (0, 1);
    // Block 2: every member votes on both, from its own account.
    for v in &vals {
        for id in [upgrade, programs] {
            let digest = crate::governance::digest_of(&c.state.governance.proposals[&id]);
            c.submit(signed(&c, v, Action::Vote { proposal_id: id, aye: true, amount: FANG, conviction: 0, proposal_digest: digest })).unwrap();
        }
    }
    c.produce_block().unwrap();
    let block = c.block(2).unwrap();
    assert!(block.txs.len() == 8 && block.txs.iter().all(|t| c.failure(&t.id()).is_none()));
    for id in [upgrade, programs] {
        let seats: Vec<(String, String)> = c.state.governance.proposals[&id].council.iter().map(|(v, cv)| (v.clone(), cv.voter.clone())).collect();
        assert_eq!(seats, g.validators.iter().map(|v| (v.address.clone(), v.address.clone())).collect::<Vec<_>>(), "each member's own seat, cast from its consensus-key account");
    }
    // A rebuild from the block logs replays both blocks to the same state.
    let root = c.state.root();
    drop(c);
    std::fs::remove_file(home.join(crate::store::DB_FILE)).unwrap();
    let mut c = Chain::open(&home).unwrap();
    assert_eq!((c.replayed, c.state.root()), (2, root));
    // Ghost Mode at execution: queued to run in block 3, it fails there.
    let ghost_id = c.state.governance.next_proposal_id;
    let tally = crate::governance::Tally { unlocked: 0, quorum: 0, quorum_reached: false, supermajority: false, council_size: 4, council_ayes: 4, council_nays: 0 };
    let p = crate::governance::Proposal { id: ghost_id, proposer: vals[0].address_with("boo"), kind: ghost(), title: "t".into(), url: String::new(), deposit: 10 * FANG, created_at: 1, voting_end: 2, execute_at: Some(3), status: ProposalStatus::Queued, aye_amount: 0, nay_amount: 0, aye_weight: 0, nay_weight: 0, voters: 0, council: Default::default(), tally: Some(tally), outcome: String::new() };
    c.state.governance.proposals.insert(ghost_id, p);
    c.state.governance.open_proposals.insert(ghost_id);
    c.state.governance.next_proposal_id += 1;
    c.produce_block().unwrap();
    let p = &c.state.governance.proposals[&ghost_id];
    assert_eq!((p.status, p.outcome.as_str()), (ProposalStatus::Failed, format!("failed when it ran: {refusal}").as_str()));
    assert!(!crate::upgrade::is_active(&c.state, crate::ghost::FEATURE));
    // The two proposals pass and run, the node's state carried on block by block.
    let mut r = Run { st: c.state.clone(), chain: MAIN.into(), h: c.height() + 1, tx_bytes: 0 };
    r.through(runs_at);
    for id in [upgrade, programs] {
        assert_eq!(r.p(id).status, ProposalStatus::Executed, "#{id}: {}", r.p(id).outcome);
    }
    assert!(crate::upgrade::is_active(&r.st, crate::upgrade::PROGRAMS_FEATURE));
    assert_eq!(r.st.upgrade.schedule, [ScheduledUpgrade { version: 2, activation_height: 1 }, ScheduledUpgrade { version: v3, activation_height: at }]);
    drop(c);
    std::fs::remove_dir_all(home).ok();
}
