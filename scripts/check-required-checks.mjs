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
//     SAFE_STEPS (exact job, step name, value and what the step runs: a reviewed exception, e.g.
//     an upload on failure);
//   - a matrix (its checks are named "<name> (<values>)") or a reusable workflow (`uses:`).
// Round 4 (audit review-r3-supply-1): the workflows are read with a real YAML parser (the `yaml`
// package, a root devDependency pinned in package-lock.json; CI installs it with --ignore-scripts),
// so every spelling GitHub accepts (quoted keys, indentless or compact sequences, multi-line and
// folded scalars, flow collections over several lines, anchors and aliases) is read as GitHub
// reads it. What it can't read the same way is refused, never skipped: a parse error or warning,
// a merge key (<<), an explicit tag, a key that isn't a plain scalar, two keys that differ only
// in case (keys are read case-insensitively), more than one document, a `needs` that isn't a job
// id or a list of them, a step that isn't a mapping, a name that isn't text. No job in any
// workflow may be able to report a required check's name but the required job itself: a computed
// name ("${{ … }}") counts as every name it could produce (a `matrix.<key>` over a literal list
// expands to its values; any other expression matches anything), compared without case.
// What a step's command does is code review's job (CODEOWNERS), not this script's: it makes sure
// a required job runs on every pull request and that each of its steps runs and counts.
//   node scripts/check-required-checks.mjs [--ruleset F] [--workflows DIR] [--list F]
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

let YAML;
try {
  YAML = await import("yaml");
} catch {
  console.error("✗ scripts/check-required-checks.mjs reads the workflows with the yaml package (pinned in package-lock.json): run npm ci --ignore-scripts first");
  process.exit(1);
}

const GITHUB_ACTIONS = 15368;
// Job-level conditions that never skip a job on a pull request.
const SAFE_JOB_IFS = new Set(["github.event_name != 'schedule'"]);
// Job-level conditions that keep a job off pull requests altogether (the nightly fuzzers): such a
// job isn't a pull-request check, and a required job can't have one.
const NOT_ON_PR_IFS = new Set(["github.event_name != 'pull_request'"]);
// Step-level conditions in required jobs that can't make the job pass without its checks: each is
// reviewed for exactly that job, step name, key and value, and the step must do only what it says
// (`uses`: the action, any pinned version; `run`: exactly that command), so a check step renamed
// to an exception's name isn't one.
const SAFE_STEPS = [
  // Uploads the Playwright traces when the tests already failed.
  { job: "Wallet e2e (Playwright, Chromium)", step: "Traces of failed tests", key: "if", value: "failure()", uses: /^actions\/upload-artifact@[0-9a-f]{40}$/ },
  // Reports dev-tooling advisories; the runtime audit in the step before blocks.
  { job: "Dependencies (cargo-deny, npm audit)", step: "npm audit (all, non-blocking)", key: "continue-on-error", value: "true", run: "npm audit" },
];
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

// ---- the workflows, read with a YAML parser (as GitHub reads them) ----
const isMap = (v) => v !== null && typeof v === "object" && !Array.isArray(v);
const scalar = (v) => typeof v === "string" || typeof v === "number" || typeof v === "boolean";
const show = (v) => (typeof v === "string" ? v : JSON.stringify(v));
// A condition as GitHub evaluates it: `${{ }}` around it is optional. Anything that isn't text
// (`if: false`, an empty `if:`) is kept as written, so only the exact reviewed strings pass.
const condOf = (v) => (typeof v === "string" ? cond(v) : v === null || v === undefined ? "(empty)" : show(v));
// Check names are compared without case or surrounding/repeated spaces.
const norm = (s) => s.trim().replace(/\s+/g, " ").toLowerCase();

// A mapping's keys, lower-cased (GitHub's keywords; refusing two that differ only in case).
function keysOf(map, where) {
  const m = new Map();
  for (const [k, v] of Object.entries(map)) {
    const lk = k.toLowerCase();
    if (m.has(lk)) bad(`${where}: "${k}" and another key differ only in case; this checker refuses what GitHub might read either way`);
    m.set(lk, v);
  }
  return m;
}

