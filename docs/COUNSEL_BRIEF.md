# Brief for securities and tax counsel

Prepared 2026-10-14 by the engineering side, for the legal track in [MAINNET.md](MAINNET.md) (LEG-1 to LEG-3). It states facts about the protocol and the plan, and lists the questions we need answered in writing before the genesis ceremony. It isn't legal advice and draws no legal conclusions of its own; where it repeats an interpretation from our research, it says so.

**This file is in a public repository.** It's written to be read by anyone. Anything privileged or personal (the founder's residence and tax position, draft entity documents, engagement terms) goes to counsel directly, not here.

Companion documents: [TOKENOMICS.md](TOKENOMICS.md) (supply, allocation, issuance, legal posture), [MAINNET-DESIGN.md](MAINNET-DESIGN.md) (decisions DEC-1 to DEC-14 and the protocol spec), [MAINNET.md](MAINNET.md) (launch blockers), [PLAN.md](PLAN.md) (Hunts, Proof of Contribution, Familiars).

## 1. What we're asking for

- **US securities counsel:** an opinion on the status of FANG at launch and on the distribution plan (sections 4–8), and a communications policy.
- **US tax counsel:** the founder's allocation, staking rewards on locked tokens, Hunts and contribution-pool payouts, and the entities.
- **EU (MiCA) counsel,** if the project markets to the EU or has an EU entity: the Art. 4(3) exemptions, Ghost Mode under AMLR, and GDPR for on-chain identifiers.
- **Foundation-jurisdiction counsel** (Cayman, or US if onshore): formation, the Cayman VASP Act, and director independence.

**Timing.** The earliest realistic genesis is Q3 2027 (MAINNET.md "Verdict"). Opinions must be final before the genesis manifest is frozen (MAINNET.md phase 4). One item is time-sensitive: comments on the SEC's proposed Regulation Crypto Assets close **2026-10-20** (section 11, item 5).

## 2. The protocol

- **What it is.** Blockula is a layer-1 blockchain that uses post-quantum signatures (ML-DSA-65, FIPS 204). It has passkey wallets, human-readable `.boo` names, and an SDK and MCP server so AI coding agents can use it. The code is MIT-licensed and public.
- **Today.** A public testnet (v0.4.x) with no mainnet, no audit and no token of value. Testnet FANG comes free from a faucet. The project's docs say testnet FANG, Hunts and Soul have no monetary value and come with no promise of any mainnet allocation (README, CONTRIBUTING, AGENTS, SECURITY).
- **Mainnet design** (MAINNET-DESIGN.md):
  - Mainnet is a **new chain** (`blockula-1`), and no testnet balance carries over.
  - Everything that can be made safe is on at genesis: transfers, names, accounts and session keys, Hunts, the contribution pool, permissionless staking with slashing, on-chain governance, the fee market and issuance (DEC-1). Ghost Mode ships disabled (section 8).
  - **Consensus:** Commonware simplex BFT. Up to 31 validators chosen by stake, one vote each. Genesis needs at least 22 independent operators, with 28–31 targeted (DEC-3).
  - **No admin keys and no faucet** on mainnet, enforced by the genesis builder (DEC-10). Every former admin power is a governance proposal with a timelock.
  - **Upgrades** only by governance-scheduled activation heights. Nodes that don't support a scheduled version halt rather than diverge.

## 3. The founder's role

