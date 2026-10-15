// The Governance page: proposals with tallies and council votes, voting with conviction (signed
// by the passkey wallet, checked by the mock node), on-chain text rendered as text, and a node
// without governance.
import { parseFang, type Tx } from "@blockula/sdk";
import { createWallet, cspViolations, expect, test, watchCsp } from "./helpers";

const FANG = 10n ** 9n;

test("lists proposals and votes with conviction", async ({ page, node }) => {
  node.proposals = [
    {
      id: 7,
      title: "Raise the active set to 40",
      summary: "set max_validators to 40",
      kind: { type: "param_change", key: "max_validators", value: "40" },
      proposer: { address: "boo1proposer", name: "vlad.boo", short: "boo1pro…" },
      status: "voting",
      deposit: (1000n * FANG).toString(),
      tally: { aye: (300n * FANG).toString(), nay: (100n * FANG).toString(), turnout: (400n * FANG).toString(), quorum: (1000n * FANG).toString() },
      council: { ayes: 15, nays: 1, of: 22 },
      voting_ends_at: node.height + 5000,
      url: "https://forum.blockula.test/p/7",
    },
    // On-chain strings are attacker-controlled: they must render as text, and only https links work.
    { id: 6, title: '<img src=x onerror="window.__pwned=1">', summary: "<b>bold</b>", status: "rejected", url: "javascript:window.__pwned=2", tally: { aye: "0", nay: "5" } },
  ];
  await watchCsp(page);
  await createWallet(page);
  await page.getByRole("link", { name: "Governance", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Governance" })).toBeVisible();

  const open = page.getByTestId("proposal").filter({ hasText: "Raise the active set to 40" });
  await expect(open).toContainText("300 FANG aye · 100 FANG nay");
  await expect(open).toContainText("400 FANG of 1000 FANG quorum");
  await expect(open).toContainText("council: 15 of 22 validators approve");
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
  expect(tx.action).toEqual({ type: "vote", proposal_id: 7, aye: true, amount: parseFang("10").toString(), conviction: 2 });
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
