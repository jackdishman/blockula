# FANG tokenomics (mainnet design)

> **Status: decided design, not final.** This is DEC-5 in [MAINNET-DESIGN.md](MAINNET-DESIGN.md), written out. It becomes final only after written opinions from securities and tax counsel (MAINNET.md LEG-1), before the genesis ceremony. Nothing here is legal, tax or investment advice, and nothing here is an offer: FANG is not sold.
>
> **Testnets:** testnet FANG, Hunts and Soul have no monetary value and come with no promise of any mainnet allocation. Mainnet (`blockula-1`) is a new chain, and no testnet balance carries over.

FANG is the native token of Blockula. It pays transaction fees, bonds validators and delegations, is locked to vote in governance, and registers `.boo` names. 1 FANG = 10⁹ drops. All on-chain amounts are integers in drops, and every formula below is integer math.

## At a glance

| | |
|---|---|
| Genesis supply | **1,000,000,000 FANG** (10¹⁸ drops) |
| Sale | None. No presale, no investors, no exchange or market-maker allocation |
| Held by keyless system accounts at genesis | 87% (870M FANG), moved only by protocol rules or governance |
| Founder | 10% (100M), vesting on-chain. There's no foundation allocation: governance may fund a foundation from the treasury once one exists |
| Issuance | 6% a year, falling 15% a year to a 1.5% floor (reached in year 10). 70% to staked validator pools, 20% to the contribution pool, 10% to the treasury |
| Fees | The base fee (per byte) is burned. The tip (at most 20× the base fee, from transferable FANG) goes to the block proposer |
| Admin keys | None. Every protocol lever is a governance proposal |

## 1. Genesis allocation

| Bucket | % | FANG | Held by | Release (enforced on-chain) | Who moves it | Votes? |
|---|---|---|---|---|---|---|
| Community distributions | 32% | 320,000,000 | Keyless system account | Nothing at genesis. Who receives distributions, when and on what criteria is decided and announced only after mainnet is running (how the first one executes is open, section 10); later ones need a governance proposal | Governance | No |
| Community treasury | 30% | 300,000,000 | Keyless `treasury` account | Unlocks linearly over 4 years (75M a year). Holds the 5% once planned for a foundation, which governance may fund from here (section 10). Also receives 10% of issuance, name fees, and the bootstrap delegation as it unwinds | Governance only (`TreasurySpend`; large spends need ⅔) | No |
| Contribution endowment | 15% | 150,000,000 | Keyless contribution pool | Released into the pool's spendable balance linearly over 10 years (15M a year), on top of 20% of issuance | Séance juries (each Séance draws at most 25% of the pool) | No |
| Validator bootstrap delegation | 10% | 100,000,000 | Keyless `bootstrap` account | Delegated by formula at every epoch boundary: equal shares to every member of the next active set (so not jailed) not run by the founder, rebalanced each epoch. Returns to the treasury linearly over years 2–4. Its staking rewards go to the treasury each epoch; operators keep their commission on it, at most `bootstrap_commission_cap_bps` (10%; a validator charging more pays the rest of it to the treasury). Its stake doesn't count toward a validator's rank for a seat, or toward the governance quorum base | The protocol formula (`staking::bootstrap_epoch`) | No |
| Founder | 10% | 100,000,000 | The founder's own account | 1-year cliff, then linear to 4 years from genesis (section 7) | The founder, within the lock | Vested tokens only, and a pledge not to vote while vesting runs |
| Genesis operators and security grants | 3% | 30,000,000 | Each recipient's own account, under a signed service agreement (genesis operators, auditors who accept FANG) | 6-month cliff, then linear to 2 years from genesis | The recipient, within the lock | Vested tokens only |
| **Total** | **100%** | **1,000,000,000** | | | | |

