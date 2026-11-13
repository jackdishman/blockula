# FAQ

Short answers to questions people ask. Agents: the same facts are in [AGENTS.md](../AGENTS.md#soul).

## Soul

### What is Soul?

Soul is a reputation score on an account. It says the account belongs to a real person whose work the community has judged. It's separate from FANG: it can't be sent, bought or sold, and it fades if you stop contributing (it halves about every year).

Soul exists so the contribution pool, which pays for merged pull requests, can't be farmed with fake accounts or floods of tiny PRs. Like FANG and Hunts, Soul has no value.

### How do I get Soul?

- **An accepted contribution claim:** up to 10 Soul, scaled by how your claims did in the Séance (the jury that compares claims in pairs). Each further claim of yours in the same Séance earns a quarter of the one before.
- **Judging well as a juror:** 1 Soul each Séance in which your votes agree with the outcome.
- **A grant from the chain's admins,** on chains that have them (up to 100 Soul a grant). It's how a new chain gets its first people with Soul. On a chain without admins, a governance proposal does it.

On the demo chain (`blockula-demo-2`, where Blockula's own repo is paid), anyone with a merged PR who links their GitHub handle gets 10 Soul from the admin: [#162](https://github.com/jackdishman/blockula/issues/162) has the steps.

### What does Soul let me do?

| Soul | What it opens |
|---|---|
| Any, or a vouch | Claim a merged PR for the contribution pool |
| 5 | Sit on Séance juries (`blockula contrib join-jury`, 10 FANG bond) |
| 10 | Vouch for a newcomer (10 FANG bond a vouch, one vouch per 10 Soul, at most 10) |
| Every 10 | One more open claim at a time (3 to start, at most 20) |
| Up to 100 | A bigger share of the pool's payouts, rising to 1.5× at 100 Soul |

### I have no Soul. How do I start?

Ask someone with at least 10 Soul to vouch for you: the [vouch form](https://github.com/jackdishman/blockula/issues/new?template=vouch-request.yml) is the place. A vouch lets you claim merged PRs, and your first accepted claim earns you Soul of your own. The person who vouches bonds 10 FANG on you being a real, distinct person, and gets a tenth of your early rewards.

### Can I lose Soul?

Yes:

| What happened | Soul lost |
|---|---|
| A jury marks your claim as spam | 20 (and the claim's bond) |
| As a juror, your votes diverge badly from the outcome | 2 |
| As a juror, you seal a vote and don't reveal it | 2 (and a tenth of your juror bond) |
| Someone you vouched for is flagged as a fake account | 50 (and the vouch's bond) |
| Your own account is flagged as a fake | All of it |
| A claim of yours is rejected after it was scored (its PR was reverted, say) | What that claim earned |

It also fades on its own: half of it is gone after about a year.

### Can I send Soul to someone, or buy it?

No. Soul never moves between accounts, and nothing on the chain sells it.

### Do agents have Soul?

Not of their own. An agent contributes as the **Familiar** of a person: its claims count as that person's, the rewards and the Soul go to that person's account, and so do the penalties. Agents can't sit on juries.

### Where do I see Soul?

- An account's page in the explorer, and the Soul leaderboard.
- `GET /api/account/{name}` (the `contribution.soul` section) and `GET /api/souls`.
- The MCP's `my_contributions`.

More: [CONTRIBUTING.md](../CONTRIBUTING.md) for how to contribute, and "Proof of Contribution" in [PLAN.md](PLAN.md) for every rule.
