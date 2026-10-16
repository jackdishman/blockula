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

/// Whether `text` says, in so many words, that it reverts PR `pr` of `repo` (merged as
/// `merge_sha`): a line that starts with GitHub's revert-button text `Reverts owner/repo#N` (or
/// `Reverts #N`, or the PR's URL), or with git's `This reverts commit <merge_sha>`. A mention
/// anywhere else ("Reverts #50. Context: #42", "Follow-up to #42") isn't one (audit F20).
pub fn states_revert_of(text: &str, repo: &str, pr: u64, merge_sha: &str) -> bool {
    let (repo, merge_sha) = (repo.to_ascii_lowercase(), merge_sha.to_ascii_lowercase());
    let ends = |rest: &str| !rest.starts_with(|c: char| c.is_ascii_alphanumeric());
    text.lines().map(|l| l.trim().to_ascii_lowercase()).any(|line| {
        if let Some(rest) = line.strip_prefix("this reverts commit ") {
            return rest.strip_prefix(merge_sha.as_str()).is_some_and(ends);
        }
        let Some(rest) = line.strip_prefix("reverts ") else { return false };
        let rest = rest.trim_start();
        [format!("#{pr}"), format!("{repo}#{pr}"), format!("https://github.com/{repo}/pull/{pr}")].iter().any(|r| rest.strip_prefix(r.as_str()).is_some_and(ends))
    })
}

/// Whether `p` is a merged PR into `repo`'s default branch whose description states that it
/// reverts PR `pr` (merged as `merge_sha`): [`states_revert_of`]. Whether it really does is
/// [`undoes`]'s question.
fn reverts_pr(p: &PullRequest, repo: &str, pr: u64, merge_sha: &str) -> bool {
    p.merged && p.number != pr && p.on_default_branch() && p.base_repo.eq_ignore_ascii_case(repo) && states_revert_of(&p.body, repo, pr, merge_sha)
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

/// The commits that may be the revert of PR `pr` (merged into `repo` as `merge_sha`), in the
/// order validators try them, given `commits` (the default branch since the merge, newest first
/// as GitHub lists them) and `pulls` (PRs that may be reverts). Validators must name the same
/// commit, so it's the commits on the branch, oldest first, that either
/// * say they revert it ([`states_revert_of`]: `git revert` writes "This reverts commit <sha>.";
///   a revert PR merged with a merge commit brings that commit along), or
/// * are the merge of a revert PR ([`reverts_pr`]; a squash merge's message may name neither);
///
/// then the revert PRs merged before the commits read, earliest first.
pub fn revert_candidates(repo: &str, pr: u64, merge_sha: &str, commits: &[Commit], pulls: &[PullRequest]) -> Vec<Revert> {
    let merge_sha = merge_sha.to_ascii_lowercase();
    let short = |sha: &str| sha.chars().take(12).collect::<String>();
    let revert_prs: BTreeMap<&str, &PullRequest> = pulls.iter().filter(|p| reverts_pr(p, repo, pr, &merge_sha)).filter_map(|p| Some((p.merge_sha.as_deref()?, p))).collect();
    let mut out: Vec<Revert> = vec![];
    for c in commits.iter().rev().filter(|c| c.sha != merge_sha) {
        if states_revert_of(&c.message, repo, pr, &merge_sha) {
            out.push(Revert { sha: c.sha.clone(), how: format!("commit {} reverts {}", short(&c.sha), short(&merge_sha)) });
        } else if let Some(p) = revert_prs.get(c.sha.as_str()) {
            out.push(Revert { sha: c.sha.clone(), how: format!("{repo}#{} reverts #{pr}, merged as {}", p.number, short(&c.sha)) });
        }
    }
    let mut earlier: Vec<(&str, u64, &str)> = revert_prs.iter().filter(|(sha, _)| !out.iter().any(|r| r.sha == **sha)).map(|(sha, p)| (p.merged_at.as_str(), p.number, *sha)).collect();
    earlier.sort();
    out.extend(earlier.into_iter().map(|(_, number, sha)| Revert { sha: sha.to_string(), how: format!("{repo}#{number} reverts #{pr}, merged as {}", short(sha)) }));
    out
}

/// One file of a diff, as GitHub lists them (`GET /repos/{repo}/pulls/{n}/files`, the `files` of
/// `GET /repos/{repo}/commits/{sha}`). `patch` is absent for binary files and very large diffs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    pub filename: String,
    /// The diff created the file.
    pub added: bool,
    pub patch: Option<String>,
}

