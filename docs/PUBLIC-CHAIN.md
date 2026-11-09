# The public demo chain: permanent, and still worth nothing

*Decided 2026-11-08.* Blockula (FANG, `blockula-demo-2`, at <https://demo.blockula.app>) is the project's one public chain. It stays up. It isn't a mainnet: its coins have no value, its faucet stays on, and it's for education and research under the [Terms of Use](https://blockula.app/terms).

This page says what "permanent" promises, what it doesn't, and what a value-bearing mainnet would still need ([TOKENOMICS.md](TOKENOMICS.md) has that design, revised the same day).

## What we promise

- **No planned resets.** Balances, .boo names, keys and history carry forward. The chain id and genesis hash stay the same.
- **Upgrades in place.** A new release reaches the demo validators one at a time (`VERSION=… deploy/host-chain.sh` and `deploy/host-validator.sh`, re-run with the same settings). A change to the protocol activates by an upgrade proposal and its schedule (`/api/upgrade`), never by a new genesis.
- **Its key and its history are kept.** The validator key lives only on its Fly volume. The validators prune, so history goes to an archive the way testnet-2's did: a daily copy of `/api/sync` off the chain.

## What we don't promise

- **That it runs forever.** It's an experiment, and the Terms say a chain can stop or be reset at any time. If a bug leaves no way forward but a new genesis, the reset is announced first, on the site and in the repo, and names and balances are carried over by a genesis allocation where that's possible.
- **That anything on it is worth something,** now or later. Activity on it earns no claim to any future token (TOKENOMICS.md, section 8).

## Growing it: four or more independent validators

Two validators run today, both the project's (`blockula-blockula-demo` and `blockula-demo-v2` on Fly). A BFT chain of `n` validators tolerates `f = ⌊(n−1)/3⌋` faulty ones, so two tolerate none, and four are the first set that survives one going down.

1. Admit outside operators with the validator guide (cloud, port forwarding or a relay) until the set is at least 4, with at least 2 not the project's.
2. Then retire one of the project's two, so that no one operator could halt the chain alone once the set is 5 or more.
3. List each validator's operator on the explorer's validators page, as the chain directory does for chains.

## If the question of a value-bearing mainnet comes back

It's the maintainer's decision, and it reverses the 2026-10-22 one (no mainnet). It would take, before any genesis:

- **Counsel:** a written opinion on the token and the founder's role (TOKENOMICS.md, section 9), and an entity to hold the domains and run the services.
- **The audit:** the external audit of the frozen genesis commit and both ML-DSA implementations (SEC-1, SEC-2 in [AUDIT_SCOPE.md](AUDIT_SCOPE.md)).
- **Independent operators:** at least 22 at genesis (DEC-3), the founder at most 2.
- **Tokenomics:** the revised DEC-5 (no founder allocation, no distribution bucket; the genesis builder enforces it), and a plan for new users' first fees without a faucet (TOKENOMICS.md, section 10).
- **A new chain:** a mainnet is its own genesis (`blockula-1`). This demo chain's balances never move to it.