The bootstrap formula runs at every epoch boundary, after the next set is chosen: (1) what its stake earned since the last boundary goes to the treasury (it sells shares worth the gain at once; a slash is booked as a loss), and so does whatever commission on it a validator charged above `bootstrap_commission_cap_bps` (10%), paid when the reward is (audit F12: a 100%-commission operator can't take the treasury's yield); (2) it keeps what its lock (below) still holds and returns the rest to the treasury; (3) it splits what it keeps equally over the next set's members whose owner isn't one of the founder's accounts (`bootstrap_excluded` in the genesis: the founder allocation's recipient and the owners of the founder's seats), leaving every other pool at once and moving each delegation to its share (one within 1 FANG of its share is left alone). The protocol moves this stake without unbonding, only at boundaries and only by the formula. It follows the set rather than choosing it: its stake isn't counted when validators are ranked for seats, so it doesn't entrench the genesis operators. A genesis that gave the bootstrap account no lock would send it all to the treasury at the first boundary, so the bucket can't be stranded (audit F12).

### Lock curves

Locks are measured in blocks, not wall-clock time (1 s blocks; a year is 31,557,600 blocks, i.e. 365.25 days). If blocks run slower than planned, vesting takes longer, which is the safe direction.

| Bucket | Start | Cliff | End |
|---|---|---|---|
| Founder | 0 | 31,557,600 (1 year) | 126,230,400 (4 years) |
| Operators and security grants | 0 | 15,778,800 (6 months) | 63,115,200 (2 years) |
| Treasury | 0 | none | 126,230,400 (4 years) |
| Contribution endowment | 0 | none | 315,576,000 (10 years) |
| Bootstrap return to the treasury | 31,557,600 | none | 126,230,400 |

The treasury's, the endowment's and the bootstrap delegation's curves are vesting locks on their keyless accounts: a treasury spend can't exceed what has unlocked (inflows such as issuance and name fees are free at once), at every epoch boundary whatever of the endowment has vested moves into the contribution pool, and whatever of the bootstrap delegation has "vested" goes back to the treasury (its formula keeps the rest delegated).

### Released by the schedule (end of each year, FANG)

Distributions from the 32% bucket aren't on a schedule, so they aren't shown. Treasury funds count as released when they become spendable by governance, not when they're spent.

| End of year | Founder | Operators, security | Treasury | Endowment to the pool | Bootstrap back to the treasury |
|---|---|---|---|---|---|
| 1 | 25M | 15M | 75M | 15M | 0 |
| 2 | 50M | 30M | 150M | 30M | 33.3M |
| 3 | 75M | 30M | 225M | 45M | 66.7M |
| 4 | 100M | 30M | 300M | 60M | 100M |
| 10 | 100M | 30M | 300M | 150M | 100M |

The genesis manifest publishes every bucket's address and lock curve next to the genesis hash, and the explorer will show circulating, locked and keyless supply live.

## 2. Issuance

New FANG is minted at every epoch boundary (an epoch is 1 hour, 3,600 blocks) and split three ways.

