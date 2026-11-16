// Joining and leaving the Séance juror pool from the wallet (app/contribute, components/explorer/JuryCard.tsx),
// against the mock node's pool (e2e/explorer-world.ts) with the wallet's own account given Soul: the card shows
// its Soul against the chain's minimum, the bond and when the next Séance can open, all from the node's rules;
// "Join the jury" previews and signs `join_jury` with the owner passkey; an enrolled juror is offered the way
// out, which signs `leave_jury` and returns the bond; and an agent key, too little Soul or FANG, or a seat on
// the open Séance are explained instead of offered. All under the production build's strict CSP.
import type { Page } from "@playwright/test";
import { seedWorld } from "./explorer-world";
import { createWallet, cspViolations, expect, test, watchCsp } from "./helpers";
import type { MockNode } from "./mock-node";

const F = 10n ** 9n;

function watchErrors(page: Page) {
  const errors: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "error" && !/^Failed to load resource/.test(m.text())) errors.push(`console: ${m.text()}`);
  });
  page.on("pageerror", (e) => errors.push(`pageerror: ${e.message}`));
  page.on("response", (r) => {
    // Multisig reads answer 404 for an account that isn't shared: that's how the page learns it.
    if (r.status() >= 400 && !/\/api\/multisig\//.test(r.url())) errors.push(`${r.status()} ${r.url()}`);
  });
  return errors;
}

type Juror = { bond_display: string; active: boolean; served: number; joined_at: number } | null;

/** The world, seen by `me`, with `me` holding `soul` Soul and the juror record given. */
function world(node: MockNode, me: string, soul: string, juror: Juror = null) {
  seedWorld(node, me);
  const set = (soulNow: string, j: Juror) =>
    node.accountExtra.set(me, {
      ...node.accountExtra.get(me),
      contribution: {
        soul: { points: Math.round(Number(soulNow) * 1000), display: soulNow, earned_display: soulNow },
        sybil: null, vouched_by: null, vouching_for: [], vouches_withdrawn: [], vouch_bonds_held: [], juror: j, claims: [],
        next_claim_bond: F.toString(), next_claim_bond_display: "1 FANG", max_open_claims: 3,
        rewards: { total_display: "0 FANG", withdrawable: "0", withdrawable_display: "0 FANG", unvested_display: "0 FANG", grants: [] },
      },
    });
  set(soul, juror);
  const pool = node.contributions as { rules: Record<string, string | number>; next_seance_at: number; open_seance: { id: number; reveal_until: number; jurors: unknown[] } | null };
  return { set, pool, juror: () => (node.accountExtra.get(me)?.contribution as { juror: Juror }).juror };
}