function readYaml(file, f) {
  const docs = YAML.parseAllDocuments(readFileSync(file, "utf8"), { version: "1.2", uniqueKeys: true, merge: false, strict: true, prettyErrors: true });
  const problems = [];
  if (!Array.isArray(docs) || docs.length !== 1) problems.push(`${Array.isArray(docs) ? docs.length : 0} YAML documents (expected one)`);
  for (const doc of Array.isArray(docs) ? docs : []) {
    for (const e of [...doc.errors, ...doc.warnings]) problems.push(e.message.split("\n")[0]);
    YAML.visit(doc, {
      Pair(_, pair) {
        if (!YAML.isScalar(pair.key)) problems.push(`a key that isn't a plain scalar (${String(pair.key)})`);
        else if (pair.key.value === "<<" || typeof pair.key.value === "symbol" || pair.key.source === "<<") problems.push("a merge key (<<), which this checker doesn't expand");
      },
      // Workflows need no explicit tags (!!str, !!merge, !custom), and they change how a node reads.
      Node(_, node) {
        if (node.tag) problems.push(`an explicit YAML tag (${node.tag})`);
      },
    });
  }
  if (problems.length) {
    bad(`${f} can't be read the way GitHub reads it, so it's refused: ${[...new Set(problems)].join("; ")}`);
    return null;
  }
  const top = docs[0].toJS({ maxAliasCount: 100 });
  if (!isMap(top)) {
    bad(`${f} isn't a workflow (a mapping at the top)`);
    return null;
  }
  return top;
}

// The regex of the names a job's check can have: its name, with each ${{ … }} replaced by what it
// can produce (a matrix key's literal values, else anything).
function namePattern(job) {
  const esc = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const parts = job.name.split(/(\$\{\{[\s\S]*?\}\})/);
  const rx = parts
    .map((p, i) => {
      if (i % 2 === 0) return p.includes("${{") ? "[\\s\\S]*" : esc(p);
      const m = /^matrix\.([A-Za-z0-9_-]+)$/.exec(cond(p));
      const values = m && job.matrixValues?.get(m[1].toLowerCase());
      return values && !values.some((v) => String(v).includes("${{")) ? `(?:${values.map((v) => esc(String(v))).join("|")})` : "[\\s\\S]*";
    })
    .join("");
  return new RegExp(`^\\s*${rx}\\s*$`, "i");
}
const canReport = (job, ctx) => (job.name.includes("${{") ? namePattern(job).test(ctx) || namePattern(job).test(norm(ctx)) : norm(job.name) === norm(ctx));

