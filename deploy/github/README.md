# Repository settings (GitHub)

Some of the project's supply-chain controls live in GitHub's settings, not in the tree: which checks must pass before a PR merges, who may push release tags, and where fuzz crashes go. Settings change without a commit or a review, so the intended ones are written down here and applied by the maintainer. Nothing in CI can apply them: no workflow token has admin rights.

| File | What it is |
|---|---|
| `ruleset-main.json` | The `main` ruleset, "main: reviewed PRs with green CI": a PR with one code-owner approval (stale approvals dismissed, the last push approved by someone else, threads resolved), squash merges and linear history, and **every CI job that runs on pull requests** as a required status check, against an up-to-date `main` |
| `ruleset-tags.json` | "release tags": only admins create, move or delete `v*` tags, the tags `release.yml` builds and attests from |
| `apply-rulesets.sh` | Shows how GitHub's rulesets differ from these files; `--apply` makes them match; `--check` exits 1 on any difference (the daily drift job) |

## Applying the rulesets

As a repository admin, with `gh auth login` done and `jq` installed:

```bash
deploy/github/apply-rulesets.sh            # dry run: the live ruleset against the file, as a diff
deploy/github/apply-rulesets.sh --apply    # write them (PUT an existing ruleset, POST a new one)
```

Read the dry run before you apply. `ruleset-tags.json` was written from the audit's description of the live "release tags" ruleset (creation, update and deletion of `refs/tags/v*` limited to admins), not from its JSON, so the first dry run shows how they differ; if the live one is stricter, copy it into the file instead. The live "main: permanent history" ruleset has no file here, and the script leaves rulesets without a file alone.

Apply `ruleset-main.json` when the branch that brings it lands on `main`. Until then, the live ruleset requires only "Node (Rust tests + local multi-validator testnet)" and "Packages + wallet build". Both names are unchanged, so merging doesn't block, but the other checks aren't enforced yet.

## Drift

`.github/workflows/github-settings.yml` runs `apply-rulesets.sh --check` every day (and on demand) with the workflow's read-only token. It fails when a live ruleset differs from its file here or is missing, so a setting changed in the web UI shows up as a red job instead of going unnoticed. GitHub only shows a ruleset's bypass actors to tokens that can write it, so the job doesn't compare them and says so; run the dry run yourself to see those. Until the rulesets are applied the job fails, as a reminder. It doesn't run on pull requests, so it isn't a required check.

## Keeping the required checks in step (audit supply-chain-B-3)

A required status check is a job's `name:`. Rename a job, and the ruleset waits for a check that's never reported: every PR blocks, and the maintainer's bypass becomes the merge path. Add a job without requiring it, and a PR can merge with it red. So:

- `.github/workflows/ci.yml`'s header lists the required checks (`#   required: <job name>`). A job that should run on PRs without blocking them is listed as `#   optional: <job name>`.
- `scripts/check-required-checks.mjs` (CI, `ops` job) fails unless that list, `ruleset-main.json` and the jobs that run on every pull request agree. It also fails on a required job that sits behind a `paths` or `branches` filter (a PR outside the filter would wait forever), and on a required check that isn't tied to the GitHub Actions app (`integration_id` 15368), since any other app or token that can post a commit status could satisfy it.
- A skipped check counts as passing, so the script refuses a required job that could skip itself or pass without its work: a job-level `if:` other than the ones in its `SAFE_JOB_IFS` (today only `github.event_name != 'schedule'`), a `needs:` on a job that isn't required (if that job fails or is skipped, so is this one), job-level `continue-on-error`, and a step-level `if:` or `continue-on-error` that isn't one of the exact exceptions in its `SAFE_STEPS` (the Playwright trace upload on failure, the non-blocking full `npm audit`). A new exception is a reviewed edit to the script. Workflows must be block-style YAML; a job it can't read is an error.
- To rename or add a required job: change the workflow, the header list and `ruleset-main.json` in one PR, and apply the ruleset when it merges.

## The admin bypass

The ruleset lets the admin role bypass it, in pull-request mode only: an admin can merge a PR that isn't approved or green, never push to `main` directly. With one maintainer this is unavoidable, because nobody can approve their own PR. It also means the review rule doesn't bind that maintainer today. When a second maintainer joins:

1. Make the bypass an emergency path: remove it, or limit it to a dedicated role that every use of it is announced from.
2. Require a second person's code-owner review for `.github/**`, `scripts/**`, `deploy/**`, `deny.toml`, `rust-toolchain.toml` and the lockfiles (`.github/CODEOWNERS`).

## Fuzz crashes: `FUZZ_CRASH_AGE_RECIPIENTS` (audit supply-chain-B-1)

The nightly fuzz job runs in public, where anyone can read workflow logs and artifacts. A crash leaves the runner only encrypted with [age](https://age-encryption.org) to the maintainers' keys in the repository variable `FUZZ_CRASH_AGE_RECIPIENTS`; with no variable set, nothing is kept ([node/fuzz/README.md](../../node/fuzz/README.md)). To set it up, each maintainer makes an age key on their own machine and keeps the identity file private:

```bash
age-keygen -o ~/.config/blockula/fuzz-age.key      # prints "Public key: age1…"
```

The admin then sets the variable to all the public keys, separated by spaces:

```bash
gh variable set FUZZ_CRASH_AGE_RECIPIENTS --repo jackdishman/blockula --body "age1… age1…"
```

These are public keys, so the variable isn't secret. Anyone who can change it (repository admins) can redirect future crash reports, though. Check it (`gh variable get FUZZ_CRASH_AGE_RECIPIENTS --repo jackdishman/blockula`) whenever maintainers change.

## Other settings to keep

These aren't rulesets, so the script doesn't apply them. Check them by hand:

- **Releases:** immutable releases on (Settings → General), so nobody can swap an asset once a release is published (docs/OPERATIONS.md "Releases").
- **Actions:** workflows from fork PRs need approval for every outside contributor, and the default `GITHUB_TOKEN` permission is read-only (every workflow declares what it needs).
- **Security:** private vulnerability reporting on (SECURITY.md links to it). Fuzz crashes are handled through it too.
