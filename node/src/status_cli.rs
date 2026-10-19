//! `blockula status`: a node's own state as one line of JSON, read from its HTTP API
//! (`/api/status` and `/metrics`). For scripts that check a node from its own machine, which has
//! no curl: deploy/restart-gate.sh runs it over `fly ssh console` to tell whether a restarted
//! validator is back (a new process, signing, linked to the other validators) before it restarts
//! the next one. Read-only: it never touches the node's home or keys.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::time::Duration;

/// How long each of the two reads may take.
const TIMEOUT: Duration = Duration::from_secs(10);

/// The node behind `rpc`, as [`summary`] puts it. Fails when either read does.
pub async fn read(rpc: &str) -> Result<Value> {
    let base = rpc.trim().trim_end_matches('/');
    let client = reqwest::Client::builder().timeout(TIMEOUT).build()?;
    let url = format!("{base}/api/status");
    let body = crate::node::get_capped(&client, &url, 1 << 20).await.with_context(|| url.clone())?;
    let status: Value = serde_json::from_slice(&body).with_context(|| url.clone())?;
    let url = format!("{base}/metrics");
    let metrics = crate::node::get_capped(&client, &url, 8 << 20).await.with_context(|| url.clone())?;
    Ok(summary(&status, &String::from_utf8_lossy(&metrics)))
}

/// What a restart gate needs from a node's `/api/status` and `/metrics`. `null` where the node
/// doesn't report it: a full node has no validator of its own, no slashing protection and no p2p.
/// - `validator`: this node's own validator address (the series labelled `own="1"`);
/// - `started_at`: when its process started (unix seconds), which moves with every restart;
/// - `p2p_peers`: connected p2p peers, and `p2p_active_peers`: those in this epoch's active set
///   (the tracker also links to the last epoch's set, candidates and bootstrappers);
/// - `voting`, `abstaining`, `signing`: in the active set of the newest epoch it entered; still
///   watching the network after a start (slashing protection); and so voting and not abstaining.
pub fn summary(status: &Value, metrics: &str) -> Value {
    let samples = samples(metrics);
    let gauge = |name: &str| samples.iter().find(|s| s.name == name && s.labels.is_empty()).map(|s| s.value).filter(|v| v.is_finite());
    let flag = |name: &str| gauge(name).map(|v| v != 0.0);
    let own = |name: &str| samples.iter().find(|s| s.name == name && s.label("own") == Some("1"));
    let (voting, abstaining) = (flag("blockula_slashing_protection_voting"), flag("blockula_slashing_protection_abstaining"));
    json!({
        "chain_id": status["chain_id"],
        "genesis_hash": status["genesis_hash"],
        "role": status["role"],
        "version": status["version"],
        "height": status["height"],
        "finalized_height": status["finalized_height"],
        "syncing": status["syncing"],
        "epoch": status["epoch"],
        "active_validators": status["active_validators"],
        "head_age_ms": status["head_age_ms"],
        "validator": own("blockula_validator_missed_slots").and_then(|s| s.label("validator")),
        "started_at": gauge("blockula_process_start_time_seconds"),
        "p2p_peers": gauge("blockula_p2p_peers").map(|v| v as u64),
        "p2p_active_peers": gauge("blockula_p2p_active_peers").map(|v| v as u64),
        "voting": voting,
        "abstaining": abstaining,
        "signing": voting.zip(abstaining).map(|(v, a)| v && !a),
        "missed_slots": own("blockula_validator_missed_slots").map(|s| s.value as u64),
        "jailed": own("blockula_validator_jailed").map(|s| s.value != 0.0),
        "halted": flag("blockula_halted"),
    })
}

/// One sample of a Prometheus text exposition.
#[derive(Debug, PartialEq)]
struct Sample {
    name: String,
    labels: Vec<(String, String)>,
    value: f64,
}

