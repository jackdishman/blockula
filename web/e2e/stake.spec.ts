// The Stake page: delegate to a validator's pool, unstake, withdraw once unbonded, all signed
// by the passkey wallet and checked by the mock node; and a node without staking.
import { formatFang, parseFang, type Tx } from "@blockula/sdk";
import { VALIDATOR } from "./mock-node";
import { createWallet, cspViolations, expect, test, watchCsp } from "./helpers";

const actionOf = (tx: Tx | undefined) => tx?.action as Record<string, unknown> | undefined;

test("delegates, unstakes and withdraws", async ({ page, node }) => {
  await watchCsp(page);
  const { account } = await createWallet(page);
  await page.getByRole("link", { name: "Stake", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Stake" })).toBeVisible();
  await expect(page.getByTestId("staked")).toHaveText("0 FANG");
  await expect(page.getByText("You haven't staked yet.")).toBeVisible();

  // Delegate 25 FANG to the first validator.
  await page.getByLabel("Validator").selectOption(VALIDATOR);
  await page.getByLabel("Amount to delegate").fill("25");
  await page.getByRole("button", { name: /^Delegate 25 FANG to/ }).click();
  await expect(page.getByRole("status")).toContainText("delegated 25 FANG");
  expect(actionOf(node.submitted.at(-1))).toEqual({ type: "delegate", validator: VALIDATOR, amount: parseFang("25").toString() });
  expect(node.submitted.at(-1)?.from).toBe(account);
  await expect(page.getByTestId("staked")).toHaveText("25 FANG");
  await expect(page.getByTestId("pool")).toContainText("25 FANG");
  await expect(page.getByTestId("pool")).toContainText("5% commission");

  // A bad amount is refused before anything is signed.
  await page.getByLabel("Validator").selectOption(VALIDATOR);
  await page.getByLabel("Amount to delegate").fill("1000");
  await expect(page.getByText(/You only have/)).toBeVisible();
  await expect(page.getByRole("button", { name: /^Delegate/ })).toBeDisabled();
  await page.getByLabel("Amount to delegate").fill("");

  // Unstake 10 of it: signed as 10 FANG, which the chain converts to pool shares at its price.
  const submitted = node.submitted.length;
  await page.getByTestId("pool").getByRole("button", { name: "Unstake" }).click();
  await page.getByLabel("Amount to unstake").fill("10");
  await page.getByRole("button", { name: "Unstake 10 FANG" }).click();
  await expect(page.getByRole("status")).toContainText("started unbonding 10 FANG");
  expect(node.submitted.length).toBe(submitted + 1);
  expect(actionOf(node.submitted.at(-1))).toEqual({ type: "undelegate", validator: VALIDATOR, shares: "0", amount: parseFang("10").toString() });
  await expect(page.getByTestId("staked")).toHaveText("15 FANG");
  await expect(page.getByText("Unbonding", { exact: true })).toBeVisible();
  await expect(page.getByText(/ready at block #/)).toBeVisible();

  // Unstake the rest ("all"): every share held.
  await page.getByTestId("pool").getByRole("button", { name: "Unstake" }).click();
  await page.getByRole("button", { name: "Unstake all" }).click();
  await expect(page.getByText("You haven't staked yet.")).toBeVisible();
  expect(actionOf(node.submitted.at(-1))).toEqual({ type: "undelegate", validator: VALIDATOR, shares: parseFang("15").toString() });

  // Once the unbonding period is over, withdraw pays it back.
  node.height += 50;
  await expect(page.getByText("ready to withdraw").first()).toBeVisible();
  const before = node.acct(account).balance;
  await page.getByRole("button", { name: /^Withdraw 25 FANG/ }).click();
  await expect(page.getByRole("status")).toContainText("withdrew their unbonded stake");
  expect(actionOf(node.submitted.at(-1))).toEqual({ type: "withdraw_unbonded" });
  expect(formatFang(node.acct(account).balance - before + BigInt(node.submitted.at(-1)?.fee ?? 0))).toBe("25 FANG");
  expect(await cspViolations(page)).toEqual([]);
});

// Audit gap-7-5: the wallet used to turn "unstake 10" into shares at the value the node reported,
// so a node that said a 25 FANG stake was worth 10 FANG and a drop got 99.99% of it unbonded, and
// one reporting 100× the shares at the true value got all of it unbonded. The amount is signed as
// an amount now, and the (mock) chain converts it at the pool's real price.
test("a node lying about the stake can't turn a partial unstake into all of it", async ({ page, node }) => {
  await watchCsp(page);
  const { account } = await createWallet(page);
  await page.getByRole("link", { name: "Stake", exact: true }).click();
  await page.getByLabel("Validator").selectOption(VALIDATOR);
  await page.getByLabel("Amount to delegate").fill("25");
  await page.getByRole("button", { name: /^Delegate 25 FANG to/ }).click();
  await expect(page.getByTestId("pool")).toContainText("25 FANG");
  const held = node.shares.get(account)?.get(VALIDATOR) as bigint;
  const lies: [string, (t: { shares: bigint; value: bigint }) => { shares: bigint; value: bigint }, string][] = [
    ["worth the amount and a drop", (t) => ({ shares: t.shares, value: parseFang("10") + 1n }), "10"],
    ["100× the shares at the true value", (t) => ({ shares: t.shares * 100n, value: t.value }), "5"],
  ];
  let unbonded = 0n;
  for (const [lie, report, amount] of lies) {
    node.reportStake = report;
    await page.reload();
    await page.getByRole("button", { name: "Unlock with passkey" }).click();
    await page.getByTestId("pool").getByRole("button", { name: "Unstake" }).click();
    await page.getByLabel("Amount to unstake").fill(amount);
    await page.getByRole("button", { name: `Unstake ${amount} FANG` }).click();
    await expect(page.getByRole("status")).toContainText(`started unbonding ${amount} FANG`);
    expect(actionOf(node.submitted.at(-1)), lie).toEqual({ type: "undelegate", validator: VALIDATOR, shares: "0", amount: parseFang(amount).toString() });
    unbonded += parseFang(amount);
    // What the chain unbonded is the amount asked for, at the pool's real price.
    expect((node.unbonding.get(account) ?? []).reduce((t, u) => t + u.amount, 0n), lie).toBe(unbonded);
  }
  node.reportStake = null;
  expect(node.shares.get(account)?.get(VALIDATOR), "10 of the 25 FANG is still staked").toBe(held - (unbonded * held) / parseFang("25"));
  await page.reload();
  await page.getByRole("button", { name: "Unlock with passkey" }).click();
  await expect(page.getByTestId("staked")).toHaveText("10 FANG");
  expect(await cspViolations(page)).toEqual([]);
});

test("explains when the network has no staking yet", async ({ page, node }) => {
  node.staking = false;
  await createWallet(page);
  await page.getByRole("link", { name: "Stake", exact: true }).click();
  await expect(page.getByText("Staking isn't active on this network yet.")).toBeVisible();
});