test.describe("joining the Séance jury from the wallet", () => {
  let errors: string[];
  test.beforeEach(async ({ page }) => {
    errors = watchErrors(page);
    await watchCsp(page);
  });
  test.afterEach(async ({ page }) => {
    expect(await cspViolations(page)).toEqual([]);
    expect(errors).toEqual([]);
  });

  test("the card shows Soul, the bond and the next Séance from the chain's rules, and Join signs join_jury with the passkey", async ({ page, node }) => {
    const { account: me } = await createWallet(page);
    const w = world(node, me, "9");
    // This chain's own rules, not numbers the wallet knows.
    Object.assign(w.pool.rules, { juror_min_soul: "7", juror_bond_display: "12.5 FANG" });
    await page.goto("/contribute");
    const card = page.getByTestId("jury-card");
    await expect(card.getByRole("heading", { name: "Join the jury" })).toBeVisible();
    await expect(card.getByTestId("jury-soul")).toContainText("9");
    await expect(card.getByTestId("jury-soul")).toContainText("jurors need at least 7 · enough");
    await expect(card.getByTestId("jury-bond")).toContainText("12.5 FANG");
    await expect(card.getByTestId("jury-status")).toContainText("Not enrolled");
    await expect(card.getByTestId("jury-leave")).toHaveCount(0);

    // The open Séance, linked, and the next opening as a block with about when.
    const cutoff = card.getByTestId("jury-cutoff");
    await expect(cutoff.getByTestId("jury-open-seance")).toContainText(/Séance #2 is open: jurors seal their votes until block #\d+ \(about \d\d:\d\d/);
    await expect(cutoff.getByRole("link", { name: "See it →" })).toHaveAttribute("href", /\/seance\/2\/?$/);
    await expect(cutoff).toContainText(new RegExp(`The one after can open from block #${w.pool.next_seance_at} \\(about \\d\\d:\\d\\d( tomorrow)?, in \\d+`));
    await expect(cutoff).toContainText("3 jurors are available");

    // Locked: one unlock, here. Then the bond field starts from the chain's rule.
    const join = card.getByTestId("jury-join");
    await join.getByRole("button", { name: "Unlock to sign" }).click();
    await expect(join.getByLabel("Bond", { exact: true })).toHaveValue("12.5");
    await join.getByRole("button", { name: "Join the jury" }).click();
    const preview = join.getByTestId("action-preview");
    await expect(preview.getByTestId("signs")).toHaveText("Join the Séance juror pool, bonding 12.5 FANG");
    await expect(preview).toContainText("(owner)");
    expect(node.submitted).toHaveLength(0);
    const before = node.acct(me).balance;
    await preview.getByRole("button", { name: "Sign & send" }).click();
    await expect(page).toHaveURL(/\/tx\/[0-9a-f]{64}\/?$/);
    expect(node.submitted.map((t) => t.action)).toEqual([{ type: "join_jury", bond: (125n * F / 10n).toString() }]);
    expect(w.juror()).toMatchObject({ active: true, bond_display: "12.5 FANG" });
    expect(before - node.acct(me).balance).toBeGreaterThan(125n * F / 10n);

    // Back on the page: enrolled, with the way out and nothing more to join.
    await page.goto("/contribute");
    await expect(card.getByTestId("jury-status")).toContainText("In the juror pool");
    await expect(card.getByTestId("jury-status")).toContainText("bond 12.5 FANG");
    await expect(card.getByTestId("jury-join")).toHaveCount(0);
    await expect(card.getByTestId("jury-leave")).toBeVisible();
  });

  test("an agent key, too little Soul and too little FANG are explained, and nothing is signed", async ({ page, node }) => {
    const { account: me, keyId } = await createWallet(page);
    const w = world(node, me, "9");
    const owner = node.acct(me).keys;
    // This browser's key made an agent's: told so once unlocked, with nothing to preview.
    node.acct(me).keys = new Map([
      ["0e".repeat(32), { label: "Vlad's laptop", role: { type: "owner" as const }, added_at: 1 }],
      [keyId, { label: "claude", role: { type: "session" as const, limit: (100n * F).toString(), period_blocks: 86_400, expires_at: 2_000_000 }, added_at: 2 }],
    ]);
    await page.goto("/contribute");
    const join = page.getByTestId("jury-card").getByTestId("jury-join");
    await join.getByRole("button", { name: "Unlock to sign" }).click();
    await expect(join.locator('[data-blocker="agent_key"]')).toContainText("agent key (“claude”)");
    await expect(join.getByRole("button", { name: "Join the jury" })).toBeDisabled();
    await expect(join.getByRole("button", { name: "Preview" })).toHaveCount(0);

    // An owner key again, but under the chain's minimum.
    node.acct(me).keys = owner;
    w.set("4.5", null);
    await page.goto("/contribute");
    await expect(join.locator('[data-blocker="low_soul"]')).toContainText("Jurors need at least 5 Soul, and this account has 4.5");
    await expect(page.getByTestId("jury-soul")).toContainText("not enough yet");
    await expect(join.getByRole("button", { name: "Join the jury" })).toBeDisabled();

    // Enough Soul, but it can't cover the bond and the fee.
    w.set("9", null);
    node.acct(me).balance = 10n * F;
    await page.goto("/contribute");
    await expect(join.locator('[data-blocker="low_balance"]')).toContainText("The bond is 10 FANG and this account can move 10 FANG now");
    await expect(join.locator('[data-blocker="low_soul"]')).toHaveCount(0);
    await expect(join.getByRole("button", { name: "Join the jury" })).toBeDisabled();
    expect(node.submitted).toHaveLength(0);
  });

  test("an enrolled juror leaves with the passkey and gets the bond back; a seated one, or an agent key, can't", async ({ page, node }) => {
    const { account: me, keyId } = await createWallet(page);
    const w = world(node, me, "9", { bond_display: "10 FANG", active: true, served: 2, joined_at: 90 });
    // Seated on the open Séance: the chain would refuse, so it's explained.
    const seats = w.pool.open_seance!.jurors;
    seats.push({ ...(seats[0] as object), juror: node.label(me) });
    await page.goto("/contribute");
    const card = page.getByTestId("jury-card");
    const leave = card.getByTestId("jury-leave");
    await expect(card.getByTestId("jury-status")).toContainText("In the juror pool · bond 10 FANG · served 2 Séances");
    await expect(card.getByTestId("jury-join")).toHaveCount(0);
    await expect(leave.locator('[data-blocker="seated"]')).toContainText(`You're on the jury of Séance #2. Leave after it closes (block #${w.pool.open_seance!.reveal_until})`);
    await expect(card.getByRole("link", { name: "Judge it →" })).toBeVisible();

    // Not seated: unlock, preview, sign.
    seats.pop();
    await page.goto("/contribute");
    await expect(leave).toContainText("returns your bond (10 FANG)");
    await leave.getByRole("button", { name: "Unlock to sign" }).click();
    await leave.getByRole("button", { name: "Leave the jury" }).click();
    const preview = leave.getByTestId("action-preview");
    await expect(preview.getByTestId("signs")).toHaveText("Leave the Séance juror pool");
    await expect(preview).toContainText("(owner)");
    const before = node.acct(me).balance;
    await preview.getByRole("button", { name: "Sign & send" }).click();
    await expect(page).toHaveURL(/\/tx\/[0-9a-f]{64}\/?$/);
    expect(node.submitted.map((t) => t.action)).toEqual([{ type: "leave_jury" }]);
    expect(w.juror()).toBeNull();
    // The bond came back, less the fee.
    expect(node.acct(me).balance - before).toBeGreaterThan(9n * F);

    await page.goto("/contribute");
    await expect(card.getByTestId("jury-status")).toContainText("Not enrolled");
    await expect(card.getByTestId("jury-leave")).toHaveCount(0);

    // Enrolled again, on a device whose key is an agent's: no way out from here.
    w.set("9", { bond_display: "10 FANG", active: true, served: 2, joined_at: 90 });
    node.acct(me).keys = new Map([
      ["0e".repeat(32), { label: "Vlad's laptop", role: { type: "owner" as const }, added_at: 1 }],
      [keyId, { label: "claude", role: { type: "session" as const, limit: (100n * F).toString(), period_blocks: 86_400, expires_at: 2_000_000 }, added_at: 2 }],
    ]);
    await page.goto("/contribute");
    await leave.getByRole("button", { name: "Unlock to sign" }).click();
    await expect(leave.locator('[data-blocker="agent_key"]')).toContainText("agent key (“claude”)");
    await expect(leave.getByRole("button", { name: "Leave the jury" })).toHaveCount(0);
    expect(node.submitted).toHaveLength(1);
  });

  test("the page is linked from the pool's program page, which lists leaving too, and from your account", async ({ page, node }) => {
    const { account: me } = await createWallet(page);
    world(node, me, "9");
    await page.goto("/programs/pool");
    await expect(page.locator("details.action", { hasText: "Leave the jury pool" })).toHaveCount(1);
    await expect(page.locator("details.action", { hasText: "Join the jury pool" })).toHaveCount(1);
    await page.getByTestId("pool-jury-link").click();
    await expect(page).toHaveURL(/\/contribute\/?$/);
    await expect(page.getByTestId("jury-card")).toBeVisible();
    // The pool itself is on the page too, as on the node explorer's contribute page.
    await expect(page.getByText("in the pool").first()).toBeVisible();

    await page.goto(`/account/${me}`);
    await expect(page.getByTestId("account-jury-link")).toHaveText("Join the jury →");
    await expect(page.getByTestId("account-jury-link")).toHaveAttribute("href", /\/contribute\/?$/);
  });
});