function parseWorkflow(file, f) {
  const top = readYaml(file, f);
  if (!top) return { pr: false, prFilter: "", jobs: [], unreadable: true };
  const wf = keysOf(top, f);
  // on: does it run on every pull request?
  const on = wf.get("on");
  let pr = false;
  let prFilter = "";
  const events = typeof on === "string" ? [on] : Array.isArray(on) ? on : isMap(on) ? Object.keys(on) : null;
  if (!events || !events.every((e) => typeof e === "string")) bad(`${f}: can't read its on: (${show(on)})`);
  else if (events.some((e) => e.toLowerCase() === "pull_request")) {
    pr = true;
    const cfg = isMap(on) ? keysOf(on, `${f}: on`).get("pull_request") : null;
    if (cfg !== null && cfg !== undefined && !isMap(cfg)) bad(`${f}: can't read on.pull_request (${show(cfg)})`);
    else if (isMap(cfg)) {
      const c = keysOf(cfg, `${f}: on.pull_request`);
      const filter = ["paths", "paths-ignore", "branches", "branches-ignore"].find((k) => c.has(k));
      if (filter) prFilter = filter;
      else if (c.has("types")) {
        const types = c.get("types");
        const list = typeof types === "string" ? [types] : Array.isArray(types) ? types : [];
        // The default activity types are what a PR's pushes trigger; fewer is a filter.
        if (!["opened", "synchronize", "reopened"].every((t) => list.includes(t))) prFilter = "types";
      }
    }
  }
  // jobs: id → { name, if, needs, continueOnError, uses, matrix, steps: [{ name, if, continueOnError, uses, run }] }
  const jobs = [];
  const jobsMap = wf.get("jobs");
  if (!isMap(jobsMap)) {
    bad(`${f}: can't read its jobs: (${show(jobsMap)})`);
    return { pr, prFilter, jobs, unreadable: true };
  }
  for (const [id, raw] of Object.entries(jobsMap)) {
    const where = `${f}: job ${id}`;
    if (!isMap(raw)) {
      bad(`${where} isn't a mapping (${show(raw)}): can't read it`);
      continue;
    }
    const j = keysOf(raw, where);
    const job = { id, name: id, if: null, needs: [], continueOnError: null, uses: j.get("uses"), matrix: false, matrixValues: null, steps: [], unreadable: [] };
    if (j.has("name")) {
      const v = j.get("name");
      if (scalar(v)) job.name = String(v);
      else {
        bad(`${where}: its name (${show(v)}) isn't text: can't tell which check it reports`);
        job.name = "${{ unreadable }}";
      }
    }
    if (j.has("if")) job.if = condOf(j.get("if"));
    if (j.has("continue-on-error")) job.continueOnError = condOf(j.get("continue-on-error"));
    const needs = j.get("needs");
    if (typeof needs === "string") job.needs = [needs];
    else if (Array.isArray(needs) && needs.every((n) => typeof n === "string")) job.needs = needs;
    else if (needs !== undefined && needs !== null) job.unreadable.push(`its needs: (${show(needs)}) isn't a job id or a list of them`);
    const strategy = j.get("strategy");
    if (strategy !== undefined) {
      const st = isMap(strategy) ? keysOf(strategy, `${where}: strategy`) : null;
      const matrix = st?.get("matrix");
      if (matrix !== undefined) {
        job.matrix = true;
        // A matrix of literal lists, without include (which can add values), expands names.
        if (isMap(matrix)) {
          const mx = keysOf(matrix, `${where}: strategy.matrix`);
          if (!mx.has("include")) {
            job.matrixValues = new Map();
            for (const [k, v] of mx) if (k !== "exclude" && Array.isArray(v) && v.every(scalar)) job.matrixValues.set(k, v);
          }
        }
      }
    }
    const steps = j.get("steps");
    if (Array.isArray(steps)) {
      steps.forEach((raw, i) => {
        if (!isMap(raw)) {
          job.unreadable.push(`its step ${i + 1} isn't a mapping (${show(raw)})`);
          return;
        }
        const s = keysOf(raw, `${where}: step ${i + 1}`);
        const name = s.get("name");
        job.steps.push({
          name: scalar(name) ? String(name) : "",
          if: s.has("if") ? condOf(s.get("if")) : null,
          continueOnError: s.has("continue-on-error") ? condOf(s.get("continue-on-error")) : null,
          uses: s.get("uses"),
          run: s.get("run"),
        });
      });
    } else if (steps !== undefined || job.uses === undefined) job.unreadable.push(`its steps: (${show(steps)}) isn't a list`);
    jobs.push(job);
  }
  return { pr, prFilter, jobs };
}

