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
            return Err(TooLarge.into());
        }
    }
    Ok(Some(serde_json::from_slice(&body)?))
}

/// A response over its size cap: the same for every node asking (GitHub's answer and the cap are),
/// so unlike a network error it's a definite answer the revert check can act on.
#[derive(Debug)]
pub struct TooLarge;

impl std::fmt::Display for TooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GitHub response too large")
    }
}

impl std::error::Error for TooLarge {}

fn too_large(e: &anyhow::Error) -> bool {
    e.downcast_ref::<TooLarge>().is_some()
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
    /// The evidence it states the revert with.
    pub by: Stated,
}

/// Where a revert candidate states that it is one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stated {
    /// Its own commit message on the default branch says `This reverts commit <merge sha>`: git's
    /// text, which nobody can write before the merge, and which only maintainers land on the
    /// default branch afterwards. It can't be edited.
    Commit,
    /// The description of a PR merged into the default branch (GitHub's `Reverts owner/repo#N`).
    /// Descriptions can be edited after the merge by their authors, so these are weaker evidence,
    /// and the claim PR's own author's weakest of all (audit F55).
    Pr { number: u64, by_claimant: bool },
}

/// Whether `text` says, in so many words, that it reverts PR `pr` of `repo` (merged as
/// `merge_sha`): a line that starts with GitHub's revert-button text `Reverts owner/repo#N` (or
/// `Reverts #N`, or the PR's URL), or with git's `This reverts commit <merge_sha>`. A mention
/// anywhere else ("Reverts #50. Context: #42", "Follow-up to #42") isn't one (audit F20). This is
/// the rule for PR descriptions; commit messages need git's form ([`commit_states_revert_of`]).
pub fn states_revert_of(text: &str, repo: &str, pr: u64, merge_sha: &str) -> bool {
    let repo = repo.to_ascii_lowercase();
    let ends = |rest: &str| !rest.starts_with(|c: char| c.is_ascii_alphanumeric());
    commit_states_revert_of(text, merge_sha)
        || text.lines().map(|l| l.trim().to_ascii_lowercase()).any(|line| {
            let Some(rest) = line.strip_prefix("reverts ") else { return false };
            let rest = rest.trim_start();
            [format!("#{pr}"), format!("{repo}#{pr}"), format!("https://github.com/{repo}/pull/{pr}")].iter().any(|r| rest.strip_prefix(r.as_str()).is_some_and(ends))
        })
}

