//! The harnesses' own regressions, on stable Rust like `tests/seeds.rs`: inputs a harness must
//! pass over without panicking (so none of them is a seed, since a seed is accepted), and its checks' verdicts. A
//! harness that panics on input the node handles correctly reports a "finding" that isn't one.

use anyhow::anyhow;
use blockula_fuzz::chain::Outcome;
use blockula_fuzz::crypto::Keys;
use blockula_fuzz::harness;
use blockula_fuzz::types::{Action, GHOST_DIGEST_BYTES, GHOST_ENC_BYTES, GHOST_OUT_BYTES, GhostBundle, HexBytes, ProposalKind};
use std::collections::BTreeSet;

/// A ghost bundle the action decoder takes: every fixed-size field filled (`fill`, then counting
/// up, so no two are alike) and a proof of zero bytes.
fn zero_proof_bundle(fill: u8) -> Box<GhostBundle> {
    let mut next = fill;
    let mut field = |len: usize| {
        next = next.wrapping_add(1);
        HexBytes(vec![next; len])
    };
    let (d, e, o) = (GHOST_DIGEST_BYTES, GHOST_ENC_BYTES, GHOST_OUT_BYTES);
    Box::new(GhostBundle { instance: field(d), anchor: field(d), nullifiers: [field(d), field(d)], commitments: [field(d), field(d)], enc: [field(e), field(e)], out: [field(o), field(o)], proof: HexBytes(vec![]) })
}

/// Review F-1: a private transfer or an unshield travels only unsigned, and `Tx::to_bytes` gives
/// it the unsigned form, not the signed envelope `program_tx_codec` puts every input in. The
/// harness used to hold the envelope's bytes against it and panic on any such action the decoder
/// takes; it must pass over them (they aren't programs' actions), in both input forms.
#[test]
fn program_tx_codec_passes_over_unsigned_ghost_actions() {
    let to = Keys::from_seed([0x44; 32]).address();
    for action in [Action::PrivateTransfer { bundle: zero_proof_bundle(0x10) }, Action::Unshield { to, amount: 7, bundle: zero_proof_bundle(0x20) }] {
        let kind = action.kind();
        let wire = action.to_bytes();
        assert_eq!(Action::from_bytes(&wire).ok().as_ref(), Some(&action), "{kind}: the action decoder must take the input, or this tests nothing");
        assert!(!harness::run("program_tx_codec", &wire), "{kind}: not one of programs' actions, so not accepted");
        assert!(!harness::run("program_tx_codec", &serde_json::to_vec(&action).unwrap()), "{kind} (JSON): not one of programs' actions, so not accepted");
    }
}

/// The refusal dormancy owes `action`, in `programs::admit`'s words.
fn programs_off(kind: &str) -> String {
    format!("programs are off: a {kind} transaction is refused until protocol v2 is in force and governance switches the `programs` feature on")
}

/// Review F-4: on a dormant chain, a program action or `program_*` parameter change that's well
/// formed in the envelope must be refused by programs' own rules, in their words; a refusal of any
/// other kind (which plain `is_err()` let through) is a finding. One that isn't well formed may be
/// refused for that.
#[test]
fn program_tx_codec_holds_dormancy_to_programs_refusal() {
    let check = |action: Action, outcome: anyhow::Result<Outcome>| harness::check_dormant_refusal(&harness::program_tx(action), &outcome);
    let param = Action::Propose { kind: ProposalKind::ParamChange { key: "program_max_tx_fuel".into(), value: 5_000_000 }, deposit: 1_000_000_000_000, title: "Lower the fuel a program call may sign for".into(), url: String::new() };
    let nested = Action::MultisigPropose { action: Box::new(param.clone()) };
    let owed = [
        (Action::ProgramsPause {}, Err(anyhow!(programs_off("programs_pause")))),
        (param.clone(), Err(anyhow!("unknown parameter `program_max_tx_fuel`"))),
        (nested.clone(), Err(anyhow!("unknown parameter `program_max_tx_fuel`").context("the proposed action"))),
    ];
    for (action, refusal) in owed {
        let kind = action.kind();
        assert!(harness::program_tx(action.clone()).check_format().is_ok(), "{kind}: well formed, or this tests nothing");
        assert_eq!(check(action.clone(), refusal), Ok(()), "{kind}: programs' own refusal");
        assert!(check(action.clone(), Err(anyhow!("bad nonce: expected 0, got 1"))).is_err(), "{kind}: refused for something else");
        assert!(check(action.clone(), Err(anyhow!(programs_off("call_program")))).is_err(), "{kind}: another kind's refusal");
        assert!(check(action.clone(), Ok(Outcome::Applied)).is_err(), "{kind}: admitted");
        assert!(check(action, Ok(Outcome::Failed("the handler refused it".into()))).is_err(), "{kind}: admitted, its action failing");
    }
    // Not well formed (a key no node knows, which the codec refuses): any refusal will do.
    let unknown = Action::Propose { kind: ProposalKind::ParamChange { key: "program_warp".into(), value: 1 }, deposit: 1_000_000_000_000, title: "Warp".into(), url: String::new() };
    assert!(harness::program_tx(unknown.clone()).check_format().is_err());
    assert_eq!(check(unknown.clone(), Err(anyhow!("bad nonce: expected 0, got 1"))), Ok(()));
    assert!(check(unknown, Ok(Outcome::Applied)).is_err(), "admitted all the same");
}

