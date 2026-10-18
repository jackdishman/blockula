// Payment links (/receive → /send). Audit main-11-1: a link says which chain it was made for, as
// a check only: a .boo name can be someone else's on another network, so Send refuses a link made
// on another chain (or another instance of this one) and says which one it was made for; links
// made before the check still fill the form, with a notice. Audit main-11-2: an amount too long
// for a QR code used to crash /receive; amounts above a u64 of drops are refused. Audit
// review-r3-wallet-3: the link carries the amount's canonical form, not the typed one. Audit
// review-r3-wallet-4: a link's chain is the sender's text: a malformed one is refused with fixed
// words, and the wallet names only networks it knows.
import { parseFang } from "@blockula/sdk";
import { CHAIN_ID, GENESIS } from "./env";
import { addressOf } from "./mock-node";
import { createWallet, cspViolations, expect, test, watchCsp } from "./helpers";

const ALICE = addressOf("a1".repeat(32));

test("receive makes a link that says its chain, and survives an absurd amount", async ({ page }) => {
  await watchCsp(page);
  const { account } = await createWallet(page);
  await page.getByRole("link", { name: "Receive" }).first().click();
  const link = page.getByTestId("paylink");
  await expect(link).toContainText(`chain=${CHAIN_ID}&genesis=${GENESIS.slice(0, 16)}`);
  await page.getByPlaceholder("Payer chooses").fill("1.5");
  await expect(link).toContainText(`/send?to=${account}&amount=1.5&chain=${CHAIN_ID}`);
  // Zero-padded, it's still 1.5 FANG, and the link says so the short way: it fits a QR code.
  await page.getByPlaceholder("Payer chooses").fill(`${"0".repeat(3000)}1.500`);
  await expect(link).toContainText(`/send?to=${account}&amount=1.5&chain=${CHAIN_ID}`);
  await expect(page.getByRole("img", { name: /^QR code: payment link to/ })).toBeVisible();
  await expect(page.getByTestId("qr-too-long")).toHaveCount(0);
  // Too many digits for any amount (and for a QR code): refused, left out of the link, no crash.
  await page.getByPlaceholder("Payer chooses").fill("9".repeat(3000));
  await expect(page.getByText("That's more FANG than there can ever be.")).toBeVisible();
  await expect(page.getByRole("img", { name: /^QR code: payment link to/ })).toBeVisible();
  await expect(page.getByTestId("qr-too-long")).toHaveCount(0);
  await expect(link).not.toContainText("amount=");
  // One drop above a u64 is refused too; the largest amount there can be is fine.
  await page.getByPlaceholder("Payer chooses").fill("18446744073.709551616");
  await expect(page.getByText("That's more FANG than there can ever be.")).toBeVisible();
  await page.getByPlaceholder("Payer chooses").fill("18446744073.709551615");
  await expect(page.getByText("That's more FANG than there can ever be.")).toHaveCount(0);
  await expect(link).toContainText("amount=18446744073.709551615");
  await expect(page.getByRole("heading", { name: "Receive FANG" })).toBeVisible();
  expect(await cspViolations(page)).toEqual([]);
});

test("send fills a link made for this chain, and refuses one made for another", async ({ page, node }) => {
  await watchCsp(page);
  await createWallet(page);
  node.names.set("alice", ALICE);
  const open = async (query: string) => {
    await page.goto(`/send?${query}`);
    await page.getByRole("button", { name: "Unlock with passkey" }).click();
  };
  const to = page.getByPlaceholder(/^alice\.boo or /);
  const amount = page.getByPlaceholder("0.0");
  const review = page.getByRole("button", { name: "Review" });

  // Made for another chain: nothing is filled in, it can't be reviewed, and it says which chain.
  await open("to=alice.boo&amount=1&memo=hi&chain=blockula-1&genesis=4d4d4d4d4d4d4d4d");
  const refused = page.getByTestId("paylink-refused");
  await expect(refused).toContainText("made for Mainnet, but your wallet is on Testnet");
  // This build's menu has no Mainnet, so it doesn't send the payer looking for one.
  await expect(refused).not.toContainText("switch networks");
  await expect(to).toHaveValue("");
  await expect(amount).toHaveValue("");
  await expect(review).toBeDisabled();
  // A well-formed chain id this wallet doesn't know isn't named: it's the sender's text.
  await open("to=alice.boo&amount=1&chain=Mainnet-upgrade-reimport-your-recovery-kit-at-evil-example");
  await expect(refused).toContainText("made for a network this wallet doesn't know, not Testnet");
  await expect(refused).not.toContainText("evil");
  await expect(to).toHaveValue("");
  await expect(review).toBeDisabled();
  // Not a chain id at all (a bidi override and words): refused with fixed text, never echoed.
  const bidi = encodeURIComponent("\u202eMainnet-upgrade: re-import your recovery kit at evil.example");
  await open(`to=alice.boo&amount=1&memo=hi&chain=${bidi}`);
  await expect(refused).toContainText("names its network in a form no Blockula network uses");
  await expect(refused).not.toContainText("evil");
  await expect(refused).not.toContainText("\u202e");
  await expect(to).toHaveValue("");
  await expect(review).toBeDisabled();
  // Another instance of this chain id (a reset testnet).
  await open(`to=alice.boo&amount=1&chain=${CHAIN_ID}&genesis=0000000000000000`);
  await expect(refused).toContainText("made for another instance of Testnet (genesis 0000000000000000…)");
  await expect(to).toHaveValue("");
  await expect(review).toBeDisabled();
  // A genesis that isn't one this wallet writes (too short to tell chains apart, or not hex) is malformed.
  for (const genesis of [GENESIS.slice(0, 2), encodeURIComponent("\u202eevil.example 6e6e")]) {
    await open(`to=alice.boo&amount=1&chain=${CHAIN_ID}&genesis=${genesis}`);
    await expect(refused).toContainText("names its network in a form no Blockula network uses");
    await expect(refused).not.toContainText("evil");
    await expect(to).toHaveValue("");
    await expect(review).toBeDisabled();
  }
  // Typing a payment yourself starts afresh.
  await page.getByRole("link", { name: "Start a new payment instead" }).click();
  await expect(page.getByTestId("paylink-refused")).toHaveCount(0);
  await to.fill("alice.boo");
  await amount.fill("0.5");
  await expect(review).toBeEnabled();

  // Made for this chain: filled in, says so, and pays the name's owner here.
  await open(`to=alice.boo&amount=1.5&memo=pumpkins&chain=${CHAIN_ID}&genesis=${GENESIS.slice(0, 16)}`);
  await expect(page.getByTestId("paylink-filled")).toContainText("made for Testnet");
  await expect(to).toHaveValue("alice.boo");
  await review.click();
  await page.getByRole("button", { name: "Confirm & send" }).click();
  await expect(page.getByRole("heading", { name: "Sent" })).toBeVisible();
  // Signed as the name (audit gap-7-1, no witnesses here): the chain pays its owner on this chain.
  expect(node.submitted.at(-1)?.action).toEqual({ type: "transfer", to: "alice.boo", amount: parseFang("1.5").toString() });
  expect(node.accounts.get(ALICE)?.balance).toBe(parseFang("1.5"));
  expect(node.submitted.at(-1)?.memo).toBe("pumpkins");

  // A link from before the check: still filled in, with a notice that it doesn't say its network.
  await open("to=alice.boo&amount=1");
  await expect(page.getByTestId("paylink-filled")).toContainText("doesn't say which network it was made for");
  await expect(to).toHaveValue("alice.boo");
  await expect(review).toBeEnabled();
  expect(await cspViolations(page)).toEqual([]);
});