/// Whether a commit message states the revert of the commit `merge_sha` the way git does: a line
/// starting `This reverts commit <merge_sha>`. Only this form counts in a commit message (audit
/// F55): the claimant's own commits, which land on the default branch with its PR, are written
/// before the merge sha exists, so they can't say it, whereas `Reverts #N` they could.
pub fn commit_states_revert_of(text: &str, merge_sha: &str) -> bool {
    let merge_sha = merge_sha.to_ascii_lowercase();
    let ends = |rest: &str| !rest.starts_with(|c: char| c.is_ascii_alphanumeric());
    text.lines().map(|l| l.trim().to_ascii_lowercase()).any(|line| line.strip_prefix("this reverts commit ").and_then(|rest| rest.strip_prefix(merge_sha.as_str())).is_some_and(ends))
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

/// The commits that may be the revert of PR `pr` (merged into `repo` as `merge_sha`, authored by
/// `author`), in the order validators try them, given `commits` (the default branch since the
/// merge, newest first as GitHub lists them, the claim PR's own commits left out) and `pulls`
/// (PRs that may be reverts). Validators must name the same commit, so it's the commits on the
/// branch, oldest first, that either
/// * say they revert it in git's words ([`commit_states_revert_of`]: `git revert` writes "This
///   reverts commit <sha>."; a revert PR merged with a merge commit brings that commit along), or
/// * are the merge of a revert PR ([`reverts_pr`]; a squash merge's message may name neither);
///
/// then the revert PRs merged before the commits read, earliest first; and last of all the revert
/// PRs the claim PR's author wrote (it can edit their descriptions any time, so they mustn't use
/// up the check's budget ahead of anyone else's: audit F55).
pub fn revert_candidates(repo: &str, pr: u64, merge_sha: &str, author: &str, commits: &[Commit], pulls: &[PullRequest]) -> Vec<Revert> {
    let merge_sha = merge_sha.to_ascii_lowercase();
    let short = |sha: &str| sha.chars().take(12).collect::<String>();
    let by_claimant = |p: &PullRequest| !author.is_empty() && p.author.eq_ignore_ascii_case(author);
    let revert_prs: BTreeMap<&str, &PullRequest> = pulls.iter().filter(|p| reverts_pr(p, repo, pr, &merge_sha)).filter_map(|p| Some((p.merge_sha.as_deref()?, p))).collect();
    let of_pr = |sha: &str, p: &PullRequest| Revert { sha: sha.to_string(), how: format!("{repo}#{} reverts #{pr}, merged as {}", p.number, short(sha)), by: Stated::Pr { number: p.number, by_claimant: by_claimant(p) } };
    let mut out: Vec<Revert> = vec![];
    for c in commits.iter().rev().filter(|c| c.sha != merge_sha) {
        if commit_states_revert_of(&c.message, &merge_sha) {
            out.push(Revert { sha: c.sha.clone(), how: format!("commit {} reverts {}", short(&c.sha), short(&merge_sha)), by: Stated::Commit });
        } else if let Some(p) = revert_prs.get(c.sha.as_str()) {
            out.push(of_pr(&c.sha, p));
        }
    }
    let mut earlier: Vec<(&str, u64, &str)> = revert_prs.iter().filter(|(sha, _)| !out.iter().any(|r| r.sha == **sha)).map(|(sha, p)| (p.merged_at.as_str(), p.number, *sha)).collect();
    earlier.sort();
    out.extend(earlier.into_iter().map(|(_, _, sha)| of_pr(sha, revert_prs[sha])));
    // Stable: each group keeps its order.
    out.sort_by_key(|r| matches!(r.by, Stated::Pr { by_claimant: true, .. }));
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
/// file by file, are removed and added back by `revert`. When `original` has no lines to compare
/// (only binary files, or diffs too large for GitHub to show), by its files instead: `revert`
/// touches more than half of them (audit review-fix-poc-3: no shape of the claim's own diff makes
/// it revert-proof). `None` only when `original` lists no files at all.
pub fn undoes(original: &[FileDiff], revert: &[FileDiff]) -> Option<bool> {
    if original.is_empty() {
        return None;
    }
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
    if total > 0 {
        return Some(2 * matched > total);
    }
    let touched = original.iter().filter(|f| revert.iter().any(|r| r.filename == f.filename)).count();
    Some(2 * touched > original.len())
}

/// Most pages of commits since the merge a revert check reads (100 each), most revert-looking
/// PRs it fetches, files of the PR whose later commits it follows (and most of those commits it
/// asks GitHub the PR of), most pages of a PR's files (100 each) or commits it reads, most
/// candidates whose diffs it fetches, and most of those that touch the claim PR's files it
/// compares.
pub const REVERT_COMMIT_PAGES: u32 = 10;
pub const REVERT_PR_FETCHES: usize = 10;
pub const REVERT_PATHS: usize = 3;
pub const REVERT_PATH_COMMITS: usize = 15;
pub const REVERT_FILE_PAGES: u32 = 3;
pub const REVERT_DIFF_FETCHES: usize = 10;
pub const REVERT_DIFF_CHECKS: usize = 5;

/// The comparison budget of one revert check (audit F55): a candidate counts against
/// [`REVERT_DIFF_CHECKS`] only if its diff touches one of the claim PR's files, so candidates
/// that state a revert but change something else can't use it up; every fetch counts against
/// [`REVERT_DIFF_FETCHES`].
#[derive(Default)]
struct Scan {
    fetched: usize,
    compared: usize,
}

impl Scan {
    fn more(&self) -> bool {
        self.fetched < REVERT_DIFF_FETCHES && self.compared < REVERT_DIFF_CHECKS
    }

    /// Judges a candidate by its diff (`None`: GitHub had none, or too large to read).
    fn undone(&mut self, original: &[FileDiff], diff: Option<&[FileDiff]>) -> bool {
        self.fetched += 1;
        let Some(diff) = diff.filter(|d| d.iter().any(|f| original.iter().any(|o| o.filename == f.filename))) else { return false };
        self.compared += 1;
        undoes(original, diff) == Some(true)
    }
}

/// The revert of PR `pr` (merged as `merge_sha`, authored by `author`): the first of the
/// [`revert_candidates`] whose diff (`diffs`, by sha) [`undoes`] the PR's (`original`), within
/// the comparison budget ([`Scan`]). When the PR's files couldn't be read at all (`original`
/// empty: every page over the size cap), the first candidate that states the revert in git's
/// words in a commit on the default branch ([`Stated::Commit`]; maintainers landed it, nobody can
/// edit it): a defined answer every validator reaches, never "couldn't tell" forever (audit
/// review-fix-poc-3), and never a PR description, which its author could rewrite.
pub fn find_revert_in(repo: &str, pr: u64, merge_sha: &str, author: &str, commits: &[Commit], pulls: &[PullRequest], original: &[FileDiff], diffs: &BTreeMap<String, Vec<FileDiff>>) -> Option<Revert> {
    let candidates = revert_candidates(repo, pr, merge_sha, author, commits, pulls);
    if original.is_empty() {
        return candidates.into_iter().find(|r| r.by == Stated::Commit);
    }
    let mut scan = Scan::default();
    for r in candidates {
        if !scan.more() {
            break;
        }
        if scan.undone(original, diffs.get(&r.sha).map(Vec::as_slice)) {
            return Some(r);
        }
    }
    None
}

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
/// (GitHub unreachable, the PR gone or never merged), so try again later.
///
/// Revert PRs are found from the default branch alone, which only maintainers write: commits
/// that look like a revert landing ([`revert_pr_candidates`]), and the PRs of the commits since
/// the merge that touch the claim's files (`commits?path=`, then `commits/{sha}/pulls`). Not
/// from the list of recently updated closed PRs, which anyone can flood by opening and closing
/// PRs until a revert falls off it (audit F55). The claim PR's own commits (which land on the
/// branch with it, and may carry any date) are left out of all of that.
pub async fn find_revert(repo: &str, pr: u64, merge_sha: &str) -> Result<Option<Revert>> {
    crate::types::check_repo(repo)?;
    find_revert_with(|path| async move { get_json_capped(&path, GITHUB_MAX_LIST_BYTES).await }, repo, pr, merge_sha).await
}

/// Up to `pages` pages (100 entries each) of a list at `path` (which has no query yet), until one
/// isn't full. A page over the size cap ([`TooLarge`]) is skipped, as every node skips it.
async fn pages<F, Fut>(get: &F, path: &str, pages: u32, parse: fn(&Value) -> Vec<FileDiff>, mut done: impl FnMut(&[FileDiff]) -> bool) -> Result<Vec<FileDiff>>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<Option<Value>>>,
{
    let mut out = vec![];
    for page in 1..=pages {
        match get(format!("{path}?per_page=100&page={page}")).await {
            Ok(Some(v)) => {
                let full = v.as_array().is_some_and(|a| a.len() == 100);
                out.extend(parse(&v));
                if !full || done(&out) {
                    break;
                }
            }
            Ok(None) => break,
            Err(e) if too_large(&e) => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

/// A commit list as [`FileDiff`]-shaped names (sha in `filename`), so [`pages`] reads both.
fn commit_shas(v: &Value) -> Vec<FileDiff> {
    parse_commits(v).into_iter().map(|c| FileDiff { filename: c.sha, added: false, patch: None }).collect()
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
    // Every file of the PR the pages hold, binary ones too (audit review-fix-poc-3: the first page
    // alone could be all binary files, sorted ahead of the change).
    let original = pages(&get, &format!("/repos/{repo}/pulls/{pr}/files"), REVERT_FILE_PAGES, parse_files, |_| false).await?;
    let own: std::collections::BTreeSet<String> = pages(&get, &format!("/repos/{repo}/pulls/{pr}/commits"), REVERT_FILE_PAGES, commit_shas, |_| false).await?.into_iter().map(|c| c.filename).collect();
    let merge = merge_sha.to_ascii_lowercase();
    let mut commits = vec![];
    for page in 1..=REVERT_COMMIT_PAGES {
        let Some(v) = get(format!("/repos/{repo}/commits?since={since}&per_page=100&page={page}")).await? else { break };
        let full = v.as_array().is_some_and(|a| a.len() == 100);
        commits.extend(parse_commits(&v).into_iter().filter(|c| !own.contains(&c.sha)));
        if !full {
            break;
        }
    }
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
            if c.sha != merge && !own.contains(&c.sha) && !touching.contains(&c.sha) {
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
    if original.is_empty() {
        return Ok(find_revert_in(repo, pr, merge_sha, &p.author, &commits, &pulls, &original, &BTreeMap::new()));
    }
    let candidates = revert_candidates(repo, pr, merge_sha, &p.author, &commits, &pulls);
    // A candidate that is a PR's merge is compared by the PR's own diff (against its base, the
    // same for every merge method: a merge commit's listed files depend on how GitHub shows a
    // two-parent commit); a plain commit by its own. The scan is `find_revert_in`'s.
    let merged_as: BTreeMap<String, u64> = pulls.iter().filter(|p| p.merged).filter_map(|p| Some((p.merge_sha.clone()?.to_ascii_lowercase(), p.number))).collect();
    let names: std::collections::BTreeSet<&str> = original.iter().map(|f| f.filename.as_str()).collect();
    let mut scan = Scan::default();
    for r in candidates {
        if !scan.more() {
            break;
        }
        let diff = match merged_as.get(&r.sha.to_ascii_lowercase()) {
            // Until the pages hold every file of the claim PR's that this one touches.
            Some(n) => Some(pages(&get, &format!("/repos/{repo}/pulls/{n}/files"), REVERT_FILE_PAGES, parse_files, |d| names.iter().all(|n| d.iter().any(|f| f.filename == *n))).await?),
            None => match get(format!("/repos/{repo}/commits/{}", r.sha)).await {
                Ok(v) => v.map(|v| parse_files(&v)),
                Err(e) if too_large(&e) => None,
                Err(e) => return Err(e),
            },
        };
        if scan.undone(&original, diff.as_deref()) {
            return Ok(Some(r));
        }
    }
    Ok(None)
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
        revert_candidates("jackdishman/blockula", 42, MERGE, "erin", commits, pulls).into_iter().map(|r| (r.sha, undoing_files())).collect()
    }

    #[test]
    fn reverts_found_in_commits_and_pull_requests() {
        let commits = parse_commits(&serde_json::from_str(COMMITS).unwrap());
        let pulls = parse_pulls(&serde_json::from_str(PULLS).unwrap());
        assert_eq!(commits.len(), 4, "entries without a sha are skipped");
        assert_eq!((pulls[1].number, pulls[1].merged_at.as_str(), pulls[2].merged), (61, "2026-10-13T09:12:44Z", false));
        let find = |commits: &[Commit], pulls: &[PullRequest]| find_revert_in("jackdishman/blockula", 42, MERGE, "erin", commits, pulls, &thing_files(), &all_undo(commits, pulls));
        // `git revert` (and a revert PR merged with a merge commit): the oldest commit that says it
        // reverts the merge, never the merge commit itself.
        let r = find(&commits, &pulls).unwrap();
        assert_eq!(r.sha, "c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00");
        let upper = find_revert_in("JackDishman/Blockula", 42, &MERGE.to_uppercase(), "alice", &commits, &[], &thing_files(), &all_undo(&commits, &[]));
        assert_eq!(upper.unwrap().sha, r.sha, "case doesn't matter");
        // A squash-merged revert PR whose message doesn't name the merge: the PR's merge commit,
        // the earliest merged PR into the default branch that states it reverts #42.
        let quiet: Vec<Commit> = commits.iter().filter(|c| c.sha == MERGE || !c.message.contains("This reverts")).cloned().collect();
        assert_eq!(find(&quiet, &pulls).unwrap().sha, "9f1c2e3d4b5a69788796a5b4c3d2e1f00f1e2d3c");
        // Unless the claim PR's own author wrote that one (it can rewrite its description any
        // time): then the others come first (audit F55).
        let by_alice = find_revert_in("jackdishman/blockula", 42, MERGE, "Alice", &quiet, &pulls, &thing_files(), &all_undo(&quiet, &pulls));
        assert_eq!(by_alice.map(|r| (r.sha, r.by)), Some(("6363636363636363636363636363636363636363".into(), Stated::Pr { number: 63, by_claimant: false })));
        // Not reverts: unmerged, another branch, another PR or repo, no statement, the PR itself.
        let not: Vec<PullRequest> = pulls.iter().filter(|p| ![61, 63].contains(&p.number)).cloned().collect();
        assert_eq!(find(&quiet, &not), None);
        assert_eq!(find_revert_in("jackdishman/blockula", 61, MERGE, "alice", &quiet, &pulls[1..2], &thing_files(), &BTreeMap::new()), None);
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
            assert!(revert_candidates("jackdishman/blockula", 42, MERGE, "alice", &commits, std::slice::from_ref(&p)).is_empty(), "{:?}", p.body);
            assert!(revert_candidates("jackdishman/blockula", 42, MERGE, "alice", &[], std::slice::from_ref(&p)).is_empty(), "the fallback too");
        }
        let states = |t: &str| states_revert_of(t, "jackdishman/blockula", 42, MERGE);
        assert!(states("Reverts jackdishman/blockula#42") && states("Reverts #42.") && states("  reverts https://github.com/jackdishman/blockula/pull/42\nmore") && states(&format!("Revert\n\nThis reverts commit {MERGE}.")));
        // In a commit message only git's words count (audit F55).
        assert!(commit_states_revert_of(&format!("Revert\n\nThis reverts commit {MERGE}."), MERGE) && !commit_states_revert_of("Reverts #42", MERGE));
        assert!(!states("Reverts #50. Context: #42") && !states("Reverts #420") && !states("See: Reverts #42") && !states("Reverts otherorg/blockula#42") && !states(&format!("This reverts commit {}.", &MERGE[..12])) && !states("Revert #42"));
        // It says so, but its diff is something else: not a revert.
        let says = pr(61, "Revert \"Add the thing\"", "Reverts #42", sha61);
        let other = parse_files(&json!([{ "filename": "src/lib.rs", "patch": "@@ -9 +9 @@\n-fn unrelated() {}\n+fn unrelated(x: u8) {}" }]));
        let diffs = BTreeMap::from([(sha61.to_string(), other)]);
        assert_eq!(find_revert_in("jackdishman/blockula", 42, MERGE, "alice", &commits, std::slice::from_ref(&says), &thing_files(), &diffs), None);
        // A partial undo (less than half of #42's changed lines) isn't one either; the whole is.
        let half = parse_files(&json!([{ "filename": "src/lib.rs", "patch": "@@ -1,3 +1,2 @@\n mod a;\n-mod thing;\n+mod old_thing;\n mod b;" }]));
        assert_eq!(undoes(&thing_files(), &half), Some(false));
        assert_eq!(undoes(&thing_files(), &undoing_files()), Some(true));
        let diffs = BTreeMap::from([(sha61.to_string(), undoing_files())]);
        assert_eq!(find_revert_in("jackdishman/blockula", 42, MERGE, "alice", &commits, std::slice::from_ref(&says), &thing_files(), &diffs).unwrap().sha, sha61);
        // No lines to compare (binary files): by the files it touches (audit review-fix-poc-3).
        let binary = vec![FileDiff { filename: "logo.png".into(), added: true, patch: None }];
        assert_eq!(undoes(&binary, &undoing_files()), Some(false));
        assert_eq!(undoes(&binary, &[FileDiff { filename: "logo.png".into(), added: false, patch: None }]), Some(true));
        assert_eq!(undoes(&[], &undoing_files()), None);
        assert_eq!(find_revert_in("jackdishman/blockula", 42, MERGE, "alice", &commits, std::slice::from_ref(&says), &binary, &diffs), None, "it touches none of #42's files");
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
            let diffs = revert_candidates("tokio-rs/axum", 3168, merge, "jplatte", commits, pulls).into_iter().map(|r| (r.sha, undo.clone())).collect();
            find_revert_in("tokio-rs/axum", 3168, merge, "jplatte", commits, pulls, &original, &diffs)
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
            (format!("/repos/{repo}/pulls/42/files?per_page=100&page=1"), files),
            (format!("/repos/{repo}/commits?since={since}&per_page=100&page=1"), commits),
            (format!("/repos/{repo}/commits?path=src/thing.rs&since={since}&per_page=100"), json!([{ "sha": undo, "commit": { "message": "Undo the thing" } }, { "sha": MERGE, "commit": { "message": "Add the thing (#42)" } }])),
            (format!("/repos/{repo}/commits/{undo}/pulls"), json!([pr77])),
            (format!("/repos/{repo}/pulls/77/files?per_page=100&page=1"), undoing),
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

    /// A stand-in for GitHub: `routes` by path (absent: 404), each path asked recorded. A route
    /// whose value is the string `"too large"` answers [`TooLarge`].
    struct Mock {
        routes: BTreeMap<String, Value>,
        asked: std::sync::Mutex<Vec<String>>,
    }

    impl Mock {
        fn new(routes: impl IntoIterator<Item = (String, Value)>) -> Mock {
            Mock { routes: routes.into_iter().collect(), asked: Default::default() }
        }
        async fn find(&self, pr: u64) -> Result<Option<Revert>> {
            let get = |path: String| {
                self.asked.lock().unwrap().push(path.clone());
                let v = self.routes.get(&path).cloned();
                async move {
                    match v {
                        Some(Value::String(s)) if s == "too large" => Err(TooLarge.into()),
                        v => Ok(v),
                    }
                }
            };
            find_revert_with(get, "jackdishman/blockula", pr, MERGE).await
        }
        fn asked(&self, path: &str) -> bool {
            self.asked.lock().unwrap().iter().any(|p| p == path)
        }
    }

    const SINCE: &str = "2026-10-11T08:30:00Z";

    fn claim_42(author: &str) -> Value {
        json!({ "number": 42, "state": "closed", "title": "Add the thing", "body": "", "merged_at": SINCE, "merge_commit_sha": MERGE, "user": { "login": author }, "base": { "ref": "main", "repo": { "full_name": "jackdishman/blockula", "default_branch": "main" } } })
    }

    fn files_json(files: &[FileDiff]) -> Value {
        Value::Array(files.iter().map(|f| json!({ "filename": f.filename, "status": if f.added { "added" } else { "modified" }, "patch": f.patch })).collect())
    }

    /// Audit F55, the R1 reviewer's bypass: the claimant puts five decoy commits in its own PR #42
    /// (merged with a merge commit, dated after the merge so the walk since the merge lists them,
    /// right after #42's merge), each saying "Reverts #42's first draft …" and touching #42's
    /// files without undoing them. They used to take all five comparisons ahead of the real revert,
    /// PR #77 (GitHub's revert button: "Reverts jackdishman/blockula#42", branch `revert-42-…`,
    /// its commit hand-edited). Now a commit message counts only in git's words, and the claim
    /// PR's own commits are left out, so #77 is found and no decoy is ever fetched.
    #[tokio::test]
    async fn decoys_in_the_claim_pr_cant_use_up_the_revert_check() {
        let repo = "jackdishman/blockula";
        let base = json!({ "ref": "main", "repo": { "full_name": repo, "default_branch": "main" } });
        let (m77, r77) = ("7777777777777777777777777777777777777777", "0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d");
        let decoys: Vec<String> = (1..=5u8).map(|i| format!("{:02x}", 0xd0 + i).repeat(20)).collect();
        let decoy = |sha: &str, i: usize| json!({ "sha": sha, "commit": { "message": format!("Step {i}\n\nReverts #42's first draft of step {i}") } });
        let pr77 = json!({ "number": 77, "state": "closed", "title": "Revert \"Add the thing\"", "body": "Reverts jackdishman/blockula#42", "merged_at": "2026-10-14T10:00:00Z", "merge_commit_sha": m77, "user": { "login": "dave" }, "base": base });
        let mut walk = vec![json!({ "sha": m77, "commit": { "message": "Merge pull request #77 from dave/revert-42-add-the-thing\n\nRevert \"Add the thing\"" } }), json!({ "sha": r77, "commit": { "message": "Undo the thing" } })];
        walk.extend(decoys.iter().enumerate().map(|(i, d)| decoy(d, i + 1)));
        walk.push(json!({ "sha": MERGE, "commit": { "message": "Merge pull request #42 from mallory/thing" } }));
        let own: Vec<Value> = decoys.iter().enumerate().map(|(i, d)| decoy(d, i + 1)).collect();
        let not_undoing = files_json(&[FileDiff { filename: "src/thing.rs".into(), added: false, patch: Some("@@ -1 +1 @@\n-    42\n+    43".into()) }]);
        let mut path_list = vec![json!({ "sha": r77, "commit": { "message": "Undo the thing" } })];
        path_list.extend(own.iter().cloned());
        let mut routes = vec![
            (format!("/repos/{repo}/pulls/42"), claim_42("mallory")),
            (format!("/repos/{repo}/pulls/42/files?per_page=100&page=1"), files_json(&thing_files())),
            (format!("/repos/{repo}/pulls/42/commits?per_page=100&page=1"), Value::Array(own)),
            (format!("/repos/{repo}/commits?since={SINCE}&per_page=100&page=1"), Value::Array(walk)),
            (format!("/repos/{repo}/commits?path=src/thing.rs&since={SINCE}&per_page=100"), Value::Array(path_list)),
            (format!("/repos/{repo}/commits/{r77}/pulls"), json!([pr77.clone()])),
            (format!("/repos/{repo}/pulls/77"), pr77),
            (format!("/repos/{repo}/pulls/77/files?per_page=100&page=1"), files_json(&undoing_files())),
        ];
        for d in &decoys {
            routes.push((format!("/repos/{repo}/commits/{d}"), json!({ "files": not_undoing })));
            routes.push((format!("/repos/{repo}/commits/{d}/pulls"), json!([claim_42("mallory")])));
        }
        let gh = Mock::new(routes);
        let r = gh.find(42).await.unwrap().expect("the real revert is found");
        assert_eq!((r.sha.as_str(), r.by), (m77, Stated::Pr { number: 77, by_claimant: false }));
        assert!(decoys.iter().all(|d| !gh.asked(&format!("/repos/{repo}/commits/{d}")) && !gh.asked(&format!("/repos/{repo}/commits/{d}/pulls"))), "no decoy is looked at");

        // The claimant's own merged follow-up PRs, touching #42's files, with descriptions edited
        // after the merge to say "Reverts #42": tried after everyone else's.
        let commits = parse_commits(&json!([
            { "sha": m77, "commit": { "message": "Merge pull request #77 from dave/revert-42-add-the-thing" } },
            { "sha": "8484848484848484848484848484848484848484", "commit": { "message": "Tidy the thing (#84)" } },
            { "sha": "8383838383838383838383838383838383838383", "commit": { "message": "Tidy the thing (#83)" } },
            { "sha": "8282828282828282828282828282828282828282", "commit": { "message": "Tidy the thing (#82)" } },
            { "sha": "8181818181818181818181818181818181818181", "commit": { "message": "Tidy the thing (#81)" } },
            { "sha": "8080808080808080808080808080808080808080", "commit": { "message": "Tidy the thing (#80)" } },
            { "sha": MERGE, "commit": { "message": "Add the thing (#42)" } }
        ]));
        let follow_up = |n: u64| parse_pr(&json!({ "number": n, "state": "closed", "body": "Reverts #42", "merged_at": "2026-10-12T00:00:00Z", "merge_commit_sha": format!("{n}").repeat(20), "user": { "login": "Mallory" }, "base": base }));
        let mut pulls: Vec<PullRequest> = (80..=84).map(follow_up).collect();
        pulls.push(parse_pr(&json!({ "number": 77, "state": "closed", "body": "Reverts jackdishman/blockula#42", "merged_at": "2026-10-14T10:00:00Z", "merge_commit_sha": m77, "user": { "login": "dave" }, "base": base })));
        let mut diffs: BTreeMap<String, Vec<FileDiff>> = (80..=84u64).map(|n| (format!("{n}").repeat(20), parse_files(&not_undoing))).collect();
        diffs.insert(m77.into(), undoing_files());
        assert_eq!(find_revert_in(repo, 42, MERGE, "mallory", &commits, &pulls, &thing_files(), &diffs).map(|r| r.sha), Some(m77.to_string()));
        // Revert PRs that change none of #42's files don't count against the comparisons (only
        // against the fetches): six of them ahead of #77 by someone else, and #77 is still found.
        let others: Vec<PullRequest> = (80..=85).map(|n| PullRequest { author: "sock".into(), ..follow_up(n) }).collect();
        let mut pulls = others.clone();
        pulls.push(parse_pr(&json!({ "number": 77, "state": "closed", "body": "Reverts #42", "merged_at": "2026-10-14T10:00:00Z", "merge_commit_sha": m77, "user": { "login": "dave" }, "base": base })));
        let readme = parse_files(&json!([{ "filename": "README.md", "patch": "@@ -1 +1 @@\n-a\n+b" }]));
        let mut diffs: BTreeMap<String, Vec<FileDiff>> = others.iter().map(|p| (p.merge_sha.clone().unwrap(), readme.clone())).collect();
        diffs.insert(m77.into(), undoing_files());
        let mut commits = commits;
        commits.insert(1, Commit { sha: "85".repeat(20), message: "Tidy (#85)".into() });
        assert_eq!(find_revert_in(repo, 42, MERGE, "mallory", &commits, &pulls, &thing_files(), &diffs).map(|r| r.sha), Some(m77.to_string()));
    }

    /// Audit review-fix-poc-3: the claim PR's first page of files is 100 binary files (no patch;
    /// GitHub sorts them ahead of `src/`), its real change on page 2. Maintainers revert it with
    /// GitHub's button (PR #61, merged with a merge commit, bringing git's "This reverts commit
    /// <merge>"). The check reads the next page and compares the change. Every page of #42's files
    /// all binary: judged by the files the revert touches. Every page over the size cap: the
    /// commit stating the revert in git's words, a defined answer, not an error every retry.
    #[tokio::test]
    async fn no_shape_of_the_claim_pr_makes_it_revert_proof() {
        let repo = "jackdishman/blockula";
        let base = json!({ "ref": "main", "repo": { "full_name": repo, "default_branch": "main" } });
        let (m61, r61) = ("6161616161616161616161616161616161616161", "5151515151515151515151515151515151515151");
        let icons = |from: usize, n: usize| -> Vec<FileDiff> { (from..from + n).map(|i| FileDiff { filename: format!("assets/icon{i:03}.png"), added: true, patch: None }).collect() };
        let pr61 = json!({ "number": 61, "state": "closed", "title": "Revert \"Add the thing\"", "body": "Reverts jackdishman/blockula#42", "merged_at": "2026-10-13T09:12:44Z", "merge_commit_sha": m61, "user": { "login": "dave" }, "base": base });
        let walk = json!([
            { "sha": m61, "commit": { "message": "Merge pull request #61 from dave/revert-42-thing\n\nRevert \"Add the thing\"" } },
            { "sha": r61, "commit": { "message": format!("Revert \"Add the thing\"\n\nThis reverts commit {MERGE}, reversing\nchanges made to 77aa88bb99cc00dd11ee22ff33aa44bb55cc66dd.") } },
            { "sha": MERGE, "commit": { "message": "Merge pull request #42 from alice/thing" } }
        ]);
        let routes = |pages: Vec<Value>, revert: Vec<FileDiff>| {
            let mut r = vec![
                (format!("/repos/{repo}/pulls/42"), claim_42("alice")),
                (format!("/repos/{repo}/commits?since={SINCE}&per_page=100&page=1"), walk.clone()),
                (format!("/repos/{repo}/commits/{r61}"), json!({ "sha": r61, "files": files_json(&revert) })),
                (format!("/repos/{repo}/pulls/61"), pr61.clone()),
                (format!("/repos/{repo}/pulls/61/files?per_page=100&page=1"), files_json(&revert)),
            ];
            r.extend(pages.into_iter().enumerate().map(|(i, v)| (format!("/repos/{repo}/pulls/42/files?per_page=100&page={}", i + 1), v)));
            r
        };
        let removed = |files: &[FileDiff]| -> Vec<FileDiff> { files.iter().map(|f| FileDiff { added: false, ..f.clone() }).collect() };
        // 100 icons, then the change.
        let mut revert = removed(&icons(0, 100));
        revert.extend(undoing_files());
        let gh = Mock::new(routes(vec![files_json(&icons(0, 100)), files_json(&thing_files())], revert.clone()));
        assert_eq!(gh.find(42).await.unwrap().map(|r| (r.sha, r.by)), Some((r61.to_string(), Stated::Commit)));
        // Only icons, three full pages of them: the revert touches them all.
        let all = icons(0, 300);
        let gh = Mock::new(routes(all.chunks(100).map(files_json).collect(), removed(&all)));
        assert_eq!(gh.find(42).await.unwrap().map(|r| r.sha), Some(r61.to_string()));
        // A commit that changes something else isn't the revert, even with icons only.
        let gh = Mock::new(routes(all.chunks(100).map(files_json).collect(), undoing_files()));
        assert_eq!(gh.find(42).await.unwrap(), None);
        // Every page over the size cap: the commit that says so in git's words.
        let big = || vec![Value::from("too large"); 3];
        let gh = Mock::new(routes(big(), vec![]));
        assert_eq!(gh.find(42).await.unwrap().map(|r| (r.sha, r.by)), Some((r61.to_string(), Stated::Commit)));
        // And with only a PR description saying so (which its author can edit): no revert, and a
        // definite answer.
        let quiet = json!([{ "sha": m61, "commit": { "message": "Merge pull request #61 from dave/revert-42-thing" } }, { "sha": r61, "commit": { "message": "Undo" } }, { "sha": MERGE, "commit": { "message": "Merge pull request #42 from alice/thing" } }]);
        let mut r = routes(big(), vec![]);
        r[1].1 = quiet;
        assert_eq!(Mock::new(r).find(42).await.unwrap(), None);
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
