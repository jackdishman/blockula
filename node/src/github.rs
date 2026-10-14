//! GitHub facts, fetched off-chain: gists (prove a handle ↔ account link) and pull requests
//! (prove a claim, or pick a hunt's winner). Only ever calls api.github.com, with path segments
//! the chain already validated, and caps response sizes. `GITHUB_TOKEN` raises rate limits.

use anyhow::{bail, Result};
use serde_json::Value;
use std::time::Duration;

pub const GITHUB_MAX_BYTES: usize = 1 << 20;

/// `Ok(None)` when GitHub says it doesn't exist (a definite answer, unlike errors).
async fn get_json(path: &str) -> Result<Option<Value>> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(8)).redirect(reqwest::redirect::Policy::none()).user_agent("blockula-node").build()?;
    let mut req = client.get(format!("https://api.github.com{path}")).header("accept", "application/vnd.github+json");
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        req = req.bearer_auth(token);
    }
    let mut res = req.send().await?;
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !res.status().is_success() {
        bail!("GitHub returned {} for {path}", res.status());
    }
    let mut body = vec![];
    while let Some(chunk) = res.chunk().await? {
        body.extend_from_slice(&chunk);
        if body.len() > GITHUB_MAX_BYTES {
            bail!("GitHub response too large");
        }
    }
    Ok(Some(serde_json::from_slice(&body)?))
}

/// A gist's owner and file contents (`None`: no such gist). `gist_id` is validated on-chain as
/// [a-z0-9]{1,64}.
pub async fn fetch_gist(gist_id: &str) -> Result<Option<(String, Vec<String>)>> {
    if gist_id.is_empty() || !gist_id.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase()) {
        bail!("bad gist id");
    }
    let Some(v) = get_json(&format!("/gists/{gist_id}")).await? else { return Ok(None) };
    let owner = v["owner"]["login"].as_str().unwrap_or_default().to_string();
    let files = v["files"].as_object().map(|m| m.values().filter_map(|f| f["content"].as_str().map(String::from)).collect()).unwrap_or_default();
    Ok(Some((owner, files)))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequest {
    /// "open" or "closed".
    pub state: String,
    pub merged: bool,
    pub merge_sha: Option<String>,
    pub author: String,
    /// `owner/name` of the repo it was merged into.
    pub base_repo: String,
    pub title: String,
    pub body: String,
    pub url: String,
}

pub fn parse_pr(v: &Value) -> PullRequest {
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
    PullRequest {
        state: s(&v["state"]),
        merged: v["merged"].as_bool().unwrap_or(false) || !v["merged_at"].is_null(),
        merge_sha: v["merge_commit_sha"].as_str().filter(|h| h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit())).map(|h| h.to_ascii_lowercase()),
        author: s(&v["user"]["login"]),
        base_repo: s(&v["base"]["repo"]["full_name"]),
        title: s(&v["title"]),
        body: s(&v["body"]),
        url: s(&v["html_url"]),
    }
}

/// `None`: no such PR. `repo` must pass `types::check_repo` (so it can't change the path).
pub async fn fetch_pr(repo: &str, pr: u64) -> Result<Option<PullRequest>> {
    crate::types::check_repo(repo)?;
    Ok(get_json(&format!("/repos/{repo}/pulls/{pr}")).await?.map(|v| parse_pr(&v)))
}

/// What a validator attests about a claim.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Merged into the claimed repo, authored by the linked handle, and the gist proves the link.
    Confirmed { merge_sha: String },
    Refuted(String),
    /// Still open: check again later (pending claims are refunded if never settled).
    Wait,
}

/// Pure check of what GitHub returned, so it's testable without the network.
pub fn verdict(handle: &str, repo: &str, account: &str, pr: &PullRequest, gist: &(String, Vec<String>)) -> Verdict {
    if !gist.0.eq_ignore_ascii_case(handle) {
        return Verdict::Refuted(format!("the linked gist is owned by @{}, not @{handle}", gist.0));
    }
    if !gist.1.iter().any(|f| f.contains(account)) {
        return Verdict::Refuted("the linked gist doesn't contain the account's address".into());
    }
    if !pr.base_repo.eq_ignore_ascii_case(repo) {
        return Verdict::Refuted(format!("the PR is in {}, not {repo}", pr.base_repo));
    }
    if !pr.author.eq_ignore_ascii_case(handle) {
        return Verdict::Refuted(format!("the PR's author is @{}, not @{handle}", pr.author));
    }
    if !pr.merged {
        return if pr.state == "open" { Verdict::Wait } else { Verdict::Refuted("the PR was closed without merging".into()) };
    }
    match &pr.merge_sha {
        Some(sha) => Verdict::Confirmed { merge_sha: sha.clone() },
        None => Verdict::Wait,
    }
}

/// Fetches the PR and gist behind a claim and judges it. `Err` = couldn't reach GitHub.
pub async fn check_claim(handle: &str, repo: &str, pr: u64, account: &str, gist_id: &str) -> Result<Verdict> {
    Ok(match tokio::try_join!(fetch_pr(repo, pr), fetch_gist(gist_id))? {
        (None, _) => Verdict::Refuted(format!("{repo}#{pr} doesn't exist")),
        (_, None) => Verdict::Refuted(format!("@{handle}'s linked gist doesn't exist")),
        (Some(p), Some(g)) => verdict(handle, repo, account, &p, &g),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pr(state: &str, merged: bool, author: &str, base: &str) -> PullRequest {
        parse_pr(&json!({ "state": state, "merged": merged, "merge_commit_sha": if merged { Value::from("A".repeat(40)) } else { Value::Null }, "user": { "login": author }, "base": { "repo": { "full_name": base } }, "title": "t", "body": "Closes #1", "html_url": "https://github.com/x/y/pull/1" }))
    }

    #[test]
    fn claim_verdicts() {
        let gist = ("Alice".to_string(), vec!["my blockula account: boo1abc".to_string()]);
        let ok = pr("closed", true, "alice", "jackdishman/blockula");
        assert_eq!(verdict("alice", "jackdishman/blockula", "boo1abc", &ok, &gist), Verdict::Confirmed { merge_sha: "a".repeat(40) });
        assert_eq!(verdict("alice", "jackdishman/blockula", "boo1abc", &pr("open", false, "alice", "jackdishman/blockula"), &gist), Verdict::Wait);
        assert!(matches!(verdict("alice", "jackdishman/blockula", "boo1abc", &pr("closed", false, "alice", "jackdishman/blockula"), &gist), Verdict::Refuted(r) if r.contains("without merging")));
        assert!(matches!(verdict("alice", "jackdishman/blockula", "boo1abc", &pr("closed", true, "mallory", "jackdishman/blockula"), &gist), Verdict::Refuted(r) if r.contains("author")));
        assert!(matches!(verdict("alice", "jackdishman/blockula", "boo1abc", &pr("closed", true, "alice", "fork/blockula"), &gist), Verdict::Refuted(r) if r.contains("fork/blockula")));
        assert!(matches!(verdict("alice", "jackdishman/blockula", "boo1xyz", &ok, &gist), Verdict::Refuted(r) if r.contains("address")));
        assert!(matches!(verdict("alice", "jackdishman/blockula", "boo1abc", &ok, &("bob".into(), gist.1.clone())), Verdict::Refuted(r) if r.contains("owned by")));
    }
}