**Today** the founder is the repository owner and the only maintainer. He:
- merges every PR, and so gates which PRs can earn testnet Hunts and pool rewards;
- controls the GitHub repo and its releases, the npm packages, and the wallet domain (`blockula.app`, registered in his own name; the testnet wallet is `testnet.blockula.app`, mainnet's will be `wallet.blockula.app`);
- runs all four testnet validators, the public RPC and the explorer in one Fly.io organization, and generated every testnet key on one laptop;
- holds no admin key on the current testnet, `blockula-testnet-2` (launched 2026-10-18): its genesis lists none, and validators join on stake with nobody's approval. The levers the earlier testnet's single admin key ("Season 0") pulled, granting Soul, setting repo weights, flagging sybils, rejecting claims and clawing back rewards, are governance proposals there, and the testnet's validators are its bootstrap council. While the founder's four validators are at least ⅔ of the active set, those proposals pass on the founder's votes alone unless token-weighted nays outweigh the ayes.

**Funding.** The project is self-funded. To our knowledge there are no investors, no token sale or SAFT, no side letters and no promises of tokens to anyone. The founder must confirm this to counsel directly.

**Planned before genesis** (TOKENOMICS.md section 9):
- More maintainers with merge rights.
- A second wallet frontend or mirror with reproducible builds.
- Independent RPCs.
- The wallet domain owned by an entity (DEC-8).
- The founder's validators limited to 2 seats at genesis and 1 later (DEC-14).

**After genesis** the founder holds 10% of supply, vesting on-chain, and has pledged not to vote while it vests, not to claim from the pool, sit on juries or award himself Hunts, and to sell only under a pre-announced plan.

## 4. Allocation and distribution

Genesis supply is 1,000,000,000 FANG. Nothing is sold (TOKENOMICS.md section 1).

| Bucket | % | Holder | Release |
|---|---|---|---|
| Community distributions | 32% | Keyless system account | Nothing at genesis. Recipients, timing and criteria are decided only after mainnet is running |
| Community treasury | 30% | Keyless; governance spends only. Holds the 5% once planned for a foundation | Linear over 4 years |
| Contribution endowment | 15% | Keyless contribution pool | Linear over 10 years into the pool |
| Validator bootstrap delegation | 10% | Keyless; delegated by formula to validators not run by the founder | Returns to the treasury over years 2–4 |
| Founder | 10% | Founder's account | 1-year cliff, 4-year linear vest, on-chain |
| Genesis operators and security grants | 3% | Recipients, under signed service agreements | 6-month cliff, 2-year vest |

- **Issuance:** 6% a year, decaying 15% a year to a 1.5% floor. 70% goes to staked validator pools, 20% to the contribution pool and 10% to the treasury. None goes to the founder, the foundation or any named person.
- **Fees:** the per-byte base fee is burned and the tip goes to the block proposer. Creating an account burns 0.1 FANG.
- **Votes:** unvested tokens can't vote, and the founder pledges not to vote while vesting runs. There's no foundation allocation at genesis (DEC-5, 2026-10-18).
- **The 3% bucket** pays genesis validator operators (through the incentivized testnet before genesis) and security work such as audits, under individual signed agreements. It's compensation for services, not a general testnet reward.
- **No distribution has been announced.** There are no criteria and no "points". The public docs say nothing beyond the no-value disclaimer.

## 5. Hunts and the contribution pool (pay for work)

**Hunts** are FANG bounties on GitHub issues.
- The creator escrows the bounty on-chain and anyone can add to it.
- Only the creator can award it. Governance can only refund its funders.
- An award requires a PR merged into the repo's default branch, and pays the account the PR's author linked.
- Governance can refund a Hunt's funders, but nobody can redirect its escrow.
- **The payer is the Hunt's creator and funders,** not the protocol or any entity.

**The contribution pool** pays for merged PRs to registered repos (Blockula's own is the first).
- **Funding:** 20% of issuance plus the 15% endowment. The pool is a keyless account.
- **Claims:** a contributor links a GitHub handle (a public gist containing their address, recorded on-chain), then claims a merged PR with a small bond. Newcomers need a vouch from someone with Soul.
- **Checks:** validators check the merge on GitHub and attest it on-chain.
- **Judging:** after a ~30-day revert window, a Séance judges matured claims in pairs. A Séance is a jury of humans with Soul, drawn at random (on mainnet from a validator randomness beacon). Jurors receive 10% of each Séance's payout.
- **Payouts:** per person, one win rate pooled over all their claims in the Séance, the first claim in full at that rate and each further one a quarter of the one before, vesting over ~90 days, with clawback if the PR is reverted.
- **Who pays:** the payer is the protocol, by rule. There's no entity, but the maintainers who merge PRs effectively decide what's eligible.
- **Familiars:** AI agents contribute as session keys of a human sponsor. Their rewards and accountability go to the sponsor.
- **Soul** is non-transferable reputation earned from jury outcomes, and it decays. It gates jury duty and boosts payouts up to 1.5×. On mainnet a published ledger in genesis bootstraps it, and the rule that derives that ledger isn't final.
- **Our research reads this as compensation for services.** The SEC's 2026 release (p. 59) names fixing bugs as a service, so these payouts fall outside its airdrop interpretation, and the recipients face tax reporting, sanctions screening and contractor terms (LEG-2). The protocol can't screen recipients; frontends and the CLI could.

## 6. Staking

- **Delegation stays with the delegator.** Delegators keep their FANG and keys; only the protocol's rules move it, and nobody takes custody. Pools account in shares, and a validator's commission is minted to its owner as shares.
- **Unbonding** takes 21 days. **Slashing:** 5% and permanent removal for double-signing, with a share paid to the reporter and the rest burned. Downtime means jail and a 0.1% slash.
- **Not at genesis:** no liquid staking token, and no custodial staking by the founder or a foundation. Docs and frontends must not market staking as a return.
- **Locked tokens:** rewards earned on locked tokens are locked on the same curve. Accounts with vesting locks may delegate at most 1% of supply to any one pool.
- **Bootstrap delegation:** the 10% bucket is delegated by a fixed formula from a keyless account, in equal shares to every active validator not run by the founder, rebalanced each epoch. Its rewards go to the treasury, and it returns to the treasury over years 2–4. The hosting policy (caps per provider, country and facility, DEC-14) is enforced on the genesis set; the protocol can't see where later validators run.

## 7. Governance

- **Conviction voting:** weight = FANG locked in the vote × a multiplier for lock length (0.1× unlocked, 1× to 6×).
- **Process:** 7 days of voting, a 2-day timelock, then execution. ⅔ for upgrades and large treasury spends, a simple majority otherwise.
- **Proposal kinds** replace every former admin power: parameter changes, treasury spends, repos, bounded Soul grants, sybil flags, claim rejections, clawbacks, Hunt refunds, upgrades, feature activation and text.
- **The treasury** is keyless; only a passed `TreasurySpend` moves it.
- **Early capture:** quorum has an absolute floor, and spending proposals are rejected for about the first 6 months.
- **Accounts can be M-of-N multisigs,** as a foundation's would be if governance funds one.
- **The first distribution is unresolved.** At launch almost nothing can vote, so how the first distribution from the keyless 32% bucket is executed is still open (TOKENOMICS.md section 10). Any mechanism a single party can trigger makes that party the distributor.

## 8. Ghost Mode (disabled at genesis)

- **What it is:** a shielded pool for private transfers, built from a Plonky3 STARK circuit with ML-KEM note encryption.
- **Switched off at genesis.** The code ships disabled and is switched on only by a governance `ActivateFeature` proposal, after its own ZK audit, ≥ 4 weeks on a public testnet with a bounty, and counsel sign-off (DEC-2).
- **Safeguards:**
  - The pool has a governance cap.
  - A "turnstile" means more can never leave the pool than entered it.
  - f+1 validators can pause it.
- **Viewing keys** (incoming, full, outgoing) are user-controlled. There's no protocol-level auditor key, and session keys and agents can't spend shielded funds.
- **Legal facts we know of** (background, to be checked):
  - DOJ said on 2025-08-21 that it won't bring new 18 U.S.C. §1960(b)(1)(C) charges against developers of truly decentralized, non-custodial software. The statement wasn't retroactive.
  - EU AMLR Art. 79 bars regulated firms from handling "anonymity-enhancing coins" from July 2027 (secondary source). A native shielded pool could get FANG treated that way by EU exchanges.

## 9. Names

- **`.boo` names** are registered on-chain. Prices depend on length (3 characters 100 FANG, 4 characters 25, 5 or more 5) and are paid to the treasury.
- **Terms:** a registration lasts a year, renewal costs 20% of the price a year, and there's a 30-day grace period. Names can be transferred and released. A genesis reserved list is assignable only by governance (DEC-11).
- **ICANN overlap:** we believe `.boo` is also a live ICANN TLD run by Google Registry (not verified), so `alice.boo` can also be a website.
- **Personal data:** GitHub handles and governance reasons (for example sybil flags) are written on-chain permanently.

## 10. Entity options

| Option | Role | Notes |
|---|---|---|
| **Dev company now** (US LLC, e.g. "Blockula Labs") | The founder's development work, IP, contracts | Cheap and fast. Holds no treasury and no protocol privileges. Any equity investors must get no token rights |
| **Cayman foundation company, or a US nonprofit, after launch** | Steward funded by a governance `TreasurySpend` (no genesis allocation; spends open ≈ 6 months after genesis): takes over the wallet domain and trademarks, engages auditors, runs the bug bounty, publishes quarterly reports | Cayman is memberless with a supervisor and used by the Optimism and Arbitrum foundations; the VASP Act's "virtual asset issuance" probably doesn't catch a no-sale launch (to confirm). A US nonprofit avoids offshore optics. Needs at least two independent directors or a supervisor |
| **Wyoming DUNA, adopted by governance after launch** | A legal wrapper for on-chain governance itself: can contract and pay taxes, and shields members | W.S. 17-32-101 et seq. (2024 SF0050, amended by 2026 SF0022 from 2026-07-01). **Needs at least 100 members,** so it can't exist at genesis. Uniswap governance adopted one in 2025. The foundation then shrinks to a service provider or winds down |
| No entity | | Rejected: personal liability, and possible general-partnership treatment of participants (Samuels v. Lido DAO, N.D. Cal., 2024-11-18). Nobody could own the domain or sign contracts |
| Swiss foundation | Same role as Cayman | Only if counsel prefers Europe |

**Proposed path:** dev company now, which holds the wallet domain and pays for audits, counsel and infrastructure through launch. No foundation allocation at genesis (DEC-5, 2026-10-18): its 5% is the treasury's, and governance may fund a foundation once one is formed. Then a DUNA adopted by governance once at least 100 members vote.

## 11. Checklist (from the research, section 6.7)

1. **Status of FANG** under the SEC's 2026-03-17 interpretation (Release 33-11412): a digital commodity at launch? A written opinion covering genesis allocations, vesting, staking, delegation pools, slashing, the burn, issuance to the pool and treasury, and governance rights.
2. **First distribution.** If one is made after mainnet is functional, could it consider past activity (on testnets or elsewhere) and stay within the release's p. 62 example, given its fn. 148? Does the testnet copy that told contributors they "earn" testnet FANG and Soul affect that? Is the no-value disclaimer now in README, CONTRIBUTING and AGENTS enough? No distribution, criteria or eligibility has been decided or announced.
3. **Hunts and the contribution pool (LEG-2):** compensation for services. Who is the payer: the protocol, the dev company, or Hunt creators? Tax reporting (1099 / W-8), OFAC screening of GitHub-linked recipients, contributor terms, GitHub API terms.
4. **The founder's allocation and tax:** taxable at genesis, at vesting, or on sale? Is §83 relevant if the allocation is tied to services (to the dev company or the network)? How are staking rewards on locked tokens treated (Rev. Rul. 2023-14)?
5. **Funding** without an issuer sale: the dev company pays for audits and counsel through launch. Does a governance treasury grant to a later foundation raise issuer questions for it or for the protocol? If it must ever sell, under what policy (programmatic, small, no representations, disclosed)? Would Regulation Crypto Assets' Rule 200 startup exemption help if adopted (Release 33-11434, proposed 2026-08-18; comments close **2026-10-20**)? Should the project comment that the proposal gives no path for projects that never sell?
6. **Entity choice and documents:** foundation jurisdiction, the Cayman VASP Act, director independence, the agreement between the dev company and a later foundation, trademark and domain ownership (DEC-8), and later DUNA formation.
7. **Communications policy:** the website, wallet, MCP, social accounts, AMAs, and statements by agents and affiliates (Release fn. 83). Terms of use and risk disclosures for the wallet, MCP and RPC (LEG-1).
8. **EU / MiCA:** the Art. 4(3) free-offer and validation-reward exemptions; whether GitHub-linked eligibility is personal data given in exchange; Art. 5(2) listings by platforms on their own initiative; GDPR for on-chain GitHub handles and accusations (LEG-3).
9. **Ghost Mode (DEC-2):** money-transmission and sanctions exposure for a shielded pool (DOJ statement of 2025-08-21; Roman Storm's conviction on the §1960 count). An opinion before `ActivateFeature { ghost }`.
10. **Names (LEG-3):** if `.boo` is a live ICANN TLD, the trademark and confusion risk. The reserved list and a dispute policy.
11. **Validator operations:** do the founder's validators (2 at genesis) or the bootstrap delegation make the founder a "central party"? Sanctions screening of genesis operators.
12. **Change of law:** what to do if the 2026 interpretation is withdrawn, or the CLARITY Act returns in the next Congress (from January 2027) with a different decentralization test.

## 12. Further open questions

- **Operational control.** Which handovers must be complete before genesis for the founder not to be a "central party" with operational control (Release fn. 54)? Today that means merge rights, releases, the wallet origin, the RPC and the explorer.
- **The founder's pledges.** No votes, no pool claims, no juries while vesting runs. Should they be published as a binding undertaking, enforced in code, or both? Is a 10b5-1-style selling plan the right model for a founder with no issuer?
- **Incentivized testnet.** Operators are to be paid from the 3% bucket under signed agreements for running testnet and genesis validators. How should those agreements be structured: in FANG locked at genesis, in fiat, or both? What can be said publicly when recruiting?
- **Soul at genesis.** Soul affects payout boosts and jury eligibility. If the genesis Soul ledger is derived from testnet jury outcomes, is that a testnet incentive in substance? Should it start empty, or follow a rule that counsel approves?
- **The first distribution's mechanism.** If governance can't yet reach quorum at launch, who may trigger the first distribution without becoming "a central party that distributes rewards to users" (Release p. 16)?
- **First FANG for new users.** With no sale and no faucet, should the treasury sponsor new users' first transactions? Does a sponsored transaction raise any issue?
- **The bug bounty** (SECURITY.md promises one on the incentivized testnet): paid in fiat by an entity, in FANG from the 3% bucket, or both?
- **Agents.** When an AI agent (a Familiar) does paid work, is the human sponsor the contractor for tax and sanctions purposes? What do the MCP's terms need to say?
- **The founder's own validators and RPC,** with no foundation at genesis: who should operate them, and under which entity?