const prJobs = new Map(); // name → "file: job id"
const prWorkflows = []; // [file, jobs] of the workflows that run on pull requests
const filteredJobs = new Map(); // jobs that run only on some pull requests (a paths/branches filter)
const files = readdirSync(workflowDir).filter((f) => /\.ya?ml$/.test(f)).sort();
if (files.length === 0) bad(`no workflows in ${workflowDir}`);
const allJobs = []; // [job, "file: job id"] across every workflow, whatever triggers it
for (const f of files) {
  const wf = parseWorkflow(join(workflowDir, f), f);
  for (const job of wf.jobs) allJobs.push([job, `${f}: ${job.id}`]);
  if (!wf.pr) continue;
  prWorkflows.push([f, wf.jobs]);
  if (wf.jobs.length === 0 && !wf.unreadable) bad(`${f}: runs on pull_request but has no jobs`);
  for (const job of wf.jobs) {
    // A job whose if: keeps it off pull requests (exactly a reviewed NOT_ON_PR_IFS condition) isn't
    // a pull-request check; any other job in this workflow is.
    if (job.if !== null && NOT_ON_PR_IFS.has(job.if)) {
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

// A check run from any other job that can carry a required name (a push-triggered copy that always
// passes, a computed name that evaluates to it, the same name in other letter case) could stand in
// for the real one on the PR's head commit.
for (const ctx of required.keys()) {
  const where = allJobs.filter(([job]) => canReport(job, ctx)).map(([job, at]) => (job.name === ctx ? at : `${at}, named ${JSON.stringify(job.name)}`));
  if (where.length > 1) bad(`the required check "${ctx}" could be reported by ${where.length} jobs (${where.join("; ")}): any of them reporting it would satisfy the ruleset. Keep required names unique across all workflows, and computed names unable to produce one`);
}

// ---- required jobs can't skip themselves or pass without their work ----
for (const [f, jobs] of prWorkflows) {
  const byId = new Map(jobs.map((j) => [j.id, j]));
  for (const job of jobs) {
    if (!required.has(job.name)) continue;
    const at = `"${job.name}" (${f}: ${job.id})`;
    for (const u of job.unreadable) bad(`${at} is required, but ${u}: this checker can't tell what it runs, so it's refused`);
    if (job.if !== null && !NOT_ON_PR_IFS.has(job.if) && !SAFE_JOB_IFS.has(job.if)) bad(`${at} is required, but its if: (${job.if}) can skip it, and a skipped check counts as passing. Drop the condition, or if it can't skip a pull request add it to SAFE_JOB_IFS in this script (now: ${[...SAFE_JOB_IFS].join("; ")})`);
    for (const n of job.needs) {
      const dep = byId.get(n);
      if (!dep) bad(`${at} needs "${n}", which isn't a job in ${f}`);
      else if (!required.has(dep.name)) bad(`${at} is required, but it needs "${dep.name}" (${f}: ${n}), which isn't: if that job fails or is skipped, ${at} is skipped, and a skipped check counts as passing. Require it too, or drop the needs`);
    }
    if (job.continueOnError !== null) bad(`${at} is required, but it has continue-on-error: ${job.continueOnError}, so it can fail and still let the PR through`);
    if (job.uses !== undefined) bad(`${at} is required, but it calls a reusable workflow (uses: ${show(job.uses)}), whose jobs report under other names and whose conditions this checker doesn't read`);
    if (job.matrix) bad(`${at} is required, but it has a matrix: its checks are named "${job.name} (…)", so "${job.name}" is never reported`);
    for (const st of job.steps) {
      for (const [key, v] of [["if", st.if], ["continue-on-error", st.continueOnError]]) {
        if (v === null) continue;
        const safe = SAFE_STEPS.some(
          (e) =>
            e.job === job.name &&
            e.step === st.name &&
            e.key === key &&
            e.value === v &&
            (e.uses ? typeof st.uses === "string" && e.uses.test(st.uses) && st.run === undefined : st.uses === undefined) &&
            (e.run !== undefined ? st.run === e.run : st.run === undefined),
        );
        if (!safe) bad(`${at} is required, but its step "${st.name || "(unnamed)"}" has ${key}: ${v}, which can let the job pass without that step's check. Drop it, or if it can't, add it to SAFE_STEPS in this script (job, step name, key, value, and what the step runs)`);
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