- **Year length:** an issuance year is 8,766 epochs (365.25 days at 1 s blocks). Year *y* covers epochs 8,766·(*y*−1) to 8,766·*y* − 1.
- **Rate:** `rate_ppm(1) = 60_000` (6%), then `rate_ppm(y+1) = max(15_000, rate_ppm(y) × 85 / 100)`, rounded down. The floor, 15,000 ppm (1.5%), is reached in year 10 and stays.
- **Per epoch:** `mint = S(y) × rate_ppm(y) / (1_000_000 × 8_766)`, in u128 and rounded down, where `S(y)` is total supply snapshotted when year *y*'s first epoch ends, before that epoch's mint (without burns, the supply at the year's first block). Using the year-start snapshot keeps each year's issuance at exactly the stated rate; minting on the current supply every epoch would compound 6% into 6.18%.
- **Split:** validators `mint × 7_000 / 10_000`, the contribution pool `mint × 2_000 / 10_000`, the treasury the remainder (10% plus rounding dust, so the parts sum exactly). The validators' share goes to active validators' pools in proportion to stake; jailed and tombstoned validators get none.
- **Insiders get none of it.** No part of issuance or fees goes to the founder, the foundation or any named person.

The rates and the split are governance parameters, bounded in code (a floor of at least 0.5%, at least 50% to validators). A new rate, decay or floor applies from the next issuance year, a new split from the next epoch. Changing them takes a ⅔ token supermajority, so a low-turnout vote can't switch off security, and the bootstrap council can't change them at all: it's the validators, and issuance (like slashing, jail, downtime, commission bounds and the tip cap) is their own pay (GOVERNANCE.md).

### The first ten years

Computed exactly with the formula above, ignoring burns. Burns (fees, account creation, slashing, forfeited deposits) make real supply lower, and later years then mint slightly less.

| Year | Rate (ppm) | Mint per epoch (FANG) | Minted in the year (FANG) | Supply at year end (FANG) |
|---|---|---|---|---|
| 1 | 60,000 | 6,844.626967830 | 59,999,999.999997780 | 1,059,999,999.999997780 |
| 2 | 51,000 | 6,167.008898015 | 54,059,999.999999490 | 1,114,059,999.999997270 |
| 3 | 43,350 | 5,509.297399041 | 48,294,500.999993406 | 1,162,354,500.999990676 |
| 4 | 36,847 | 4,885.840326071 | 42,829,276.298338386 | 1,205,183,777.298329062 |
| 5 | 31,319 | 4,305.857942186 | 37,745,150.721202476 | 1,242,928,928.019531538 |
| 6 | 26,621 | 3,774.584872553 | 33,088,010.992799598 | 1,276,016,939.012331136 |
| 7 | 22,627 | 3,293.684152296 | 28,872,435.279026736 | 1,304,889,374.291357872 |
| 8 | 19,232 | 2,862.837376953 | 25,095,632.446369998 | 1,329,985,006.737727870 |
| 9 | 16,347 | 2,480.180801407 | 21,741,264.905133762 | 1,351,726,271.642861632 |
| 10 | 15,000 | 2,313.015523002 | 20,275,894.074635532 | 1,372,002,165.717497164 |

Year 1 per epoch: 4,791.238877481 FANG to validator pools, 1,368.925393566 to the contribution pool, 684.462696783 to the treasury. After year 10, supply grows 1.5% a year: about 1.59B FANG at year 20 and 2.49B at year 50 without burns. Supply is a u64 of drops today, which holds about 18.4B FANG; at this schedule with no burns that limit is passed in year 185.

## 3. Fees, burns and names

- **Fees.** Each transaction names the most it will pay (`fee`). It's charged `base_fee × size + tip`. The base part is burned and the tip goes to the block proposer, and proposals order transactions by tip per byte.
- **Base fee.** Priced per byte, because post-quantum signatures make a transfer ~5.4 KB. It moves with block fullness: the target is 50% of the 2 MiB block limit, a full block raises it 12.5% and an empty one lowers it 12.5%, and it never drops below today's minimum (10,000 drops + 10 drops per byte, about 0.000064 FANG for a transfer). Each block's base fee (and its size limit) is fixed 17 blocks ahead, so every proposer knows it exactly. Wallets read the upcoming ones at `/api/fees` and set `fee` to the highest; only the block's own base fee and the tip are charged.
- **Why burn it.** Burning prices block space: spam costs the spammer, and a proposer can't fill its own blocks for free. It isn't a mechanism to manage the token's price.
- **Account creation.** Opening an account burns 0.1 FANG (governance can change it), paid by whoever opens it on top of what they send. Every other record on chain is already bonded.
- **Names.** Registration costs depend on length: 3 characters 100 FANG, 4 characters 25 FANG, 5 or more 5 FANG, paid to the treasury. A registration lasts a year; renewal costs 20% of the price per year (at most 10 years paid ahead). An expired name stops resolving; for 30 days only its owner can renew it, then anyone can register it. Names can be transferred and released. A reserved list in genesis is assignable only by governance.
- **Other burns.** A proposal deposit is burned if the proposal doesn't reach quorum. A slash pays the reporter a share and burns the rest.

## 4. Staking

- **Self-custodial delegation.** Anyone can delegate FANG to a validator's pool. Delegated FANG stays under the delegator's control: only the protocol's rules move it, and nobody takes custody. Pools account in shares, so rewards compound into every delegator's position and a slash scales everyone alike. A validator's commission is minted to its owner as shares.
- **Validators.** Registration is permissionless with a minimum self-bond. At each epoch boundary the next active set is the top 31 candidates by pool stake, with a limit on how many seats change per epoch. Votes inside the set are equal-weight.
- **Unbonding** takes 21 days, and unbonding stake stays slashable for anything done before it started.
- **Slashing.** Double-signing costs the pool 5% and removes the validator for good (tombstoning); the reporter gets a share of the slash and the rest is burned. Downtime jails the validator with a 0.1% slash; it may unjail after a waiting period.
- **Not at genesis:** no liquid staking token, and no custodial staking service run by the founder or a foundation.

Staking rewards are payment for validating and securing the network. They aren't a return on an investment, and this page doesn't quote a rate.

## 5. Governance

- **No admin keys.** Every former admin lever is a proposal: parameter changes, treasury spends, repo registration, bounded Soul grants, sybil flags, claim rejections, clawbacks of contribution grants, refunds of Hunts, upgrades, feature activation (for example Ghost Mode) and text.
- **Process.** A proposal bonds a deposit (refunded once it reaches quorum, burned if it doesn't). Voting lasts 7 days, then a 2-day timelock, then it executes. Upgrades, feature activation, changes to issuance, the validators' other economics and governance's own safety parameters, and large treasury spends need ⅔; everything else a simple majority.
- **Voting weight** is the FANG a voter locks in the vote × a conviction multiplier: 0.1× with no lock, and 1× to 6× for locks of 1, 2, 4, 8, 16 or 32 lock periods. Liquid, staked and unbonding FANG can all be locked. Locks block transfers out until they expire and the voter calls `Unlock`.
- **Unvested tokens can't vote**, nor can what a still-vesting account's stake earned and hasn't sold yet (it vests with the stake). Soul doesn't vote at genesis.
- **Launch safety.** At genesis almost nothing can vote, so quorum has an absolute floor in FANG as well as a share of votable supply, and treasury spends are rejected until about 6 months after genesis. For the first year a **bootstrap council** can also pass a proposal: ⅔ of the active validator set, provided token-weighted Nay doesn't exceed Aye, with the timelock still applying. It can't change governance's safety parameters or the validators' own economics (issuance, slashing, jail, downtime, commission bounds, the bootstrap delegation's commission cap, the tip cap): those are the token holders' alone.
- **Multisig accounts.** Any account can become an M-of-N multisig of its owner keys. A foundation that governance funds should hold its funds in one (at least two signers not the founder).

## 6. Vesting rules

- A vesting lock is a curve (`start`, `cliff`, `end`, `total`). Before the cliff nothing has vested; after it, `vested = total × (h − start) / (end − start)`, capped at `total`. The locked amount at height `h` is `total − vested`.
- Transferable balance = liquid − max(0, largest lock − staked − unbonding). Locked tokens can be staked and delegated, never transferred (nor escrowed, spent on names or given to the pool); fees can be paid from them.
- **Rewards earned by locked stake are locked on the same curve.** Staking rewards paid to an account with a vesting lock join that lock: what's still unvested of them stays locked, so a vesting allocation can't pay liquid income before its cliff. Until they're sold they're counted at cost: they neither cover the lock nor vote. Nor can it pay liquid income to someone else: the commission its stake pays in a pool it doesn't own goes to the treasury, not to the pool's owner, so a 100%-commission validator gains nothing from it.
- **Insider delegation cap.** An account with a vesting lock may delegate at most 1% of supply (10M FANG) to any one pool, so locked insider stake can't take over seats while staking is thin.
- **Insider seats.** The chain also bounds the seats locked stake can buy, at every epoch boundary: an insider's delegation to someone else's pool earns but doesn't count toward that pool's rank, a newcomer validator owned by an account with a vesting lock joins only if that account then holds no other seat, and the founder's validators (`bootstrap_excluded`) join only while they hold at most 2 seats and fewer than f = ⌊(n−1)/3⌋ of the set, so insiders can't halt the chain or veto its council. Seats held already, such as genesis seats, stay.
- Unvested tokens don't vote (section 5).

## 7. The founder's allocation

- **10% of genesis supply, 100,000,000 FANG,** to an account whose address the founder supplies at the genesis ceremony. It's published in the genesis manifest with its lock curve.
- **Vesting on-chain:** nothing for the first year; 25M at the 1-year cliff; then linear until 4 years after genesis. The protocol enforces it; nobody can speed it up.
- **While locked it can be staked and delegated,** within the 1% per-pool cap, and its rewards are locked on the same curve. It can't be transferred and can't vote.
- **Share over time.** The founder holds 10% of supply at genesis, and there's no foundation allocation beside it: under the 20% line the CLARITY Act drew for "mature" networks (section 9). Issuance dilutes it: without staking, the founder's 100M is about 8.3% of supply after 4 years.
- **Pledges** (see "What the founder must not do"): no votes while vesting runs, even with vested tokens; no contribution-pool claims, jury seats or self-awarded Hunts while vesting runs; Soul set to 0 in the genesis ledger; any sale of vested tokens only under a pre-announced plan.

## 8. Deliberately absent

- **No sale** of FANG or anything that converts into it: no presale, SAFT, node or validator licenses, redeemable NFTs, paid allowlists or points.
- **No investors** and no private rounds.
- **No allocation to exchanges or market makers,** and no arranged listings.
- **No admin keys,** no faucet and no emergency or upgrade key on mainnet.
- **No share of issuance or fees** for the founder, the foundation or any named person, now or by later proposal.
- **No promised airdrop.** Testnet activity earns no claim to any mainnet allocation.

## 9. Legal posture

> **Not legal advice; pending counsel.** This summarizes engineering-side research (as of 2026-10-14) to prepare the brief for counsel, [COUNSEL_BRIEF.md](COUNSEL_BRIEF.md). Every conclusion here needs a written opinion before genesis.

**The aim** is a permissionless protocol whose token is the native asset of a working network: never sold, with no promises of managerial efforts, and no central party.

**United States.**
- **SEC interpretive release, 2026-03-17** (Release No. 33-11412, joined by the CFTC). It sorts crypto assets into five categories; only "digital securities" are securities. A "digital commodity" derives its value from the programmatic operation of a crypto system that is functional and decentralized, not from the managerial efforts of others. Its examples include BTC, ETH and SOL. Staking, governance votes, gas fees and protocol issuance are described as normal features of one.
  - An investment contract forms when an issuer induces an investment of money with promises of essential managerial efforts. The release names a business plan with milestones, timeline, personnel, funding and how holders will profit as the riskiest kind of promise. Statements by affiliates and agents count.
  - A "central party" is anyone with operational, economic or voting control.
  - Self-custodial staking, including delegation to an operator, isn't a securities transaction.
  - A free airdrop is covered when it's announced after the network is functional, for past use of a testing environment. It isn't covered when it's announced during testing to drive engagement, or when tokens are given for services (fixing bugs is the release's example). So Hunts and pool payouts are best read as compensation for work, outside the airdrop guidance.
- **Regulation Crypto Assets** (Release 33-11434), proposed 2026-08-18, comments due 2026-10-20: exemptions for raises and a safe harbor for projects that have finished their promised efforts. It gives no guidance for projects that never sell. Its disclosure list (supply, lockups, release schedules, related-person holdings, mint and burn) is what this page publishes.
- **Congress.** The GENIUS Act (payment stablecoins, signed 2025-07-18) doesn't apply to FANG. The CLARITY Act passed the House on 2025-07-17, but Senate cloture failed on 2026-09-15; it must be reintroduced in the next Congress (from January 2027). Its "mature blockchain" test kept affiliated holders under 20% of supply; this design keeps the founder at 10%, with no foundation allocation.
- **Uncertainty.** The release is guidance: a later Commission can withdraw it, courts aren't bound by it, and states and private plaintiffs can still sue. The 2026-11-03 midterms and a 2029 administration change are live risks.
- **Tax** is separate. Staking rewards are income when received (Rev. Rul. 2023-14), as are airdrops (Rev. Rul. 2019-24). How the founder's locked allocation is taxed is a question for tax counsel.

**European Union (MiCA, Regulation 2023/1114, Title II applies from 2024-12-30).** The white-paper rules for offers to the public don't apply to crypto-assets offered for free or created automatically as validation rewards (Art. 4(3)). That exemption falls away if the offeror announces an intention to seek admission to trading (Art. 4(4)). An exchange that lists a token on its own initiative must write the white paper itself (Art. 5(2)). Open questions for EU counsel: whether Hunts and pool payouts (given for work) count as "free", and whether linking a GitHub handle is personal data given in exchange.

**Where the real exposure is.** Not the 10% allocation, but operational control: today one person maintains the code, merges the PRs that earn pool rewards, publishes releases, and runs the wallet domain, the RPC and the testnet validators. That has to be handed over before genesis: more maintainers with merge rights, more wallet frontends and RPCs, the domain owned by an entity, and at most 2 validators of the founder's.

### What the founder must not do

1. **Sell FANG, or anything that converts into it,** before or at launch: no presale, SAFT, node or validator license, redeemable NFT, paid allowlist, or points sold for money.
2. **Promise returns or price:** no "APY", "yield", "deflationary", price targets, buybacks or "we'll get listed", and no roadmap that bundles milestones, funding and how holders profit.
3. **Arrange or pay for exchange listings or market makers,** or lend them tokens.
4. **Keep any admin, upgrade, veto or emergency key** over the protocol, or run more than a quarter of validators through the foundation.
5. **Announce an airdrop, its criteria or "testnet points"** before mainnet is functional, or describe testnet FANG or Soul as convertible.
6. **Take any slice of issuance or fees** for himself or the foundation, now or by later proposal.
7. **Vote while vesting runs,** even with vested tokens, or vote on proposals that pay himself or his company.
8. **Claim from the contribution pool, sit on juries, or award himself Hunts** while vesting runs.
9. **Sell vested tokens without a pre-announced plan.** Once vested, sell only under a published schedule or cap, with disclosure, never around announcements.
10. **Call FANG an investment,** or the project a company whose success holders share.
11. **Stay the single point of operation:** one maintainer, one wallet domain, one RPC, one explorer.

## 10. Not decided yet

These are open in the design and are settled in the governance and economics work before genesis:

- ✅ **How the first distribution is executed (settled).** The 32% bucket is keyless, and at launch almost nothing can vote: insider tokens are locked and the founder doesn't vote. A distribution that waits for an ordinary governance quorum could wait forever, and one that a single party can trigger makes that party the distributor. So the first community distribution executes through the **bootstrap council**: a `Distribution` proposal (Merkle root, total, claim deadline) passes once ⅔ of the active validator set (22–31 independent operators, the founder at most 2 seats) votes for it and token-weighted Nay doesn't exceed Aye. It then waits out the 2-day timelock, and nothing is distributed before `governance_spend_height` (≈ 6 months). Recipients claim with a Merkle proof (`ClaimDistribution`, `blockula gov claim`), and unclaimed amounts return to the community bucket after the deadline. The criteria stay open: they're published with the proposal, never before mainnet works.
- **First FANG for fees.** With no sale and no faucet, liquid FANG at launch is little more than validator commission. How new users pay their first fees (for example treasury-sponsored onboarding transactions, as PLAN.md describes) is open.
- **Governance numbers:** the quorum floor, the length of a conviction lock period, the proposal deposit, and the threshold for a "large" treasury spend.
- ✅ **No foundation allocation (settled 2026-10-18).** The 5% once planned for a foundation's multisig is the treasury's (30%, on the treasury's 4-year curve, which was the foundation's too), so no foundation has to exist at genesis. The founder's company pays for audits, counsel and infrastructure through launch. Once a foundation is formed, governance may fund it with a `TreasurySpend` to its multisig: from `governance_spend_height` (≈ 6 months), up to what the treasury has unlocked (37.5M at 6 months, plus inflows), in tranches if it likes. A spend above `treasury_large_bps` (1% of the treasury) within 30 days needs ⅔, and in the first year the bootstrap council can pass one too. The chain won't treat a foundation funded this way as an insider: `bootstrap_excluded` is fixed in the genesis and a spend arrives liquid, so the bootstrap delegation would back its validators and the insider seat rules wouldn't count them. Its funding proposal should commit it to running no validators of its own, never voting and publishing quarterly reports. Such a grant is a vote on the treasury's balance, not a standing slice of issuance or fees, which stays ruled out (section 8).
- ✅ **The majority for issuance changes (settled):** a ⅔ token supermajority, never the bootstrap council, within the code-enforced bounds above.
- **Clawback scope.** The recommendation is that `Clawback` applies to contribution-pool grants only, never to a genesis allocation.