/// A file list (a bare array, or a commit's `files`; entries without a name are skipped).
pub fn parse_files(v: &Value) -> Vec<FileDiff> {
    let list = v.as_array().or_else(|| v["files"].as_array()).map(Vec::as_slice).unwrap_or_default();
    list.iter().filter_map(|f| Some(FileDiff { filename: f["filename"].as_str().filter(|n| !n.is_empty())?.to_string(), added: f["status"] == "added", patch: f["patch"].as_str().map(String::from) })).collect()
}

/// Lines a patch adds and removes (trimmed; blank ones don't count), by content.
fn changed_lines(patch: &str) -> (BTreeMap<&str, usize>, BTreeMap<&str, usize>) {
    let (mut added, mut removed) = (BTreeMap::new(), BTreeMap::new());
    for line in patch.lines() {
        let (side, rest) = match line.as_bytes().first() {
            Some(b'+') => (&mut added, &line[1..]),
            Some(b'-') => (&mut removed, &line[1..]),
            _ => continue,
        };
        let rest = rest.trim();
        if !rest.is_empty() {
            *side.entry(rest).or_insert(0usize) += 1;
        }
    }
    (added, removed)
}

/// Whether the diff `revert` undoes the diff `original` (audit F20: a revert must actually revert
/// the claim's change, not just say so): more than half of the lines `original` added and removed,
/// file by file, are removed and added back by `revert`. `None` when there's nothing to compare
/// (no patches: binary files, diffs too large for GitHub to show), which never counts as a
/// revert.
pub fn undoes(original: &[FileDiff], revert: &[FileDiff]) -> Option<bool> {
    let (mut matched, mut total) = (0usize, 0usize);
    for f in original {
        let Some(patch) = &f.patch else { continue };
        let (added, removed) = changed_lines(patch);
        total += added.values().sum::<usize>() + removed.values().sum::<usize>();
        let Some(r) = revert.iter().find(|r| r.filename == f.filename).and_then(|r| r.patch.as_deref()) else { continue };
        let (r_added, r_removed) = changed_lines(r);
        let overlap = |a: &BTreeMap<&str, usize>, b: &BTreeMap<&str, usize>| a.iter().map(|(l, n)| (*n).min(b.get(l).copied().unwrap_or(0))).sum::<usize>();
        matched += overlap(&added, &r_removed) + overlap(&removed, &r_added);
    }
    (total > 0).then_some(2 * matched > total)
}

/// The revert of PR `pr`: the first of the [`revert_candidates`] (at most
/// [`REVERT_DIFF_CHECKS`]) whose diff (`diffs`, by sha) [`undoes`] the PR's (`original`). `Err`
/// when some candidate states a revert but its diff can't be compared: couldn't tell, so no
/// attestation (never a rejection on ambiguity).
pub fn find_revert_in(repo: &str, pr: u64, merge_sha: &str, commits: &[Commit], pulls: &[PullRequest], original: &[FileDiff], diffs: &BTreeMap<String, Vec<FileDiff>>) -> Result<Option<Revert>> {
    let mut unclear = None;
    for r in revert_candidates(repo, pr, merge_sha, commits, pulls).into_iter().take(REVERT_DIFF_CHECKS) {
        match diffs.get(&r.sha).and_then(|d| undoes(original, d)) {
            Some(true) => return Ok(Some(r)),
            Some(false) => {}
            None => unclear = unclear.or(Some(r)),
        }
    }
    match unclear {
        Some(r) => bail!("{} but its diff can't be compared with #{pr}'s (no patch), so it isn't attested", r.how),
        None => Ok(None),
    }
}

