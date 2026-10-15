//! GitHub facts, fetched off-chain: gists (prove a handle ↔ account link), pull requests (prove a
//! claim, or pick a hunt's winner) and reverts (undo a claim, beacon.rs). Only ever calls
//! api.github.com, with path segments the chain already validated, and caps response sizes.
//! `GITHUB_TOKEN` raises rate limits.

use anyhow::{bail, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

pub const GITHUB_MAX_BYTES: usize = 1 << 20;
/// Lists (commits, pull requests) are bigger: a page of 100 PRs runs to a few MB.
pub const GITHUB_MAX_LIST_BYTES: usize = 8 << 20;

/// `Ok(None)` when GitHub says it doesn't exist (a definite answer, unlike errors).
async fn get_json(path: &str) -> Result<Option<Value>> {
    get_json_capped(path, GITHUB_MAX_BYTES).await
}

async fn get_json_capped(path: &str, cap: usize) -> Result<Option<Value>> {
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
        if body.len() > cap {
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
    pub number: u64,
    /// "open" or "closed".
    pub state: String,
    pub merged: bool,
    /// ISO 8601 (`2026-10-12T09:00:00Z`), empty if unmerged.
    pub merged_at: String,
    pub merge_sha: Option<String>,
    pub author: String,
    /// `owner/name` of the repo it was merged into.
    pub base_repo: String,
    /// The branch it targets, and that repo's default branch.
    pub base_branch: String,
    pub default_branch: String,
    pub title: String,
    pub body: String,
    pub url: String,
}

pub fn parse_pr(v: &Value) -> PullRequest {
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
    PullRequest {
        number: v["number"].as_u64().unwrap_or(0),
        state: s(&v["state"]),
        merged: v["merged"].as_bool().unwrap_or(false) || !v["merged_at"].is_null(),
        merged_at: s(&v["merged_at"]),
        merge_sha: v["merge_commit_sha"].as_str().filter(|h| h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit())).map(|h| h.to_ascii_lowercase()),
        author: s(&v["user"]["login"]),
        base_repo: s(&v["base"]["repo"]["full_name"]),
        base_branch: s(&v["base"]["ref"]),
        default_branch: s(&v["base"]["repo"]["default_branch"]),
        title: s(&v["title"]),
        body: s(&v["body"]),
        url: s(&v["html_url"]),
    }
}

impl PullRequest {
    /// Targets the repo's default branch. A merge into any other branch hasn't shipped.
    pub fn on_default_branch(&self) -> bool {
        !self.base_branch.is_empty() && self.base_branch == self.default_branch
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
    /// Merged into the claimed repo's default branch, authored by the linked handle, and the gist
    /// proves the link.
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
    if !pr.on_default_branch() {
        return Verdict::Refuted(format!("the PR merged into {}, not {repo}'s default branch ({})", pr.base_branch, pr.default_branch));
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

// ------------------------------------------------------------------------------------ reverts

/// A commit as `GET /repos/{repo}/commits` lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    pub message: String,
}

fn is_sha(h: &str) -> bool {
    h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A commit list (entries without a 40-hex sha are skipped).
pub fn parse_commits(v: &Value) -> Vec<Commit> {
    let list = v.as_array().map(Vec::as_slice).unwrap_or_default();
    list.iter().filter_map(|c| Some(Commit { sha: c["sha"].as_str().filter(|h| is_sha(h))?.to_ascii_lowercase(), message: c["commit"]["message"].as_str().unwrap_or_default().to_string() })).collect()
}

/// A pull request list (`GET /repos/{repo}/pulls`).
pub fn parse_pulls(v: &Value) -> Vec<PullRequest> {
    v.as_array().map(|a| a.iter().map(parse_pr).collect()).unwrap_or_default()
}

/// What undid a merged PR.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Revert {
    /// The commit validators attest (`AttestRevert::revert_sha`).
    pub sha: String,
    /// How it was found (logs).
    pub how: String,
}

/// Whether `text` refers to PR `pr` of `repo`: `#123` (bare or as `owner/repo#123`) or its URL,
/// never `#1234` or another repo's `#123`.
pub fn references_pr(text: &str, repo: &str, pr: u64) -> bool {
    let (text, repo, num) = (text.to_ascii_lowercase(), repo.to_ascii_lowercase(), pr.to_string());
    let ends = |rest: &str| !rest.starts_with(|c: char| c.is_ascii_alphanumeric());
    let bare = text.match_indices(&format!("#{num}")).any(|(i, m)| {
        let owner = text[..i].rsplit(|c: char| c.is_whitespace() || "([{,;:\"'".contains(c)).next().unwrap_or_default();
        ends(&text[i + m.len()..]) && (owner.is_empty() || owner == repo)
    });
    bare || text.match_indices(&format!("github.com/{repo}/pull/{num}")).any(|(i, m)| ends(&text[i + m.len()..]))
}

/// Whether `p` is a merged PR into `repo`'s default branch that reverts PR `pr` (merged as
/// `merge_sha`): titled "Revert …" with a body that references the PR or names the merge commit
/// (GitHub's revert button writes "Reverts owner/repo#123").
fn reverts_pr(p: &PullRequest, repo: &str, pr: u64, merge_sha: &str) -> bool {
    p.merged && p.number != pr && p.on_default_branch() && p.base_repo.eq_ignore_ascii_case(repo) && p.title.trim_start().to_ascii_lowercase().starts_with("revert") && (references_pr(&p.body, repo, pr) || p.body.to_ascii_lowercase().contains(merge_sha))
}

/// PRs worth fetching because a commit looks like their revert landing: a squash merge titled
/// "Revert … (#N)", or "Merge pull request #N from owner/revert-…" (GitHub's revert branches).
/// Oldest first, without repeats.
pub fn revert_pr_candidates(commits: &[Commit]) -> Vec<u64> {
    let mut out: Vec<u64> = vec![];
    for c in commits.iter().rev() {
        let subject = c.message.lines().next().unwrap_or_default().trim().to_ascii_lowercase();
        let n = if subject.starts_with("revert") {
            subject.strip_suffix(')').and_then(|s| s.rsplit_once("(#")).and_then(|(_, n)| n.parse().ok())
        } else if let Some(rest) = subject.strip_prefix("merge pull request #") {
            rest.split_once(" from ").filter(|(_, branch)| branch.contains("/revert-")).and_then(|(n, _)| n.parse().ok())
        } else {
            None
        };
        if let Some(n) = n.filter(|n| !out.contains(n)) {
            out.push(n);
        }
    }
    out
}

/// The revert of PR `pr` (merged into `repo` as `merge_sha`), given `commits` (the default
/// branch since the merge, newest first as GitHub lists them) and `pulls` (closed PRs: the recent
/// ones and the [`revert_pr_candidates`]). Validators must name the same commit, so it's the
/// oldest on the branch that either
/// * says it reverts `merge_sha` (`git revert` writes "This reverts commit <sha>."; a revert PR
///   merged with a merge commit brings that commit along), or
/// * is the merge of a revert PR ([`reverts_pr`]; a squash merge's message may name neither);
///
/// failing both, the earliest-merged revert PR (its merge older than the commits read).
pub fn find_revert_in(repo: &str, pr: u64, merge_sha: &str, commits: &[Commit], pulls: &[PullRequest]) -> Option<Revert> {
    let merge_sha = merge_sha.to_ascii_lowercase();
    let short = |sha: &str| sha.chars().take(12).collect::<String>();
    let revert_prs: BTreeMap<&str, &PullRequest> = pulls.iter().filter(|p| reverts_pr(p, repo, pr, &merge_sha)).filter_map(|p| Some((p.merge_sha.as_deref()?, p))).collect();
    for c in commits.iter().rev().filter(|c| c.sha != merge_sha) {
        let m = c.message.to_ascii_lowercase();
        if m.contains("revert") && m.contains(&merge_sha) {
            return Some(Revert { sha: c.sha.clone(), how: format!("commit {} reverts {}", short(&c.sha), short(&merge_sha)) });
        }
        if let Some(p) = revert_prs.get(c.sha.as_str()) {
            return Some(Revert { sha: c.sha.clone(), how: format!("{repo}#{} reverts #{pr}, merged as {}", p.number, short(&c.sha)) });
        }
    }
    revert_prs.values().map(|p| (p.merged_at.clone(), p.number, p.merge_sha.clone().unwrap_or_default())).min().map(|(_, number, sha)| Revert { how: format!("{repo}#{number} reverts #{pr}, merged as {}", short(&sha)), sha })
}

/// Most pages of commits since the merge a revert check reads (100 each), and most revert-looking
/// PRs it fetches.
pub const REVERT_COMMIT_PAGES: u32 = 10;
pub const REVERT_PR_FETCHES: usize = 10;

/// Looks for a revert of a claim's PR on GitHub (see [`find_revert_in`]). `Err`: couldn't tell
/// (GitHub unreachable, the PR gone or never merged).
pub async fn find_revert(repo: &str, pr: u64, merge_sha: &str) -> Result<Option<Revert>> {
    let Some(p) = fetch_pr(repo, pr).await? else { bail!("{repo}#{pr} doesn't exist") };
    // Goes into a query string: GitHub's own timestamp format only.
    if p.merged_at.len() != 20 || !p.merged_at.bytes().all(|b| b.is_ascii_digit() || b"-:TZ".contains(&b)) {
        bail!("{repo}#{pr} has no merge time");
    }
    let mut commits = vec![];
    for page in 1..=REVERT_COMMIT_PAGES {
        let Some(v) = get_json_capped(&format!("/repos/{repo}/commits?since={}&per_page=100&page={page}", p.merged_at), GITHUB_MAX_LIST_BYTES).await? else { break };
        let batch = parse_commits(&v);
        let full = v.as_array().is_some_and(|a| a.len() == 100);
        commits.extend(batch);
        if !full {
            break;
        }
    }
    let mut pulls = get_json_capped(&format!("/repos/{repo}/pulls?state=closed&sort=updated&direction=desc&per_page=100"), GITHUB_MAX_LIST_BYTES).await?.map(|v| parse_pulls(&v)).unwrap_or_default();
    let known: Vec<u64> = pulls.iter().map(|p| p.number).collect();
    for n in revert_pr_candidates(&commits).into_iter().filter(|n| *n != pr && !known.contains(n)).take(REVERT_PR_FETCHES) {
        if let Some(p) = fetch_pr(repo, n).await? {
            pulls.push(p);
        }
    }
    Ok(find_revert_in(repo, pr, merge_sha, &commits, &pulls))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pr(state: &str, merged: bool, author: &str, base: &str) -> PullRequest {
        pr_into(state, merged, author, base, "main")
    }

    fn pr_into(state: &str, merged: bool, author: &str, base: &str, branch: &str) -> PullRequest {
        parse_pr(&json!({ "state": state, "merged": merged, "merge_commit_sha": if merged { Value::from("A".repeat(40)) } else { Value::Null }, "user": { "login": author }, "base": { "ref": branch, "repo": { "full_name": base, "default_branch": "main" } }, "title": "t", "body": "Closes #1", "html_url": "https://github.com/x/y/pull/1" }))
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
        // Only merges into the default branch count. An open PR elsewhere may still be retargeted.
        assert!(matches!(verdict("alice", "jackdishman/blockula", "boo1abc", &pr_into("closed", true, "alice", "jackdishman/blockula", "scratch"), &gist), Verdict::Refuted(r) if r.contains("merged into scratch, not jackdishman/blockula's default branch (main)")));
        assert_eq!(verdict("alice", "jackdishman/blockula", "boo1abc", &pr_into("open", false, "alice", "jackdishman/blockula", "scratch"), &gist), Verdict::Wait);
        assert!(matches!(verdict("alice", "jackdishman/blockula", "boo1abc", &parse_pr(&json!({ "state": "closed", "merged": true, "merge_commit_sha": "a".repeat(40), "user": { "login": "alice" }, "base": { "repo": { "full_name": "jackdishman/blockula" } } })), &gist), Verdict::Refuted(_)));
    }

    const MERGE: &str = "3a5e0c1d9b7f4e2a8c6d0b1f3e5a7c9d2b4f6e8a";

    /// `GET /repos/jackdishman/blockula/commits?since=…` after PR #42 merged as `MERGE` (trimmed:
    /// fields we don't read are cut, the shape is GitHub's). Newest first.
    const COMMITS: &str = r#"[
      { "sha": "9f1c2e3d4b5a69788796a5b4c3d2e1f00f1e2d3c", "commit": { "author": { "name": "alice", "date": "2026-10-13T09:12:44Z" }, "committer": { "name": "GitHub", "date": "2026-10-13T09:12:44Z" }, "message": "Revert \"Add the thing\" (#61)\n\n* Revert \"Add the thing\"\n\nThis reverts commit 3a5e0c1d9b7f4e2a8c6d0b1f3e5a7c9d2b4f6e8a.", "comment_count": 0 }, "parents": [{ "sha": "1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e" }] },
      { "sha": "1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e", "commit": { "author": { "name": "bob", "date": "2026-10-12T17:01:02Z" }, "committer": { "name": "GitHub", "date": "2026-10-12T17:01:02Z" }, "message": "Fix typo in README (#60)", "comment_count": 0 }, "parents": [{ "sha": "c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00" }] },
      { "sha": "c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00", "commit": { "author": { "name": "carol", "date": "2026-10-12T10:00:00Z" }, "committer": { "name": "carol", "date": "2026-10-12T10:00:00Z" }, "message": "Revert \"Add the thing\"\n\nThis reverts commit 3a5e0c1d9b7f4e2a8c6d0b1f3e5a7c9d2b4f6e8a, reversing\nchanges made to 77aa88bb99cc00dd11ee22ff33aa44bb55cc66dd.", "comment_count": 0 }, "parents": [{ "sha": "3a5e0c1d9b7f4e2a8c6d0b1f3e5a7c9d2b4f6e8a" }] },
      { "sha": "3a5e0c1d9b7f4e2a8c6d0b1f3e5a7c9d2b4f6e8a", "commit": { "author": { "name": "alice", "date": "2026-10-11T08:30:00Z" }, "committer": { "name": "GitHub", "date": "2026-10-11T08:30:00Z" }, "message": "Add the thing (#42)\n\nReverts nothing; see 3a5e0c1d for history.", "comment_count": 0 }, "parents": [{ "sha": "77aa88bb99cc00dd11ee22ff33aa44bb55cc66dd" }] },
      { "sha": "not-a-sha", "commit": { "message": "This reverts commit 3a5e0c1d9b7f4e2a8c6d0b1f3e5a7c9d2b4f6e8a." } }
    ]"#;

    /// `GET /repos/jackdishman/blockula/pulls?state=closed&sort=updated&direction=desc` (trimmed).
    const PULLS: &str = r#"[
      { "number": 63, "state": "closed", "title": "Revert \"Add the thing\"", "body": "Reverts jackdishman/blockula#42", "merged_at": "2026-10-14T12:00:00Z", "merge_commit_sha": "6363636363636363636363636363636363636363", "user": { "login": "dave" }, "base": { "ref": "main", "repo": { "full_name": "jackdishman/blockula", "default_branch": "main" } }, "html_url": "https://github.com/jackdishman/blockula/pull/63" },
      { "number": 61, "state": "closed", "title": "Revert \"Add the thing\"", "body": "Reverts jackdishman/blockula#42\r\n\r\nBroke the build.", "merged_at": "2026-10-13T09:12:44Z", "merge_commit_sha": "9f1c2e3d4b5a69788796a5b4c3d2e1f00f1e2d3c", "user": { "login": "alice" }, "base": { "ref": "main", "repo": { "full_name": "jackdishman/blockula", "default_branch": "main" } }, "html_url": "https://github.com/jackdishman/blockula/pull/61" },
      { "number": 59, "state": "closed", "title": "Revert \"Add the thing\"", "body": "Reverts jackdishman/blockula#42", "merged_at": null, "merge_commit_sha": "5959595959595959595959595959595959595959", "user": { "login": "mallory" }, "base": { "ref": "main", "repo": { "full_name": "jackdishman/blockula", "default_branch": "main" } }, "html_url": "https://github.com/jackdishman/blockula/pull/59" },
      { "number": 58, "state": "closed", "title": "revert: drop the thing", "body": "Undoes #420 and otherorg/blockula#42", "merged_at": "2026-10-12T00:00:00Z", "merge_commit_sha": "5858585858585858585858585858585858585858", "user": { "login": "erin" }, "base": { "ref": "main", "repo": { "full_name": "jackdishman/blockula", "default_branch": "main" } }, "html_url": "https://github.com/jackdishman/blockula/pull/58" },
      { "number": 57, "state": "closed", "title": "Revert \"Add the thing\" on the release branch", "body": "Reverts #42", "merged_at": "2026-10-11T23:00:00Z", "merge_commit_sha": "5757575757575757575757575757575757575757", "user": { "login": "erin" }, "base": { "ref": "release", "repo": { "full_name": "jackdishman/blockula", "default_branch": "main" } }, "html_url": "https://github.com/jackdishman/blockula/pull/57" },
      { "number": 56, "state": "closed", "title": "Follow-up to #42", "body": "Reverts nothing, builds on #42", "merged_at": "2026-10-11T22:00:00Z", "merge_commit_sha": "5656565656565656565656565656565656565656", "user": { "login": "erin" }, "base": { "ref": "main", "repo": { "full_name": "jackdishman/blockula", "default_branch": "main" } }, "html_url": "https://github.com/jackdishman/blockula/pull/56" }
    ]"#;

    #[test]
    fn reverts_found_in_commits_and_pull_requests() {
        let commits = parse_commits(&serde_json::from_str(COMMITS).unwrap());
        let pulls = parse_pulls(&serde_json::from_str(PULLS).unwrap());
        assert_eq!(commits.len(), 4, "entries without a sha are skipped");
        assert_eq!((pulls[1].number, pulls[1].merged_at.as_str(), pulls[2].merged), (61, "2026-10-13T09:12:44Z", false));
        // `git revert` (and a revert PR merged with a merge commit): the oldest commit that names
        // the merge, never the merge commit itself.
        let r = find_revert_in("jackdishman/blockula", 42, MERGE, &commits, &pulls).unwrap();
        assert_eq!(r.sha, "c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00");
        assert_eq!(find_revert_in("JackDishman/Blockula", 42, &MERGE.to_uppercase(), &commits, &[]).unwrap().sha, r.sha, "case doesn't matter");
        // A squash-merged revert PR whose message doesn't name the merge: the PR's merge commit,
        // the earliest merged "Revert …" PR into the default branch that references #42.
        let quiet: Vec<Commit> = commits.iter().filter(|c| c.sha == MERGE || !c.message.contains("This reverts")).cloned().collect();
        assert_eq!(find_revert_in("jackdishman/blockula", 42, MERGE, &quiet, &pulls).unwrap().sha, "9f1c2e3d4b5a69788796a5b4c3d2e1f00f1e2d3c");
        // Not reverts: unmerged, another branch, another PR or repo, no "Revert" title, the PR itself.
        let not: Vec<PullRequest> = pulls.iter().filter(|p| ![61, 63].contains(&p.number)).cloned().collect();
        assert_eq!(find_revert_in("jackdishman/blockula", 42, MERGE, &quiet, &not), None);
        assert_eq!(find_revert_in("jackdishman/blockula", 61, MERGE, &quiet, &pulls[1..2]), None);
        assert_eq!(find_revert_in("jackdishman/blockula", 42, MERGE, &[], &[]), None);
    }

    /// Recorded from tokio-rs/axum (trimmed): #3168 merged as `e09cc59…` was reverted by #3287, a
    /// squash merge whose commit message is only its title. Its PR is found by following the
    /// commit, and it wins over a later `git revert` of the same merge (validators must agree).
    #[test]
    fn squash_merged_revert_prs_are_followed() {
        let merge = "e09cc593655de82d01971b55130a83842ac46684";
        let mut commits = parse_commits(&json!([
            { "sha": "8d2b1f6e0a4c3d5e7f9a1b2c3d4e5f60718293a4", "commit": { "message": "Remove the `__private_docs` feature (#3290)" } },
            { "sha": "3525a1345987a1f3cead86a995cb3ca39a72b629", "commit": { "message": "Revert \"Make `status` a const function in rejection handling\" (#3287)" } },
            { "sha": "a0a1a2a3a4a5a6a7a8a9b0b1b2b3b4b5b6b7b8b9", "commit": { "message": "Merge pull request #3200 from someone/revert-3100-other\n\nRevert \"Other\"" } },
            { "sha": merge, "commit": { "message": "Make `status` a const function in rejection handling (#3168)" } }
        ]));
        assert_eq!(revert_pr_candidates(&commits), vec![3200, 3287]);
        assert!(revert_pr_candidates(&parse_commits(&json!([{ "sha": merge, "commit": { "message": "Fix the thing (#12)" } }, { "sha": merge, "commit": { "message": "Revert \"x\"" } }]))).is_empty());
        let base = json!({ "ref": "main", "repo": { "full_name": "tokio-rs/axum", "default_branch": "main" } });
        let reverted = parse_pr(&json!({ "number": 3287, "state": "closed", "title": "Revert \"Make `status` a const function in rejection handling\"", "body": "This reverts commit e09cc593655de82d01971b55130a83842ac46684 / PR #3168.\r\n\r\nAlso updates changelogs to note the crate yanks.", "merged_at": "2025-03-27T07:31:19Z", "merge_commit_sha": "3525a1345987a1f3cead86a995cb3ca39a72b629", "base": base }));
        let other = parse_pr(&json!({ "number": 3200, "state": "closed", "title": "Revert \"Other\"", "body": "Reverts tokio-rs/axum#3100", "merged_at": "2025-02-01T00:00:00Z", "merge_commit_sha": "a0a1a2a3a4a5a6a7a8a9b0b1b2b3b4b5b6b7b8b9", "base": base }));
        let pulls = [other, reverted.clone()];
        let r = find_revert_in("tokio-rs/axum", 3168, merge, &commits, &pulls).unwrap();
        assert_eq!(r.sha, "3525a1345987a1f3cead86a995cb3ca39a72b629");
        assert_eq!(find_revert_in("tokio-rs/axum", 3168, merge, &commits, &[]), None, "a title alone isn't a revert of #3168");
        // A body naming only the merge commit is enough too.
        let by_sha = PullRequest { body: "This reverts commit e09cc593655de82d01971b55130a83842ac46684.".into(), ..reverted };
        assert_eq!(find_revert_in("tokio-rs/axum", 3168, merge, &commits, &[by_sha]).unwrap().sha, r.sha);
        commits.insert(0, Commit { sha: "f".repeat(40), message: format!("Revert again\n\nThis reverts commit {merge}.") });
        assert_eq!(find_revert_in("tokio-rs/axum", 3168, merge, &commits, &pulls).unwrap().sha, r.sha, "the oldest revert on the branch");
    }

    /// Against GitHub itself (network, so ignored): `cargo test live_revert -- --ignored`.
    /// tokio-rs/axum#3168 was reverted by #3287 ("This reverts commit e09cc59… / PR #3168").
    #[tokio::test]
    #[ignore]
    async fn live_revert_lookup() {
        let pr = fetch_pr("tokio-rs/axum", 3168).await.unwrap().unwrap();
        let r = find_revert("tokio-rs/axum", 3168, pr.merge_sha.as_deref().unwrap()).await.unwrap();
        println!("#3168 merged as {:?} at {}: {r:?}", pr.merge_sha, pr.merged_at);
        assert!(r.is_some());
    }

    #[test]
    fn pr_references() {
        let r = |t: &str| references_pr(t, "jackdishman/blockula", 42);
        assert!(r("Reverts jackdishman/blockula#42") && r("Reverts #42") && r("(#42)") && r("see #42.") && r("https://github.com/jackdishman/blockula/pull/42") && r("JackDishman/Blockula#42"));
        assert!(!r("#420") && !r("#4") && !r("otherorg/blockula#42") && !r("https://github.com/jackdishman/blockula/pull/420") && !r("issue 42") && !r("abc#42"));
    }
}
