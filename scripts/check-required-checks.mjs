#!/usr/bin/env node
// The main-branch ruleset requires CI jobs by their `name:` (audit supply-chain-B-3). A renamed job
// is a required check that's never reported, so every PR waits on it and the maintainer's bypass
// becomes the merge path; a new job that isn't required can go red and still merge. CI (`ops`)
// fails unless these agree:
//   - deploy/github/ruleset-main.json's required status checks (the intended ruleset; the
//     maintainer applies it with deploy/github/apply-rulesets.sh, docs/OPERATIONS.md
//     "Repository settings"), each tied to the GitHub Actions app (integration_id 15368), so a
//     commit status posted by anything else can't stand in for it;
//   - the jobs that run on pull requests in .github/workflows/*.yml: each required context is
//     exactly one such job's name, and every such job is required (or listed as optional);
//   - the list in .github/workflows/ci.yml's header ("#   required: <name>", "#   optional: <name>").
// A workflow that runs on pull_request with a paths or branches filter doesn't report on every
// PR, so its jobs can't be required: that's an error too.
// GitHub counts a skipped check as passing, so a required job must not be able to skip itself or
// pass without its work running (round 3). It's refused when it has:
//   - a job-level `if:` other than the conditions in SAFE_JOB_IFS (which keep it on every PR);
//   - `needs:` on a job that isn't required (if that job fails or is skipped, this one is skipped);
//   - `continue-on-error` on the job, or a step-level `if:` or `continue-on-error` that isn't in
//     SAFE_STEPS (exact job, step name and value: a reviewed exception, e.g. an upload on failure).
//   node scripts/check-required-checks.mjs [--ruleset F] [--workflows DIR] [--list F]
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

const GITHUB_ACTIONS = 15368;
// Job-level conditions that never skip a job on a pull request.
const SAFE_JOB_IFS = new Set(["github.event_name != 'schedule'"]);
// Step-level conditions in required jobs that can't make the job pass without its checks: each
// is "<job name> / <step name> / <key>: <value>", reviewed.
const SAFE_STEPS = new Set([
  // Uploads the Playwright traces when the tests already failed.
  "Wallet e2e (Playwright, Chromium) / Traces of failed tests / if: failure()",
  // Reports dev-tooling advisories; the runtime audit in the step before blocks.
  "Dependencies (cargo-deny, npm audit) / npm audit (all, non-blocking) / continue-on-error: true",
]);
const cond = (v) => v.replace(/^\$\{\{\s*([\s\S]*?)\s*\}\}$/, "$1").trim();
const args = process.argv.slice(2);
const opt = (k, d) => {
  const i = args.indexOf(k);
  return i >= 0 ? args[i + 1] : d;
};
const root = new URL("..", import.meta.url).pathname;
const rulesetFile = opt("--ruleset", join(root, "deploy/github/ruleset-main.json"));
const workflowDir = opt("--workflows", join(root, ".github/workflows"));
const listFile = opt("--list", join(workflowDir, "ci.yml"));

const errors = [];
const bad = (m) => errors.push(m);

// ---- the ruleset ----
const ruleset = JSON.parse(readFileSync(rulesetFile, "utf8"));
const statusRule = (ruleset.rules ?? []).find((r) => r.type === "required_status_checks");
const required = new Map();
if (!statusRule) bad(`${rulesetFile} has no required_status_checks rule`);
else {
  if (statusRule.parameters?.strict_required_status_checks_policy !== true) bad(`${rulesetFile}: strict_required_status_checks_policy must be true (a PR is tested against the current main)`);
  for (const c of statusRule.parameters?.required_status_checks ?? []) {
    if (required.has(c.context)) bad(`${rulesetFile} requires "${c.context}" twice`);
    required.set(c.context, c);
    if (c.integration_id !== GITHUB_ACTIONS) bad(`${rulesetFile}: "${c.context}" must have integration_id ${GITHUB_ACTIONS} (GitHub Actions), or any app or token that can post a commit status satisfies it`);
  }
  if (required.size === 0) bad(`${rulesetFile} requires no status checks`);
}