/// The pinned refusal is no dead branch: every kind of programs' action has a seed that's well
/// formed in the envelope, so `fresh_seeds_are_accepted` and `committed_seeds_are_accepted` hold
/// each one's refusal on both dormant chains to programs' words.
#[test]
fn every_kind_of_programs_action_has_a_well_formed_seed() {
    let mut kinds = BTreeSet::new();
    for (target, _, bytes) in harness::seeds() {
        if target != "program_tx_codec" {
            continue;
        }
        // The wire seeds (each has a JSON twin of the same action).
        let Ok(action) = Action::from_bytes(&bytes) else { continue };
        let tx = harness::program_tx(action);
        if tx.check_format().is_ok() && tx.check_prefix(&harness::genesis().address_prefix).is_ok() {
            kinds.insert(match &tx.action {
                Action::MultisigPropose { action } => format!("multisig_propose of {}", action.kind()),
                a => a.kind().to_string(),
            });
        }
    }
    let all = ["upload_code", "create_program", "call_program", "programs_pause", "propose", "multisig_propose of propose"];
    assert_eq!(kinds, all.into_iter().map(String::from).collect(), "programs' actions with a well-formed seed");
}

/// `state_machine` is the v1 fixture and nothing else: its four seeds are the ones the target has
/// always had (byte for byte: pinned by their hashes), they run no program transaction, and an
/// input that starts with what the v2 fixture's inputs once started with is only a v1 op stream.
#[test]
fn state_machine_is_the_v1_fixture_as_it_was() {
    let seeds = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("seeds/state_machine");
    // The first 16 hex digits of blake3 of each seed at the commit before programs' fuzz harness.
    let pinned = [("hunts-and-agents", "7269e68b3a42a5f0"), ("raw-actions", "8addcc7405ccc63e"), ("seance", "e62c49161ebd1b9d"), ("validators-and-v1", "5fc4a40deb5126eb")];
    let mut found: Vec<String> = std::fs::read_dir(&seeds).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    found.sort();
    assert_eq!(found, pinned.map(|(name, _)| name.to_string()), "seeds/state_machine holds the four v1 seeds and nothing else");
    for (name, want) in pinned {
        let bytes = std::fs::read(seeds.join(name)).unwrap();
        let got = hex::encode(&blockula_fuzz::crypto::hash(&bytes)[..8]);
        assert_eq!(got, want, "seeds/state_machine/{name} is not the seed v1 has always had");
        assert!(bytes[0] < 36, "a v1 seed starts with an op's kind");
    }
    // And they run as they always did: the blocks, the transactions applied, and the digest of every
    // block's root, as the harness of 9e8cd806 (the commit before programs' fuzzing) gave them.
    for (name, blocks, applied, roots) in [("hunts-and-agents", 10, 13, "b891ad379ab6a29c"), ("raw-actions", 5, 7, "ab1be50e743ac9cd"), ("seance", 34, 37, "43a250f20d72f6d6"), ("validators-and-v1", 6, 9, "3f6c38fe337643f1")] {
        let (trace, ran) = harness::state_machine_trace(&std::fs::read(seeds.join(name)).unwrap());
        assert_eq!((trace.len(), ran, hex::encode(&blockula_fuzz::crypto::hash(&trace.concat())[..8])), (blocks, applied, roots.to_string()), "{name} runs otherwise than it did at 9e8cd806");
    }
    // The old v2 header is no header here: a v1 stream whose first bytes are letters.
    let header_like = [&b"BLKv2PRG"[..], &[40, 0, 0, 0, 0], &[41, 0, 0, 0, 0]].concat();
    let _ = harness::run("state_machine", &header_like);
}

