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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    pub message: String,
    /// Its parents' shas, first parent first (40-hex ones only).
    pub parents: Vec<String>,
}

fn is_sha(h: &str) -> bool {
    h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A commit list (entries without a 40-hex sha are skipped).
pub fn parse_commits(v: &Value) -> Vec<Commit> {
    let list = v.as_array().map(Vec::as_slice).unwrap_or_default();
    let sha = |v: &Value| v.as_str().filter(|h| is_sha(h)).map(str::to_ascii_lowercase);
    list.iter()
        .filter_map(|c| {
            let parents = c["parents"].as_array().map(|ps| ps.iter().filter_map(|p| sha(&p["sha"])).collect()).unwrap_or_default();
            Some(Commit { sha: sha(&c["sha"])?, message: c["commit"]["message"].as_str().unwrap_or_default().to_string(), parents })
        })
        .collect()
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileDiff {
    pub filename: String,
    /// GitHub's `status`: `added`, `removed`, `modified`, `renamed`, `copied`, `changed` or
    /// `unchanged`.
    pub status: String,
    /// The blob GitHub names for it (`sha`: the file as the diff leaves it; for a removed file,
    /// the blob removed), lowercase 40-hex.
    pub sha: Option<String>,
    /// A renamed file's old name.
    pub previous_filename: Option<String>,
    pub patch: Option<String>,
}

impl FileDiff {
    /// The diff created the file.
    pub fn added(&self) -> bool {
        self.status == "added"
    }

    /// Where the file was before the diff (its old name if renamed).
    fn before(&self) -> &str {
        self.previous_filename.as_deref().unwrap_or(&self.filename)
    }
}

/// A file list (a bare array, or a commit's `files`; entries without a name are skipped).
pub fn parse_files(v: &Value) -> Vec<FileDiff> {
    let list = v.as_array().or_else(|| v["files"].as_array()).map(Vec::as_slice).unwrap_or_default();
    let text = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(String::from);
    list.iter()
        .filter_map(|f| {
            Some(FileDiff {
                filename: text(&f["filename"])?,
                status: f["status"].as_str().unwrap_or_default().to_string(),
                sha: f["sha"].as_str().filter(|h| is_sha(h)).map(str::to_ascii_lowercase),
                previous_filename: text(&f["previous_filename"]),
                patch: f["patch"].as_str().map(String::from),
            })
        })
        .collect()
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

/// Whether the diff `revert` brings back the pre-PR state of `original`'s file `f`, which has no
/// lines to compare (a binary file, or a diff too large for GitHub to show): a file the PR added
/// (or copied) is removed, one it removed is added back (as the same blob, when GitHub names
/// both), one it changed (or renamed) gets back the blob it had before the PR (`base`: path →
/// blob sha at the PR's base, [`base_blobs`]), under its old name. Merely touching the file isn't
/// enough (audit review-r2-poc-1: a later "compress the icons" PR isn't a revert of adding them).
fn restores(f: &FileDiff, revert: &[FileDiff], base: &BTreeMap<String, String>) -> bool {
    let named = |name: &str| revert.iter().find(|r| r.filename == name);
    let zero = |h: &str| h.bytes().all(|b| b == b'0');
    match f.status.as_str() {
        "added" | "copied" => named(&f.filename).is_some_and(|r| r.status == "removed"),
        "removed" => named(&f.filename).is_some_and(|r| r.status == "added" && match (&f.sha, &r.sha) {
            (Some(was), Some(now)) if !zero(was) => was == now,
            _ => true,
        }),
        "modified" | "changed" | "renamed" => {
            let Some(blob) = base.get(f.before()) else { return false };
            let back = named(f.before()).is_some_and(|r| r.status != "removed" && r.sha.as_ref() == Some(blob));
            // A rename is undone by renaming back, or by removing the new name and adding the old.
            let gone = f.status != "renamed" || named(f.before()).is_some_and(|r| r.previous_filename.as_deref() == Some(f.filename.as_str())) || named(&f.filename).is_some_and(|r| r.status == "removed");
            back && gone
        }
        _ => false,
    }
}

/// Whether the diff `revert` undoes the diff `original` (audit F20: a revert must actually revert
/// the claim's change, not just say so): more than half of the lines `original` added and removed,
/// file by file, are removed and added back by `revert`. When `original` has no lines to compare
/// (only binary files, or diffs too large for GitHub to show), by its files instead: `revert`
/// brings more than half of them back to their pre-PR state ([`restores`]; audit
/// review-fix-poc-3 and review-r2-poc-1). `None` when there is nothing to judge: `original` lists
/// no files, or none whose pre-PR state is known.
pub fn undoes(original: &[FileDiff], revert: &[FileDiff], base: &BTreeMap<String, String>) -> Option<bool> {
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
    let known = original.iter().filter(|f| matches!(f.status.as_str(), "added" | "copied" | "removed") || base.contains_key(f.before())).count();
    if known == 0 {
        return None;
    }
    let restored = original.iter().filter(|f| restores(f, revert, base)).count();
    Some(2 * restored > original.len())
}

/// Whether any file of `original` has lines [`undoes`] compares (then those decide).
fn has_lines(original: &[FileDiff]) -> bool {
    original.iter().filter_map(|f| f.patch.as_deref()).any(|p| {
        let (added, removed) = changed_lines(p);
        !added.is_empty() || !removed.is_empty()
    })
}

/// The patchless files of `original` whose pre-PR blob a revert must bring back (changed or
/// renamed ones), when it has no lines to compare at all; their paths before the PR.
fn needs_base(original: &[FileDiff]) -> Vec<&str> {
    if has_lines(original) {
        return vec![];
    }
    original.iter().filter(|f| matches!(f.status.as_str(), "modified" | "changed" | "renamed")).map(FileDiff::before).collect()
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
/// Most directories whose listing at the PR's base a check reads, for the pre-PR blobs of a
/// binary-only PR's changed files ([`restores`]).
pub const REVERT_BASE_DIRS: usize = 3;
/// Paths longer than this (percent-encoded) aren't followed or looked up: GitHub may refuse such a
/// URL, and a refusal would read as "unreachable" on every retry.
pub const REVERT_MAX_PATH: usize = 1_024;

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
    fn undone(&mut self, original: &[FileDiff], diff: Option<&[FileDiff]>, base: &BTreeMap<String, String>) -> bool {
        self.fetched += 1;
        let touches = |f: &FileDiff| original.iter().any(|o| o.filename == f.filename || o.before() == f.filename || f.previous_filename.as_deref() == Some(o.filename.as_str()));
        let Some(diff) = diff.filter(|d| d.iter().any(touches)) else { return false };
        self.compared += 1;
        undoes(original, diff, base) == Some(true)
    }
}

/// The revert of PR `pr` (merged as `merge_sha`, authored by `author`): the first of the
/// [`revert_candidates`] whose diff (`diffs`, by sha) [`undoes`] the PR's (`original`, with the
/// pre-PR blobs `base` of its binary files), within the comparison budget ([`Scan`]). A PR with
/// no files to compare has no revert: nothing is attested without a diff check (audit
/// review-r2-poc-1; since oversized pages are read in smaller ones, that's a PR that changes no
/// file, or whose every file entry alone is over the size cap). [`find_revert_with`] runs the same
/// scan, fetching each candidate's diff as it gets to it; this is its pure form, for tests.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub fn find_revert_in(repo: &str, pr: u64, merge_sha: &str, author: &str, commits: &[Commit], pulls: &[PullRequest], original: &[FileDiff], diffs: &BTreeMap<String, Vec<FileDiff>>, base: &BTreeMap<String, String>) -> Option<Revert> {
    if original.is_empty() {
        return None;
    }
    let mut scan = Scan::default();
    for r in revert_candidates(repo, pr, merge_sha, author, commits, pulls) {
        if !scan.more() {
            break;
        }
        if scan.undone(original, diffs.get(&r.sha).map(Vec::as_slice), base) {
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
/// with a patch first (their lines decide), then binary ones (a binary-only PR's revert is found
/// this way too); among each, files it created first (later commits touching one are few and
/// telling), then the rest, in GitHub's order. Paths too long to put in a URL are left out.
fn followed_paths(files: &[FileDiff]) -> Vec<&str> {
    let mut paths: Vec<&FileDiff> = files.iter().filter(|f| query_value(&f.filename).len() <= REVERT_MAX_PATH).collect();
    paths.sort_by_key(|f| (f.patch.is_none(), !f.added()));
    paths.into_iter().take(REVERT_PATHS).map(|f| f.filename.as_str()).collect()
}

/// Looks for a revert of a claim's PR on GitHub (see [`find_revert_in`]). `Err`: couldn't tell,
/// so try again later: GitHub couldn't be reached or answered with an error (a rate limit, an
/// outage), or the PR is gone or was never merged. Every other answer GitHub gives, an oversized
/// one included ([`page_of`]), is a definite answer that every validator reads the same way.
///
/// Revert PRs are found from the default branch alone, which only maintainers write: commits
/// that look like a revert landing ([`revert_pr_candidates`]), and the PRs of the commits since
/// the merge that touch the claim's files (`commits?path=`, then `commits/{sha}/pulls`). Not
/// from the list of recently updated closed PRs, which anyone can flood by opening and closing
/// PRs until a revert falls off it (audit F55). The claim PR's own commits (which land on the
/// branch with it, and may carry any date) are left out of all of that: those GitHub lists for
/// the PR, and every ancestor of its merge (`commits?sha=<merge>`: the commits a merge commit
/// brings along, however many, and the copies a rebase merge lands, which end at the merge sha).
/// Never by message: a revert made later can't be an ancestor of the merge, but its message is
/// one the claimant could have written in advance (audit review-r2-poc-2).
pub async fn find_revert(repo: &str, pr: u64, merge_sha: &str) -> Result<Option<Revert>> {
    crate::types::check_repo(repo)?;
    find_revert_with(|path| async move { get_json_capped(&path, GITHUB_MAX_LIST_BYTES).await }, repo, pr, merge_sha).await
}

/// `path` with `per_page` and `page` added to its query.
fn paged(path: &str, per_page: u32, page: u32) -> String {
    format!("{path}{}per_page={per_page}&page={page}", if path.contains('?') { '&' } else { '?' })
}

/// Entries in a list response.
fn entries(v: &Value) -> usize {
    v.as_array().map_or(0, Vec::len)
}

/// Page `page` (100 entries) of the list at `path`, and how many entries GitHub has there
/// (`< 100`: the list ends). A page over the size cap is read again as ten pages of 10, and such a
/// page over the cap as ten of 1; only an entry over the cap by itself is skipped (counted, not
/// returned). Every node asks the same pages and skips the same entries, so an oversized answer
/// (a claimant can make one: squash messages, commit bodies) is a definite one, never an error
/// retried for the whole revert window (audit review-fix-poc-3).
async fn page_of<F, Fut, T>(get: &F, path: &str, page: u32, parse: fn(&Value) -> Vec<T>) -> Result<(Vec<T>, usize)>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<Option<Value>>>,
{
    match get(paged(path, 100, page)).await {
        Ok(Some(v)) => return Ok((parse(&v), entries(&v))),
        Ok(None) => return Ok((vec![], 0)),
        Err(e) if too_large(&e) => {}
        Err(e) => return Err(e),
    }
    let (mut out, mut count) = (vec![], 0);
    for tens in (page - 1) * 10 + 1..=page * 10 {
        let n = match get(paged(path, 10, tens)).await {
            Ok(Some(v)) => {
                out.extend(parse(&v));
                entries(&v)
            }
            Ok(None) => 0,
            Err(e) if too_large(&e) => {
                let mut n = 0;
                for one in (tens - 1) * 10 + 1..=tens * 10 {
                    match get(paged(path, 1, one)).await {
                        Ok(Some(v)) if entries(&v) > 0 => {
                            out.extend(parse(&v));
                            n += 1;
                        }
                        Ok(_) => break,
                        Err(e) if too_large(&e) => n += 1,
                        Err(e) => return Err(e),
                    }
                }
                n
            }
            Err(e) => return Err(e),
        };
        count += n;
        if n < 10 {
            break;
        }
    }
    Ok((out, count))
}

/// Up to `pages` pages (100 entries each, [`page_of`]) of a list at `path`, until one isn't full
/// or `done` says so.
async fn pages<F, Fut, T>(get: &F, path: &str, pages: u32, parse: fn(&Value) -> Vec<T>, done: impl FnMut(&[T]) -> bool) -> Result<Vec<T>>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<Option<Value>>>,
{
    Ok(list(get, path, pages, parse, done).await?.0)
}

/// [`pages`], and whether the list may go on past them (every page read was full).
async fn list<F, Fut, T>(get: &F, path: &str, pages: u32, parse: fn(&Value) -> Vec<T>, mut done: impl FnMut(&[T]) -> bool) -> Result<(Vec<T>, bool)>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<Option<Value>>>,
{
    let mut out = vec![];
    for page in 1..=pages {
        let (found, count) = page_of(get, path, page, parse).await?;
        out.extend(found);
        if count < 100 || done(&out) {
            return Ok((out, false));
        }
    }
    Ok((out, true))
}

/// `get`'s answer, with one over the size cap read as none (a single object, so there's no
/// smaller page to ask for; every node gets the same answer).
async fn get_or_none<F, Fut>(get: &F, path: String) -> Result<Option<Value>>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<Option<Value>>>,
{
    match get(path).await {
        Err(e) if too_large(&e) => Ok(None),
        r => r,
    }
}

/// The commit a PR merged as `merge` was merged onto (its pre-PR state), given `ancestors`
/// (`commits?sha=<merge>&since=<merged_at>`, by sha): the first first-parent ancestor of `merge`
/// that isn't listed there. A merge commit's or a squash's first parent was on the branch before
/// the merge; a rebase merge's copies were all committed when it merged, so the walk steps back
/// over them to the commit before the first. `None` if `merge` itself isn't listed.
fn merge_base(merge: &str, ancestors: &BTreeMap<&str, &Commit>) -> Option<String> {
    let mut at = *ancestors.get(merge)?;
    for _ in 0..ancestors.len() {
        let first = at.parents.first()?;
        match ancestors.get(first.as_str()) {
            Some(c) => at = c,
            None => return Some(first.clone()),
        }
    }
    None
}

/// The blobs `paths` had at commit `base` (path → blob sha): from the listings of their
/// directories (`GET /repos/{repo}/contents/{dir}?ref=`, which name every file's blob whatever its
/// size), at most [`REVERT_BASE_DIRS`] of them. A path not found isn't in the map.
async fn base_blobs<F, Fut>(get: &F, repo: &str, base: &str, paths: &[&str]) -> Result<BTreeMap<String, String>>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<Option<Value>>>,
{
    let mut dirs: Vec<&str> = vec![];
    for p in paths {
        let dir = p.rsplit_once('/').map_or("", |(d, _)| d);
        if !dirs.contains(&dir) && query_value(dir).len() <= REVERT_MAX_PATH {
            dirs.push(dir);
        }
    }
    let mut out = BTreeMap::new();
    for dir in dirs.into_iter().take(REVERT_BASE_DIRS) {
        let path = if dir.is_empty() { format!("/repos/{repo}/contents?ref={base}") } else { format!("/repos/{repo}/contents/{}?ref={base}", query_value(dir)) };
        let Some(v) = get_or_none(get, path).await? else { continue };
        fn file(e: &Value) -> Option<(&str, &str)> {
            if e["type"] != "file" {
                return None;
            }
            Some((e["path"].as_str()?, e["sha"].as_str().filter(|h| is_sha(h))?))
        }
        for (path, sha) in v.as_array().map(Vec::as_slice).unwrap_or_default().iter().filter_map(file).filter(|(p, _)| paths.contains(p)) {
            out.insert(path.to_string(), sha.to_ascii_lowercase());
        }
    }
    Ok(out)
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
        async move { Ok::<_, anyhow::Error>(get_or_none(get, path).await?.map(|v| parse_pr(&v))) }
    };
    let Some(p) = fetch_pr(pr).await? else { bail!("{repo}#{pr} doesn't exist") };
    // Goes into a query string: GitHub's own timestamp format only.
    if p.merged_at.len() != 20 || !p.merged_at.bytes().all(|b| b.is_ascii_digit() || b"-:TZ".contains(&b)) {
        bail!("{repo}#{pr} has no merge time");
    }
    let since = &p.merged_at;
    let merge = merge_sha.to_ascii_lowercase();
    // Every file of the PR the pages hold, binary ones too (audit review-fix-poc-3: the first page
    // alone could be all binary files, sorted ahead of the change).
    let original = pages(&get, &format!("/repos/{repo}/pulls/{pr}/files"), REVERT_FILE_PAGES, parse_files, |_| false).await?;
    // The PR's own commits: none of them can be its revert, and they mustn't fill the windows
    // below (audit F55). By sha only, never by message (audit review-r2-poc-2): those GitHub lists
    // for the PR (250 at most), and the merge's ancestors since it merged, which hold every one of
    // them that reached the branch (a rebase merge's copies too) whatever their number.
    let own_commits = pages(&get, &format!("/repos/{repo}/pulls/{pr}/commits"), REVERT_FILE_PAGES, parse_commits, |_| false).await?;
    let (ancestors, more_ancestors) = list(&get, &format!("/repos/{repo}/commits?sha={merge}&since={since}"), REVERT_COMMIT_PAGES, parse_commits, |_| false).await?;
    let own: std::collections::BTreeSet<&str> = own_commits.iter().chain(&ancestors).map(|c| c.sha.as_str()).chain([merge.as_str()]).collect();
    let walk = pages(&get, &format!("/repos/{repo}/commits?since={since}"), REVERT_COMMIT_PAGES, parse_commits, |_| false).await?;
    // The branch lists everything after the merge before the merge itself, so when the walk reaches
    // the merge it holds every later commit there is. If the merge's ancestors since it merged
    // didn't all fit in the pages read (a claim PR of more than 1,000 commits), the PR's commits
    // past them could still turn up in a path's history: then only commits the walk holds are
    // followed.
    let walk_only = more_ancestors && walk.iter().any(|c| c.sha == merge);
    let in_walk: std::collections::BTreeSet<String> = if walk_only { walk.iter().map(|c| c.sha.clone()).collect() } else { Default::default() };
    let commits: Vec<Commit> = walk.into_iter().filter(|c| !own.contains(c.sha.as_str())).collect();
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
        let listed = pages(&get, &format!("/repos/{repo}/commits?path={}&since={since}", query_value(path)), 1, parse_commits, |_| false).await?;
        for c in listed.into_iter().rev() {
            if !own.contains(c.sha.as_str()) && (!walk_only || in_walk.contains(&c.sha)) && !touching.contains(&c.sha) {
                touching.push(c.sha);
            }
        }
    }
    for sha in touching.iter().take(REVERT_PATH_COMMITS) {
        if let Some(v) = get_or_none(&get, format!("/repos/{repo}/commits/{sha}/pulls")).await? {
            add(parse_pulls(&v), &mut pulls);
        }
    }
    let known: Vec<u64> = pulls.iter().map(|p| p.number).collect();
    for n in revert_pr_candidates(&commits).into_iter().filter(|n| *n != pr && !known.contains(n)).take(REVERT_PR_FETCHES) {
        if let Some(p) = fetch_pr(n).await? {
            add(vec![p], &mut pulls);
        }
    }
    let candidates = revert_candidates(repo, pr, merge_sha, &p.author, &commits, &pulls);
    if original.is_empty() || candidates.is_empty() {
        return Ok(None);
    }
    // A binary-only PR's changed files must get their pre-PR blobs back: read them at the commit
    // it merged onto.
    let wanted = needs_base(&original);
    let base = match (wanted.is_empty(), merge_base(&merge, &ancestors.iter().map(|c| (c.sha.as_str(), c)).collect())) {
        (false, Some(at)) => base_blobs(&get, repo, &at, &wanted).await?,
        _ => BTreeMap::new(),
    };
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
            // Until the pages read hold every one of the claim PR's files (or run out).
            Some(n) => Some(pages(&get, &format!("/repos/{repo}/pulls/{n}/files"), REVERT_FILE_PAGES, parse_files, |d| names.iter().all(|n| d.iter().any(|f| f.filename == *n || f.previous_filename.as_deref() == Some(*n)))).await?),
            None => get_or_none(&get, format!("/repos/{repo}/commits/{}", r.sha)).await?.map(|v| parse_files(&v)),
        };
        if scan.undone(&original, diff.as_deref(), &base) {
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
        let find = |commits: &[Commit], pulls: &[PullRequest]| find_revert_in("jackdishman/blockula", 42, MERGE, "erin", commits, pulls, &thing_files(), &all_undo(commits, pulls), &BTreeMap::new());
        // `git revert` (and a revert PR merged with a merge commit): the oldest commit that says it
        // reverts the merge, never the merge commit itself.
        let r = find(&commits, &pulls).unwrap();
        assert_eq!(r.sha, "c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00");
        let upper = find_revert_in("JackDishman/Blockula", 42, &MERGE.to_uppercase(), "alice", &commits, &[], &thing_files(), &all_undo(&commits, &[]), &BTreeMap::new());
        assert_eq!(upper.unwrap().sha, r.sha, "case doesn't matter");
        // A squash-merged revert PR whose message doesn't name the merge: the PR's merge commit,
        // the earliest merged PR into the default branch that states it reverts #42.
        let quiet: Vec<Commit> = commits.iter().filter(|c| c.sha == MERGE || !c.message.contains("This reverts")).cloned().collect();
        assert_eq!(find(&quiet, &pulls).unwrap().sha, "9f1c2e3d4b5a69788796a5b4c3d2e1f00f1e2d3c");
        // Unless the claim PR's own author wrote that one (it can rewrite its description any
        // time): then the others come first (audit F55).
        let by_alice = find_revert_in("jackdishman/blockula", 42, MERGE, "Alice", &quiet, &pulls, &thing_files(), &all_undo(&quiet, &pulls), &BTreeMap::new());
        assert_eq!(by_alice.map(|r| (r.sha, r.by)), Some(("6363636363636363636363636363636363636363".into(), Stated::Pr { number: 63, by_claimant: false })));
        // Not reverts: unmerged, another branch, another PR or repo, no statement, the PR itself.
        let not: Vec<PullRequest> = pulls.iter().filter(|p| ![61, 63].contains(&p.number)).cloned().collect();
        assert_eq!(find(&quiet, &not), None);
        assert_eq!(find_revert_in("jackdishman/blockula", 61, MERGE, "alice", &quiet, &pulls[1..2], &thing_files(), &BTreeMap::new(), &BTreeMap::new()), None);
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
        assert_eq!(find_revert_in("jackdishman/blockula", 42, MERGE, "alice", &commits, std::slice::from_ref(&says), &thing_files(), &diffs, &BTreeMap::new()), None);
        // A partial undo (less than half of #42's changed lines) isn't one either; the whole is.
        let half = parse_files(&json!([{ "filename": "src/lib.rs", "patch": "@@ -1,3 +1,2 @@\n mod a;\n-mod thing;\n+mod old_thing;\n mod b;" }]));
        assert_eq!(undoes(&thing_files(), &half, &BTreeMap::new()), Some(false));
        assert_eq!(undoes(&thing_files(), &undoing_files(), &BTreeMap::new()), Some(true));
        let diffs = BTreeMap::from([(sha61.to_string(), undoing_files())]);
        assert_eq!(find_revert_in("jackdishman/blockula", 42, MERGE, "alice", &commits, std::slice::from_ref(&says), &thing_files(), &diffs, &BTreeMap::new()).unwrap().sha, sha61);
        // No lines to compare (binary files): by the files it touches (audit review-fix-poc-3).
        let binary = vec![FileDiff { filename: "logo.png".into(), status: "added".into(), ..Default::default() }];
        assert_eq!(undoes(&binary, &undoing_files(), &BTreeMap::new()), Some(false));
        let logo = |status: &str| [FileDiff { filename: "logo.png".into(), status: status.into(), ..Default::default() }];
        assert_eq!(undoes(&binary, &logo("removed"), &BTreeMap::new()), Some(true));
        // Changing the file it added isn't undoing it (audit review-r2-poc-1).
        assert_eq!(undoes(&binary, &logo("modified"), &BTreeMap::new()), Some(false));
        assert_eq!(undoes(&[], &undoing_files(), &BTreeMap::new()), None);
        assert_eq!(find_revert_in("jackdishman/blockula", 42, MERGE, "alice", &commits, std::slice::from_ref(&says), &binary, &diffs, &BTreeMap::new()), None, "it touches none of #42's files");
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
            find_revert_in("tokio-rs/axum", 3168, merge, "jplatte", commits, pulls, &original, &diffs, &BTreeMap::new())
        };
        let pulls = [other, reverted.clone()];
        let r = find(&commits, &pulls).unwrap();
        assert_eq!(r.sha, "3525a1345987a1f3cead86a995cb3ca39a72b629");
        assert_eq!(find(&commits, &[]), None, "a title alone isn't a revert of #3168");
        // A body naming only the merge commit is enough too.
        let by_sha = PullRequest { body: "This reverts commit e09cc593655de82d01971b55130a83842ac46684.".into(), ..reverted };
        assert_eq!(find(&commits, &[by_sha]).unwrap().sha, r.sha);
        commits.insert(0, Commit { sha: "f".repeat(40), message: format!("Revert again\n\nThis reverts commit {merge}."), ..Default::default() });
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
            (format!("/repos/{repo}/commits?path=src/thing.rs&since={since}&per_page=100&page=1"), json!([{ "sha": undo, "commit": { "message": "Undo the thing" } }, { "sha": MERGE, "commit": { "message": "Add the thing (#42)" } }])),
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
        assert!(asked.contains(&format!("/repos/{repo}/commits?path=src/thing.rs&since={since}&per_page=100&page=1")), "the file #42 created is followed first");
        assert_eq!(query_value("a b/ü&x=1.rs"), "a%20b/%C3%BC%26x%3D1.rs");
    }

    /// A stand-in for GitHub: `routes` by path (absent: 404), each path asked recorded. A route
    /// whose value is the string `"too large"` answers [`TooLarge`], `"unreachable"` a network
    /// error.
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
                        Some(Value::String(s)) if s == "unreachable" => Err(anyhow::anyhow!("connection reset by peer")),
                        v => Ok(v),
                    }
                }
            };
            find_revert_with(get, "jackdishman/blockula", pr, MERGE).await
        }
        fn asked(&self, path: &str) -> bool {
            self.asked.lock().unwrap().iter().any(|p| p == path)
        }
        fn asked_count(&self, f: impl Fn(&str) -> bool) -> usize {
            self.asked.lock().unwrap().iter().filter(|p| f(p)).count()
        }
    }

    /// The routes GitHub answers a list at `path` with, in pages of 100, 10 and 1: any page that
    /// holds an entry `huge` says is over the size cap.
    fn list_routes(path: &str, entries: &[Value], huge: impl Fn(&Value) -> bool) -> Vec<(String, Value)> {
        let mut out = vec![];
        for per_page in [100u32, 10, 1] {
            for (i, page) in entries.chunks(per_page as usize).enumerate() {
                let v = if page.iter().any(&huge) { Value::from("too large") } else { Value::Array(page.to_vec()) };
                out.push((paged(path, per_page, i as u32 + 1), v));
            }
        }
        out
    }

    fn commit(sha: &str, message: &str, parents: &[&str]) -> Value {
        json!({ "sha": sha, "commit": { "message": message }, "parents": parents.iter().map(|p| json!({ "sha": p })).collect::<Vec<_>>() })
    }

    const SINCE: &str = "2026-10-11T08:30:00Z";

    fn claim_42(author: &str) -> Value {
        json!({ "number": 42, "state": "closed", "title": "Add the thing", "body": "", "merged_at": SINCE, "merge_commit_sha": MERGE, "user": { "login": author }, "base": { "ref": "main", "repo": { "full_name": "jackdishman/blockula", "default_branch": "main" } } })
    }

    fn files_json(files: &[FileDiff]) -> Value {
        Value::Array(files.iter().map(|f| json!({ "filename": f.filename, "status": if f.status.is_empty() { "modified" } else { f.status.as_str() }, "sha": f.sha, "previous_filename": f.previous_filename, "patch": f.patch })).collect())
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
        // One more of the PR's commits that the PR's commit list doesn't name (past GitHub's 250 of
        // them, or a copy a rebase merge landed), whose subject looks like a revert landing: an
        // ancestor of the merge, so it mustn't take a PR fetch either.
        let landing = "Revert the first draft (#90)";
        walk.push(json!({ "sha": "e1".repeat(20), "commit": { "message": landing } }));
        walk.push(json!({ "sha": MERGE, "commit": { "message": "Merge pull request #42 from mallory/thing" } }));
        let own: Vec<Value> = decoys.iter().enumerate().map(|(i, d)| decoy(d, i + 1)).collect();
        let ancestors = json!([commit(MERGE, "Merge pull request #42 from mallory/thing", &["77aa88bb99cc00dd11ee22ff33aa44bb55cc66dd", &"e1".repeat(20)]), commit(&"e1".repeat(20), landing, &[&decoys[4]])]);
        let not_undoing = files_json(&[FileDiff { filename: "src/thing.rs".into(), status: "modified".into(), patch: Some("@@ -1 +1 @@\n-    42\n+    43".into()), ..Default::default() }]);
        let mut path_list = vec![json!({ "sha": r77, "commit": { "message": "Undo the thing" } })];
        path_list.extend(own.iter().cloned());
        let mut routes = vec![
            (format!("/repos/{repo}/pulls/42"), claim_42("mallory")),
            (format!("/repos/{repo}/pulls/42/files?per_page=100&page=1"), files_json(&thing_files())),
            (format!("/repos/{repo}/pulls/42/commits?per_page=100&page=1"), Value::Array(own)),
            (format!("/repos/{repo}/commits?sha={MERGE}&since={SINCE}&per_page=100&page=1"), ancestors),
            (format!("/repos/{repo}/commits?since={SINCE}&per_page=100&page=1"), Value::Array(walk)),
            (format!("/repos/{repo}/commits?path=src/thing.rs&since={SINCE}&per_page=100&page=1"), Value::Array(path_list)),
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
        assert!(!gh.asked(&format!("/repos/{repo}/pulls/90")), "nor the rebased copy's nomination");

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
        assert_eq!(find_revert_in(repo, 42, MERGE, "mallory", &commits, &pulls, &thing_files(), &diffs, &BTreeMap::new()).map(|r| r.sha), Some(m77.to_string()));
        // Revert PRs that change none of #42's files don't count against the comparisons (only
        // against the fetches): six of them ahead of #77 by someone else, and #77 is still found.
        let others: Vec<PullRequest> = (80..=85).map(|n| PullRequest { author: "sock".into(), ..follow_up(n) }).collect();
        let mut pulls = others.clone();
        pulls.push(parse_pr(&json!({ "number": 77, "state": "closed", "body": "Reverts #42", "merged_at": "2026-10-14T10:00:00Z", "merge_commit_sha": m77, "user": { "login": "dave" }, "base": base })));
        let readme = parse_files(&json!([{ "filename": "README.md", "patch": "@@ -1 +1 @@\n-a\n+b" }]));
        let mut diffs: BTreeMap<String, Vec<FileDiff>> = others.iter().map(|p| (p.merge_sha.clone().unwrap(), readme.clone())).collect();
        diffs.insert(m77.into(), undoing_files());
        let mut commits = commits;
        commits.insert(1, Commit { sha: "85".repeat(20), message: "Tidy (#85)".into(), ..Default::default() });
        assert_eq!(find_revert_in(repo, 42, MERGE, "mallory", &commits, &pulls, &thing_files(), &diffs, &BTreeMap::new()).map(|r| r.sha), Some(m77.to_string()));
    }

    /// Audit review-fix-poc-3: the claim PR's first page of files is 100 binary files (no patch;
    /// GitHub sorts them ahead of `src/`), its real change on page 2. Maintainers revert it with
    /// GitHub's button (PR #61, merged with a merge commit, bringing git's "This reverts commit
    /// <merge>"). The check reads the next page and compares the change. Every page of #42's files
    /// all binary: judged by whether the revert removes the files #42 added. Pages over the size
    /// cap are read in smaller pages; with no file to compare at all, nothing is attested.
    #[tokio::test]
    async fn no_shape_of_the_claim_pr_makes_it_revert_proof() {
        let repo = "jackdishman/blockula";
        let base = json!({ "ref": "main", "repo": { "full_name": repo, "default_branch": "main" } });
        let (m61, r61) = ("6161616161616161616161616161616161616161", "5151515151515151515151515151515151515151");
        let icons = |from: usize, n: usize| -> Vec<FileDiff> { (from..from + n).map(|i| FileDiff { filename: format!("assets/icon{i:03}.png"), status: "added".into(), ..Default::default() }).collect() };
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
        let removed = |files: &[FileDiff]| -> Vec<FileDiff> { files.iter().map(|f| FileDiff { status: "removed".into(), ..f.clone() }).collect() };
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
        // Every page of 100 over the size cap: read in pages of 10, and compared.
        let mut files: Vec<FileDiff> = icons(0, 150);
        files.extend(thing_files());
        let mut revert = removed(&icons(0, 150));
        revert.extend(undoing_files());
        let mut r = routes(vec![Value::from("too large"); 2], revert);
        r.extend(files.chunks(10).enumerate().map(|(i, c)| (format!("/repos/{repo}/pulls/42/files?per_page=10&page={}", i + 1), files_json(c))));
        let gh = Mock::new(r);
        assert_eq!(gh.find(42).await.unwrap().map(|r| (r.sha, r.by)), Some((r61.to_string(), Stated::Commit)));
        assert!(gh.asked(&format!("/repos/{repo}/pulls/42/files?per_page=10&page=16")) && !gh.asked(&format!("/repos/{repo}/pulls/42/files?per_page=10&page=17")), "page 2 of 100 is pages 11–20 of 10, and the list ends at 152");
        // Every file entry over the cap by itself: nothing to compare, so no revert is attested,
        // not even the commit that says so in git's words (audit review-r2-poc-1).
        let mut r = routes(vec![Value::from("too large")], undoing_files());
        r.push((format!("/repos/{repo}/pulls/42/files?per_page=10&page=1"), Value::from("too large")));
        r.extend((1..=2).map(|i| (format!("/repos/{repo}/pulls/42/files?per_page=1&page={i}"), Value::from("too large"))));
        let gh = Mock::new(r);
        assert_eq!(gh.find(42).await.unwrap(), None);
        assert!(gh.asked(&format!("/repos/{repo}/pulls/42/files?per_page=1&page=3")), "a single over the cap is skipped and the next one asked");
        // Stated only in a PR description (no git words in any commit): the PR, whose diff removes
        // the icons #42 added, is the revert.
        let quiet = json!([{ "sha": m61, "commit": { "message": "Merge pull request #61 from dave/revert-42-thing" } }, { "sha": r61, "commit": { "message": "Undo" } }, { "sha": MERGE, "commit": { "message": "Merge pull request #42 from alice/thing" } }]);
        let mut r = routes(vec![files_json(&icons(0, 2))], removed(&icons(0, 2)));
        r[1].1 = quiet.clone();
        assert_eq!(Mock::new(r).find(42).await.unwrap().map(|r| (r.sha, r.by)), Some((m61.to_string(), Stated::Pr { number: 61, by_claimant: false })));
        // And with no file to compare, not that either.
        let mut r = routes(vec![Value::from("too large")], removed(&icons(0, 2)));
        r[1].1 = quiet;
        r.push((format!("/repos/{repo}/pulls/42/files?per_page=10&page=1"), json!([])));
        assert_eq!(Mock::new(r).find(42).await.unwrap(), None);
    }

    /// Audit review-r2-poc-2: the claim PR can't hide a revert behind its own commit messages. #42
    /// ("Add the thing", by mallory) is squash-merged; its commit list also holds 21 empty commits
    /// whose messages are what a GitHub-button revert squash-merged as #70..#90 would land as
    /// (`Revert "Add the thing" (#M)` + `Reverts jackdishman/blockula#42`). Maintainer dave's
    /// revert #77 lands with exactly that message. Commits are left out by sha (the PR's list, the
    /// merge's ancestors), never by message, so #77 is found; when #42 was rebase-merged, its
    /// copies (ancestors of the merge, with the same messages) are left out and #77 still isn't.
    #[tokio::test]
    async fn the_claim_prs_own_messages_cant_hide_a_revert() {
        let repo = "jackdishman/blockula";
        let base = json!({ "ref": "main", "repo": { "full_name": repo, "default_branch": "main" } });
        let landing = |m: u64| format!("Revert \"Add the thing\" (#{m})\n\nReverts {repo}#42");
        let m77 = "7777777777777777777777777777777777777777";
        let before = "77aa88bb99cc00dd11ee22ff33aa44bb55cc66dd";
        let mut own = vec![commit(&"a0".repeat(20), "Add the thing", &[before])];
        own.extend((70..=90u64).map(|m| commit(&format!("{m:040x}"), &landing(m), &[&"a0".repeat(20)])));
        let pr77 = |author: &str| json!({ "number": 77, "state": "closed", "title": "Revert \"Add the thing\"", "body": format!("Reverts {repo}#42"), "merged_at": "2026-10-14T10:00:00Z", "merge_commit_sha": m77, "user": { "login": author }, "base": base });
        let routes = |merge_ancestors: Vec<Value>, author: &str| {
            let mut walk = vec![commit(m77, &landing(77), &[MERGE])];
            walk.extend(merge_ancestors.iter().cloned());
            vec![
                (format!("/repos/{repo}/pulls/42"), claim_42("mallory")),
                (format!("/repos/{repo}/pulls/42/files?per_page=100&page=1"), files_json(&thing_files())),
                (format!("/repos/{repo}/pulls/42/commits?per_page=100&page=1"), Value::Array(own.clone())),
                (format!("/repos/{repo}/commits?sha={MERGE}&since={SINCE}&per_page=100&page=1"), Value::Array(merge_ancestors.clone())),
                (format!("/repos/{repo}/commits?since={SINCE}&per_page=100&page=1"), Value::Array(walk.clone())),
                (format!("/repos/{repo}/commits?path=src/thing.rs&since={SINCE}&per_page=100&page=1"), Value::Array(walk)),
                (format!("/repos/{repo}/commits/{m77}/pulls"), json!([pr77(author)])),
                (format!("/repos/{repo}/pulls/77"), pr77(author)),
                (format!("/repos/{repo}/pulls/77/files?per_page=100&page=1"), files_json(&undoing_files())),
            ]
        };
        // Squash-merged: one commit, the merge itself.
        let squash = vec![commit(MERGE, "Add the thing (#42)", &[before])];
        let r = Mock::new(routes(squash.clone(), "dave")).find(42).await.unwrap();
        assert_eq!(r.map(|r| (r.sha, r.by)), Some((m77.to_string(), Stated::Pr { number: 77, by_claimant: false })));
        // Even a revert the claimant opened itself (its message pre-planted too) is found, last.
        let r = Mock::new(routes(squash, "mallory")).find(42).await.unwrap();
        assert_eq!(r.map(|r| r.by), Some(Stated::Pr { number: 77, by_claimant: true }));
        // Rebase-merged: the merge sha is the last of the copies, all of them with #42's messages.
        let mut copies = vec![];
        let mut parent = before.to_string();
        for (i, c) in own.iter().enumerate() {
            let sha = if i == own.len() - 1 { MERGE.to_string() } else { format!("{:02x}", 0xc0 + i).repeat(20) };
            copies.push(commit(&sha, c["commit"]["message"].as_str().unwrap(), &[&parent]));
            parent = sha;
        }
        copies.reverse();
        let gh = Mock::new(routes(copies, "dave"));
        assert_eq!(gh.find(42).await.unwrap().map(|r| r.sha), Some(m77.to_string()));
        assert_eq!(gh.asked_count(|p| p.starts_with(&format!("/repos/{repo}/pulls/")) && !p.contains("/pulls/42") && !p.contains("/pulls/77")), 0, "no copy nominated a PR");
    }

    /// Audit F55 (round 3): GitHub lists at most 250 of a PR's commits, so a claim PR of 300
    /// (merged with a merge commit) had 50 the check didn't leave out: future-dated decoys
    /// touching its file, subjects `Revert "x" (#N)`, which took every PR fetch and path slot
    /// ahead of the real revert (#77, squash-merged, found only by its PR). They are ancestors of
    /// the merge, so `commits?sha=<merge>` names them all.
    #[tokio::test]
    async fn the_claim_prs_commits_cant_flood_the_walk_however_many() {
        let repo = "jackdishman/blockula";
        let base = json!({ "ref": "main", "repo": { "full_name": repo, "default_branch": "main" } });
        let m77 = "7777777777777777777777777777777777777777";
        let sha = |i: usize| format!("{i:040x}");
        // #42's commits, oldest first: 250 fillers, then 50 decoys.
        let mine: Vec<Value> = (1..=300).map(|i| commit(&sha(i), &if i > 250 { format!("Revert \"step {i}\" (#{})", 100 + i) } else { format!("step {i}") }, &[&sha(i - 1)])).collect();
        let decoys: Vec<Value> = mine[250..].iter().rev().cloned().collect();
        let merge = commit(MERGE, "Merge pull request #42 from mallory/thing", &["77aa88bb99cc00dd11ee22ff33aa44bb55cc66dd", &sha(300)]);
        let squash77 = commit(m77, "Undo the thing (#77)\n\nIt broke the build.", &[MERGE]);
        let pr77 = json!({ "number": 77, "state": "closed", "title": "Undo the thing", "body": format!("Reverts {repo}#42"), "merged_at": "2026-10-14T10:00:00Z", "merge_commit_sha": m77, "user": { "login": "dave" }, "base": base });
        let mut walk = vec![squash77.clone(), merge.clone()];
        walk.extend(decoys.iter().cloned());
        let mut ancestors = vec![merge];
        ancestors.extend(mine.iter().rev().cloned());
        let mut routes = vec![
            (format!("/repos/{repo}/pulls/42"), claim_42("mallory")),
            (format!("/repos/{repo}/pulls/42/files?per_page=100&page=1"), files_json(&thing_files())),
            (format!("/repos/{repo}/commits/{m77}/pulls"), json!([pr77.clone()])),
            (format!("/repos/{repo}/pulls/77"), pr77),
            (format!("/repos/{repo}/pulls/77/files?per_page=100&page=1"), files_json(&undoing_files())),
        ];
        let mut path_list = vec![squash77];
        path_list.extend(decoys.iter().cloned());
        routes.push((format!("/repos/{repo}/commits?path=src/thing.rs&since={SINCE}&per_page=100&page=1"), Value::Array(path_list)));
        routes.extend(mine[..250].chunks(100).enumerate().map(|(i, c)| (format!("/repos/{repo}/pulls/42/commits?per_page=100&page={}", i + 1), Value::Array(c.to_vec()))));
        routes.extend(list_routes(&format!("/repos/{repo}/commits?since={SINCE}"), &walk, |_| false).into_iter().filter(|(p, _)| p.contains("per_page=100")));
        let without = Mock::new(routes.clone());
        routes.extend(list_routes(&format!("/repos/{repo}/commits?sha={MERGE}&since={SINCE}"), &ancestors, |_| false).into_iter().filter(|(p, _)| p.contains("per_page=100")));
        let gh = Mock::new(routes);
        let r = gh.find(42).await.unwrap();
        assert_eq!(r.map(|r| (r.sha, r.by)), Some((m77.to_string(), Stated::Pr { number: 77, by_claimant: false })));
        assert_eq!(gh.asked_count(|p| p.contains("/pulls/") && (351..=400).any(|n| p.ends_with(&format!("/pulls/{n}")))), 0, "no decoy nominated a PR");
        assert_eq!(gh.asked_count(|p| p.ends_with("/pulls") && !p.contains(m77)), 0, "no decoy took a path slot");
        // Without the merge's ancestors, the 50 decoys take all of it: the attack this closes.
        assert_eq!(without.find(42).await.unwrap(), None);
    }

    /// The same with more of #42's commits than GitHub lists (250 for the PR, 1,000 ancestors
    /// read): 16 more, touching its file and dated so they sort after the 1,000, show up in the
    /// file's history only. The walk reached the merge, so it holds every later commit there is,
    /// and only those are followed: #77 is found and no path slot goes to the 16.
    #[tokio::test]
    async fn a_claim_pr_past_what_github_lists_cant_fill_the_path_slots() {
        let repo = "jackdishman/blockula";
        let base = json!({ "ref": "main", "repo": { "full_name": repo, "default_branch": "main" } });
        let m77 = "7777777777777777777777777777777777777777";
        let sha = |i: usize| format!("{i:040x}");
        let mine = |i: usize| commit(&sha(i), &format!("step {i}"), &[&sha(i - 1)]);
        let merge = commit(MERGE, "Merge pull request #42 from mallory/thing", &["77aa88bb99cc00dd11ee22ff33aa44bb55cc66dd", &sha(2_000)]);
        let squash77 = commit(m77, "Undo the thing (#77)", &[MERGE]);
        let pr77 = json!({ "number": 77, "state": "closed", "title": "Undo the thing", "body": format!("Reverts {repo}#42"), "merged_at": "2026-10-14T10:00:00Z", "merge_commit_sha": m77, "user": { "login": "dave" }, "base": base });
        let mut ancestors = vec![merge.clone()];
        ancestors.extend((1_001..=1_999).rev().map(mine));
        let mut walk = vec![squash77.clone(), merge];
        walk.extend((1_002..=1_999).rev().map(mine));
        let mut path_list = vec![squash77];
        path_list.extend((1..=16).map(mine));
        let mut routes = vec![
            (format!("/repos/{repo}/pulls/42"), claim_42("mallory")),
            (format!("/repos/{repo}/pulls/42/files?per_page=100&page=1"), files_json(&thing_files())),
            (format!("/repos/{repo}/pulls/42/commits?per_page=100&page=1"), Value::Array((2_001..=2_100).map(mine).collect())),
            (format!("/repos/{repo}/commits?path=src/thing.rs&since={SINCE}&per_page=100&page=1"), Value::Array(path_list)),
            (format!("/repos/{repo}/commits/{m77}/pulls"), json!([pr77.clone()])),
            (format!("/repos/{repo}/pulls/77"), pr77),
            (format!("/repos/{repo}/pulls/77/files?per_page=100&page=1"), files_json(&undoing_files())),
        ];
        for (path, list) in [(format!("/repos/{repo}/commits?sha={MERGE}&since={SINCE}"), &ancestors), (format!("/repos/{repo}/commits?since={SINCE}"), &walk)] {
            assert_eq!(list.len(), 1_000);
            routes.extend(list_routes(&path, list, |_| false).into_iter().filter(|(p, _)| p.contains("per_page=100")));
        }
        let gh = Mock::new(routes);
        assert_eq!(gh.find(42).await.unwrap().map(|r| r.sha), Some(m77.to_string()));
        assert_eq!(gh.asked_count(|p| p.ends_with("/pulls") && !p.contains(m77)), 0, "none of the 16 took a path slot");
    }

    /// Audit review-fix-poc-3 (round 3): a claimant controls how big some of the branch's commits
    /// are (a squash merge's message holds every commit message of the PR), so a page of the
    /// commit walk or of a path's history can be over the 8 MiB cap. That used to be an error on
    /// every retry for the whole window; now the page is read as pages of 10, then of 1, and only
    /// the commit over the cap by itself (here #42's own squash merge) is skipped. Only GitHub
    /// being unreachable is an error.
    #[tokio::test]
    async fn oversized_pages_are_read_in_smaller_ones() {
        let repo = "jackdishman/blockula";
        let r61 = "5151515151515151515151515151515151515151";
        let mut walk = vec![commit(r61, &format!("Revert \"Add the thing\"\n\nThis reverts commit {MERGE}."), &["0f".repeat(20).as_str()])];
        walk.extend((1..=14u8).map(|i| commit(&format!("{:02x}", 0x10 + i).repeat(20), &format!("Other change {i}"), &[MERGE])));
        walk.push(commit(MERGE, "Add the thing (#42)\n\n* a squash message of many megabytes", &["77aa88bb99cc00dd11ee22ff33aa44bb55cc66dd"]));
        let huge = |c: &Value| c["sha"] == MERGE;
        let routes = |extra: Vec<(String, Value)>| {
            let mut r = vec![
                (format!("/repos/{repo}/pulls/42"), claim_42("alice")),
                (format!("/repos/{repo}/pulls/42/files?per_page=100&page=1"), files_json(&thing_files())),
                (format!("/repos/{repo}/commits/{r61}"), json!({ "sha": r61, "files": files_json(&undoing_files()) })),
            ];
            r.extend(list_routes(&format!("/repos/{repo}/commits?since={SINCE}"), &walk, huge));
            r.extend(list_routes(&format!("/repos/{repo}/commits?sha={MERGE}&since={SINCE}"), &walk[walk.len() - 1..], huge));
            r.extend(list_routes(&format!("/repos/{repo}/commits?path=src/thing.rs&since={SINCE}"), &[walk[0].clone(), walk[walk.len() - 1].clone()], huge));
            r.extend(extra);
            r
        };
        let gh = Mock::new(routes(vec![]));
        assert_eq!(gh.find(42).await.unwrap().map(|r| (r.sha, r.by)), Some((r61.to_string(), Stated::Commit)));
        assert!(gh.asked(&format!("/repos/{repo}/commits?since={SINCE}&per_page=1&page=15")) && !gh.asked(&format!("/repos/{repo}/commits?since={SINCE}&per_page=1&page=5")), "only the page of 10 over the cap is read one by one");
        // A network error is still an error (try again later), wherever it happens.
        for path in [format!("/repos/{repo}/commits?since={SINCE}&per_page=10&page=2"), format!("/repos/{repo}/commits?path=src/thing.rs&since={SINCE}&per_page=1&page=1")] {
            let gh = Mock::new(routes(vec![(path.clone(), Value::from("unreachable"))]));
            assert!(gh.find(42).await.unwrap_err().to_string().contains("connection reset"), "{path}");
        }
    }

    /// Audit review-r2-poc-1: a binary-only claim PR is reverted only when its files get their
    /// pre-PR state back. Alice's #42 adds two PNGs (no patch). Mallory's #90 "Compress icons"
    /// changes both; merged from a `…/revert-…` branch with its description edited to "Reverts
    /// #42", or carrying a commit that says "This reverts commit <merge>", it used to count as the
    /// revert (more than half the files touched) and reject Alice's claim. A revert removes them.
    /// A PR that changes a binary file is reverted when the file's blob before the PR (read at the
    /// commit it merged onto) comes back.
    #[tokio::test]
    async fn touching_a_binary_claims_files_isnt_reverting_them() {
        let repo = "jackdishman/blockula";
        let base = json!({ "ref": "main", "repo": { "full_name": repo, "default_branch": "main" } });
        let (m90, c90) = ("9090909090909090909090909090909090909090", "9191919191919191919191919191919191919191");
        let before = "77aa88bb99cc00dd11ee22ff33aa44bb55cc66dd";
        let png = |name: &str, status: &str, sha: &str| FileDiff { filename: format!("assets/{name}"), status: status.into(), sha: Some(sha.repeat(20)), ..Default::default() };
        let pr90 = json!({ "number": 90, "state": "closed", "title": "Compress icons", "body": format!("Reverts {repo}#42"), "merged_at": "2026-10-14T10:00:00Z", "merge_commit_sha": m90, "user": { "login": "mallory" }, "base": base });
        let routes = |claim: Vec<FileDiff>, change: Vec<FileDiff>, extra: Vec<(String, Value)>| {
            let walk = json!([
                commit(m90, "Merge pull request #90 from mallory/revert-icon-padding\n\nCompress icons", &[MERGE, c90]),
                commit(c90, &format!("Compress icons\n\nThis reverts commit {MERGE}."), &[MERGE]),
                commit(MERGE, "Merge pull request #42 from alice/icons", &[before, &"a1".repeat(20)])
            ]);
            let mut r = vec![
                (format!("/repos/{repo}/pulls/42"), claim_42("alice")),
                (format!("/repos/{repo}/pulls/42/files?per_page=100&page=1"), files_json(&claim)),
                (format!("/repos/{repo}/commits?sha={MERGE}&since={SINCE}&per_page=100&page=1"), json!([walk[2].clone()])),
                (format!("/repos/{repo}/commits?since={SINCE}&per_page=100&page=1"), walk),
                (format!("/repos/{repo}/commits/{c90}"), json!({ "sha": c90, "files": files_json(&change) })),
                (format!("/repos/{repo}/pulls/90"), pr90.clone()),
                (format!("/repos/{repo}/pulls/90/files?per_page=100&page=1"), files_json(&change)),
            ];
            r.extend(extra);
            r
        };
        let added = vec![png("a.png", "added", "aa"), png("b.png", "added", "bb")];
        // Changed, not removed: no revert, by the PR or by the commit.
        let gh = Mock::new(routes(added.clone(), vec![png("a.png", "modified", "a2"), png("b.png", "modified", "b2")], vec![]));
        assert_eq!(gh.find(42).await.unwrap(), None);
        // Removed: the revert (the commit stating it in git's words, oldest on the branch).
        let gh = Mock::new(routes(added, vec![png("a.png", "removed", "aa"), png("b.png", "removed", "bb")], vec![]));
        assert_eq!(gh.find(42).await.unwrap().map(|r| (r.sha, r.by)), Some((c90.to_string(), Stated::Commit)));
        // #42 changed an existing icon: only its pre-PR blob coming back is a revert.
        let changed = vec![png("logo.png", "modified", "1b")];
        let listing = json!([{ "type": "file", "name": "logo.png", "path": "assets/logo.png", "sha": "1a".repeat(20) }, { "type": "file", "name": "x.png", "path": "assets/x.png", "sha": "ee".repeat(20) }]);
        let at_base = (format!("/repos/{repo}/contents/assets?ref={before}"), listing);
        let gh = Mock::new(routes(changed.clone(), vec![png("logo.png", "modified", "1a")], vec![at_base.clone()]));
        assert_eq!(gh.find(42).await.unwrap().map(|r| r.sha), Some(c90.to_string()));
        assert!(Mock::new(routes(changed.clone(), vec![png("logo.png", "modified", "1c")], vec![at_base.clone()])).find(42).await.unwrap().is_none(), "another version isn't the old one");
        assert!(Mock::new(routes(changed.clone(), vec![png("logo.png", "modified", "1a")], vec![])).find(42).await.unwrap().is_none(), "no base, no revert");
        // The pure rule, renames included.
        let base_blobs = BTreeMap::from([("assets/old.png".to_string(), "0a".repeat(20))]);
        let renamed = [FileDiff { previous_filename: Some("assets/old.png".into()), ..png("new.png", "renamed", "0b") }];
        let back = [FileDiff { previous_filename: Some("assets/new.png".into()), ..png("old.png", "renamed", "0a") }];
        assert_eq!(undoes(&renamed, &back, &base_blobs), Some(true));
        assert_eq!(undoes(&renamed, &[png("old.png", "added", "0a")], &base_blobs), Some(false), "the new name must go too");
        assert_eq!(undoes(&renamed, &[png("old.png", "added", "0a"), png("new.png", "removed", "0b")], &base_blobs), Some(true));
        assert_eq!(undoes(&renamed, &back, &BTreeMap::new()), None, "nothing known to compare");
        // A removed file comes back as the blob it was.
        let gone = [png("c.png", "removed", "cc")];
        assert_eq!((undoes(&gone, &[png("c.png", "added", "cc")], &BTreeMap::new()), undoes(&gone, &[png("c.png", "added", "cd")], &BTreeMap::new())), (Some(true), Some(false)));
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
