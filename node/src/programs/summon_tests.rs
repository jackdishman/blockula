//! `blockula summon --programs` and `blockula dev --programs` (WBS S6b): `SummonOpts`' `upgrades` and
//! `features`, empty by default so every existing caller's genesis, and its hash, are what they were,
//! and `with_programs` writing protocol v2 at block 1 and the `programs` feature into a new one.

use crate::chain::{Chain, Genesis, SummonOpts};
use crate::programs::probe::Dev;
use crate::programs::{status, Status};
use crate::upgrade::{PROGRAMS_FEATURE, PROGRAMS_VERSION};
use serde_json::{json, Value};

fn summoned(tag: &str, opts: SummonOpts) -> (std::path::PathBuf, String, Value) {
    let home = Dev::fresh_home(tag);
    Chain::summon(&home, &opts).unwrap();
    let text = std::fs::read_to_string(home.join("genesis.json")).unwrap();
    let json = serde_json::from_str(&text).unwrap();
    (home, text, json)
}

fn keys_of(j: &Value) -> Vec<&str> {
    j.as_object().unwrap().keys().map(String::as_str).collect()
}

/// A summon that doesn't ask for programs writes the genesis it always did: no `upgrades` and no
/// `features` key, exactly the keys a genesis had before `SummonOpts` could carry them (each list is
/// left out of the JSON while it's empty, so the genesis hash, which is over this text's contents, is
/// the one every earlier summon had), and the text is what `Genesis` writes back byte for byte.
#[test]
fn a_default_summon_writes_the_genesis_it_always_did() {
    let (home, text, j) = summoned("summon-default", SummonOpts::new("summon-default-1", 1));
    let mut keys = keys_of(&j);
    keys.sort_unstable();
    // The keys of a faucet chain's genesis at 7874c07d, the base of this stage (`min_self_bond` is
    // a faucet chain's one parameter override).
    assert_eq!(keys, ["address_prefix", "allocations", "block_time_ms", "chain_id", "created_at_ms", "decimals", "epoch_blocks", "faucet", "operators", "params", "token", "validator_admins", "validators"]);
    let g = Genesis::load(&home.join("genesis.json")).unwrap();
    assert!(g.features.is_empty() && g.upgrades.is_empty());
    assert_eq!(serde_json::to_string_pretty(&g).unwrap(), text, "what Genesis writes is the file");
    // The options say so too: empty unless asked, whatever else is set.
    let o = SummonOpts::new("x", 1);
    assert!(o.upgrades.is_empty() && o.features.is_empty());
    let o = SummonOpts { epoch_blocks: 60, faucet: false, ..SummonOpts::new("x", 4) };
    assert!(o.upgrades.is_empty() && o.features.is_empty());
    std::fs::remove_dir_all(home).ok();
}

/// `with_programs`: v2 scheduled at height 1 and the `programs` feature, and nothing else of the
/// genesis changes. The chain opens, its first block is a v2 block, and programs are on from it
/// (META `programs` exists from block 0, so the next block's status is `On`).
#[test]
fn programs_on_from_block_1_writes_v2_and_the_feature() {
    let (home_a, _, plain) = summoned("summon-plain", SummonOpts::new("summon-programs-1", 1));
    let (home, _, on) = summoned("summon-programs", SummonOpts::new("summon-programs-1", 1).with_programs());
    assert_eq!(on["upgrades"], json!([{ "version": PROGRAMS_VERSION, "activation_height": 1 }]));
    assert_eq!(on["features"], json!([PROGRAMS_FEATURE]));
    // The same chain otherwise: every other key, the keys and the clock aside, has its shape.
    let (mut a, mut b) = (keys_of(&plain), keys_of(&on));
    b.retain(|k| !["upgrades", "features"].contains(k));
    a.sort_unstable();
    b.sort_unstable();
    assert_eq!(a, b);
    assert_eq!((on["chain_id"].clone(), on["epoch_blocks"].clone(), on["block_time_ms"].clone()), (plain["chain_id"].clone(), plain["epoch_blocks"].clone(), plain["block_time_ms"].clone()));
    let mut c = Chain::open(&home).unwrap();
    assert!(c.state.programs.meta.is_some(), "META `programs` exists from genesis");
    c.produce_block_with(vec![]).unwrap();
    assert_eq!(crate::upgrade::version_at(&c.state, 1), PROGRAMS_VERSION);
    assert_eq!(status(&c.state, crate::econ::now(&c.state)), Status::On);
    std::fs::remove_dir_all(home).ok();
    std::fs::remove_dir_all(home_a).ok();
}
