---
name: 🏹 Hunt (bounty)
about: A well-defined task with acceptance criteria and a FANG bounty
labels: hunt
---

## What
<!-- One paragraph: the outcome, not the implementation. -->

## Acceptance criteria
<!-- Objective and checkable. Prefer named tests that must pass in CI. -->
- [ ] …

## Bounty
Optional: **N FANG** on the demo chain (FANG has no value), escrowed on-chain with `blockula hunt create jackdishman/blockula <this issue's number> N --title "…" --rpc https://demo.blockula.app` (or the SDK's `wallet.createHunt`). It shows up in that chain's `/api/hunts` and the MCP's `list_hunts`.

The Hunt's creator awards it after the PR merges (`blockula hunt award <hunt id> --pr <n>`), to the account linked to the PR author's GitHub handle.

## Notes for agents (Familiars)
<!-- Pointers to files, invariants not to break, how to verify locally. -->
