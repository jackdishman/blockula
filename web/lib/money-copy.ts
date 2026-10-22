/**
 * The safety lines the wallet shows before money moves: on Send's review, and on the review steps
 * for staking and voting. One place, so counsel's edits land here.
 *
 * DRAFT for counsel review (D4, decided: the legal texts publish as "provisional, under counsel
 * review"): adapted from the legal-safety review's in-product copy (sections 4 to 6). The
 * founder-run line (D3, D8, decided) is true at launch and comes out once outside operators run
 * most of the active set. Plain words, no value or return language, never a rate.
 */

/** Above Send's Confirm button, on every network. */
export const PAYMENTS_FINAL =
  "Payments are final: nobody can reverse them, Blockula included. Anyone who asks you to send FANG to “verify”, “unlock” or “upgrade” your wallet is a scammer.";

/** Before staking (delegate), and moving stake. `unbondingFor` is "21 days" or "the unbonding period". */
export const stakeRisk = (unbondingFor: string) =>
  `You can unstake any time, but it takes ${unbondingFor} to come back. Staked FANG can be slashed if the validator misbehaves, and rewards aren't guaranteed.`;

/** Before unstaking. */
export const unstakeRisk = (unbondingFor: string) =>
  `It stops earning now and comes back in ${unbondingFor}. Until then it can still be slashed for anything the validator did while you were staked with it.`;

/** Before moving stake to another validator. */
export const moveRisk = (unbondingFor: string) =>
  `It moves now, without waiting out the unbonding period. For the next ${unbondingFor} it can still be slashed for anything the validator you left did while you were with it.`;

/** On mainnet, under every staking review. */
export const FOUNDER_RUN = "Most validators are run by Blockula's founder today.";

/** Before voting. `lockedUntil` is "about Nov 14 (block #1,210,601)". */
export const voteTerms = (amount: string, lockedUntil: string, conviction: number) => [
  "Your vote and its amount are public.",
  conviction === 0
    ? `${amount} stays locked until voting ends, ${lockedUntil}. Locked FANG can be staked but not sent.`
    : `${amount} stays locked until ${lockedUntil}, after voting ends. Locked FANG can be staked but not sent.`,
  "You can change your vote while voting is open, but you can't shorten the lock.",
];