/// Most pages of commits since the merge a revert check reads (100 each), most revert-looking
/// PRs it fetches, files of the PR whose later commits it follows (and most of those commits it
/// asks GitHub the PR of), and most candidates whose diffs it compares.
pub const REVERT_COMMIT_PAGES: u32 = 10;
pub const REVERT_PR_FETCHES: usize = 10;
pub const REVERT_PATHS: usize = 3;
pub const REVERT_PATH_COMMITS: usize = 15;
pub const REVERT_DIFF_CHECKS: usize = 5;

/// A file path as a query-string value: every byte but unreserved ones percent-encoded.
fn query_value(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// The PR's files whose later commits a revert check follows (a revert touches them all): files
/// it created first (later commits touching one are few and telling), then the rest, in GitHub's
/// order; only files with a patch, which [`undoes`] can compare.
fn followed_paths(files: &[FileDiff]) -> Vec<&str> {
    let mut paths: Vec<&FileDiff> = files.iter().filter(|f| f.patch.is_some()).collect();
    paths.sort_by_key(|f| !f.added);
    paths.into_iter().take(REVERT_PATHS).map(|f| f.filename.as_str()).collect()
}

/// Looks for a revert of a claim's PR on GitHub (see [`find_revert_in`]). `Err`: couldn't tell
/// (GitHub unreachable, the PR gone or never merged, a stated revert whose diff can't be read).
///
/// Revert PRs are found from the default branch alone, which only maintainers write: commits
/// that look like a revert landing ([`revert_pr_candidates`]), and the PRs of the commits since
/// the merge that touch the claim's files (`commits?path=`, then `commits/{sha}/pulls`). Not
/// from the list of recently updated closed PRs, which anyone can flood by opening and closing
/// PRs until a revert falls off it (audit F55).
pub async fn find_revert(repo: &str, pr: u64, merge_sha: &str) -> Result<Option<Revert>> {
    crate::types::check_repo(repo)?;
    find_revert_with(|path| async move { get_json_capped(&path, GITHUB_MAX_LIST_BYTES).await }, repo, pr, merge_sha).await
}

/// [`find_revert`] against `get` (`GET https://api.github.com{path}`, `Ok(None)` for a 404), so
/// tests can stand in for GitHub.
pub async fn find_revert_with<F, Fut>(get: F, repo: &str, pr: u64, merge_sha: &str) -> Result<Option<Revert>>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<Option<Value>>>,
{
    let fetch_pr = |n: u64| {
        let path = format!("/repos/{repo}/pulls/{n}");
        let get = &get;
        async move { Ok::<_, anyhow::Error>(get(path).await?.map(|v| parse_pr(&v))) }
    };
    let Some(p) = fetch_pr(pr).await? else { bail!("{repo}#{pr} doesn't exist") };
    // Goes into a query string: GitHub's own timestamp format only.
    if p.merged_at.len() != 20 || !p.merged_at.bytes().all(|b| b.is_ascii_digit() || b"-:TZ".contains(&b)) {
        bail!("{repo}#{pr} has no merge time");
    }
    let since = &p.merged_at;
    let original = get(format!("/repos/{repo}/pulls/{pr}/files?per_page=100")).await?.map(|v| parse_files(&v)).unwrap_or_default();
    let mut commits = vec![];
    for page in 1..=REVERT_COMMIT_PAGES {
        let Some(v) = get(format!("/repos/{repo}/commits?since={since}&per_page=100&page={page}")).await? else { break };
        let batch = parse_commits(&v);
        let full = v.as_array().is_some_and(|a| a.len() == 100);
        commits.extend(batch);
        if !full {
            break;
        }
    }
    let merge = merge_sha.to_ascii_lowercase();
    let mut pulls: Vec<PullRequest> = vec![];
    let add = |found: Vec<PullRequest>, pulls: &mut Vec<PullRequest>| {
        for f in found {
            if f.number != pr && !pulls.iter().any(|q| q.number == f.number) {
                pulls.push(f);
            }
        }
    };
    // The commits since the merge that touch the PR's files, oldest first, and their PRs.
    let mut touching: Vec<String> = vec![];
    for path in followed_paths(&original) {
        let Some(v) = get(format!("/repos/{repo}/commits?path={}&since={since}&per_page=100", query_value(path))).await? else { continue };
        for c in parse_commits(&v).into_iter().rev() {
            if c.sha != merge && !touching.contains(&c.sha) {
                touching.push(c.sha);
            }
        }
    }
    for sha in touching.iter().take(REVERT_PATH_COMMITS) {
        if let Some(v) = get(format!("/repos/{repo}/commits/{sha}/pulls")).await? {
            add(parse_pulls(&v), &mut pulls);
        }
    }
    let known: Vec<u64> = pulls.iter().map(|p| p.number).collect();
    for n in revert_pr_candidates(&commits).into_iter().filter(|n| *n != pr && !known.contains(n)).take(REVERT_PR_FETCHES) {
        if let Some(p) = fetch_pr(n).await? {
            add(vec![p], &mut pulls);
        }
    }
    // A candidate that is a PR's merge is compared by the PR's own diff (against its base, the
    // same for every merge method: a merge commit's listed files depend on how GitHub shows a
    // two-parent commit); a plain commit by its own.
    let merged_as: BTreeMap<String, u64> = pulls.iter().filter(|p| p.merged).filter_map(|p| Some((p.merge_sha.clone()?.to_ascii_lowercase(), p.number))).collect();
    let mut diffs = BTreeMap::new();
    for r in revert_candidates(repo, pr, merge_sha, &commits, &pulls).into_iter().take(REVERT_DIFF_CHECKS) {
        let path = match merged_as.get(&r.sha.to_ascii_lowercase()) {
            Some(n) => format!("/repos/{repo}/pulls/{n}/files?per_page=100"),
            None => format!("/repos/{repo}/commits/{}", r.sha),
        };
        if let Some(v) = get(path).await? {
            diffs.insert(r.sha, parse_files(&v));
        }
    }
    find_revert_in(repo, pr, merge_sha, &commits, &pulls, &original, &diffs)
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
      { "sha": "3a5e0c1d9b7f4e2a8c6d0b1f3e5a7c9d2b4f6e8a", "commit": { "author": { "name": "alice", "date": "2026-10-11T08:30:00Z" }, "committer": { "name": "GitHub", "date": "2026-10-11T08:30:00Z" }, "message": "Add the thing (#42)\n\nThis reverts commit 3a5e0c1d9b7f4e2a8c6d0b1f3e5a7c9d2b4f6e8a? No: it's the merge itself.", "comment_count": 0 }, "parents": [{ "sha": "77aa88bb99cc00dd11ee22ff33aa44bb55cc66dd" }] },
      { "sha": "not-a-sha", "commit": { "message": "This reverts commit 3a5e0c1d9b7f4e2a8c6d0b1f3e5a7c9d2b4f6e8a." } }
    ]"#;

    /// `GET /repos/jackdishman/blockula/pulls?state=closed` (trimmed): revert PRs and look-alikes.
    const PULLS: &str = r#"[
      { "number": 63, "state": "closed", "title": "Revert \"Add the thing\"", "body": "Reverts jackdishman/blockula#42", "merged_at": "2026-10-14T12:00:00Z", "merge_commit_sha": "6363636363636363636363636363636363636363", "user": { "login": "dave" }, "base": { "ref": "main", "repo": { "full_name": "jackdishman/blockula", "default_branch": "main" } }, "html_url": "https://github.com/jackdishman/blockula/pull/63" },
      { "number": 61, "state": "closed", "title": "Revert \"Add the thing\"", "body": "Reverts jackdishman/blockula#42\r\n\r\nBroke the build.", "merged_at": "2026-10-13T09:12:44Z", "merge_commit_sha": "9f1c2e3d4b5a69788796a5b4c3d2e1f00f1e2d3c", "user": { "login": "alice" }, "base": { "ref": "main", "repo": { "full_name": "jackdishman/blockula", "default_branch": "main" } }, "html_url": "https://github.com/jackdishman/blockula/pull/61" },
      { "number": 59, "state": "closed", "title": "Revert \"Add the thing\"", "body": "Reverts jackdishman/blockula#42", "merged_at": null, "merge_commit_sha": "5959595959595959595959595959595959595959", "user": { "login": "mallory" }, "base": { "ref": "main", "repo": { "full_name": "jackdishman/blockula", "default_branch": "main" } }, "html_url": "https://github.com/jackdishman/blockula/pull/59" },
      { "number": 58, "state": "closed", "title": "revert: drop the thing", "body": "Undoes #420 and otherorg/blockula#42", "merged_at": "2026-10-12T00:00:00Z", "merge_commit_sha": "5858585858585858585858585858585858585858", "user": { "login": "erin" }, "base": { "ref": "main", "repo": { "full_name": "jackdishman/blockula", "default_branch": "main" } }, "html_url": "https://github.com/jackdishman/blockula/pull/58" },
      { "number": 57, "state": "closed", "title": "Revert \"Add the thing\" on the release branch", "body": "Reverts #42", "merged_at": "2026-10-11T23:00:00Z", "merge_commit_sha": "5757575757575757575757575757575757575757", "user": { "login": "erin" }, "base": { "ref": "release", "repo": { "full_name": "jackdishman/blockula", "default_branch": "main" } }, "html_url": "https://github.com/jackdishman/blockula/pull/57" },
      { "number": 56, "state": "closed", "title": "Follow-up to #42", "body": "Reverts nothing, builds on #42", "merged_at": "2026-10-11T22:00:00Z", "merge_commit_sha": "5656565656565656565656565656565656565656", "user": { "login": "erin" }, "base": { "ref": "main", "repo": { "full_name": "jackdishman/blockula", "default_branch": "main" } }, "html_url": "https://github.com/jackdishman/blockula/pull/56" }
    ]"#;

    /// PR #42's diff (`GET …/pulls/42/files`, trimmed): it adds a file and changes another.
    fn thing_files() -> Vec<FileDiff> {
        parse_files(&json!([
            { "filename": "src/thing.rs", "status": "added", "patch": "@@ -0,0 +1,4 @@\n+pub fn thing() -> u32 {\n+    42\n+}\n+" },
            { "filename": "src/lib.rs", "status": "modified", "patch": "@@ -1,2 +1,3 @@\n mod a;\n+mod thing;\n-mod old_thing;\n mod b;" }
        ]))
    }

    /// A commit's diff that undoes #42 (`GET …/commits/{sha}`'s `files`), as `git revert` makes it.
    fn undoing_files() -> Vec<FileDiff> {
        parse_files(&json!({ "sha": "x", "files": [
            { "filename": "src/thing.rs", "status": "removed", "patch": "@@ -1,4 +0,0 @@\n-pub fn thing() -> u32 {\n-    42\n-}\n-" },
            { "filename": "src/lib.rs", "status": "modified", "patch": "@@ -1,3 +1,2 @@\n mod a;\n-mod thing;\n+mod old_thing;\n mod b;" }
        ] }))
    }

    /// Every candidate's diff undoes #42.
    fn all_undo(commits: &[Commit], pulls: &[PullRequest]) -> BTreeMap<String, Vec<FileDiff>> {
        revert_candidates("jackdishman/blockula", 42, MERGE, commits, pulls).into_iter().map(|r| (r.sha, undoing_files())).collect()
    }

    #[test]
    fn reverts_found_in_commits_and_pull_requests() {
        let commits = parse_commits(&serde_json::from_str(COMMITS).unwrap());
        let pulls = parse_pulls(&serde_json::from_str(PULLS).unwrap());
        assert_eq!(commits.len(), 4, "entries without a sha are skipped");
        assert_eq!((pulls[1].number, pulls[1].merged_at.as_str(), pulls[2].merged), (61, "2026-10-13T09:12:44Z", false));
        let find = |commits: &[Commit], pulls: &[PullRequest]| find_revert_in("jackdishman/blockula", 42, MERGE, commits, pulls, &thing_files(), &all_undo(commits, pulls)).unwrap();
        // `git revert` (and a revert PR merged with a merge commit): the oldest commit that says it
        // reverts the merge, never the merge commit itself.
        let r = find(&commits, &pulls).unwrap();
        assert_eq!(r.sha, "c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00");
        let upper = find_revert_in("JackDishman/Blockula", 42, &MERGE.to_uppercase(), &commits, &[], &thing_files(), &all_undo(&commits, &[])).unwrap();
        assert_eq!(upper.unwrap().sha, r.sha, "case doesn't matter");
        // A squash-merged revert PR whose message doesn't name the merge: the PR's merge commit,
        // the earliest merged PR into the default branch that states it reverts #42.
        let quiet: Vec<Commit> = commits.iter().filter(|c| c.sha == MERGE || !c.message.contains("This reverts")).cloned().collect();
        assert_eq!(find(&quiet, &pulls).unwrap().sha, "9f1c2e3d4b5a69788796a5b4c3d2e1f00f1e2d3c");
        // Not reverts: unmerged, another branch, another PR or repo, no statement, the PR itself.
        let not: Vec<PullRequest> = pulls.iter().filter(|p| ![61, 63].contains(&p.number)).cloned().collect();
        assert_eq!(find(&quiet, &not), None);
        assert_eq!(find_revert_in("jackdishman/blockula", 61, MERGE, &quiet, &pulls[1..2], &thing_files(), &BTreeMap::new()).unwrap(), None);
        assert_eq!(find(&[], &[]), None);
    }

    /// Audit F20: a merged "Revert …" PR that merely mentions the claim's PR isn't its revert,
    /// on the commit path or the fallback; and one that says it is but doesn't undo its diff isn't
    /// either. A stated revert whose diff can't be read is "couldn't tell" (`Err`), never a
    /// revert.
    #[test]
    fn a_revert_must_say_so_and_undo_the_diff() {
        let base = json!({ "ref": "main", "repo": { "full_name": "jackdishman/blockula", "default_branch": "main" } });
        let pr = |number: u64, title: &str, body: &str, sha: &str| parse_pr(&json!({ "number": number, "state": "closed", "title": title, "body": body, "merged_at": "2026-10-13T09:12:44Z", "merge_commit_sha": sha, "base": base }));
        let sha61 = "9f1c2e3d4b5a69788796a5b4c3d2e1f00f1e2d3c";
        let commits = parse_commits(&json!([{ "sha": sha61, "commit": { "message": "Revert \"Add the flag\" (#61)" } }]));
        // GitHub's revert-button text for #50, plus one sentence of context naming #42.
        let of_50 = pr(61, "Revert \"Add the flag\"", "Reverts jackdishman/blockula#50\r\n\r\nThe flag built on #42, which stays.", sha61);
        let follow_up = pr(61, "revert: drop the flag", "Follow-up to #42: this removes the flag added in #50.", sha61);
        for p in [of_50, follow_up] {
            assert!(revert_candidates("jackdishman/blockula", 42, MERGE, &commits, std::slice::from_ref(&p)).is_empty(), "{:?}", p.body);
            assert!(revert_candidates("jackdishman/blockula", 42, MERGE, &[], std::slice::from_ref(&p)).is_empty(), "the fallback too");
        }
        let states = |t: &str| states_revert_of(t, "jackdishman/blockula", 42, MERGE);
        assert!(states("Reverts jackdishman/blockula#42") && states("Reverts #42.") && states("  reverts https://github.com/jackdishman/blockula/pull/42\nmore") && states(&format!("Revert\n\nThis reverts commit {MERGE}.")));
        assert!(!states("Reverts #50. Context: #42") && !states("Reverts #420") && !states("See: Reverts #42") && !states("Reverts otherorg/blockula#42") && !states(&format!("This reverts commit {}.", &MERGE[..12])) && !states("Revert #42"));
        // It says so, but its diff is something else: not a revert.
        let says = pr(61, "Revert \"Add the thing\"", "Reverts #42", sha61);
        let other = parse_files(&json!([{ "filename": "src/lib.rs", "patch": "@@ -9 +9 @@\n-fn unrelated() {}\n+fn unrelated(x: u8) {}" }]));
        let diffs = BTreeMap::from([(sha61.to_string(), other)]);
        assert_eq!(find_revert_in("jackdishman/blockula", 42, MERGE, &commits, std::slice::from_ref(&says), &thing_files(), &diffs).unwrap(), None);
        // A partial undo (less than half of #42's changed lines) isn't one either; the whole is.
        let half = parse_files(&json!([{ "filename": "src/lib.rs", "patch": "@@ -1,3 +1,2 @@\n mod a;\n-mod thing;\n+mod old_thing;\n mod b;" }]));
        assert_eq!(undoes(&thing_files(), &half), Some(false));
        assert_eq!(undoes(&thing_files(), &undoing_files()), Some(true));
        let diffs = BTreeMap::from([(sha61.to_string(), undoing_files())]);
        assert_eq!(find_revert_in("jackdishman/blockula", 42, MERGE, &commits, std::slice::from_ref(&says), &thing_files(), &diffs).unwrap().unwrap().sha, sha61);
        // No patches to compare: couldn't tell.
        let binary = vec![FileDiff { filename: "logo.png".into(), added: true, patch: None }];
        assert_eq!(undoes(&binary, &undoing_files()), None);
        assert!(find_revert_in("jackdishman/blockula", 42, MERGE, &commits, std::slice::from_ref(&says), &binary, &diffs).unwrap_err().to_string().contains("can't be compared"));
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
        let original = parse_files(&json!([{ "filename": "axum-core/src/macros.rs", "patch": "@@ -1 +1 @@\n-pub fn status(&self) -> StatusCode {\n+pub const fn status(&self) -> StatusCode {" }]));
        let undo = parse_files(&json!({ "files": [{ "filename": "axum-core/src/macros.rs", "patch": "@@ -1 +1 @@\n-pub const fn status(&self) -> StatusCode {\n+pub fn status(&self) -> StatusCode {" }, { "filename": "CHANGELOG.md", "patch": "@@ -1 +1,2 @@\n+- yanked" }] }));
        let find = |commits: &[Commit], pulls: &[PullRequest]| {
            let diffs = revert_candidates("tokio-rs/axum", 3168, merge, commits, pulls).into_iter().map(|r| (r.sha, undo.clone())).collect();
            find_revert_in("tokio-rs/axum", 3168, merge, commits, pulls, &original, &diffs).unwrap()
        };
        let pulls = [other, reverted.clone()];
        let r = find(&commits, &pulls).unwrap();
        assert_eq!(r.sha, "3525a1345987a1f3cead86a995cb3ca39a72b629");
        assert_eq!(find(&commits, &[]), None, "a title alone isn't a revert of #3168");
        // A body naming only the merge commit is enough too.
        let by_sha = PullRequest { body: "This reverts commit e09cc593655de82d01971b55130a83842ac46684.".into(), ..reverted };
        assert_eq!(find(&commits, &[by_sha]).unwrap().sha, r.sha);
        commits.insert(0, Commit { sha: "f".repeat(40), message: format!("Revert again\n\nThis reverts commit {merge}.") });
        assert_eq!(find(&commits, &pulls).unwrap().sha, r.sha, "the oldest revert on the branch");
    }

    /// Audit F55: a hand-made revert (PR #77 "Undo the thing", "Reverts #42", merged with a merge
    /// commit from a branch not named `revert-…`, its commit message quoting nothing) is found
    /// from the default branch alone: the commits since the merge that touch #42's files, and
    /// their PRs. No list of recently updated closed PRs is read, so nobody can push the revert
    /// off one by opening and closing PRs.
    #[tokio::test]
    async fn reverts_are_found_from_the_branch_not_the_recent_pr_list() {
        let repo = "jackdishman/blockula";
        let base = json!({ "ref": "main", "repo": { "full_name": repo, "default_branch": "main" } });
        let (m77, undo) = ("7777777777777777777777777777777777777777", "0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d");
        let claim = json!({ "number": 42, "state": "closed", "title": "Add the thing", "body": "", "merged_at": "2026-10-11T08:30:00Z", "merge_commit_sha": MERGE, "base": base });
        let pr77 = json!({ "number": 77, "state": "closed", "title": "Undo the thing", "body": "Reverts #42\r\n\r\nIt broke the build.", "merged_at": "2026-10-14T10:00:00Z", "merge_commit_sha": m77, "base": base });
        let commits = json!([
            { "sha": m77, "commit": { "message": "Merge pull request #77 from dave/undo-the-thing\n\nUndo the thing" } },
            { "sha": undo, "commit": { "message": "Undo the thing" } },
            { "sha": MERGE, "commit": { "message": "Add the thing (#42)" } }
        ]);
        let files = json!([
            { "filename": "src/thing.rs", "status": "added", "patch": "@@ -0,0 +1,4 @@\n+pub fn thing() -> u32 {\n+    42\n+}\n+" },
            { "filename": "src/lib.rs", "status": "modified", "patch": "@@ -1,2 +1,3 @@\n mod a;\n+mod thing;\n-mod old_thing;\n mod b;" }
        ]);
        let undoing = json!([
            { "filename": "src/thing.rs", "status": "removed", "patch": "@@ -1,4 +0,0 @@\n-pub fn thing() -> u32 {\n-    42\n-}\n-" },
            { "filename": "src/lib.rs", "status": "modified", "patch": "@@ -1,3 +1,2 @@\n mod a;\n-mod thing;\n+mod old_thing;\n mod b;" }
        ]);
        let since = "2026-10-11T08:30:00Z";
        let routes: BTreeMap<String, Value> = [
            (format!("/repos/{repo}/pulls/42"), claim),
            (format!("/repos/{repo}/pulls/42/files?per_page=100"), files),
            (format!("/repos/{repo}/commits?since={since}&per_page=100&page=1"), commits),
            (format!("/repos/{repo}/commits?path=src/thing.rs&since={since}&per_page=100"), json!([{ "sha": undo, "commit": { "message": "Undo the thing" } }, { "sha": MERGE, "commit": { "message": "Add the thing (#42)" } }])),
            (format!("/repos/{repo}/commits/{undo}/pulls"), json!([pr77])),
            (format!("/repos/{repo}/pulls/77/files?per_page=100"), undoing),
        ]
        .into_iter()
        .collect();
        let asked = std::sync::Mutex::new(vec![]);
        let get = |path: String| {
            asked.lock().unwrap().push(path.clone());
            let v = routes.get(&path).cloned();
            async move { Ok(v) }
        };
        let r = find_revert_with(get, repo, 42, MERGE).await.unwrap().expect("found");
        assert_eq!(r.sha, m77);
        assert!(r.how.contains("#77 reverts #42"), "{}", r.how);
        let asked = asked.into_inner().unwrap();
        assert!(!asked.iter().any(|p| p.contains("state=closed")), "{asked:?}");
        assert!(!asked.contains(&format!("/repos/{repo}/commits/{m77}")), "a PR's merge is compared by the PR's diff");
        assert!(asked.contains(&format!("/repos/{repo}/commits?path=src/thing.rs&since={since}&per_page=100")), "the file #42 created is followed first");
        assert_eq!(query_value("a b/ü&x=1.rs"), "a%20b/%C3%BC%26x%3D1.rs");
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
}
