// The Governance page: proposals with tallies and council votes, voting with conviction (signed
// by the passkey wallet after a review step, checked by the mock node; no lock beyond the vote
// unless asked for), on-chain text rendered as text, releasing vote locks once one has ended, and
// a node without governance.
import { parseFang, proposalDigest, type ProposalKind, type Tx } from "@blockula/sdk";
import { RPC } from "./env";
import { addressOf } from "./mock-node";
import { createWallet, cspViolations, expect, test, watchCsp } from "./helpers";

const FANG = 10n ** 9n;

test("lists proposals and votes with conviction", async ({ page, node }) => {
  // As node/src/api.rs `proposal_json` serves them.
  const big = (fang: bigint) => ({ drops: (fang * FANG).toString(), display: `${fang} FANG` });
  const label = (address: string, name: string | null = null) => ({ address, name });
  const council = [...Array(16)].map((_, i) => ({ validator: label(`boo1validator${i}`), aye: i < 15, voter: label(`boo1validator${i}`) }));
  node.proposals = [
    {
      id: 7,
      title: "Raise the active set to 40",
      url: "https://forum.blockula.test/p/7",
      kind: { type: "param_change", key: "max_validators", value: "40" },
      summary: "set max_validators to 40",
      proposer: label("boo1proposer", "vlad.boo"),
      deposit: (1000n * FANG).toString(),
      status: "voting",
      created_at: node.height - 10,
      voting_end: node.height + 5000,
      execute_at: null,
      supermajority: false,
      ayes: { amount: big(300n), weight: big(300n) },
      nays: { amount: big(100n), weight: big(100n) },
      voters: 4,
      council,
      tally: null,
      outcome: "",
    },
    // review-r3-clients-1: a cancellation says it supersedes the upgrade proposals still open.
    { id: 8, title: "Postpone the v2 feature flag", url: "", kind: { type: "cancel_upgrade", version: 2, activation_height: 90_000 }, summary: "cancel the upgrade to protocol v2 at block 90000", proposer: label("boo1proposer"), deposit: "0", status: "voting", created_at: node.height - 5, voting_end: node.height + 5000, execute_at: null, supermajority: true, ayes: { amount: big(0n), weight: big(0n) }, nays: { amount: big(0n), weight: big(0n) }, voters: 0, council: [], tally: null, outcome: "" },
    // On-chain strings are attacker-controlled: they must render as text, and only https links work.
    { id: 6, title: '<img src=x onerror="window.__pwned=1">', url: "javascript:window.__pwned=2", kind: { type: "grant_soul", to: addressOf("5e".repeat(32)), amount: 1000, reason: "<b>bold</b>" }, summary: "<b>bold</b>", proposer: label("boo1proposer"), deposit: "0", status: "rejected", created_at: 1, voting_end: 2, execute_at: null, supermajority: false, ayes: { amount: big(0n), weight: big(0n) }, nays: { amount: big(5n), weight: big(5n) }, voters: 1, council: [], tally: { unlocked_supply: big(1000n), quorum: big(40n), quorum_reached: false, supermajority: false, council_size: 22, council_ayes: 0, council_nays: 0 }, outcome: "quorum not reached" },
  ];
  await watchCsp(page);
  await createWallet(page);
  await page.getByRole("link", { name: "Governance", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Governance" })).toBeVisible();

  const open = page.getByTestId("proposal").filter({ hasText: "Raise the active set to 40" });
  // What it does is rendered from the kind the vote binds, not the node's summary.
  await expect(open.getByTestId("proposal-does")).toHaveText("Set the chain parameter max_validators to 40 (in that parameter's own units: drops, blocks, basis points…)");
  await expect(open).toContainText("300 FANG aye · 100 FANG nay");
  await expect(open).toContainText("400 FANG voted");
  await expect(open).toContainText("council: 15 validators approve, 1 reject");
  await expect(open).toContainText("by vlad.boo");
  await expect(open.getByRole("link", { name: "Full text ↗" })).toHaveAttribute("href", "https://forum.blockula.test/p/7");

  const cancel = page.getByTestId("proposal").filter({ hasText: "Postpone the v2 feature flag" });
  await expect(cancel.getByTestId("proposal-does")).toHaveText("Cancel the upgrade to protocol v2 at block 90000 and any upgrade scheduled after it; upgrade proposals still open when it runs are superseded and can't take effect");

  // The hostile proposal is plain text, with no link and no script run.
  const hostile = page.getByTestId("proposal").filter({ hasText: "onerror" });
  await expect(hostile).toContainText('<img src=x onerror="window.__pwned=1">');
  await expect(hostile.getByTestId("proposal-does")).toContainText("(reason, as written: “<b>bold</b>”)");
  await expect(hostile.getByRole("link", { name: "Full text ↗" })).toHaveCount(0);
  await expect(hostile.getByTestId("vote-form")).toHaveCount(0);
  expect(await page.evaluate(() => (window as unknown as { __pwned?: number }).__pwned)).toBeUndefined();

  // No lock beyond the vote itself unless asked for (USER-4): conviction 0 by default.
  await expect(open.getByLabel("Conviction")).toHaveValue("0");
  await expect(open.getByLabel("Conviction").locator("option:checked")).toHaveText("0.1× · locked until voting ends");

  // Vote aye with 10 FANG at 2× conviction (locked two lock periods: two weeks here), after a review.
  await open.getByRole("button", { name: "Aye" }).click();
  await open.getByLabel("Amount to vote with").fill("10");
  await open.getByLabel("Conviction").selectOption("2");
  await expect(open.getByLabel("Conviction")).toContainText("2× · locked 14 days after voting ends");
  await open.getByRole("button", { name: "Vote aye with 10 FANG (weight 20 FANG)" }).click();
  const review = open.getByTestId("review");
  await expect(review).toContainText("Vote aye on proposal #7 with 10 FANG");
  // What the vote binds, rendered from the kind: the same words as the card.
  await expect(review).toContainText("Set the chain parameter max_validators to 40");
  await expect(review).toContainText("2×: counts as 20 FANG");
  // Locked through voting's end (block 5,000 from now), then two weeks of lock periods.
  const end = Number(node.proposals.find((x) => x.id === 7)?.voting_end) + 1 + 2 * 604_800;
  await expect(review.getByTestId("vote-locked-until")).toContainText(`(block #${end.toLocaleString("en-US")})`);
  await expect(review.getByTestId("review-terms")).toContainText("Your vote and its amount are public.");
  await expect(review.getByTestId("review-terms")).toContainText("10 FANG stays locked until about");
  await expect(review.getByTestId("review-terms")).toContainText("you can't shorten the lock");
  expect(node.submitted).toHaveLength(0);
  await review.getByRole("button", { name: "Confirm vote" }).click();
  await expect(page.getByRole("status")).toContainText("voted aye on proposal #7 with 10 FANG");
  const tx = node.submitted.at(-1) as Tx;
  expect(tx.action).toMatchObject({ type: "vote", proposal_id: 7, aye: true, amount: parseFang("10").toString(), conviction: 2 });
  // Bound to the proposal as shown (gap-7-6); the mock node checks it as nodes do.
  expect(tx.action.type === "vote" && tx.action.proposal_digest).toMatch(/^[0-9a-f]{64}$/);
  await expect(open).toContainText("320 FANG aye");

  // Changing the vote replaces it.
  await open.getByRole("button", { name: "Nay" }).click();
  await open.getByLabel("Conviction").selectOption("0");
  await open.getByRole("button", { name: "Vote nay with 10 FANG (weight 1 FANG)" }).click();
  await expect(review.getByTestId("review-terms")).toContainText("10 FANG stays locked until voting ends, about");
  await review.getByRole("button", { name: "Confirm vote" }).click();
  await expect(open).toContainText("300 FANG aye · 101 FANG nay");

  // No vote lock has ended: nothing to release yet, and it says when the next one ends (USER-19).
  await expect(page.getByRole("button", { name: "Release expired locks" })).toBeDisabled();
  await expect(page.getByTestId("locks-hint")).toContainText("None has ended yet: your next vote lock ends about");
  // The tally bars and everything else render under the strict CSP.
  expect(await cspViolations(page)).toEqual([]);
});

// USER-19: "Release expired locks" used to be always on, and answered "no expired vote locks to
// release" from the node. It waits for a vote lock to end now, read from the account's own votes.
test("releases vote locks once one has ended, not before", async ({ page, node }) => {
  // A lock period of 2 blocks, so a lock can end within the test.
  const cors = { "access-control-allow-origin": "*" };
  await page.route(`${RPC}/api/params`, (route) =>
    route.fulfill({ status: 200, contentType: "application/json", headers: cors, body: JSON.stringify({ params: { unbonding_blocks: "20", lock_period_blocks: "2" }, genesis: { block_time_ms: 1000, epoch_blocks: 3600, address_prefix: "boo", epochs_per_year: 8766 }, issuance: { year: 1, rate_ppm: 60000 } }) }),
  );
  await createWallet(page);
  const zero = { amount: { drops: "0", display: "0 FANG" }, weight: { drops: "0", display: "0 FANG" } };
  const ends = node.height + 4;
  node.proposals = [{ id: 3, title: "Short vote", url: "", kind: { type: "param_change", key: "max_validators", value: "40" }, summary: "", proposer: { address: addressOf("66".repeat(32)), name: null }, deposit: "0", status: "voting", created_at: node.height - 1, voting_end: ends, execute_at: null, supermajority: false, ayes: zero, nays: zero, voters: 0, council: [], tally: null, outcome: "" }];
  await page.getByRole("link", { name: "Governance", exact: true }).click();
  const card = page.getByTestId("proposal").filter({ hasText: "Short vote" });
  await expect(page.getByTestId("locks-hint")).toHaveText("You have no vote locks to release.");
  await expect(page.getByRole("button", { name: "Release expired locks" })).toBeDisabled();
  await card.getByRole("button", { name: "Aye" }).click();
  await card.getByLabel("Amount to vote with").fill("1");
  await card.getByLabel("Conviction").selectOption("1");
  await card.getByRole("button", { name: /^Vote aye with 1 FANG/ }).click();
  await expect(card.getByTestId("vote-locked-until")).toContainText(`(block #${ends + 1 + 2})`);
  await card.getByRole("button", { name: "Confirm vote" }).click();
  await expect(page.getByRole("status")).toContainText("voted aye on proposal #3");
  await expect(page.getByRole("button", { name: "Release expired locks" })).toBeDisabled();
  await expect(page.getByTestId("locks-hint")).toContainText("None has ended yet");

  // Voting ends and its lock period passes: now there's something to release.
  node.height = ends + 10;
  await expect(page.getByTestId("locks-hint")).toHaveText("Your vote lock on proposal #3 has ended and holds nothing now. Releasing it clears it from your account, which can keep 64 locks at most.", { timeout: 15_000 });
  await page.getByRole("button", { name: "Release expired locks" }).click();
  await expect(page.getByRole("status")).toContainText("unlocked their expired vote locks");
  expect(node.submitted.at(-1)?.action).toEqual({ type: "unlock" });
  await expect(page.getByTestId("locks-hint")).toHaveText("You have no vote locks to release.");
  await expect(page.getByRole("button", { name: "Release expired locks" })).toBeDisabled();
});

// Review WM-R1: with no account read (it failed, or hasn't answered yet) the wallet knows nothing
// about its votes, so it mustn't say there are none and switch the button off.
test("keeps Release on, and claims nothing, when the account can't be read", async ({ page }) => {
  const { account } = await createWallet(page);
  await page.route(`${RPC}/api/account/${account}`, (route) => route.fulfill({ status: 503, contentType: "application/json", headers: { "access-control-allow-origin": "*" }, body: JSON.stringify({ error: "busy" }) }));
  await page.goto("/governance");
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  await expect(page.getByTestId("locks-hint")).toHaveText("Release vote locks that have ended:");
  await expect(page.getByRole("button", { name: "Release expired locks" })).toBeEnabled();
});

test("explains when the network has no governance yet", async ({ page, node }) => {
  node.governance = false;
  await createWallet(page);
  await page.getByRole("link", { name: "Governance", exact: true }).click();
  await expect(page.getByText("On-chain governance isn't active on this network yet.", { exact: false })).toBeVisible();
});

test("the testnet footer says it's for test chains", async ({ page }) => {
  await page.goto("/welcome");
  await expect(page.getByTestId("imprint-note")).toHaveText("Not audited. For test chains only.");
});

// Round 3 (gap-7-6, the R1 reviewer's PoC): a node that keeps a proposal's kind, title and link true
// and lies only in its summary. The card used to show the summary while the vote signed the drain.
test("a proposal card says what the vote binds, whatever the node's summary says", async ({ page, node }) => {
  const mallory = addressOf("66".repeat(32));
  const zero = { amount: { drops: "0", display: "0 FANG" }, weight: { drops: "0", display: "0 FANG" } };
  const base = { url: "", proposer: { address: mallory, name: null }, deposit: "1000000000000", status: "voting", created_at: node.height - 1, voting_end: node.height + 5000, execute_at: null, supermajority: true, ayes: zero, nays: zero, voters: 0, council: [], tally: null, outcome: "" };
  const drain: ProposalKind = { type: "treasury_spend", to: mallory, amount: (10_000_000n * 10n ** 9n).toString() };
  node.proposals = [
    { ...base, id: 8, title: "Community garden", kind: drain, summary: "plant garlic in the community garden (no funds move)" },
    // review-r2-clients-1: a number JSON can't carry exactly. Not described as a guess, and no vote.
    { ...base, id: 9, title: "Big id", kind: { type: "distribution", distribution_id: 2 ** 60, merkle_root: "ab".repeat(32), total: "5", claim_until: 50 }, summary: "distribute 5 drops" },
  ];
  await createWallet(page);
  await page.getByRole("link", { name: "Governance", exact: true }).click();
  const garden = page.getByTestId("proposal").filter({ hasText: "Community garden" });
  await expect(garden.getByTestId("proposal-does")).toHaveText(`Pay 10,000,000 FANG from the treasury to ${mallory}`);
  await expect(garden).not.toContainText("garlic");
  const digest = proposalDigest(drain, "Community garden", "");
  await expect(garden.getByTestId("proposal-digest")).toHaveText(`vote binds ${digest.slice(0, 16)}…`);
  await garden.getByRole("button", { name: "Nay" }).click();
  await garden.getByLabel("Amount to vote with").fill("1");
  await garden.getByRole("button", { name: /^Vote nay with 1 FANG/ }).click();
  // The review says what the vote binds, from the kind: the drain, not the garden.
  await expect(garden.getByTestId("review")).toContainText(`Pay 10,000,000 FANG from the treasury to ${mallory}`);
  await expect(garden.getByTestId("review")).not.toContainText("garlic");
  await garden.getByRole("button", { name: "Confirm vote" }).click();
  await expect(page.getByRole("status")).toContainText("voted nay on proposal #8");
  const tx = node.submitted.at(-1) as Tx;
  expect(tx.action.type === "vote" && tx.action.proposal_digest).toBe(digest);

  const big = page.getByTestId("proposal").filter({ hasText: "Big id" });
  await expect(big.getByTestId("proposal-does")).toContainText("distribution #a number too large for this client to read exactly");
  await expect(big.getByTestId("cannot-vote")).toBeVisible();
  await expect(big.getByTestId("vote-form")).toHaveCount(0);
});
