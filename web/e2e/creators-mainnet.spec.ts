// Contribution pages on the mainnet build (product review, wallet-creators; CONTRIB-2): the
// testnet's "no monetary value" line would be false here, so they carry mainnet's note instead
// (lib/creators.ts CONTRIBUTION_NOTES, waiting on decision D7).
import { seedWorld } from "./explorer-world";
import { cspViolations, expect, test, watchCsp } from "./helpers";

test("contribution pages on mainnet carry mainnet's note, not the testnet's", async ({ page, node }) => {
  await watchCsp(page);
  seedWorld(node);
  for (const path of ["/hunt/0", "/claim/0", "/seance/1"]) {
    await page.goto(path);
    const note = page.getByTestId("contribution-note");
    await expect(note, path).toContainText("Blockula runs Hunts and the contribution pool on the testnet for now.");
    await expect(note, path).not.toContainText("Testnet FANG");
  }
  expect(await cspViolations(page)).toEqual([]);
});
