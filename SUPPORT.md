# Getting help with Blockula

Blockula is an open-source project with one maintainer today. Help is best effort, in public, with no service level.

## Read first

| If you're | Start with |
|---|---|
| Using the wallet | The wallet's own tutorial, then [README.md](README.md) |
| Launching your own ledger | [docs/LAUNCH.md](docs/LAUNCH.md) |
| Building an app | [sdk-ts/README.md](sdk-ts/README.md), then [docs/INTEGRATION.md](docs/INTEGRATION.md) for payment flows |
| Giving an agent a wallet | [AGENTS.md](AGENTS.md) and [mcp/README.md](mcp/README.md) |
| Running a node or validator | [docs/RUN-A-NODE.md](docs/RUN-A-NODE.md), then [docs/OPERATIONS.md](docs/OPERATIONS.md) |
| Contributing | [CONTRIBUTING.md](CONTRIBUTING.md) |

## Ask

- **A bug:** the [bug report form](https://github.com/jackdishman/blockula/issues/new?template=bug.yml).
- **A node or validator** (including a testnet self-bond): the [node form](https://github.com/jackdishman/blockula/issues/new?template=node-operator.yml).
- **A vouch** for the contribution pool: the [vouch form](https://github.com/jackdishman/blockula/issues/new?template=vouch-request.yml).
- **A question:** open an issue. GitHub Discussions will take questions once it's switched on.
- **A vulnerability:** privately, through a [security advisory](https://github.com/jackdishman/blockula/security/advisories/new). Never in a public issue. See [SECURITY.md](SECURITY.md).

Is the network up? Every node answers `/api/health`, and the explorer shows the newest blocks.

## What nobody can do for you

- **Recover a key.** Your passkey or recovery kit is the only way into your wallet. Nobody, Blockula included, holds a copy.
- **Reverse a transaction.** Finalized transactions are final.
- **Get FANG back** that was sent to the wrong address, or taken by someone who had your key.

## Stay safe

- **Blockula will never ask for your recovery words,** your key file or a wallet backup. Anyone who does is trying to rob you.
- Nobody from the project will message you first to offer help, and nobody will ask you to send FANG to "verify" or "unlock" anything.
- FANG has no value, and the project sells none. Anyone offering to sell you FANG "from the team" isn't the team.
- Use the wallet only at its real address, which the README links.

This repo follows a [code of conduct](CODE_OF_CONDUCT.md).
