// The Governance page: proposals with tallies and council votes, voting with conviction (signed
// by the passkey wallet, checked by the mock node), on-chain text rendered as text, and a node
// without governance.
import { parseFang, type Tx } from "@blockula/sdk";
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
    // On-chain strings are attacker-controlled: they must render as text, and only https links work.
    { id: 6, title: '<img src=x onerror="window.__pwned=1">', url: "javascript:window.__pwned=2", kind: { type: "text" }, summary: "<b>bold</b>", proposer: label("boo1proposer"), deposit: "0", status: "rejected", created_at: 1, voting_end: 2, execute_at: null, supermajority: false, ayes: { amount: big(0n), weight: big(0n) }, nays: { amount: big(5n), weight: big(5n) }, voters: 1, council: [], tally: { unlocked_supply: big(1000n), quorum: big(40n), quorum_reached: false, supermajority: false, council_size: 22, council_ayes: 0, council_nays: 0 }, outcome: "quorum not reached" },
  ];
  await watchCsp(page);
  await createWallet(page);
  await page.getByRole("link", { name: "Governance", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Governance" })).toBeVisible();

  const open = page.getByTestId("proposal").filter({ hasText: "Raise the active set to 40" });
  await expect(open).toContainText("300 FANG aye · 100 FANG nay");
  await expect(open).toContainText("400 FANG voted");
  await expect(open).toContainText("council: 15 validators approve, 1 reject");
  await expect(open).toContainText("by vlad.boo");
  await expect(open.getByRole("link", { name: "Full text ↗" })).toHaveAttribute("href", "https://forum.blockula.test/p/7");

  // The hostile proposal is plain text, with no link and no script run.
  const hostile = page.getByTestId("proposal").filter({ hasText: "onerror" });
  await expect(hostile).toContainText('<img src=x onerror="window.__pwned=1">');
  await expect(hostile).toContainText("<b>bold</b>");
  await expect(hostile.getByRole("link", { name: "Full text ↗" })).toHaveCount(0);
  await expect(hostile.getByTestId("vote-form")).toHaveCount(0);
  expect(await page.evaluate(() => (window as unknown as { __pwned?: number }).__pwned)).toBeUndefined();

  // Vote aye with 10 FANG at 2× conviction (locked two lock periods: two weeks here).
  await open.getByRole("button", { name: "Aye" }).click();
  await open.getByLabel("Amount to vote with").fill("10");
  await open.getByLabel("Conviction").selectOption("2");
  await expect(open.getByLabel("Conviction")).toContainText("2× · locked 14 days after voting ends");
  await open.getByRole("button", { name: "Vote aye with 10 FANG (weight 20 FANG)" }).click();
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
  await expect(open).toContainText("300 FANG aye · 101 FANG nay");

  await page.getByRole("button", { name: "Release expired locks" }).click();
  await expect(page.getByRole("status")).toContainText("unlocked their expired vote locks");
  expect(node.submitted.at(-1)?.action).toEqual({ type: "unlock" });
  // The tally bars and everything else render under the strict CSP.
  expect(await cspViolations(page)).toEqual([]);
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
