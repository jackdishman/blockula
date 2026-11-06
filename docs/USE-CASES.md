# What to do with your chain

A small chain run by friends, a club, a class or a team. Here are eight things people can do on one today, with the first commands to try. Each assumes a chain [you launched](LAUNCH.md), and `<node>` is one of its nodes (`http://localhost:7070` on the machine that runs it).

The unit has no value on any of them. It's how the chain counts and charges fees, not money.

## 1. A shared tab for a trip or a house

**Who:** housemates, or friends on a trip.
**What:** everyone claims a `.boo` name, and pays the others back with a memo saying what for. The explorer shows who sent what and when, and nobody can edit it afterwards.
**Uses:** transfers with memos, `.boo` names, the explorer.

```bash
blockula wallet new
blockula wallet faucet --rpc <node>
blockula wallet name sam --rpc <node>
blockula wallet send alex.boo 12.5 --memo "groceries, Tuesday" --rpc <node>
```

Then open `<node>/#/account/alex.boo` to see everything Alex sent and got.

## 2. A class that runs its own blockchain

**Who:** a teacher and students, each with a laptop.
**What:** the teacher launches a chain; every student joins with a full node, and four of them become validators. Close one validator's laptop and the chain keeps going; close two and it stops until one comes back. Everyone sees the same blocks in their own explorer.
**Uses:** full nodes, validators, consensus, the explorer.

```bash
blockula launch --name "CS 101 Chain" --token CS --reach home --yes       # the teacher
blockula join --genesis http://<teacher's address>:7070 --genesis-hash <hash> --home ~/.blockula/cs-101-chain   # each student
```

The validator steps are in [LAUNCH.md](LAUNCH.md#3-friends-become-validators). `<node>/#/validators` shows who's making blocks and who missed turns.

## 3. A club that votes

**Who:** a club, a band, a reading group.
**What:** someone puts a question to a vote ("Move meetings to Thursdays?"), members vote aye or nay, and the result is on the chain. A vote stays open a week. In the chain's first year, ⅔ of its validators can also pass a proposal as a council, so on a small chain the people running it decide.
**Uses:** governance text proposals and votes.

```bash
blockula gov propose text --title "Move meetings to Thursdays?" --key ~/.blockula/<chain id>/validator.key   # bonds the 1,000 deposit
blockula gov list --rpc <node>                         # each proposal's id and digest
blockula gov vote 0 aye 10 --digest <digest> --rpc <node>
```

A vote locks the amount you vote with until voting ends (longer with `--conviction`). The deposit comes back if enough of the chain votes or the council does, so the creator, whose validator account holds the starting supply, is the one to propose.

## 4. A team's bounty board

**Who:** a team with a GitHub repo, or friends hacking on a side project.
**What:** put bounties on GitHub issues. Whoever fixes one gets it once the PR merges, people or coding agents alike. The bounty sits in escrow on the chain until the creator awards it.
**Uses:** Hunts, GitHub links, the MCP for agents.

```bash
blockula hunt create our-org/our-repo 42 50 --title "Dark mode for the settings page" --rpc <node>
blockula hunt list --rpc <node>
blockula hunt award 3 --pr 57 --rpc <node>       # after it merges: pays the account the PR's author linked
```

Contributors link their GitHub once (`blockula contrib link-github <handle> <gist id>`). Agents can take part with the [contributor prompt](BUILD-ON-IT.md#7-contributors-paste-this-into-your-coding-agent).

## 5. Agents with an allowance

**Who:** anyone trying out coding agents that pay for things.
**What:** give an agent its own key on your account, with a spending limit and an expiry the chain enforces. It can send within that budget and nothing else: no staking, no voting, no changing your keys.
**Uses:** session keys, the MCP, the passkey wallet.

```bash
claude mcp add blockula -e BLOCKULA_RPC=<node> -- npx -y @blockula/mcp
claude
> create a wallet, then use_account with my account boo1…
```

Paste the grant code it gives you into the wallet's **Devices & agents → Give an agent a budget**, pick a limit, then tell it to `use_account` again. The steps are in [AGENTS.md](../AGENTS.md#rules-of-the-road).

## 6. A hackathon sandbox

**Who:** people building apps at a hackathon or in a workshop.
**What:** a chain with a faucet, so every team gets free units to build with: a tip jar, a ticket counter, a leaderboard that pays. The SDK runs in Node and the browser, and passkeys mean nobody handles a seed phrase.
**Uses:** the SDK, passkey wallets, names, the HTTP API.

```bash
npm install @blockula/sdk
```

```ts
import { Client, Wallet } from "@blockula/sdk";

const wallet = Wallet.generate(new Client("<node>"));
await wallet.faucet();
await wallet.registerName("tipjar");
await wallet.send("alice.boo", "1", { memo: "great demo" });
```

[BUILD-ON-IT.md](BUILD-ON-IT.md) has the rest: the API, changing the apps, and hosting the wallet so every team can use it.

## 7. A programs lab

**Who:** people curious about programs that live on a ledger.
**What:** upload a WebAssembly program, create it, call it, and see what it stored. Programs are experimental and off by default, so make the chain with them on.
**Uses:** programs.

```bash
blockula launch --name "Program Lab" --token LAB --reach local --programs --yes
blockula program upload node/vm/tests/programs/counter.wasm --interface node/vm/tests/programs/counter.iface.json --key ~/.blockula/program-lab/validator.key
```

`upload` prints the `create` command to run next, and `create` prints the program's address to `call`. Run these from a clone of this repo, which has the example programs. [BUILD-ON-IT.md](BUILD-ON-IT.md#3-extend-the-chain) has the whole round, and [PROGRAMS-V0.md](PROGRAMS-V0.md) the specification.

## 8. Practice running validators

**Who:** people who want to learn how validators and staking work, with nothing at stake.
**What:** run a validator, delegate to a friend's, watch rewards come in each epoch, get jailed for missing turns and unjail. Unbonding takes 21 days, the same as on a real network, so you learn what that means too.
**Uses:** validators, staking, jailing.

```bash
blockula validator list --rpc <node>
blockula stake delegate <validator address> 10 --rpc <node>
blockula stake show --rpc <node>
```

[RUN-A-NODE.md](RUN-A-NODE.md#become-a-validator) covers running one as a service, and [OPERATIONS.md](OPERATIONS.md) the rest.