/// The fixture's four profiles are chosen by an input's first byte, modulo four, and an empty input
/// is the first.
#[test]
fn the_profile_is_the_first_byte() {
    let letters: Vec<char> = [0u8, 1, 2, 3, 4, 7, 255].iter().map(|b| harness::state_machine_programs_profile(&[*b, 40, 0, 0, 0, 0])).collect();
    assert_eq!(letters, ['G', 'T', 'X', 'P', 'G', 'P', 'P']);
    assert_eq!(harness::state_machine_programs_profile(&[]), 'G');
}

/// The fixture's seeds do what their names say: the program transactions they end with, by kind
/// and outcome (`state_machine_programs_receipts`, which also runs each as the determinism rerun
/// does, through `State::run_block`, and holds the two to one root). A name that stops being true
/// means the seed went stale.
#[test]
fn the_v2_seeds_reach_what_they_name() {
    let seeds: std::collections::BTreeMap<&str, Vec<u8>> = harness::state_machine_programs_seeds().into_iter().collect();
    let reach = |name: &str| harness::state_machine_programs_receipts(&seeds[name]);
    let expect = |name: &str, wanted: &[(&str, &str)]| {
        let reached = reach(name);
        for (kind, code) in wanted {
            assert!(reached.get(&(*kind, *code)).is_some_and(|n| *n > 0), "{name} should end a {kind} as {code}: {reached:?}");
        }
        assert!(harness::state_machine_programs(&seeds[name]), "{name}: nothing applied");
    };
    expect("programs-counter-and-token", &[("call_program", "ok"), ("call_program", "aborted")]);
    expect("programs-router", &[("call_program", "ok"), ("call_program", "trapped")]);
    expect("programs-escrow", &[("call_program", "ok"), ("call_program", "aborted")]);
    expect("programs-batch", &[("call_program", "ok"), ("call_program", "aborted")]);
    expect("programs-create-pay-upload", &[("create_program", "ok"), ("upload_code", "ok"), ("upload_code", "invalid_code"), ("upload_code", "interface_mismatch")]);
    expect("programs-value-cap", &[("call_program", "ok"), ("call_program", "value_cap")]);
    expect("programs-exit-only", &[("call_program", "ok")]);
    // A pause stops everything after it: the one call before it is the only receipt.
    assert_eq!(reach("programs-pause").values().sum::<usize>(), 1, "{:?}", reach("programs-pause"));
    // In exit-only, nothing that carries FANG or stores code ran.
    assert_eq!(reach("programs-exit-only").values().sum::<usize>(), 2, "{:?}", reach("programs-exit-only"));
    // One transaction settling several deals (payouts to three accounts, one new), and a batch of
    // refunds not all of whose callers are the seller, which aborts whole.
    assert_eq!(reach("programs-escrow-batch"), [(("call_program", "ok"), 6), (("call_program", "aborted"), 1)].into());
    // Batches of every size, each one landing.
    assert_eq!(reach("programs-batch-sizes").get(&("call_program", "ok")), Some(&9));
    // The fee's edge: the exact charge applies (three calls, an upload, a create), a drop less and an
    // idle chain's fee are refused and leave no receipt.
    assert_eq!(reach("programs-fee-edge"), [(("call_program", "ok"), 3), (("create_program", "ok"), 1), (("upload_code", "ok"), 1)].into());
    // The tight block's fuel: one call for the most a transaction may sign fits, the second doesn't;
    // the next block takes one of two for half the budget, and a call for one unit of fuel runs out.
    expect("programs-fuel-budget", &[("call_program", "ok"), ("call_program", "trapped")]);
    assert_eq!(reach("programs-fuel-budget"), [(("call_program", "ok"), 2), (("call_program", "trapped"), 1)].into());
    // Governance: exit-only with calls in flight, back on, a value cap of nothing.
    expect("programs-governance", &[("call_program", "ok"), ("call_program", "value_cap")]);
    // The pause over its lifetime: the calls that ran are those outside a pause, and the second run
    // of pauses (started at block 690, a day of 10 blocks) was signed by two validators, renewed by
    // two others.
    assert_eq!(reach("programs-pause-lifetime"), [(("call_program", "ok"), 5)].into());
    let (height, pause, _rate, on) = harness::state_machine_programs_end(&seeds["programs-pause-lifetime"]);
    assert_eq!((height, pause, on), (773, Some((690, 690, 721, 2)), true));
    // The long one is long, and has every kind of outcome a long run has.
    assert!(seeds["programs-long"].len() >= 1_400, "the long seed is {} bytes", seeds["programs-long"].len());
    expect("programs-long", &[("call_program", "ok"), ("call_program", "aborted"), ("call_program", "trapped"), ("call_program", "native"), ("create_program", "ok"), ("upload_code", "invalid_code")]);
}