impl Sample {
    fn label(&self, key: &str) -> Option<&str> {
        self.labels.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

/// The samples in Prometheus text format (`name{k="v",…} value [timestamp]`), skipping comments
/// and lines that don't parse.
fn samples(text: &str) -> Vec<Sample> {
    text.lines().filter_map(|l| sample(l.trim())).collect()
}

fn sample(line: &str) -> Option<Sample> {
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let end = line.find(|c: char| c == '{' || c.is_whitespace())?;
    let (name, mut rest) = (line[..end].to_string(), &line[end..]);
    let mut labels = vec![];
    if let Some(inner) = rest.strip_prefix('{') {
        (labels, rest) = labels_of(inner)?;
    }
    let value = rest.split_whitespace().next()?.parse::<f64>().ok()?;
    Some(Sample { name, labels, value })
}

/// `k="v",…}` and what follows the `}`. Values may escape `\\`, `\"` and `\n`.
fn labels_of(mut s: &str) -> Option<(Vec<(String, String)>, &str)> {
    let mut out = vec![];
    loop {
        s = s.trim_start_matches([' ', ',']);
        if let Some(tail) = s.strip_prefix('}') {
            return Some((out, tail));
        }
        let (key, after) = s.split_once('=')?;
        let after = after.strip_prefix('"')?;
        let mut value = String::new();
        let mut chars = after.char_indices();
        let close = loop {
            match chars.next()? {
                (i, '"') => break i,
                (_, '\\') => match chars.next()?.1 {
                    'n' => value.push('\n'),
                    c => value.push(c),
                },
                (_, c) => value.push(c),
            }
        };
        out.push((key.trim().to_string(), value));
        s = &after[close + 1..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::{Chain, SummonOpts};
    use parking_lot::Mutex;
    use std::sync::Arc;

    #[test]
    fn prometheus_text_parses() {
        let text = "# HELP a x\n# TYPE a gauge\na 3\nb{k=\"v\",own=\"1\"} 0.5\nc{x=\"a \\\"q\\\" \\\\ }\",} NaN\nd 7 1792000000000\n\nbroken{k=\"v\" 1\nnovalue\ne{} 2\n";
        let s = samples(text);
        assert_eq!(s.len(), 5, "{s:?}");
        assert_eq!((s[0].name.as_str(), s[0].value), ("a", 3.0));
        assert_eq!((s[1].label("k"), s[1].label("own"), s[1].value), (Some("v"), Some("1"), 0.5));
        assert_eq!(s[2].label("x"), Some("a \"q\" \\ }"));
        assert!(s[2].value.is_nan());
        assert_eq!((s[3].name.as_str(), s[3].value), ("d", 7.0), "a timestamp after the value");
        assert_eq!((s[4].name.as_str(), s[4].labels.len(), s[4].value), ("e", 0, 2.0));
    }

    #[test]
    fn a_full_node_reports_no_validator_signing_or_peers() {
        let status = json!({ "chain_id": "c", "genesis_hash": "ab", "role": "full_node", "height": 9, "syncing": false, "active_validators": 4 });
        let v = summary(&status, "blockula_height 9\nblockula_process_start_time_seconds 1792000000.25\nblockula_validator_missed_slots{validator=\"tboo1x\",own=\"0\"} 2\n");
        assert_eq!((v["role"].as_str(), v["height"].as_u64(), v["started_at"].as_f64()), (Some("full_node"), Some(9), Some(1792000000.25)));
        for k in ["validator", "p2p_peers", "p2p_active_peers", "voting", "abstaining", "signing", "missed_slots", "jailed"] {
            assert!(v[k].is_null(), "{k}: {v}");
        }
    }

    /// Against a node's real `/api/status` and `/metrics` over HTTP, so the names read here are
    /// the ones the node writes: a validator just after its start (watching, so not signing yet),
    /// then signing, then out of the set.
    #[tokio::test]
    async fn reads_a_validator_over_http() {
        let root = std::env::temp_dir().join(format!("blockula-status-cli-{}-{}", crate::chain::now_ms(), std::process::id()));
        let homes = Chain::summon(&root, &SummonOpts::new("status-cli", 1)).unwrap();
        let chain = Arc::new(Mutex::new(Chain::open(&homes[0]).unwrap()));
        chain.lock().produce_block_with(vec![]).unwrap();
        let me = chain.lock().me().unwrap();
        let mine = crate::pq::PublicKey::from_bytes(&hex::decode(&chain.lock().state.validators.get(&me).unwrap().consensus_key).unwrap()).unwrap().to_string();
        let mut n = crate::api::Node::new(chain, None, vec![], crate::api::ClientIpPolicy::default());
        let key = crate::crypto::Keys::from_seed([1; 32]).public_key();
        let p = crate::protection::Protection::in_memory(&key, crate::protection::Timing::for_block_time(1_000));
        p.enter_epoch(0, Some(0), 4, 0);
        n.protection = Some(p.clone());
        // The tracker's labels: this one-validator set's own key stands in for a linked active
        // peer (no real tracker lists the node itself), the others for candidates.
        n.p2p_peers = Some(Arc::new(move || Some(vec![mine.clone(), "0a0b0c0d0e0f".into(), "1a1b1c1d1e1f".into()])));
        crate::api::mark_process_start();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, crate::api::router(n).into_make_service_with_connect_info::<std::net::SocketAddr>()).await });
        let v = read(&url).await.unwrap();
        let status: Value = reqwest::get(format!("{url}api/status")).await.unwrap().json().await.unwrap();
        assert_eq!((v["genesis_hash"].as_str(), v["chain_id"].as_str()), (status["genesis_hash"].as_str(), Some("status-cli")));
        assert_eq!((v["role"].as_str(), v["height"].as_u64(), v["active_validators"].as_u64()), (Some("validator"), Some(1), Some(1)));
        assert_eq!(v["validator"].as_str(), Some(me.as_str()), "its own validator, from the series labelled own=\"1\"");
        assert_eq!((v["p2p_peers"].as_u64(), v["p2p_active_peers"].as_u64()), (Some(3), Some(1)), "{v}");
        assert_eq!((v["voting"].as_bool(), v["abstaining"].as_bool(), v["signing"].as_bool()), (Some(true), Some(true), Some(false)));
        assert_eq!((v["missed_slots"].as_u64(), v["jailed"].as_bool(), v["halted"].as_bool()), (Some(0), Some(false), Some(false)));
        assert!(v["started_at"].as_f64().is_some_and(|t| t > 1.6e9), "{v}");
        // An epoch with nobody else to watch: it signs. Then an epoch whose set it isn't in.
        p.enter_epoch(1, Some(0), 1, 1_000);
        let v = read(&url).await.unwrap();
        assert_eq!((v["voting"].as_bool(), v["abstaining"].as_bool(), v["signing"].as_bool()), (Some(true), Some(false), Some(true)), "{v}");
        p.enter_epoch(2, None, 4, 2_000);
        let v = read(&url).await.unwrap();
        assert_eq!((v["voting"].as_bool(), v["signing"].as_bool()), (Some(false), Some(false)), "{v}");
        // Nothing answering: an error, never a status.
        assert!(read("http://127.0.0.1:9").await.is_err());
        std::fs::remove_dir_all(root).ok();
    }
}