// ---- the workflows (the subset of YAML these files use: block style, two-space indents) ----
const unquote = (v) => {
  v = v.trim();
  const q = /^"((?:[^"\\]|\\.)*)"|^'((?:[^']|'')*)'/.exec(v);
  if (q) return q[1] !== undefined ? q[1].replace(/\\(.)/g, "$1") : q[2].replace(/''/g, "'");
  return v.replace(/\s+#.*$/, "").trim();
};
const indent = (l) => l.length - l.trimStart().length;
const meaningful = (l) => l.trim() !== "" && !l.trim().startsWith("#");

function parseWorkflow(file) {
  const lines = readFileSync(file, "utf8").split("\n");
  const block = (key) => {
    const i = lines.findIndex((l) => new RegExp(`^${key}:`).test(l));
    if (i < 0) return { head: null, body: [] };
    const head = lines[i].slice(key.length + 1);
    const body = [];
    for (let j = i + 1; j < lines.length && !(meaningful(lines[j]) && indent(lines[j]) === 0); j++) body.push(lines[j]);
    return { head: head.replace(/\s+#.*$/, "").trim(), body };
  };
  // on: does it run on every pull request?
  const on = block("on");
  let pr = false;
  let prFilter = "";
  if (on.head) {
    // on: pull_request | on: [push, pull_request] | on: { pull_request: … }
    if (/\bpull_request\b(?!_target)/.test(on.head)) {
      pr = true;
      if (/pull_request\s*:\s*\{[^}]*\b(paths|paths-ignore|branches|branches-ignore)\b/.test(on.head)) prFilter = "a filter";
    }
  } else {
    for (let j = 0; j < on.body.length; j++) {
      const l = on.body[j];
      if (!meaningful(l) || indent(l) !== 2) continue;
      const m = /^ {2}pull_request\s*:(.*)$/.exec(l);
      if (!m) continue;
      pr = true;
      const nested = [m[1]];
      for (let k = j + 1; k < on.body.length && !(meaningful(on.body[k]) && indent(on.body[k]) <= 2); k++) if (meaningful(on.body[k])) nested.push(on.body[k]);
      const f = /\b(paths|paths-ignore|branches|branches-ignore)\s*:/.exec(nested.join("\n"));
      if (f) prFilter = f[1];
    }
  }
  // jobs: id → { name, if, needs, continueOnError, steps: [{ name, if, continueOnError }] }
  const jobs = [];
  const flow = [];
  const jb = block("jobs").body;
  for (let j = 0; j < jb.length; j++) {
    if (/^ {2}[A-Za-z0-9_-]+:\s*[^\s#]/.test(jb[j])) flow.push(jb[j].trim());
    const m = /^ {2}([A-Za-z0-9_-]+):\s*(#.*)?$/.exec(jb[j]);
    if (!m) continue;
    const job = { id: m[1], name: m[1], if: "", needs: [], continueOnError: "", steps: [] };
    let inSteps = false;
    let step = null;
    for (let k = j + 1; k < jb.length && !(meaningful(jb[k]) && indent(jb[k]) <= 2); k++) {
      const l = jb[k];
      if (!meaningful(l)) continue;
      const kv = /^ {4}([A-Za-z-]+):\s*(.*)$/.exec(l);
      if (kv) {
        inSteps = kv[1] === "steps";
        step = null;
        if (kv[1] === "name" || kv[1] === "if") job[kv[1]] = unquote(kv[2]);
        if (kv[1] === "continue-on-error") job.continueOnError = unquote(kv[2]) || "(set)";
        if (kv[1] === "needs") {
          const v = unquote(kv[2]);
          if (v.startsWith("[")) job.needs.push(...v.replace(/^\[|\]$/g, "").split(",").map((x) => unquote(x)).filter(Boolean));
          else if (v) job.needs.push(v);
          else for (let n = k + 1; n < jb.length && /^ {6}- /.test(jb[n]); n++) job.needs.push(unquote(jb[n].replace(/^ {6}- /, "")));
        }
        continue;
      }
      if (!inSteps) continue;
      const item = /^ {6}- ([A-Za-z-]+):\s*(.*)$/.exec(l);
      if (item || /^ {6}-\s*$/.test(l)) {
        step = { name: "", if: "", continueOnError: "" };
        job.steps.push(step);
      }
      const sk = item ?? /^ {8}([A-Za-z-]+):\s*(.*)$/.exec(l);
      if (step && sk) {
        if (sk[1] === "name") step.name = unquote(sk[2]);
        if (sk[1] === "if") step.if = cond(unquote(sk[2])) || "(set)";
        if (sk[1] === "continue-on-error") step.continueOnError = unquote(sk[2]) || "(set)";
      }
    }
    jobs.push(job);
  }
  return { pr, prFilter, jobs, flow };
}

const prJobs = new Map(); // name → "file: job id"
const prWorkflows = []; // [file, jobs] of the workflows that run on pull requests
const filteredJobs = new Map(); // jobs that run only on some pull requests (a paths/branches filter)
const files = readdirSync(workflowDir).filter((f) => /\.ya?ml$/.test(f)).sort();
if (files.length === 0) bad(`no workflows in ${workflowDir}`);
const allJobs = new Map(); // name → ["file: job id", …] across every workflow, whatever triggers it
for (const f of files) {
  const wf = parseWorkflow(join(workflowDir, f));
  for (const job of wf.jobs) allJobs.set(job.name, [...(allJobs.get(job.name) ?? []), `${f}: ${job.id}`]);
  if (!wf.pr) continue;
  prWorkflows.push([f, wf.jobs]);
  if (wf.jobs.length === 0) bad(`${f}: runs on pull_request but no jobs were found (this checker reads block-style YAML only)`);
  for (const l of wf.flow) bad(`${f}: job "${l}" is written in flow style; this checker reads block-style YAML only, so it can't tell what the job does`);
  for (const job of wf.jobs) {
    // A job whose `if:` names pull_request is taken not to run on PRs (e.g. the nightly fuzzers);
    // a skipped job would report success, which is no check at all.
    if (/pull_request/.test(job.if)) {
      if (required.has(job.name)) bad(`"${job.name}" (${f}: ${job.id}) is required, but its if: (${job.if}) skips it on pull requests, and a skipped check counts as passing`);
      continue;
    }
    if (wf.prFilter) {
      if (required.has(job.name)) bad(`"${job.name}" (${f}: ${job.id}) is required, but ${f} runs on pull_request only with ${wf.prFilter}: a PR outside it would wait for the check forever`);
      else filteredJobs.set(job.name, `${f}: ${job.id}, ${wf.prFilter}`);
      continue;
    }
    if (job.name.includes("${{")) {
      bad(`${f}: job ${job.id} runs on pull requests with a computed name (${job.name}): a required check needs a fixed one`);
      continue;
    }
    if (prJobs.has(job.name)) bad(`two jobs that run on pull requests are both named "${job.name}" (${prJobs.get(job.name)}, ${f}: ${job.id}): a check name must say which job it is`);
    prJobs.set(job.name, `${f}: ${job.id}`);
  }
}

// A check run from any other job with a required name (a push-triggered copy that always passes,
// say) could stand in for the real one on the PR's head commit.
for (const ctx of required.keys()) {
  const where = allJobs.get(ctx) ?? [];
  if (where.length > 1) bad(`the required check "${ctx}" is the name of ${where.length} jobs (${where.join(", ")}): any of them reporting it would satisfy the ruleset. Keep required names unique across all workflows`);
}

// ---- required jobs can't skip themselves or pass without their work ----
for (const [f, jobs] of prWorkflows) {
  const byId = new Map(jobs.map((j) => [j.id, j]));
  for (const job of jobs) {
    if (!required.has(job.name)) continue;
    const at = `"${job.name}" (${f}: ${job.id})`;
    const c = cond(job.if);
    if (c && !/pull_request/.test(c) && !SAFE_JOB_IFS.has(c)) bad(`${at} is required, but its if: (${c}) can skip it, and a skipped check counts as passing. Drop the condition, or if it can't skip a pull request add it to SAFE_JOB_IFS in this script (now: ${[...SAFE_JOB_IFS].join("; ")})`);
    for (const n of job.needs) {
      const dep = byId.get(n);
      if (!dep) bad(`${at} needs "${n}", which isn't a job in ${f}`);
      else if (!required.has(dep.name)) bad(`${at} is required, but it needs "${dep.name}" (${f}: ${n}), which isn't: if that job fails or is skipped, ${at} is skipped, and a skipped check counts as passing. Require it too, or drop the needs`);
    }
    if (job.continueOnError) bad(`${at} is required, but it has continue-on-error: ${job.continueOnError}, so it can fail and still let the PR through`);
    for (const st of job.steps) {
      for (const [key, v] of [["if", st.if], ["continue-on-error", st.continueOnError]]) {
        if (!v) continue;
        const entry = `${job.name} / ${st.name || "(unnamed step)"} / ${key}: ${v}`;
        if (!SAFE_STEPS.has(entry)) bad(`${at} is required, but its step "${st.name || "(unnamed)"}" has ${key}: ${v}, which can let the job pass without that step's check. Drop it, or if it can't, add "${entry}" to SAFE_STEPS in this script`);
      }
    }
  }
}

// ---- the list in ci.yml ----
const listed = { required: new Set(), optional: new Set() };
for (const l of readFileSync(listFile, "utf8").split("\n")) {
  const m = /^#\s+(required|optional):\s+(.+?)\s*$/.exec(l);
  if (m) listed[m[1]].add(m[2]);
}
if (listed.required.size === 0) bad(`${listFile} lists no required checks ("#   required: <job name>")`);

for (const ctx of required.keys()) {
  if (!prJobs.has(ctx)) bad(`the ruleset requires "${ctx}", which no job reports on pull requests: every PR would wait for it, and merging would take the maintainer's bypass (rename the job back, or update deploy/github/ruleset-main.json)`);
  if (!listed.required.has(ctx)) bad(`the ruleset requires "${ctx}", but ${listFile}'s header doesn't list it as required`);
}
for (const ctx of listed.required) if (!required.has(ctx)) bad(`${listFile}'s header lists "${ctx}" as required, but deploy/github/ruleset-main.json doesn't require it`);
for (const ctx of listed.optional) {
  if (required.has(ctx)) bad(`"${ctx}" is listed as optional but the ruleset requires it`);
  if (!prJobs.has(ctx) && !filteredJobs.has(ctx)) bad(`"${ctx}" is listed as optional, but no job by that name runs on pull requests`);
}
for (const [name, where] of filteredJobs) {
  if (!listed.optional.has(name)) bad(`"${name}" (${where}) runs only on some pull requests, so it can't be a required check, and a PR it covers could merge with it red: drop the filter and require it, or list it as optional in ${listFile}'s header`);
}
for (const [name, where] of prJobs) {
  if (!required.has(name) && !listed.optional.has(name)) bad(`"${name}" (${where}) runs on every pull request but isn't a required check: a PR could merge with it red. Require it in deploy/github/ruleset-main.json and ${listFile}'s header, or list it there as optional`);
}

if (errors.length) {
  for (const e of errors) console.error(`✗ ${e}`);
  process.exit(1);
}
console.log(`✓ required checks: the ruleset's ${required.size} match ${listFile.replace(root, "")}'s list and the jobs that run on every pull request${listed.optional.size ? ` (${listed.optional.size} optional)` : ""}`);