/// The texts the oracles read in admission's refusals are still said (an oracle that matches on a
/// message nobody writes any more checks nothing): the fee below its charge, and the block's fuel
/// spent.
#[test]
fn the_refusals_the_oracles_read_are_still_said() {
    let seeds: std::collections::BTreeMap<&str, Vec<u8>> = harness::state_machine_programs_seeds().into_iter().collect();
    let said = |name: &str, text: &str| harness::state_machine_programs_refusals(&seeds[name]).iter().filter(|r| r.contains(text)).count();
    // The fee seed signs four fees short of the charge: a call a drop short, one at an idle chain's
    // fee, and an upload and a create a drop short.
    assert_eq!(said("programs-fee-edge", "fee too low"), 4, "{:?}", harness::state_machine_programs_refusals(&seeds["programs-fee-edge"]));
    assert_eq!(said("programs-fuel-budget", "the block's program fuel is spent"), 2);
}

/// `program_commit`'s seeds take their diffs in as they say, the cap and exit-only refuse the two
/// that raise the control, and an overdraft or a program that isn't one is refused by `apply_diff`,
/// whatever else the state keeps (it's rolled back to its root).
#[test]
fn program_commit_seeds_and_refusals() {
    use harness::Commit::{Kept, Refused, Rejected};
    for (name, seed) in harness::program_commit_seeds() {
        assert!(harness::plan_is_stable(&seed), "{name}: a plan doesn't read back from its bytes");
        let want = if name.starts_with("value-") { Refused } else { Kept };
        assert_eq!(harness::commit_of(&seed), want, "{name}");
    }
    let fang = 1_000_000_000u64;
    // The default profile, user 1; no writes; one move: the escrow (a guest holding 30 FANG) pays 31.
    let overdraft = [&[1u8, 0, 1, 3, 5][..], &(31 * fang).to_le_bytes()].concat();
    assert_eq!(harness::commit_of(&overdraft), Rejected, "an overdraft");
    // One write to a program that doesn't exist (selector 15 modulo 16), bonded by the signer.
    let nobody = [&[1u8, 1, 15, 0, 1, 2, 1, 0, 7][..], &fang.to_le_bytes(), &[0]].concat();
    assert_eq!(harness::commit_of(&nobody), Rejected, "a program that isn't one");
    // A diff of nothing is kept, and any bytes are some plan.
    assert_eq!(harness::commit_of(&[]), Kept);
    for n in 0..200u32 {
        let noise: Vec<u8> = (0..n * 7).map(|i| (i.wrapping_mul(2_654_435_761).wrapping_add(n) >> 13) as u8).collect();
        assert!(harness::plan_is_stable(&noise), "noise of {} bytes", noise.len());
    }
}

/// A smoke fuzz of the three targets that exercise programs' state and receipts: each seed under a few deterministic
/// mutations (a byte changed, inserted or deleted, a run of bytes repeated), which the harnesses
/// must pass over without a panic, whatever they make of them. It doesn't replace the fuzzer (the
/// campaign runs millions), but it keeps CI honest about the plain cases the seeds don't spell out.
#[test]
fn mutated_seeds_run_clean() {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move |n: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as usize % n.max(1)
    };
    let receipts = harness::program_receipt_seeds().into_iter().map(|(name, bytes)| (&*name.leak(), bytes)).collect();
    let runs = [("state_machine_programs", harness::state_machine_programs_seeds(), 12), ("program_commit", harness::program_commit_seeds(), 120), ("program_receipt", receipts, 60)];
    for (target, seeds, mutations) in runs {
        for (name, seed) in seeds {
            // The pause's lifetime is 770 blocks a run: a couple of mutants is plenty.
            let mutations = if name.ends_with("lifetime") { 2 } else { mutations };
            for _ in 0..mutations {
                let mut input = seed.clone();
                for _ in 0..1 + next(3) {
                    let at = next(input.len().max(1));
                    match next(4) {
                        0 if at < input.len() => input[at] = next(256) as u8,
                        1 => input.insert(at.min(input.len()), next(256) as u8),
                        2 if at < input.len() => {
                            input.remove(at);
                        }
                        _ => {
                            let run: Vec<u8> = input[at.min(input.len())..].iter().take(1 + next(8)).copied().collect();
                            input.splice(at.min(input.len())..at.min(input.len()), run);
                        }
                    }
                }
                let _ = harness::run(target, &input);
            }
            eprintln!("{target}/{name}: {mutations} mutations ran clean");
        }
    }
}

/// The numbers the oracles copy from the spec (`oracle::spec`) are the schedule's and the limits'
/// and the parameters' defaults the node holds: a schedule that moves fails here, and the spec and
/// the oracles are read again, not followed silently.
#[test]
fn the_oracles_spec_numbers_are_the_nodes() {
    use blockula_vm::meter::{MAX_RESERVATION, MAX_TX_FUEL, SCHEDULE_V0};
    let node: std::collections::BTreeMap<&str, u64> = [
        ("load_base", SCHEDULE_V0.load.load_base),
        ("load_per_byte", SCHEDULE_V0.load.load_per_byte),
        ("upload_base", SCHEDULE_V0.load.upload_base),
        ("upload_per_byte", SCHEDULE_V0.load.upload_per_byte),
        ("rate_scale", blockula_fuzz::econ::RATE_SCALE),
        ("max_reservation", MAX_RESERVATION as u64),
        ("max_fuel_rate", blockula_fuzz::programs::MAX_FUEL_RATE),
        ("floor_rate", blockula_fuzz::programs::params::Param::FuelFloorRate.default(&Default::default())),
        ("max_block_fuel", blockula_fuzz::programs::params::Param::MaxBlockFuel.default(&Default::default())),
        ("max_tx_fuel", blockula_fuzz::programs::params::Param::MaxTxFuel.default(&Default::default())),
        ("value_cap", blockula_fuzz::programs::params::Param::ValueCap.default(&Default::default())),
    ]
    .into();
    assert!(MAX_TX_FUEL <= 20_000_000);
    for (name, spec) in harness::oracle_spec_numbers() {
        assert_eq!(node[name], spec, "the oracles' {name} is {spec}, the node's {}", node[name]);
    }
}

/// An input's work is bounded by what its transactions sign for: past 40 million fuel (the least
/// that bounds an instrumented run well inside the campaign's timeout) it signs no more, however many
/// calls it holds; and its blocks are capped, with the empty ones past the eighth made quickly.
#[test]
fn an_input_signs_for_a_bounded_amount_of_fuel_and_makes_a_bounded_number_of_blocks() {
    // Twenty calls of the counter, each for all `program_max_tx_fuel` (a fuel byte of 31): the profile
    // byte, then per call: kind 40, sender, and the call's bytes (program 0, flags 0, method 0), a
    // payload of six (the fuel byte, the storage limit, the bindings, the fee).
    let mut input = vec![0u8];
    for i in 0..20u8 {
        input.extend([40, i % 6, 0, 0, 0, 6, 31, 0x20, 0x4e, 0, 0, 0]);
    }
    let signed = harness::state_machine_programs_receipts(&input).values().sum::<usize>() + harness::state_machine_programs_refusals(&input).len();
    assert_eq!(signed, 5, "40 million of 10-million calls: four, and the fifth that passes it");
    // 300 ops that each let 63 blocks pass: the run stops at 800 blocks (and 2 at the start).
    let mut long = vec![3u8];
    for _ in 0..300 {
        long.extend([46, 0, 8, 63, 0, 0]);
    }
    let (height, ..) = harness::state_machine_programs_end(&long);
    assert_eq!(height, 802, "the run's blocks are capped at 800 past the fixture's two");
}
