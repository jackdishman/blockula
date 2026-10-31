# Programs v0: the specification

> **This file is the specification of programs in protocol v2:** what the node, the SDK, the
> MCP, the CLI and the explorers implement, and what an auditor reads the code against. It amends
> [PROGRAMS.md](PROGRAMS.md) (revision 3, "PRG" below, the longer design): where the two disagree,
> this file wins for protocol v2 as the node runs it, and everything PRG specifies that this file
> doesn't mention, and that the v0 cut (section 2) keeps, holds as written. Programs, never
> contracts. The code cites this file by section (`docs/PROGRAMS-V0.md §6.2`, or a bare `§6.2` or
> `A30` in a comment), and `scripts/check-spec-citations.mjs` fails CI when a citation of this
> file, a bare `§` or amendment id in the programs code (`node/src/programs`, `node/vm`,
> `node/fuzz/src`, `sdk-ts/src`, `mcp/src`), or an item or step number they give ("§6.1 step 4")
> names something that isn't here, so a section keeps its number. It checks that the numbers
> exist, not that the text still says what the citer means; the script's header lists what it
> doesn't read.
>
> **Programs are optional and off by default.** A chain has them from block 1 when it is made with
> them (`blockula summon --programs`, `blockula dev --programs`), or when its governance switches
> them on (§1.3). Every chain's unit is its own and worthless by design: the value cap (§7.1) and
> the pause (§7.2) are experimental controls over units that programs hold, with no monetary meaning
> and no promise attached. For the people who run it, see
> [OPERATIONS.md](OPERATIONS.md#programs-optional-off-by-default) (limits, incident response, the
> pause), [GOVERNANCE.md](GOVERNANCE.md#programs-optional-off-by-default) (the `program_*` keys) and
> [LAUNCH.md](LAUNCH.md#programs-experimental-off-by-default); for an auditor,
> [AUDIT_SCOPE.md](AUDIT_SCOPE.md#programs-optional-off-by-default).

Status: revision 3, 2026-10-29. Revision 2 (2026-10-20) was written after two adversarial reviews,
a read-through against the landed branches and a verification pass; the Review log at the end
answers every finding of those, and of the stage reviews since. Revision 3 brings it up to the code
as built through the block-pass thread (A3), the node-fault safeguards, the clients, the fuzz
harness and the operating documents, and sets it in the launch kit's frame (§0, §9.3): there is
no launch date and no mainnet in it, and the rules that name `blockula-1` are rules the code has,
inert for any other chain id.

**Reading this file.** The names `W1`, `W2`, `V`, `L`, `S1` to `S9`, `P2`, `N`, `T` and `F` are the
build stages of the programs work: W1 the VM crate (`node/vm`), W2 protocol versions, activation
and the genesis rules, V what the node needed added to the VM, L the `blockula-1` rules (§1.7),
S1 to S9 the node's modules, the API, the clients and the documents, P2 the v1 replay gate
([REPLAY.md](REPLAY.md)), N the cheaper transaction check, T the deferred tests and F the codec
fuzzer. "Review S5H-R3" names a finding of that stage's review, answered in the code or the
appendix. A1 to A40 are the amendments to PRG that §0 lists. Commit ids and `file:line`
references name the code as it was when that part was written; lines have drifted, so find a rule
by its text and function name. `WBS` is the build plan, which is not in this tree (a mention such
as "WBS S4b" names a stage). `node/vm` names are W1's.

---

## 0. Scope, and every amendment to PRG

**What this release builds.** Protocol v2 with programs v0 in the binary: third-party
WebAssembly programs that anyone can upload, create and call, behind the `programs` feature.
Programs are optional and off by default. A chain has them from block 1 when it is made with them
(`blockula summon --programs`, `blockula dev --programs`: protocol v2 scheduled at height 1 and
`programs` among the genesis' features, §1.5), or gets them later by governance: schedule v2 if it
isn't in force, then `ActivateFeature { programs }` (§1.3). The same binary runs
`blockula-testnet-2` (a v1 chain) and must replay it byte for byte.

**The rule everything follows: v2 = v1 + dormant programs.** Protocol v2 changes nothing in state
or behaviour until the `programs` feature is active. Program state is created when the feature
first switches on. Section 1 lists the only things v2 changes while it's off. The working rule,
"tags 62+ are refused at admission unless v2 is in force and the feature is on, and not paused",
holds exactly until programs first switch on; after governance switches them off again, exit-only
(§7.3) still admits value-free calls and `ProgramsPause`, so that FANG already in programs can
leave.

**Two rules that name `blockula-1`.** `Genesis::check` refuses a `blockula-1` genesis that lists
`programs`, and governance there switches programs on only under protocol v3 or later (§1.5, §1.7,
A27). They were written for a chain that would have launched v2 before its program rules had been
audited (§9.3 tells it as history). They are rules the code has, not a plan: a chain of the launch
kit names its own chain id, so they are inert for it, and such a chain may list `programs` at
genesis or activate them under v2. They stay as they are because the genesis check and the
activation check must give every binary the same answer, and because lifting them would change no
state a chain has (§1.7).

**Amendments.** Each one replaces what PRG says on the point; the section that specifies it is in
brackets.

| # | PRG says | v0 does | Why |
|---|---|---|---|
| A1 | Tags 62–70, namespaces 66–73, `programs_freeze`, `/simulate`, grants, upgrades, close, removal (PRG 7, 8.1, 9.1, 14.1) | Tags **62, 63, 64, 69** only. 65–68 and 70 keep their numbers and meanings but aren't decoded ("unknown action tag", as today). Namespaces 66, 67, 68, 69, 72 and META `programs` are used; 70, 71, 73 and 74–79 are reserved. No `programs_freeze`, no `/api/simulate`, no scoped keys (§2) | The cut that can be built, tested and audited in the time there is (the v0 design's recommended cut). Each deferred piece arrives later as a new tag or record behind a protocol version, without changing any v0 format (§9) |
| A2 | At the v2 activation height `begin_block` inserts META `programs` and moves the schema record to 10; `SUPPORTED_SCHEMAS = 9..=10` (PRG 5.2, 14.4 items 2–3) | **No `begin_block`, no migration at any version boundary.** META `programs` appears when the feature first switches on (an `ActivateFeature` executing, or a genesis that lists `programs`). `SCHEMA_VERSION` stays **9**; there's no schema range in this release (§1, §4.7) | Under "v2 = v1 + dormant programs" nothing exists to migrate at v2. Program records are additive (new namespaces, a new META key) and each carries its own format byte, so no existing record is re-encoded and testnet-2's bytes never move |
| A3 | A fresh engine per block, each distinct code translated once per block, load counted once per block (PRG 4.3, 5.3) | **Load is counted per transaction:** a block's fuel counts each transaction's whole reservation, its declared code's load included, also when an earlier transaction declared the same code (§5.4). That's the consensus rule. **How the node meets it is its own business:** v0's implementation runs a block pass that holds a program transaction on one large-stack thread with one engine, which loads each distinct code once a pass and is dropped after any fault, past 1 MiB of loaded code, on a change of schedule and when the pass ends (§6.1 steps 3–4); outside a pass each VM run gets a thread and an engine of its own. Neither changes an outcome or a fuel figure, so neither needs a protocol version | Removes the cross-transaction cache, the "translate everything before the block" rule, soroban-wasmi's translate-inside-a-call deadlock, and the per-block set of counted code hashes. Keeps the drop-after-fault rule and adds a memory bound: an engine is dropped after any fault and when the code loaded into it would pass 1 MiB. W1's `new_block_engine` returns a `Box<dyn BlockEngine>` that isn't `Send`, so the engine lives in a thread-local of the pass thread. Every transaction already pays its full load (PRG 4.5), so the work an engine does is what was charged. The setup fuel doesn't meter (a thread, an engine and its linker, once a pass) is measured against `LOAD_BASE` before the freeze (§5.2 item 3: within budget on both floor runners) |
| A4 | Every program execution runs on the P3 executor thread (64 MiB) or an API pool (PRG 4.3, 12.2) | Each program transaction runs on a thread spawned for it with `std::thread::Builder::stack_size(64 MiB)` inside `std::thread::scope`. The node's block loops stay where they are (§6.1) | P3 (one executor thread for every consensus path) is a refactor of `consensus.rs` and `node.rs` that v0 doesn't need: only VM work needs the guaranteed stack. PRG 12.2's other prerequisite, the replay cache, does land (A29) |
| A5 | Code and programs are usable as soon as the transaction that uploads or creates them applies | **Code is usable from the block after its upload; a program from the block after its creation** (`uploaded_at < h`, `created_at < h`) (§2) | Every transaction's declared code is then in the state the block started from, so admission, proposers and verifiers agree on it trivially, and nothing a block does can change the code another transaction in it loads |
| A6 | A fuel price ring (`FuelSlot`, `fuel_fees`) moved by a controller, fixed `FEE_LAG` ahead, and `verify`'s stateless pre-checks of Σ `exec`, each `fuel_limit` and each fee's fuel part against the ring's slot (PRG 5.2, 5.3) | **A congestion-priced rate without the ring:** META `programs` holds the current `fuel_rate` (milli-drops per fuel), which each block's end moves by `econ::next_slot`'s formula on the block's counted fuel against `program_max_block_fuel`, never below the council's floor `program_fuel_floor_rate`. A transaction is priced at the rate of the state it executes on (§5.6). `verify` has no stateless fuel check: its sequential pass admits each transaction against the budget and the rate (§5.4) | A fixed rate let anyone fill every block's fuel for 0.01 FANG a block with no price response (Review log, safety F2). The ring only bought knowing the rate 17 blocks ahead, which nothing needs: no stateless check reads the fuel rate (verifiers judge program transactions in the sequential pass, on the parent's state, as proposers and execution do), and wallets sign a maximum, so a margin costs them nothing. The stateless Σ `exec` check would bound nothing the sequential pass doesn't, and would have to live in P2's shared verify helpers (build F3) |
| A7 | `UploadCode`'s deposit has no signed bound; whether the 256-byte record deposit falls under `storage_limit` is an open question of the v0 design | `UploadCode` **gains a `u64 storage_limit`**: the most its deposits may bond. `CreateProgram`'s record deposit counts against its `storage_limit`, with `init`'s writes (§2.2, §2.3) | So every program transaction's worst case is `fee + Σ value + storage_limit` in signed bytes, plus `account_creation_fee` for a create that opens its program's account |
| A8 | `CreateProgram` and `CallProgram` may nest in a `MultisigPropose`, with reservation rules for proposals and approvals (PRG 5.1, 8.1, 9.2) | **No program tag nests in v0.** Decoding refuses 62, 63, 64 and 69 inside a `MultisigPropose`, so multisig accounts can't use programs yet (§2.1) | Removes the multisig reservation rules and the "stored action meets the program checks later" path. Allowing it later is a pure relaxation at a new version |
| A9 | `AwardHunt`, `TreasurySpend` and `Unshield` may credit a program, against the value cap (PRG 8.3, 9.3) | **A native credit reaches a program only as a `Transfer`**, to a program that `accepts_transfers`, while programs are on. `AwardHunt`, `TreasurySpend` and `Unshield` to a program fail (§7.1) | Four credit sites (`chain.rs:1423`, `chain.rs:1587`, `governance.rs:890`, `ghost.rs:633`); one guard covers them. Fewer paths into `value_held` |
| A10 | A receipts root per block in namespace 70, swept after a day; receipts in `store::BlockMeta`, a Borsh `ProgramReceipt` with a `FailCode` enum, a call trace and a `receipt_digest` (PRG 6.5, 8.4) | **No receipts root.** Each program transaction's receipt is a display-only field of the executed block (`Block::programs`), like `Block::failed` and `Block::base_fee`: written to the block log, recomputed on replay, pruned with the block. It's serde JSON with numbered outcomes (W1's `FailCode` 1–3, the node's 10–20), no trace and no digest, and a block records at most 256 KiB of claims (events and return data) over its receipts (§8.1) | Receipts hold what programs claim (events, return data, failure codes); the facts they move are already state and under the root. Keeps `store.rs` (another workspace's region) and `STORE_FORMAT` untouched. The claims budget stops anyone writing tens of MB of hex to every full node's block log a second, which a root's price would otherwise have bounded. A later version adds the root from an activation height without changing the outcome numbers |
| A11 | A caught panic becomes `Failed` (`RuntimeFault`), charged, and the block's engine is rebuilt (PRG 4.3) | **A node fault is never a transaction outcome; what a node does about one depends on the path** (§6.2). A proposer leaves the transaction out and quarantines it (nothing is committed there, so leaving it out claims nothing about it); a verifier abstains; executing a certified block halts the node (`Halt::ProgramFault`, exit 13; or `Halt::ProgramResources`, exit 14, which restarts, for a thread or an engine this node couldn't make, or a block this node's own verification ran without a fault) | A fault one node hits and another doesn't must not become a deterministic-looking `Failed`. A deterministic engine bug can't stall the chain either: honest proposers leave the transaction out, and honest verifiers abstain on a block that carries it, so it's never finalized. A certified block faults only on a node that differs from the quorum that verified it |
| A12 | Proposers include `Failed` program transactions (PRG 9.5) | Same, with the exact rule: a transaction carrying tag 62, 63 or 64 whose outcome is `Failed` is included and charged; every other `Failed` transaction is still left out as today (`consensus.rs:662-671`); and one whose trial faulted is left out and quarantined (A11, A28) (§6.3) | Keeps v1 behaviour byte for byte: no v1 transaction carries those tags |
| A13 | An `apply_j` flag, `run_programs: false` (PRG 9.5) | `apply_j`'s `full_checks: bool` becomes `mode: ApplyMode { Block, Mempool }`. `Mempool` skips `check_format` (as `false` does today) **and never runs the VM** (§6.4) | `full_checks == false` is exactly the mempool's three simulations and the fuzz harness's dry run today; naming it makes "the mempool never runs the VM" a type, not a convention |
| A14 | `ProgramsPause` signed by a consensus key or owner without a tip, admitted through `staking::system_tx_key` (PRG 14.1, Appendix A) | Signed from the validator's own account (its consensus key) or its owner's (an owner key), exactly as `GhostPause` is (`staking::validator_for_signer`, `staking.rs:1937`); no `system_tx_key` change and no tip rule of its own. The consensus-key form is a validator duty with a slot in the mempool's lane; the owner-key form may tip. It works while programs are exit-only (§2.5, §7.2) | W2 gives each genesis validator's consensus-key account a liquid float, so the validator's own account pays; reusing GhostPause's admission adds no new signing path. An owner-key signature isn't a duty (`duty_of` wants the consensus key, `staking.rs:2000-2001`), so forcing it to tip 0 would keep it out of a full mempool |
| A15 | Fuel schedule v1 and its limits: 60M a block (bounds [10M, 60M]), 20M a transaction (and at most a third of the block's), 1 MiB of code a transaction, `MAX_RESERVATION` 2^25 (PRG 4.5, 5.2, 5.3, 14.2) | **Schedule v0** (W1's `meter::SCHEDULE_V0`: operators 3/4/20/4/30, load 10,000 + 40/B, upload 50,000 + 71/B, instantiation priced per entity). The code bounds stay PRG's 60M a block and 20M a transaction (W1's `CALIBRATED_BLOCK_FUEL`, `MAX_TX_FUEL`), but **the parameters default to 40M a block and 10M a transaction**, the block's lower bound is 20M, and the only cross-bound is `program_max_tx_fuel ≤ program_max_block_fuel`; 512 KiB of distinct code a transaction (W1's `MAX_TX_CODE_BYTES`), `MAX_RESERVATION` 2^26, a 250 milli-drops-per-fuel floor (§4.5, §5) | Schedule v1 is 2–175× underpriced on the floor machines (section 4.3 of [programs/phase0.md](programs/phase0.md)). v0 is overpriced 1.5–3× against the worst floor figures on every item but upload (1.22×, §5.2; the floor runners' re-run under v0 is in `node/vm/CALIBRATION-*.md`, every pattern GO); the lower defaults keep a 1.5× margin under the 150 ms block run on top of that, and the council can raise them. The 20M lower bound fits the largest upload (19.8M); 2^26 fits `MAX_TX_FUEL` plus the largest load (41.2M). PRG's "a third of the block" kept one transaction from filling a block at a fixed price; the fuel rate (A6) now prices a full block whoever fills it |
| A16 | The interface carries name, docs, build info, events, errors, types and storage layouts (PRG 11.1) | **Interface format 1 is methods only**: name, kind, payable, argument and return type descriptors, with PRG 11.1's type widths (`bytes` with a u32 maximum, `text` and `vec` with u16 ones). It lives in `node/vm/src/interface.rs` (§3) | It's all the chain enforces (kind, payable) plus what tooling needs to encode calls. Format 2 adds the rest. The widths are inherited and frozen with format 1 on purpose: a u32 `bytes` maximum lets a later `MAX_INPUT` raise need no format change |
| A17 | Nine `program_*` parameters with a ring (PRG 14.2) | Seven, stored as a sorted key → u64 map in META `programs`; `program_max_code_bytes` and `program_receipt_roots_kept` don't exist in v0 (§4.5; A15 and A33 change three defaults and bounds). A `program_*` key is known only once META exists, so the defaults run for at least the first vote and timelock after programs switch on | Fewer knobs to audit; the map lets a later version add keys without re-encoding the record. Accepting the keys earlier would be a v2 behaviour change while programs are off (Review log, build F11) |
| A18 | Code, interface and program-record deposits refund through `RemoveCode` and `CloseProgram` (PRG 7.4) | Bonded and recorded with their payer, **with no refund path in v0** (tags 67, 68 come later). Storage-entry deposits refund on removal as PRG says (§4.2) | Removal and closing are deferred; their records are kept so a later version can refund exactly |
| A19 | A program may carry an authority that upgrades it with notice (PRG 7.3); its record holds a pending upgrade, an offered authority and a closed status (PRG 6.1) | `authority`, `upgrade_delay_blocks` and `policy` are validated and stored, **with no power until tag 65 exists** (§2.3). `ProgramRecord` format 1 has **no** `pending`, `offered_to` or `status` fields: tags 65–67 bring them in format 2 (§4.2) | So a v0 program can become upgradable by the authority it was created with once upgrades ship, with at least the minimum notice; wallets say so (§8). The three fields would be audit surface for actions that don't exist, and their design may still change, which needs format 2 anyway |
| A20 | Version-1 upload validation lets `memory.grow` return −1 and ignores custom sections (PRG 4.2) | Upload refuses `memory.grow`, custom sections and start sections, and caps `bk_` exports at 64 (W1's `validate.rs`) (§2.2) | The engines charge a refused grow differently, and two uploads that differ only in a custom section would hash apart yet behave the same (section 3 of [programs/phase0.md](programs/phase0.md)) |
| A21 | Mainnet's genesis starts at v2 "with a schedule entry at height 0" (PRG 14.3) | `upgrades = [{ version: 2, activation_height: 1 }]` is what a chain born at v2 writes (`blockula summon --programs`), and the only schedule `blockula-1` accepts besides none (W2, `upgrade::MAINNET_GENESIS_VERSION`) (§1.5) | The code requires activation heights ≥ 1 (`upgrade.rs:150`); the genesis block keeps v1 |
| A22 | Exit-only also refuses scheduling upgrades and allows close, `RemoveCode` and authority changes (PRG 14.1) | Exit-only covers v0's tags only: uploads and creates refused, calls with value refused, commits that raise the FANG programs control refused (A26), native credits to programs refused (§7.3) | The other tags don't exist yet |
| A23 | D3: programs are audited by agents only, never externally (PRG 13, 15) | An external audit of the programs scope ([AUDIT_SCOPE.md](AUDIT_SCOPE.md)) and the fuzz campaign (§10.6) are required before a supported programs-on release; the rules are frozen from that release on and not before (§9.2). Programs themselves are optional and off by default (§0); a chain made with them before a supported release runs unaudited, experimentally and with no compatibility promise, and takes on what OPERATIONS.md lists | The maintainer's decision that this release isn't audited by agents alone; the launch kit made the feature optional |
| A24 | `/view` and `/simulate` are off on validators by default (PRG 9.5) | Same for `/view` (there's no `/simulate`, A1): on by default on full and RPC nodes, off on validators unless started with `--program-views` (§8.2) | A validator's HTTP is private by convention only (docs, #44); a view runs a VM for whoever asks |
| A25 | Each validator signs `ProgramsPause` at most once per 30 days, a signature that never reached the threshold included; votes reset every epoch (PRG 14.1) | **Votes live a day, not an epoch; only a signature that starts or renews a pause spends the 30-day window; a running pause can be renewed; and the window doesn't apply while a fix is pending** (an open `ScheduleUpgrade` or `DeactivateFeature { programs }` proposal, or a scheduled upgrade not yet active) ; and a run of pauses is capped at 30 days from its start, with no new run within 60 days of the last run's start (§7.2, the maintainer's decision of 2026-10-20) | PRG's window was sized for f + 1 = 11 of 32 independent seats. With four seats (f + 1 = 2) it allowed two 3-day pauses in 30 days against a fix path of at least 7 days, and an uncoordinated signature in a one-hour epoch burned half of it (Review log, build F2 and F6, safety F14). §7.4 shows a pause covering every day of the fix path at n = 4 |
| A26 | Exit-only refuses commits that raise `value_held` or escrow (PRG 14.1) | Exit-only refuses commits that raise **the FANG programs control**: `value_held` plus `sponsored`, the escrow of deposits whose payer is a program. A signer's own deposit, refundable only to the signer, is allowed (§7.3) | Withdrawals that write a record (a `claimed` marker, a new holder's balance) bond a deposit; refusing those kept users from exiting, the opposite of exit-only's purpose (safety F7) |
| A27 | `ActivateFeature { programs }` may run under any version that knows the feature (PRG 14.1) | **On `blockula-1`, only under protocol v3 or later** (`upgrade::check_may_activate`, beside W2's Ghost Mode rule; landed as W2's stage L, `06c35fd`). Its genesis can't list programs either (`Genesis::check`), so v2's program rules never run on `blockula-1` (§1.5, §1.7, §9.3). Every other chain id may activate them under v2 | The rule was written for a chain that would have run v2's program rules before an audit had cleared them: a council vote must not be able to switch them on (safety F8); the fixes, or a release that only records that the audit cleared v2's rules, come as v3. A rule keyed on the chain id and the version is the same on every binary, so it can't split the chain the way a runbook-only rule could be broken or a changed constant would. It is inert for a chain with its own id (§0) |
| A28 | Node faults in proposals and verification: no rule (PRG 4.3 rebuilds the engine) | Node-local safeguards, never consensus (§6.2): a **quarantine** (a transaction whose trial faulted is refused by this node's mempool, skipped by its proposals and abstained on in verification until its `valid_until` passes; its sender's program transactions are held for 600 blocks, its transfers never; an upload's faulting bytes for a day; a call's declared code never); a **trial marker** that carries the quarantine across a crash; **strikes** that stop a leader proposing a program transaction its proposals twice failed to certify with; and a **one-time retry** (exit 14) of a block this node's own verification ran without a fault | So a transaction that crashes or faults a node is tried at most once per node, a crash can't loop a node through its own mempool, one caller's fault can't censor a program or an account, and a fault some validators hit but not others costs two views per leader, not every view |
| A29 | A replay cache of verified and proposed blocks' post-states is a prerequisite of programs (PRG 12.2, P4) | A replay cache lands, of the post-states `replay` computes for unexecuted ancestors (not of verified blocks), keyed by block digest, so each block runs at most once as an ancestor per validator: four runs a block at most, not PRG's three (§5.3, §6.6). P3 and P5 stay out (A4, §6.3) | Without it a verifier that falls behind re-executes every unexecuted ancestor at every vote, so falling behind costs more VM work and it falls further behind (safety F5, build F4) |
| A30 | A program opening an account pays `account_creation_fee` and credits at least `MIN_NEW_ACCOUNT` (PRG 9.3) | As PRG says, for both `OPEN_DEDUCT` and `OPEN_PAY`, which W1 lacked (stage V adds it to `Overlay::transfer`); and a keyless system account (`chain::system_account`) is an existing account to the VM, so no program pays a creation fee for one, as no native action does (§6.1) | The node's native rules (`chain.rs:1412-1414`, `econ.rs:548-551`); without them a program could open zero-balance accounts and pay to "open" the treasury (safety F15, build F9) |
| A31 | Host ABI v1 as PRG 10.2 lists it: input and output, storage with `storage_next`, context with `caller_auth` and `origin_session`, FANG, calls, events, `epoch`, `chain_instance`, `tx_hash`, the beacon, `resolve_name`, and crypto (`blake3`, `blake3_derive_key`, `sha256`, `keccak256`, `ml_dsa65_verify`, `verify_account_sig`) (PRG 10.2–10.4) | **W1's subset** (`abi.rs`): `input_len`, `input_read`, `return_data`, `abort`, `debug_log`, `storage_read`, `storage_write`, `storage_remove`, `self_address`, `caller`, `value`, `block_height`, `self_balance`, `transfer`, `call`, `try_call`, `last_return`, `emit` (each `_v1`), plus `last_return_len_v1` and `last_abort_code_v1`, which PRG lacks. The rest arrives as new `_v1` functions behind a protocol version (§9.1) | A function is frozen once programs run with value, so a smaller table is less to audit now, and functions are only ever added (PRG 10.1): nothing a v0 program does changes when they arrive. Iteration, the beacon (whose retention window is part of the ABI, PRG 10.3), names and in-program signatures each need their own review |
| A32 | PRG 6.1's record layouts: `ProgramRecord` with `address`; `BlobRef { uploader: [u8; 32], … }` without `uploaded_at`; `ProgramsMeta { params, state_bytes, value_held, escrow, fuel_height, fuel_fees, pause, pause_signers, roots_from }`; Borsh values with no format byte | Format-1 records (§4.2): every value led by a format byte; `ProgramRecord` without `address` (it's the key) and without A19's fields, with `record_deposit` and `record_payer` (A18) and `sponsored` (A40); `BlobRef.uploader` a string, plus `uploaded_at` (A5); `ProgramsMeta` with a `programs` count, `sponsored` (A40), `fuel_rate` (A6), `votes` and `pause_signed` (A25), and parameters as a sorted map (A17) | Each field serves a v0 rule, and none serves a deferred one; addresses are strings in node records, as everywhere in the node's state; the format byte lets a later version add format 2 beside 1 with no schema bump (A2, §9.1) |
| A33 | `program_state_soft_bytes` 512 MiB by default, in [64 MiB, 1 GiB] (PRG 6.2, 14.2) | **64 MiB by default, in [16 MiB, 512 MiB]** (§4.5, §4.6) | A training wheel like the value cap: the ceiling at the default is 128 MiB accounted (a few hundred MiB of RAM), safe on testnet-2's 2 GB machines too; the council raises it toward 512 MiB (a 1 GiB ceiling, about 3 GiB of RAM) on validators with 16 GB as programs prove themselves. Twice the bound plus native state stays far under the snapshot's 4 GiB |
| A34 | A deposit priced at "the committed program bytes plus what this transaction's overlay has added so far" (PRG 4.4, 6.3) | **Priced once the write lands**: `used` includes the written bytes, for entries, upload blobs and program records alike (§4.6) | PRG's wording leaves open whether a write pays for its own bytes; W1's overlay prices after (VM-4), and one convention for all three means no write straddles a step of the curve at the lower price |
| A35 | A pause stops tags 62–68, and native credits to programs still work (PRG 14.1) | **While paused, native credits to programs are refused too** (§7.1, §7.2) | During an incident no value should flow in; a plain credit runs no code either way |
| A36 | `Σ value ≤ the sender's transferable FANG after the charge` at admission (PRG 8.1) | Checked when the action runs, by `econ::check_locks` as for every action: a call or create whose sender can't cover its `value` or deposits fails, charged (`native`, §8.1). The mempool's pessimistic debit (§6.4) keeps such a transaction out of the mempool | One place for the rule, the one every native action uses, over the post-fee state. A sender who spends the FANG elsewhere after queuing a call pays its fee, as for any failed action |
| A37 | PRG 8.1's stateless rules | Three more, so each meaning has one encoding: a call's `version_digest` is never 64 zeros (a binding's may be); a create without `init` has `fuel_limit` 0 and no bindings; one without `authority` has `upgrade_delay_blocks` 0 and policy `open` (§2.3, §2.4) | A call always names the version it previewed (the SDK and MCP never sign zeros by default anyway, PRG 8.1); a field that has no effect can't vary |
| A38 | Node policy: at most 16 pending uploads, a new one evicting the lowest tip rate if its own is a tenth higher; `TipRate` as `tip / max(size / max_block_bytes, reserved / max_block_fuel)` (PRG 5.3, 7.1) | **The 17th upload is refused** ("too many pending uploads"); `TipRate` compares `tip / max(size / consensus::MAX_BLOCK_BYTES, exec / CALIBRATED_BLOCK_FUEL)`, from the transaction's own bytes (§2.2, §5.6) | Neither is consensus. Every pending upload is admittable at the current rate, so proposers include and charge it within blocks (two maximal uploads fit a default block's fuel): a slot can't be held for free. `TipRate::of` is a pure function of the transaction (the mempool keys its queues by it), and for every fuel-free transaction it orders exactly as tip per byte does |
| A39 | Clients: the MCP signs program calls through elicitation and an allowlist (`program_call`, `program_simulate`, `program_events`); the explorer renders `ActionSpec` forms for deployed programs; the SDK proves storage, grants and receipts; interfaces at `/api/program/{address}/interface` (PRG 6.4, 11.2, 11.3, 11.5) | **Reads first**: the MCP is read-only (`program_show`, `program_view`, `program_code`), the explorer shows programs without forms, the SDK proves the program record only (`provenProgram`), interfaces are served by hash (`/api/interface/{hash}`); signing is the CLI's and the SDK's (§8) | Agent signing needs `/simulate` (A1) and PRG 11.5's confirmation flow, and call forms need interface format 2's docs and bounds (A16): each is its own release, and nothing on chain changes when it lands |
| A40 | The value cap bounds `value_held`, Σ balances of program accounts, deposits excluded (PRG 9.3, 14.1) | **The cap bounds the FANG programs control**, `value_held + sponsored`, the measure exit-only uses (A26): a commit, a `Transfer` or an adoption that raises it past `program_value_cap` fails. A sponsored deposit or its refund moves FANG between the two terms and leaves the control unchanged (§7.1) | A program at the cap could bond its balance into storage it sponsors, take in another 10M FANG and repeat, up to the storage curve's ~55M FANG at the default soft cap; and a sponsored refund at the cap failed as if it raised the cap (Review log, verification F2) |

---

## 1. Activation and versions

### 1.1 The rule

Protocol v2 is v1 plus programs, dormant. Every program behaviour is gated on all of:
- `upgrade::version_at(state, h) ≥ upgrade::PROGRAMS_VERSION` (2, W2), at the height `h` the
  transaction executes at;
- META `programs` present (programs were switched on at least once);
- for the full behaviour, the `programs` feature active (`upgrade::is_active`); with META present
  and the feature off, programs are **exit-only** (§7.3);
- not paused (§7.2), except for what a pause leaves running.

`programs::status(state, h) -> Status { Dormant, On, ExitOnly, Paused { until } }` computes this
in one place (`node/src/programs/mod.rs`), and every check calls it. `Dormant` below v2 or without
META; `Paused` takes precedence over `On` and `ExitOnly` while a pause is running at `h`
(`from ≤ h < until`), and from `until` on the status is `On` or `ExitOnly` again, by the feature
flag. Only a running pause counts (`programs::status`, `programs/mod.rs:91-94` at `296a466b`):
not the last run's record once it lapses, which stays in META for §7.2's spacing check, nor one
whose `from` is still ahead (the rest of the block that starts it, §7.2). `check_invariants`
guarantees `from < until` (§4.4), so a stored pause always covers at least one height.

### 1.2 What v2 changes while programs are off

Exactly these, and nothing else. A v2 binary computes v1's state, records and outcomes at every
height of a v1 chain, and at every height of a v2 chain before `programs` first switches on.

1. **The tx codec decodes tags 62, 63, 64 and 69** (`Action::decode_at`, `types.rs:1113-1195`;
   `check_format`, `types.rs:1205-1522`), statelessly. Admission refuses them (`Err`, never
   charged) unless `status` allows them: `On` for 62–64 (`ExitOnly` too, for a call that carries
   no value, §7.3), anything but `Dormant` for 69. A v1 binary can't parse them and
   rejects any block carrying one (`consensus.rs:766`); a v2 binary rejects it too, because
   `verify` rejects a block with an unadmittable transaction (`consensus.rs:848-853`). So no block
   with one is ever finalized while programs are off, on either binary (PRG 14.1's argument,
   unchanged).
   **The codec also lets a `ParamChange` of programs' seven parameters through** (§4.5's keys,
   `programs::params::Param`), on its own and inside a `MultisigPropose`, so that a block can carry
   one once programs exist: `ProposalKind::check` (`types.rs:2146-2156` at `296a466b`), which v1
   limits to `ChainParams::KEYS` (`types.rs:1791-1794`). Admission refuses it while `status` is
   `Dormant`, in v1's codec words ("unknown parameter `{key}`", after "the proposed action: " when
   nested): `programs::params::admit` (`programs/params.rs:181-196`), which `programs::admit`
   calls for every transaction before it looks at the tag (`programs/admit.rs:32`). Any other
   `program_*` key the codec still refuses, as v1 does. Same effect as v1, in a different place:
   unadmittable, never charged, and a block carrying one is invalid on both binaries (v1 refuses
   it in `check_format`, v2 in admission, which `verify` runs too). Only which refusal a
   transaction meets first can differ: one that also breaks an earlier admission rule is refused
   in that rule's words, which no block or record holds.
2. **`known_features(2)` includes `programs`** (W2), so `ActivateFeature { programs }` can be
   proposed once v2 is in force (W2 checks a feature against the proposal's own block's version,
   `1653478`, so v1 binaries and v2 binaries agree on every v1 block) and executed there, except
   on `blockula-1`, which takes v3 (§1.7).
3. **Hooks that are inert without programs** (each is a lookup that finds nothing, adds 0, or
   returns at once):
   - admission's refusal of a transaction whose `from` is a program (§2.1);
   - the native-credit guard at the four credit sites (§7.1);
   - the fuel term of the fee (0 for every transaction but tags 62–64, §5.1);
   - the block's fuel counter (stays 0; `count_fuel` writes nothing for a reservation of 0);
   - the fuel controller's end-of-block step (returns while META is absent, §5.6);
   - `econ::charge_new_accounts` skipping accounts a program opened (none are);
   - `ParamChange` routing of `program_*` keys to programs' map (`programs::is_program_param`,
     false while `Dormant`; until META exists no such change gets past admission, item 1, §4.5);
   - the `commit_block`, `apply_block` and block-loop fault checks (no fault without program
     execution, §6.2).
4. **Non-consensus:** `SUPPORTED_PROTOCOL_VERSION = 2`, so blocks signal 2 (W2); the mempool's cap
   on pending uploads (§2.2); the quarantine, strikes and verified-clean set (empty without
   program execution, §6.2); proposal order by tip per share of the scarcer resource (the same
   order as today's tip per byte for every transaction that reserves no fuel, §5.6); the replay cache (off until programs first
   switch on, §6.6); the `ApplyMode` rename (§6.4); the new API routes (§8).

Each inert hook is still gated on `status`, not on "no programs exist", so a later refactor can't
wake one early (PRG 14.4's rule). P2's replay gate ([REPLAY.md](REPLAY.md)) proves the whole list against
testnet-2's archived history: every recorded root, every outcome kind.

### 1.3 Switching programs on

`upgrade::exec_activate_feature` (`upgrade.rs:418-428`; on W2 it's `(state, j, chain_id, feature,
height)` and refuses a feature `known_features(version_at(state, height))` doesn't list, and
whatever `check_may_activate` refuses at that version: `upgrade.rs:496-519` at `06c35fd`) gains one line after it
inserts the feature: `if feature == PROGRAMS_FEATURE { crate::programs::on_activate(state,
Some(j)); }`. It's idempotent:
- **First activation** (META `programs` absent): inserts `ProgramsMeta` (§4.2) with the default
  parameters (§4.5), `fuel_rate` at the floor, zero counters, no pause. Journaled in
  `j.programs`, so a proposal whose execution fails leaves nothing.
- **Reactivation** (META present: programs were on, then switched to exit-only): nothing. The
  feature flag being back is what lifts exit-only. Parameters, counters, the fuel rate, programs
  and any pause are kept as they are.

`ActivateFeature` runs in `governance::end_block` of the block `h` it executes in, after that
block's transactions, so program transactions are admitted from `h + 1`.

W2's stage L (`06c35fd`) gave `check_may_activate(feature, chain_id, version)` the version in
force (`version_at(state, runs_at)` at `prepare_kind`, W2 `governance.rs:590`;
`version_at(state, height)` in `exec_activate_feature`) and the rule of §1.7.

### 1.4 Switching programs off: exit-only

`DeactivateFeature { programs }` (simple majority, or the council in year 1, `governance.rs:722-758`)
runs the existing `exec_deactivate_feature` (`upgrade.rs:431-438`), unchanged: it removes the flag.
META `programs` stays. **Exit-only is "META present, feature off"**: §7.3's rules. Nothing is
deleted, and switching programs back on takes ⅔ again (`ActivateFeature`'s supermajority).

### 1.5 Genesis

- **A chain made with programs** (`blockula summon --programs`, `blockula dev --programs`). Its
  genesis has `upgrades: [{ "version": 2, "activation_height": 1 }]` and `features: ["programs"]`
  (`SummonOpts::with_programs`); without the flag `summon` writes neither and the chain is a v1
  chain that can switch programs on later (§1.3). W2's `Genesis::check` validates `features`
  against block 1's version (`upgrade::check_known_feature(f, version_in(&upgrades, 1))`), so a
  chain accepts `programs` in its genesis exactly when it is born at v2.
- **A genesis-listed feature when block 0 is v1.** The genesis block executes nothing and keeps
  `GENESIS_PROTOCOL_VERSION = 1` (`consensus.rs:110`). `Genesis::initial_state` inserts every
  listed feature with `activated_at: 0` (`chain.rs:380-382`); right after that loop it calls
  `crate::programs::on_activate(&mut state, None)` when `programs` is listed, so META
  `programs` is part of the genesis state and its root. The first transaction that can see
  programs is in block 1, which runs v2. Because of A5, the first upload can land in block 1, the
  first create in block 2 and the first call in block 3.
- **A chain born at v2 without `programs`** has no program state until `ActivateFeature {
  programs }` executes.
- **Dev and test chains** get the flag that writes the two fields (`--programs`; the simulator
  writes them itself, §10.2); they run the released v2 (W2 keeps the preview version, v3, for
  unreleased rules).
- **`blockula-1`** (a rule the code has, inert for every other chain id). Its genesis has either
  no `upgrades` or exactly `[{ "version": 2, "activation_height": 1 }]`, the only schedules W2's
  `check_genesis_upgrades` accepts for it, and `features: []`: `Genesis::check` refuses `ghost`
  there and, since stage L, `programs` too ("blockula-1 starts with programs off: this release
  doesn't carry them, and governance switches them on from protocol v3"; the message's first
  clause is older than the integration and is kept as the code has it). One genesis state for
  every binary, whatever it carries (§9.3).

### 1.6 Testnet-2

`blockula-testnet-2` has no schedule, so it runs v1 forever on this binary: `status` is `Dormant`
at every height, every hook is inert, and its state, roots, snapshots and store are byte for byte
what v0.5.2 writes. It can only meet programs through a later `ScheduleUpgrade { 2 }` and
`ActivateFeature { programs }`, which this binary supports exactly as a chain born at v2.

### 1.7 `blockula-1`: programs by governance only from v3 (a rule the code has)

This section describes a rule the code keeps for one chain id. It is inert for every other id,
which includes every chain the launch kit makes, and it is kept as it is (§0, §9.3).

`check_may_activate` (W2's, `upgrade.rs`, beside its Ghost Mode rule; stage L, landed on W2's
branch as `06c35fd`) refuses `ActivateFeature { programs }` on `blockula-1` while the version
in force is below `MAINNET_PROGRAMS_ACTIVATION_VERSION` (3, `const`-asserted above
`MAINNET_GENESIS_VERSION`): "blockula-1 switches programs on by governance only from protocol v3:
v2's program rules run there only if its genesis lists them". It's checked when the proposal is
made (at `runs_at`'s version, so an `ActivateFeature` may be proposed to run after a scheduled v3;
W2's doc comment says why reading a later block's version is safe for this answer, which every
binary that runs `blockula-1` gives the same, unlike `known_features`, read at the proposal's own
block since `1653478`) and when it runs. It covers a first activation and a reactivation after
exit-only alike. And since no genesis of `blockula-1` may list programs on this release (§1.5),
"only if its genesis lists them" never happens there: v2's program rules never run on
`blockula-1`.

W2's tests that asserted the opposite were rewritten with it:
`upgrade::tests::mainnet_never_switches_ghost_mode_on` lost its `(programs, blockula-1)` case to
the new `mainnet_switches_programs_on_by_governance_only_from_v3` (refused under v2, first
activation and after exit-only alike; on from v3; dry-run and dev chains under v2); in
`genesis_builder::mainnet_tests`,
`the_council_upgrades_from_block_1_and_switches_programs_on_under_v3` (formerly
`…_switches_programs_on_from_block_1`),
`ghost_mode_and_v2_programs_stay_off_on_mainnet_whatever_the_council_votes`,
`on_mainnet_programs_may_be_proposed_to_run_after_a_scheduled_v3` (refused while it would run
before v3's height, accepted from the block whose proposals run there, and failing when it runs if
v3 was withdrawn meanwhile), `the_council_proposes_and_votes_through_a_node` (the programs
proposal refused at `submit`), and for the genesis `mainnet_is_born_at_v2_with_programs_off`
(programs at genesis only on a dry run) and `a_hand_edited_mainnet_genesis_is_refused`.

Why it is a rule in code, and why this one:
- v2's program rules are whatever the binary carries (§9.2). The rule was written for a chain
  that would run them before any audit had cleared them: the genesis leaves `programs` off, and
  without this rule the bootstrap council could still switch on exactly the rules the audit
  hadn't cleared, by one vote, or switch them back on after a `DeactivateFeature` while a fix is
  pending (Review log, safety F8).
- It reads only the chain id and the version in force, so every binary that will ever run
  `blockula-1` applies it the same way. A rule a fix release would add (a constant raised in a
  new binary) would split the chain at the first activation between nodes on the old and the new
  binary.
- Its cost, for a chain that has this id: switching programs on takes a release that supports v3
  (which may change nothing else) and a `ScheduleUpgrade { 3 }`, about 7 days at the governance
  floors (§7.4), before the council's `ActivateFeature`. The version is then the on-chain record
  that the rules it runs were cleared.
- Every other chain (testnets, dry-run chains, dev chains, a chain of the launch kit) may switch
  programs on under v2.

From v3 on, the rule is an operating one: the council doesn't activate programs under a version a
known fix supersedes, or it schedules the fix to land before the activation runs. No later
version can add a rule like this one for itself ("activation on `blockula-1` only from vN"):
`Propose` reads `check_may_activate` at `runs_at`'s version and execution at the block's, so
keyed on the version in force at the block it never refuses (where vN is in force the version
isn't below N), and keyed any other way it refuses, while v(N−1) is in force, what older binaries
accept: a split, like raising `MAINNET_PROGRAMS_ACTIVATION_VERSION`. W2's `check_may_activate` doc
comment says so, and `upgrade::tests::mainnet_switches_programs_on_by_governance_only_from_v3`
pins the answer for every version from v1 to 16 past the preview one (stage L's review, L-1,
fixed in `df74f52c`).

**W2's other `blockula-1` rule, and programs.** On `blockula-1` (keyed on the chain id, like this
one), a validator's council seat follows its owner's vote over its consensus key's
(`governance::owner_outranks_key`, W2 `governance.rs:688`): the consensus key is hot and its
account holds the operator float, so a leaked key can't replace a council vote the owner cast.
It applies to programs' governance exactly as to any proposal, with nothing to add here:
`ActivateFeature` and `DeactivateFeature { programs }`, the `program_*` `ParamChange`s the
council may pass (§4.5) and the fix path's `ScheduleUpgrade` (§7.4). It doesn't touch
`ProgramsPause`, which is no vote: both of its signing forms (§2.5) count once, for the same
validator (`staking::validator_for_signer`), so a leaked consensus key can add its validator's
one pause signature (and spend that validator's window, §7.2), never a second.

---

## 2. The v0 actions

### 2.1 What every program action shares

**Encoding conventions** are `Action::encode`'s (`types.rs:797-1084`), as PRG 8.1 states them:
- `u8`; `u64` little-endian (`put_u64`); `bool` one byte, 0 or 1 (`Reader::bool`);
- `str`: u16 little-endian length, then UTF-8 (`put_str`, `Reader::str` with a per-field maximum);
- `bytes`: **u32** little-endian length, then raw bytes. New helpers `put_bytes` and
  `Reader::bytes(field, max)`; the reader refuses a length past `max` before taking anything
  (ghost bundles already write a u32 length inline, `types.rs:592-596`);
- `opt_str`: `""` for none (`Reader::opt_str`); `opt(x)`: a presence byte, 0 or 1, then `x`;
- `hex64`: a `str` of exactly 64 lowercase hex characters;
- `addr`: a `str` holding an address (never a name: `check_address`, `types.rs:2170`) whose prefix
  `Tx::check_prefix` checks against the chain's (`types.rs:2006-2015`, through `addresses()`).

**JSON** follows the existing `Action` serde form (`#[serde(tag = "type", rename_all =
"snake_case", deny_unknown_fields)]`): byte fields are lowercase hex (`HexBytes`), amounts
(`value`, `storage_limit`) are decimal strings (`u64_str`), `fuel_limit` and
`upgrade_delay_blocks` are JSON numbers bounded by `MAX_JSON_INT` (`json_ints`,
`types.rs:1547-1567`), options are `null`, and `policy` is `"open"` or `"additive"`.

**Signers: owner keys only, from an ordinary account.** No new rule is needed for it:
- a session key signs only `Transfer` and a Familiar's `SubmitClaim` (`chain.rs:1275-1285`), so
  it can't sign a program action;
- a validator's consensus key acting for its owner signs only the system actions
  `staking::system_tx_key` lists (`staking.rs:1963-1975`), and no program action is one;
  `ProgramsPause` is signed with a key that's on the signing account (§2.5). That refusal is
  `State::admit`'s alone: `programs::admit` by itself resolves the key to its validator
  (`validator_for_signer`) and admits a pause from it. A consensus key on the validator's own
  account is that account's original key, so it signs whatever an owner key may (tags 62–64 and
  69, a tip included);
- a multisig account's owner keys only propose and approve (`governance::check_multisig_admission`,
  `governance.rs:1295-1303`), and no program action nests (below), so multisig accounts don't use
  programs in v0.

**No nesting.** `Action::decode_at` refuses tags 62, 63, 64 and 69 when `nested` ("a multisig
proposal can't hold a program action"), beside its refusals of 43 and 50–54
(`types.rs:1177`, `:1185`).

**Accounts the action names** (`Action::addresses()`, `types.rs:1634-1648`) are each call's
`program` and each binding's (tags 63, 64) and a create's `authority`: so `check_prefix` holds
them to the chain's prefix, and the block's history rows index a call under the programs it
names (`State::named_by`, `chain.rs:1052-1054`).

**A program never sends a transaction.** While programs are not Dormant, `programs::admit`
refuses any transaction, of any tag, whose `from` is a program address ("programs have no keys");
while they're Dormant there are no programs to find. `State::admit`'s key check refuses such a
transaction earlier anyway (no key is on a program's account), so this rule is defence in depth,
as is the address: a program address is a KDF output, so no key hashes to it.

**Digests** (`node/src/programs/digest.rs`; the TS SDK mirrors all five, §8.4), BLAKE3's
`derive_key` as `types.rs:429` uses it:

```
wasm_hash      = derive_key("blockula program code v1", wasm)
interface_hash = derive_key("blockula program interface v1", interface)
version_digest = derive_key("blockula program version v1", str address ‖ wasm_hash ‖ interface_hash ‖ version u32 LE)
address        = render(prefix, derive_key("blockula program address v1", creator's 32 address bytes ‖ nonce u64 LE))
selector       = derive_key("blockula program method v1", method name)[0..8]
```

`str address` is the address as `put_str` writes it. `version` is 1 for every v0 program (only
tag 65 would raise it). `nonce` is the creating transaction's. The selector has one home, the
VM's `blockula_vm::interface::selector` and `SELECTOR_CONTEXT` (§3.4), which interface decoding
needs for its distinct-selectors rule (§3.2); `digest.rs` re-exports both and defines the other
four, never a second selector (V-2).

**Who pays what** (§5 has the fee): a program transaction's `from` pays its fee (bytes and fuel,
burned, plus the tip to the proposer), attached `value`, and the deposits it bonds, at most
`storage_limit`. A create also burns `account_creation_fee` when it opens its program's account.
Locked FANG pays only the burned base fee: value and deposits are debited in the action phase and
`econ::check_locks` holds them to transferable FANG (`chain.rs:1376-1383`), as for any action.

**One step per block.** Because of A5, and because the mempool never runs a program or writes a
record (§6.4), a sender can't queue a create behind its own pending upload, or a call behind its
own pending create: the mempool judges each against the executed state, where the code or the
program isn't usable yet, and refuses it with the height it becomes usable from ("code uploaded
at block N is usable from block N + 1"). Clients wait for the upload to finalize and for one more
block before they sign the create, and the same before the first call (§8.3, §8.4).

### 2.2 `UploadCode` (tag 62)

```
62 ‖ bytes wasm ‖ bytes interface ‖ u64 storage_limit
```
```json
{ "type": "upload_code", "wasm": "0061736d…", "interface": "01…", "storage_limit": "30000000000" }
```

**Stateless (`check_format`):**
- `wasm`: 8 to `MAX_CODE_BYTES` (262,144) bytes, starting with the Wasm 1.0 header
  `00 61 73 6d 01 00 00 00`;
- `interface`: 1 to `MAX_INTERFACE_BYTES` (16,384) bytes that decode and check as an interface
  (§3.2): cheap, so a malformed one never reaches admission;
- `Tx::max_bytes` (`types.rs:1942`) returns `MAX_CODE_TX_BYTES` = 320 KiB for this tag (the CBlock
  codec's 1 MiB per tx, `consensus.rs:246-249`, and the HTTP body limit, `api.rs:317`, already
  allow it).

**Admission** (`programs::admit`): `status` is `On` (§1.1), and its reservation fits the block
(§5.4); nothing else is state-dependent.
**Mempool policy** (not consensus, A38): a node holds at most `MAX_MEMPOOL_UPLOADS` = 16 pending
uploads and refuses the 17th ("too many pending uploads; try again shortly"), and refuses an
upload larger than the next block's `max_block_bytes` (a parameter whose floor, 64 KiB, is below
`MAX_CODE_TX_BYTES`), which no proposer could include (`consensus.rs:651`). It never validates or
translates the code: a proposer's trial does that, under §6.2's rule (a fault there leaves the
upload out, quarantines it and holds its `wasm_hash` and its sender's program transactions for a
while).

**Execution** (`programs::upload`, Block mode; in Mempool mode §6.4):
1. `wasm_hash` and `interface_hash` (§2.1).
2. If the code is stored already, nothing is validated: its exports are its `CodeRecord`'s. (An
   upload of a pair that's stored already only refreshes it, step 6, once step 4 holds.)
3. If the code is new, on the transaction's large-stack thread (§6.1): W1's
   `vm::upload::check(wasm, &schedule) -> Result<Result<ModuleInfo, Refusal>, NodeFault>`, which
   runs `validate::validate` (refuses floats, `memory.grow`, start and custom sections, imports
   outside ABI v1, more than 64 `bk_` exports, …) and then a test translation on a throwaway
   engine, under its own `catch_unwind`; `testing::MemHost::upload` runs the same function.
   - `Ok(Ok(info))`: the code is valid; go on.
   - `Ok(Err(Refusal::Invalid(e)))`: the transaction fails, charged, receipt `invalid_code` with
     `e.rule.number()` (W1's `Rule`, `#[repr(u8)]`, 1–14, pinned by
     `validate::tests::rules_keep_their_numbers`).
   - `Ok(Err(Refusal::Translate(_)))`: fails, charged, receipt `translation_refused`.
   - `Err(NodeFault::Panic)` (a panic in validation or translation): a node fault tied to this
     transaction; `Err(NodeFault::Engine(_))` (the throwaway engine couldn't be built): a node
     fault of the setup kind (§6.2). Never an outcome.
4. The pair must match, whichever half is new (or neither): the interface's method names are
   exactly the code's `bk_` exports with the prefix removed (the new code's `ModuleInfo`, or the
   stored `CodeRecord.exports`). Otherwise it fails (`interface_mismatch`).
5. Deposits, code first, then interface, for each blob not yet stored: `size × price(used)`
   (§4.6), where `used` is program state's bytes **once that blob lands**: `state_bytes`, plus
   the blobs this upload added before it, plus its own size. Past the ceiling there's no price
   and the upload fails (`state_full`); if the deposits add up to more than `storage_limit`, it
   fails (`storage_limit`).
6. Debit the sum from `from`, add it to `escrow`, add the sizes to `state_bytes`, write each new
   `CodeRecord` / `InterfaceRecord` and its `BlobRef { uploader: from, deposit, size, refs: 0,
   uploaded_at: h, last_used_at: h }` (§4.2). A blob that was already stored moves its
   `last_used_at` to `h` only (so a later `RemoveCode` waits a day, PRG 7.4).

**Fee:** bytes, plus fuel for `exec = SCHEDULE_V0.upload_fuel(wasm.len(), interface.len())` with
no load (§5.2): at most 19,825,488, which W1 `const`-asserts is ≤ `MAX_TX_FUEL`. An upload's
`exec` isn't held to `program_max_tx_fuel` (that parameter bounds a signed `fuel_limit`; an
upload signs none), only to the block's budget (§5.4), whose lower bound fits the largest one
(§4.5). **State:** namespaces 67, 68, 72; META `escrow`, `state_bytes`.

### 2.3 `CreateProgram` (tag 63)

PRG's encoding, unchanged:

```
63 ‖ hex64 wasm_hash ‖ hex64 interface_hash ‖ opt_str authority ‖ u64 upgrade_delay_blocks
   ‖ u8 policy ‖ bool accepts_transfers ‖ opt(str method ‖ bytes input ‖ u64 value) init
   ‖ u64 fuel_limit ‖ u64 storage_limit ‖ u8 m ‖ m × binding
binding = addr program ‖ hex64 version_digest
```
```json
{ "type": "create_program", "wasm_hash": "…", "interface_hash": "…", "authority": null,
  "upgrade_delay_blocks": 0, "policy": "open", "accepts_transfers": false,
  "init": { "method": "init", "input": "…", "value": "0" },
  "fuel_limit": 2000000, "storage_limit": "500000000",
  "bindings": [{ "program": "boo1…", "version_digest": "…" }] }
```

**Stateless:**
- both hashes `hex64`; `authority` none or an `addr`; `policy` 0 (`open`) or 1 (`additive`);
- `authority` none ⇒ `upgrade_delay_blocks` = 0 and `policy` = 0 (an immutable program has one
  encoding);
- `init` none ⇒ `fuel_limit` = 0 and no bindings (no code runs at creation; this and the rule
  above are A37's);
- `init` some ⇒ `method` an identifier (`[a-z][a-z0-9_]{0,31}`), `input` ≤ `MAX_INPUT` (32 KiB),
  1 ≤ `fuel_limit` ≤ `MAX_TX_FUEL` (20,000,000: W1's `meter::MAX_TX_FUEL`, the code bound of
  `program_max_tx_fuel`);
- 0 ≤ m ≤ `MAX_BINDINGS` (16) bindings, distinct programs, each `version_digest` a `hex64`
  (64 zeros means "any version").

**Admission** (state at `h`):
- `status` is `On` (exit-only refuses creates);
- both blobs are stored and `uploaded_at < h` (A5), and they match: the interface's methods are
  the code's stored exports (§2.2 step 4; they may come from different uploads);
- the interface declares an `Init` method ⇔ `init` is present, and then `init.method` is it;
  `init.value > 0` only if it's payable (§3);
- `authority` some ⇒ `upgrade_delay_blocks ≥ program_min_upgrade_delay_blocks`;
- each binding names a program with `created_at < h` (every program is active in v0: nothing
  closes one), and a non-zero digest equals its `version_digest`;
- `fuel_limit ≤ program_max_tx_fuel`; the distinct code it declares (its own `wasm_hash` and the
  bindings' code, when `init` runs) is ≤ `MAX_TX_CODE_BYTES` (512 KiB); its reservation is at most
  `program_max_block_fuel` and fits what's left of the block's (§5.4);
- the address it would create (from `from` and `tx.nonce`) isn't a program and is either absent
  or adoptable (below).

**Execution** (`programs::create`):
1. The address, as admitted. **Adoption:** if an account exists there, it must have no keys, no
   locks and nonce 0 (FANG someone sent to the address before the create ran). Its balance joins
   `value_held` (§7.1). Otherwise the create fails. Adoption exists so that a balance sent ahead,
   by mistake or to front-run a create seen in the mempool, can't block it, **not as a way to
   fund a program**: the address is derived from the creating transaction's nonce (§2.1), so if
   the create fails after admission (an `init` trap or abort, `value_cap`, `state_full`,
   `storage_limit`, `native`), that nonce is spent, no later create reaches the address, and
   whatever was sent there is stranded in a keyless account for good. Clients never send FANG to
   a program address before its create is final (§8.3, §8.4); a program is funded through
   `init.value`, or by `Transfer` once it exists and `accepts_transfers`. A salted address that a
   failed create could retry is a new tag in a later version (§9.1), not a change to tag 63.
2. The record deposit: `RECORD_BYTES` (256) × `price(used)`, where `used` is program state's
   bytes **once the record lands** (`state_bytes + RECORD_BYTES`), bonded from `from`; it counts
   against `storage_limit` (A7), so `init`'s writes get `storage_limit − record deposit`. Past the
   ceiling there's no price (`state_full`); a record deposit alone above `storage_limit` fails it
   (`storage_limit`).
3. Write `ProgramRecord` format 1 (§4.2): `created_at = h`, `version = 1`, `authority`,
   `upgrade_delay_blocks`, `policy`, `accepts_transfers` as signed (inert, A19), zero storage
   totals (and `sponsored` 0), `record_deposit` and `record_payer = from`; add `RECORD_BYTES` to META `state_bytes`
   (so `init`'s writes are priced on top of it); both `BlobRef`s gain a
   reference (`refs += 1`); META `programs` counts it. Open the program's account if it's
   absent: `econ::charge_new_accounts` then burns `account_creation_fee` from `from` (the
   account is a touched one that didn't exist, `econ.rs:548-565`), as PRG 6.3 wants.
4. If `init` is present, run it (§6.1): one top-level call of kind `Init` to the new program,
   with `value`, its bindings as the declared set, `fuel_limit`, and the remaining storage
   limit. The program's record is already in the state the run reads, so its code resolves.
   A trap, abort, limit or failed commit check fails the whole create: the record, the account,
   the deposits and `value` all roll back with the action journal, and only the fee sticks.
5. Commit the diff (§6.1 step 6), then the value cap (§7.1) on the FANG programs control, which
   an adopted balance and `init.value` raise (and `init`'s sponsored writes leave unchanged).

**Fee:** bytes, plus fuel for `exec = fuel_limit` and `load` = the declared code (its own and
the bindings', when `init` runs; nothing when it doesn't). **State:** namespaces 66, 69 (init's
writes), 72; account; META.

### 2.4 `CallProgram` (tag 64)

PRG's encoding, unchanged:

```
64 ‖ u8 n ‖ n × (addr program ‖ hex64 version_digest ‖ str method ‖ bytes input ‖ u64 value)
   ‖ u64 fuel_limit ‖ u64 storage_limit ‖ u8 m ‖ m × binding
```
```json
{ "type": "call_program",
  "calls": [{ "program": "boo1…", "version_digest": "…", "method": "swap", "input": "…", "value": "5000000000" }],
  "fuel_limit": 400000, "storage_limit": "0",
  "bindings": [{ "program": "boo1…", "version_digest": "…" }] }
```

**Stateless:** 1 ≤ n ≤ `MAX_CALLS` (8); each `program` an `addr`; each `version_digest` a `hex64`
other than 64 zeros (a call always names the version it previewed, A37); each `method` an
identifier; Σ `input` ≤ `MAX_INPUT` (32 KiB); 1 ≤ `fuel_limit` ≤ `MAX_TX_FUEL`; bindings as for
tag 63, none equal to a call's program. The same program may appear in several calls.

**Admission** (state at `h`):
- `status` is `On`; in exit-only, only calls whose `value` is 0 (§7.3);
- each call's program exists with `created_at < h`, and its `version_digest` is the one in force;
  its interface has `method`, of kind `Call` (never `Init` or `View`), and `value > 0` only if
  it's payable;
- bindings as for tag 63;
- `fuel_limit ≤ program_max_tx_fuel`; the distinct declared code ≤ `MAX_TX_CODE_BYTES`; the
  reservation is at most `program_max_block_fuel` and fits what's left of the block's (§5.4).

The SDK and CLI never fill a binding with zeros by default (PRG 8.1).

**Execution** (`programs::call`, §6.1): run the calls in order, atomically, with the bindings as
the declared set; a program not in the set traps (`UnboundProgram`). Commit, then the value cap.
On any failure only the fee and nonce stick.

**Fee:** bytes, plus fuel for `exec = fuel_limit` and `load` = the distinct code of every declared
program. **State:** namespace 69; the accounts the diff touches; META.

### 2.5 `ProgramsPause` (tag 69)

```
69
```
```json
{ "type": "programs_pause" }
```

**Stateless:** nothing to check. **Admission:** version ≥ 2 and META `programs` present (any
`status` but `Dormant`: a pause works in exit-only too); the signer is a validator of the current
epoch's active set acting through its own account or its owner's
(`staking::validator_for_signer`, `staking.rs:1937-1951`), and may sign now (§7.2's window and
live votes, checked here so a refused signature is never charged). **Execution:** §7.2. **Fee:**
bytes only (`exec` 0).

**Three ways to sign it, and which are admitted** (S5h review S5H-R3; each pinned by
`programs::pause::tests::the_duty_lane_and_the_owner_keys_tip`). There is no tip rule of its own:
- *The consensus key, from the validator's own account* (`from` is the validator's address). The
  consensus key is a key on that account (its original key, `State::keys_of`,
  `chain.rs:1078-1085`), so it is an ordinary key there, as for the other duties the node signs
  from that account. **Admitted, and it may tip**, like any key on that account, out of that
  account's transferable FANG. It is a validator duty (`staking::duty_of` gains
  `"programs_pause"`, `staking.rs:1990-2013`) with a slot in the mempool's lane, so a full
  mempool can't keep it out. This is the form `blockula program pause` signs.
- *An owner key, from the owner's account* (`from` is the validator's owner, when that is another
  account). An ordinary owner key. **Admitted, and it may tip.** It is not a duty (`duty_of` wants
  the consensus key, `staking.rs:2000-2001`), so in a full mempool it competes by its tip.
- *The consensus key, from the owner's account* (SF-13's form, in which the consensus key signs a
  system transaction for the owner who pays). **Not admitted:** `ProgramsPause` isn't one of the
  actions `staking::system_tx_key` lists, so `State::admit` answers "this key isn't authorized on
  <owner>". (`State::admit`'s tip ban for a consensus key signing for an owner binds only that form.)

A validator that owns itself has one account and one key, so the first two forms are the same.
Both admitted forms name the same validator, so each counts once (§1.7, §7.2).
**Mempool:** see the first two forms. **State:** META `programs` (`pause`, `votes`,
`pause_signed`).

### 2.6 Reserved tags

| Tag | Name (PRG) | v0 |
|---|---|---|
| 65 | `UpgradeProgram` | not decoded |
| 66 | `ProgramAdmin` | not decoded |
| 67 | `CloseProgram` | not decoded |
| 68 | `RemoveCode` | not decoded |
| 70 | `AddScopedKey` | not decoded |
| 71–79 | further program actions | unassigned, kept for programs |

`every_tag_round_trips_through_the_wire_form` (`types.rs:2715-2731`) becomes
`(0..=54).chain(60..=64).chain([69])`, and `sdk-ts/test/tx-vectors.ts:15` likewise; a test pins
65–68 and 70 as "unknown action tag".

### 2.7 The open questions, decided

| Question (open in the v0 design) | v0 |
|---|---|
| A signed bound on upload deposits? | Yes: `UploadCode.storage_limit` (A7) |
| Does the 256-byte record deposit fall under `storage_limit`? | Yes (A7) |
| Does a create's own code count in `load`? | Yes when `init` runs (its code is translated); no when it doesn't (nothing is) |
| Refunds of record and blob deposits? | None in v0; payer and amount recorded for the later `CloseProgram` / `RemoveCode` (A18) |
| Authority? | Stored, checked against the minimum delay, inert until tag 65 (A19) |
| Receipts root in v0? | No (A10) |
| `CallProgram` with a zero `version_digest`? | Refused statelessly; bindings may use zeros |
| An interface with an `Init` method but a create without `init`? | Refused: `init` is present exactly when the interface declares one |
| Is a pause tied to one activation, and what does it do while programs are off? | It isn't tied to an activation: it holds whatever `status` is, lapses on its own, and can be signed (and renewed) in exit-only (§7.2) |
| Is storage priced before or after the write? | After: at program state's bytes once the write lands, the written bytes included, for entries, blobs and program records alike (§4.6) |
| Can a council vote switch programs on under v2 on `blockula-1`? | No: from v3 only, and no genesis of `blockula-1` lists them either (§1.5, §1.7) |

## 3. Interface format (C3)

### 3.1 What it is for

An interface describes a program's methods: what the chain enforces (each method's kind and
whether it takes value) and what tooling needs to build a call (argument and return types). The
chain never checks inputs against the argument types; a program decodes its own input (PRG
11.1). Format 1 has methods only (A16): names, docs, events, errors, named types and storage
layouts come with format 2.

Authors don't write it by hand: the CLI encodes it from JSON (§8.3) and, later, the program SDK's
macro generates it. It's uploaded beside the code, stored once under `interface_hash`, and bound
into every call through `version_digest`.

### 3.2 Bytes (format 1)

A hand-written codec, so a TypeScript port is a page of code and the bytes are the spec:

```
interface = u8 format (= 1) ‖ u8 n (1..=64) ‖ n × method       methods strictly ascending by name, bytewise
method    = ident name ‖ u8 kind ‖ u8 flags ‖ u8 a (0..=16) ‖ a × arg ‖ u8 has_return (0 | 1) ‖ [type return]
arg       = ident name ‖ type
ident     = u8 len (1..=32) ‖ len bytes matching [a-z][a-z0-9_]*
kind      = 0 Call | 1 View | 2 Init
flags     = bit 0: payable; every other bit 0
type      = u8 tag ‖ payload
            1 u8 | 2 u16 | 3 u32 | 4 u64 | 5 u128 | 6 i64 | 7 i128 | 8 bool
            | 9 fang (u64 drops) | 10 address (32 bytes) | 11 name (a .boo label) | 12 hash32
            | 13 bytes ‖ u32 max (1..=32,768) | 14 text ‖ u16 max (1..=32,768)
            | 15 option ‖ type | 16 vec ‖ type ‖ u16 max (1..=1,024)
```

Every value is Borsh-encoded in a call's input by convention (PRG 11.3), which the chain doesn't
check. Rules, all checked by `interface::decode` (stateless, so in `check_format`, §2.2):
- `format` is 1; at most `MAX_INTERFACE_BYTES` (16,384) bytes; no trailing bytes;
- 1 to `MAX_METHODS` (64) methods, names strictly ascending (so no duplicates and one encoding);
- at most one `Init` method; a `View` method isn't payable; an `Init` method has no return type;
- argument names distinct within a method; at most 16 arguments;
- types nest (`option`, `vec`) at most 4 deep: at most `MAX_NESTING` (4) `option`/`vec` layers
  around a leaf type, in any mix (`option<vec<option<vec<u8>>>>` is the deepest), and a fifth is
  refused (V-1; `interface.rs` `Reader::ty`); `max` values in range; unknown tags refused;
- method **selectors** are distinct (§2.1's `selector`): 64 names can't collide by chance, but the
  check is what lets a later version key agent grants on selectors (PRG 9.1) without a new rule.

### 3.3 The match with the code

At upload, and again at create when the two halves came from different uploads (§2.2–2.3): the
set of method names equals the set of the code's exported functions named `bk_<name>`, with the
prefix removed. W1's validation already refuses any other export but `memory`, any `bk_` export
that isn't `[] → []`, and more than 64 of them. So every method has an export, every export a
method, and a call to `bk_<method>` never meets a missing export at run time
(`TrapKind::NoMethod` stays for code that reaches a method through `call_v1` with a name the
interface doesn't have).

### 3.4 Where it lives

`node/vm/src/interface.rs` (a new module of W1's crate, added by stage V): `Interface`, `Method`,
`Type`, `decode`, `encode`, `check_exports(&Interface, &[String])`, `selector` with its
`SELECTOR_CONTEXT` (their only definition: `node/src/programs/digest.rs` re-exports them, §2.1)
and `Interface::method(name) -> Option<MethodInfo>`. It reuses what W1 landed: `host::MethodKind`
(`Call`, `View`, `Init`) and `host::MethodInfo { kind, payable }`, the type `View::method(program,
name)` returns; `config::MAX_METHODS` (64) and `config::MAX_INTERFACE_BYTES` (16 KiB); and
`abi::is_ident` for every identifier. The crate is engine-neutral and has no node types, so the
fuzzers fuzz the codec alone (`interface_codec`, §10). It needs no new dependency (`blake3` is
already the crate's, for selectors). The node keeps the JSON form
(`node/src/programs/interface_json.rs`):

```json
{ "format": 1, "methods": [
  { "name": "transfer", "kind": "call", "payable": false,
    "args": [{ "name": "to", "type": "address" }, { "name": "amount", "type": "fang" }], "returns": null },
  { "name": "balance_of", "kind": "view", "payable": false,
    "args": [{ "name": "who", "type": "address" }], "returns": "u128" },
  { "name": "init", "kind": "init", "payable": true,
    "args": [{ "name": "memo", "type": { "text": 64 } }, { "name": "owners", "type": { "vec": ["address", 8] } }], "returns": null } ] }
```

Scalar types are strings; `{ "bytes": max }`, `{ "text": max }`, `{ "option": type }` and
`{ "vec": [type, max] }` are the rest. The API serves the decoded JSON next to the raw hex (§8.2),
so v0's TypeScript clients need no interface decoder; the SDK gets a port when it builds or
verifies interfaces itself (a later release). Every identifier in it is the deployer's claim and
travels as `untrusted_*` through the MCP (PRG 9.7).

## 4. State

### 4.1 Namespaces and keys

| ns | Name | Key | Value | v0 |
|---|---|---|---|---|
| 66 | `PROGRAMS` | the program's address as a string (UTF-8 bytes, as `ACCOUNTS` keys accounts) | `ProgramRecord` | used |
| 67 | `PROGRAM_CODE` | `wasm_hash` (32 bytes) | `CodeRecord`, written once | used |
| 68 | `PROGRAM_INTERFACES` | `interface_hash` (32 bytes) | `InterfaceRecord`, written once | used |
| 69 | `PROGRAM_STORAGE` | the program's 32 address bytes ‖ the program's key (1–128 bytes) | `Entry` | used |
| 70 | `PROGRAM_RECEIPT_ROOTS` | | | reserved (A10) |
| 71 | `KEY_GRANTS` | | | reserved (tag 70) |
| 72 | `PROGRAM_BLOB_REFS` | kind (0 code, 1 interface) ‖ hash (32 bytes) | `BlobRef` | used |
| 73 | `PROGRAM_DUE` | | | reserved (tag 65) |
| 74–79 | | | | reserved for programs |
| META `programs` | | | `ProgramsMeta` | present once programs first switched on |

`state/mod.rs`'s `ns` gains the constants (after `STAKE_VIEWS = 65`, `state/mod.rs:99`) with a
comment reserving 70, 71, 73–79; `meta` gains `PROGRAMS = b"programs"`. `state/codec.rs` gains
`ByteKey(Vec<u8>)`, a raw, order-preserving `RecordKey` (next to the `String`, `u64` and
`[u8; 32]` impls, `state/codec.rs:72-98`) for namespaces 69 and 72. A program's storage keys are
contiguous under its address, so the API scans them by prefix.

### 4.2 Records

**Every value starts with a format byte**, then the Borsh body (`state/codec.rs` conventions:
little-endian, u32-length strings and vectors, one-byte option and enum tags, strict decoding).
A generic `Versioned<T>` wrapper in `node/src/programs/state.rs` writes `1u8` then `T`, and its
decoder refuses any other first byte ("program record format N, this binary reads 1"). A later
version adds format 2 beside it (§9). Field order below is the encoding order.

```rust
/// ns 66, format 1.
pub struct ProgramRecord {
    pub creator: String,                 // the account whose tx created it
    pub created_at: u64,                 // callable from created_at + 1 (A5)
    pub wasm_hash: [u8; 32],
    pub interface_hash: [u8; 32],
    pub version: u32,                    // 1; only tag 65 raises it (it's in version_digest, §2.1)
    pub authority: Option<String>,       // stored, inert until tag 65 (A19)
    pub upgrade_delay_blocks: u64,       // 0 when authority is None
    pub policy: u8,                      // 0 open, 1 additive (inert)
    pub accepts_transfers: bool,         // plain Transfers may credit it (§7.1)
    pub items: u64,                      // its storage entries
    pub bytes: u64,                      // Σ their capacities
    pub deposit: u64,                    // Σ their deposits
    pub sponsored: u64,                  // Σ the deposits of those it paid itself (SPONSOR, §6.1), ≤ deposit
    pub record_deposit: u64,             // RECORD_BYTES × price at creation
    pub record_payer: String,            // who bonded it (the creator)
}
// No pending upgrade, offered authority or status: tags 65–67 bring them in format 2 (A19).
// Every program is active in v0.

/// ns 67, format 1. Written once at upload; never rewritten (its BlobRef carries what changes).
pub struct CodeRecord {
    pub wasm: Vec<u8>,                   // ≤ MAX_CODE_BYTES (256 KiB)
    pub exports: Vec<String>,            // its method names (bk_ exports, prefix removed), ascending
}

/// ns 68, format 1. Written once. In memory it also holds the decoded `Interface` (decoded when
/// the record is inserted or loaded), so admission never parses an interface; only `bytes` is
/// encoded.
pub struct InterfaceRecord {
    pub bytes: Vec<u8>,                  // ≤ MAX_INTERFACE_BYTES (16 KiB), format 1 (§3)
}

/// ns 69, format 1: W1's overlay `Entry`, as stored.
pub struct Entry {
    pub value: Vec<u8>,                  // ≤ 16 KiB
    pub payer: [u8; 32],                 // whose deposit this is: refunded to it, exactly
    pub deposit: u64,                    // fixed when written (PRG 6.3)
    pub capacity: u32,                   // key + value + 96, rounded up to a multiple of 64
}

/// ns 72, format 1.
pub struct BlobRef {
    pub uploader: String,                // who paid the deposit
    pub deposit: u64,
    pub size: u32,                       // the blob's length
    pub refs: u32,                       // programs whose record cites it (pending upgrades will too)
    pub uploaded_at: u64,                // usable from uploaded_at + 1 (A5)
    pub last_used_at: u64,               // moved by a re-upload of the same bytes (PRG 7.4)
}

/// META `programs`, format 1.
pub struct ProgramsMeta {
    pub params: BTreeMap<String, u64>,   // §4.5: a sorted map, so a later version adds keys freely
    pub programs: u64,                   // how many program records exist
    pub state_bytes: u64,                // code + interfaces + entry capacities + RECORD_BYTES per program
    pub value_held: u64,                 // Σ balances of program accounts (§7.1)
    pub sponsored: u64,                  // Σ records' `sponsored`: escrow programs paid (§7.1)
    pub escrow: u64,                     // Σ deposits: entries, blobs, program records
    pub fuel_rate: u64,                  // milli-drops per fuel, moved each block (§5.6)
    pub pause: Option<Pause>,            // { from: u64, until: u64, started: u64, signers: Vec<String> } (§7.2); kept after it lapses
    pub votes: BTreeMap<String, u64>,    // validator → height of its pending pause signature (§7.2)
    pub pause_signed: BTreeMap<String, u64>, // validator → height of the last pause it started or renewed
}
```

Addresses in node records are strings (`creator`, `uploader`, `record_payer`, validators), as
everywhere in the node's state; `Entry.payer` is 32 bytes because it's the VM's own type
(`vm::overlay::Entry`), rendered with the chain's prefix (`crypto::render_address`,
`crypto.rs:46`) when a refund credits it.

### 4.3 The module, and what isn't a record

`node/src/programs/state.rs` holds the `SubState` (`state/mod.rs:157-171`), following
`ghost.rs:147-171`:

```rust
pub struct Programs {
    pub records: TMap<String, Versioned<ProgramRecord>>,       // 66
    pub code: TMap<[u8; 32], Versioned<CodeRecord>>,           // 67
    pub interfaces: TMap<[u8; 32], Versioned<InterfaceRecord>>,// 68
    pub storage: TMap<ByteKey, Versioned<Entry>>,              // 69
    pub blob_refs: TMap<ByteKey, Versioned<BlobRef>>,          // 72
    pub meta: Option<ProgramsMeta>,                            // META "programs" while Some
    pub block: BlockRun,                                       // not a record (below)
    pub fault: Option<Fault>,                                  // not a record (below)
}
```

`tables()` lists the five maps; `meta_records()` emits `(b"programs", …)` only while `meta` is
`Some`; `load_meta` accepts that key. `State` gains `pub programs: crate::programs::Programs` after
`ghost` (`chain.rs:946`), and the hand-written lists grow by one: `modules()` (`state/mod.rs:213`,
6 → 7), the arrays in `tables` and `tables_mut` (`:246`, `:280`), the exhaustive destructurings
(`:221`, `:255`, `:291`) and `load_meta`'s array (`:325`, which the compiler doesn't check; a
test loads META `programs` back from its records).

**Not records** (outside the root, never in a snapshot, which is always taken at a block boundary):
- `block: BlockRun { height, fuel, receipts: Vec<(String, ProgramReceipt)> }`, the per-block
  scratch. It's keyed by height: readers take a `BlockRun` whose `height` isn't the one they
  judge as empty (fuel 0, no receipts), and the first write at another height resets it, so it
  needs no `begin_block`. The pair `(height, fuel)` is journaled (`ProgramsJournal`), so a
  rolled-back trial restores both, and a rolled-back first trial at a new height leaves the
  older height there, which readers then take as empty; `count_fuel` writes nothing for a
  reservation of 0, so no other transaction touches it. `receipts` isn't journaled (§8.1).
- `fault: Option<Fault { height: u64, tx: String, class: FaultClass, what: String }>`: set by a
  node fault (where it happened, which transaction, `Tx` (tied to that transaction) or `Setup` (a
  thread or an engine this node couldn't make), and the fault's `Display`, for logs). **Sticky
  for the life of that `State` value**, across heights and into its clones (§6.2), never
  journaled; only a proposer's trial takes it (`Option::take`) to leave the transaction out
  (§6.3).

**`ProgramsJournal`** joins `Journal` (`chain.rs:598-621`) as `programs`, with `absorb`
(`chain.rs:638-664`) and `State::rollback` (`chain.rs:1182-1208`) extended (both destructure
exhaustively, so the compiler finds them). It holds a `JMap` per table, the old `meta` and
`block.fuel`, and one field that's not an undo record, like `lock_slack`: `opened`, the accounts
a program's commit opened, which `econ::charge_new_accounts` skips (§6.1).

The node-local **quarantine**, strikes and `verified_clean` set (§6.2) aren't state at all:
they're `Chain`'s, beside the mempool.

### 4.4 Invariants

`programs::check_invariants(state)` runs from `State::check_invariants` (`state/mod.rs:498-503`,
with the other modules'), and `programs::escrow(state)` joins the supply sum
(`state/mod.rs:490`): every drop of `escrow` is held by the program module. It checks:
- META absent ⇒ every program table is empty and the `programs` feature is off;
- META present ⇒ the version the next block runs is ≥ 2; the feature on ⇒ META present;
- `escrow` = Σ entry deposits + Σ blob deposits + Σ `record_deposit`;
- `value_held` = Σ balances of program accounts; `programs` = the number of records;
  `sponsored` = Σ records' `sponsored`, and each record's `sponsored` = Σ the deposits of its
  entries whose `payer` is that program (so ≤ its `deposit`);
- `state_bytes` = Σ code sizes + Σ interface sizes + Σ entry capacities + `RECORD_BYTES` × records,
  and ≤ `2 × MAX_STATE_SOFT_BYTES` (the bound, not the parameter: lowering the parameter stops new
  growth but doesn't make the state invalid);
- each program's `items`, `bytes` and `deposit` match its entries; each entry's program exists;
  each entry's `capacity` covers its rounded size (at least `storage::capacity` of its key and
  value, a whole number of `CAPACITY_UNIT`s, at most `MAX_CAPACITY`: an overwrite that fits keeps
  the capacity it was bonded for, §4.6; `programs/state.rs:436-437` at `296a466b`) and its value
  ≤ 16 KiB;
- each record's blobs exist; each `BlobRef.refs` is the number of records citing it; its `size`
  is its blob's length; each `CodeRecord.exports` matches its code;
- program accounts have no keys, no locks and nonce 0, and every record has an account;
- `params` has only known keys, each within its bounds (§4.5); `program_max_tx_fuel ≤
  program_max_block_fuel`;
- `fuel_rate` in [1, `MAX_FUEL_RATE`] (§5.6: the floor parameter can move under it, and
  readers take `max(fuel_rate, floor)`, so the invariant is the bound, not the parameter);
- `pause`: `from < until`; `votes` and `pause_signed` name validators, with heights at or below
  the state's.

`check_bounds` (`state/mod.rs:565-586`) gains the per-entry caps, so every program record stays
within `MAX_KEY_BYTES`/`MAX_VALUE_BYTES` (`state/mod.rs:125-127`; a 256 KiB code record fits).

### 4.5 Parameters (`ProgramsMeta.params`)

A sorted `String → u64` map. Readers call `programs::param(state, key)`, which returns the map's
value or, for a key the map doesn't hold, the key's default: so a later version adds a key with
no migration. `ParamChange` writes the map.

| Key | Default | Bounds | Class |
|---|---|---|---|
| `program_value_cap` (drops) | 10,000,000 FANG | [0, `MAX_SUPPLY`] | council |
| `program_fuel_floor_rate` (milli-drops per fuel: the floor under `fuel_rate`, §5.6) | 250 | [1, 1,000,000] | council |
| `program_max_tx_fuel` | 10,000,000 | [1,000,000, `MAX_TX_FUEL` = 20,000,000] | council |
| `program_max_block_fuel` | 40,000,000 | [20,000,000, `CALIBRATED_BLOCK_FUEL` = 60,000,000] | council |
| `program_storage_price` (drops per byte) | 100,000 | [1,000, 10,000,000] | council |
| `program_state_soft_bytes` | 64 MiB (A33) | [16 MiB, `MAX_STATE_SOFT_BYTES` = 512 MiB] | council |
| `program_min_upgrade_delay_blocks` | 2 days (172,800 at 1 s) | [1 hour, 90 days], in blocks | **tokens only** |

- **Routing.** `ParamChange { key, value }` with a key starting `program_` goes to
  `programs::check_param` / `programs::set_param` when the version in force is ≥ 2 **and META
  `programs` exists**: at proposal, `prepare_kind` (`governance.rs:504-507`) reads the version in
  force at the proposal's own block, as W2 reads a feature's (`1653478`; with META present v2 is
  in force there anyway, since versions never go down); at execution, `State::exec_set_param` (`chain.rs:1705-1712`), which takes no height,
  reads it at `self.econ.height`, which is the executing block's height there (`econ::end_block`
  runs before `governance::end_block`, `chain.rs:1000-1002`). Otherwise it falls through to
  `ChainParams`, which refuses the key as unknown (`params.rs:332-335`), but no transaction
  carrying one gets that far. v1 refuses a key outside `ChainParams::KEYS` statelessly, in the
  codec (`ProposalKind::check`, `types.rs:1791-1794`). v2's codec lets programs' seven keys
  through (`types.rs:2146-2156` at `296a466b`), and admission refuses them while `status` is
  `Dormant`, in v1's codec words ("unknown parameter `{key}`"; `programs::params::admit`, which
  `programs::admit` calls for every transaction, a `Propose` nested in a `MultisigPropose`
  included, before its tag dispatch): the same effect (unadmittable, never charged, a block
  carrying one invalid on both binaries), in a different place (§1.2 item 1). Any other
  `program_*` key the codec still refuses, as v1 does. So a program parameter can't be proposed
  before programs were first switched on: the council
  proposes them after the `ActivateFeature` executes, and the defaults below run for at least one
  vote and timelock (4 days at the governance floors of 3 days' voting and a 1-day timelock; 9 days
  at the default periods) after programs first switch on, from block 1 with programs on at
  genesis. GOVERNANCE.md and OPERATIONS.md explain this before activation; `/api/params`
  exposes the program parameters only after META exists. Accepting the keys
  earlier would change v2 while programs are off (§1.2).
- **Classes.** `params::tokens_only` (`params.rs:259-261`) becomes `!COUNCIL_KEYS.contains(key)
  && !programs::COUNCIL_PARAMS.contains(key)`: every program key but the minimum delay is the
  council's, as PRG 14.2 says, and `needs_supermajority` and `council_may_pass`
  (`governance.rs:722-758`) follow from it. `/api/params` lists them under `programs` with their
  class.
- **Upper bounds are code constants**, raised only by a protocol version after re-measurement
  (PRG 14.2's rule): W1's `meter::MAX_TX_FUEL` and `meter::CALIBRATED_BLOCK_FUEL`, the rest in
  `node/src/programs/params.rs`. `program_max_tx_fuel ≤ program_max_block_fuel` always holds by
  the bounds (20M ≤ 20M), and the block's lower bound fits the largest upload (19.8M, §2.2). A
  transaction's **reservation** can still exceed the block's budget (`fuel_limit` plus up to
  21.2M of load against a budget as low as 20M); admission refuses it as "larger than a block"
  (§5.4), and clients check `reserved ≤ max_block_fuel` before signing (§5.5, §8.4).
- **Timing.** A change applies from the block after the one it executes in, like any `ParamChange`.
  There's no 17-block lead (A6): pending transactions it prices out are dropped by the mempool's
  post-block check, and wallets size fees from `/api/fees` with margin.

### 4.6 Deposits and the price curve

PRG 6.3 as written, with v0's numbers and A34's pricing point:
- `price(used) = program_storage_price × 2^⌊8 × used / soft⌋` drops per byte, `soft` =
  `program_state_soft_bytes`: W1's `storage::price(base, soft, used)`, which the node calls with
  the two parameters and nothing else computes. `used = 2 × soft` is still priced; a write or
  upload that would take `state_bytes` past `2 × soft` is refused (`TrapKind::StateFull` inside
  a call; `state_full` for an upload or a create). At the 64 MiB default the ceiling is 128 MiB
  accounted, a few hundred MiB of RAM, which even testnet-2's 2 GB machines carry; at the 512 MiB
  bound it's 1 GiB accounted (about 3 GiB of RAM, inside a 16 GB validator). A
  `const` assertion in `programs/params.rs` keeps `2 × MAX_STATE_SOFT_BYTES` plus 1 GiB of native
  headroom under `snapshot::MAX_STATE_BYTES` (4 GiB, `snapshot.rs:93`), as PRG 14.2 asks.
- **`used` is program state's bytes once the write lands, the written bytes included**, for
  every deposit: an entry (W1's overlay: committed `state_bytes` plus what the transaction's
  layers added so far plus this write's growth, `overlay.rs` `write` → `used(view, grows)`), an
  upload's blob (`state_bytes` plus the blobs the upload added before it plus its size, §2.2) and
  a program record (`state_bytes + RECORD_BYTES`, §2.3). Prices are fixed when bonded, in
  execution order, and never re-priced at commit. The node's `ProgramView::storage_price(used)`
  is `storage::price(program_storage_price, program_state_soft_bytes, used)` and nothing else.
- Entries: capacity `storage::capacity(key, value)` = `round_up(key + value + 96, 64)` (W1's
  `ENTRY_OVERHEAD`, `CAPACITY_UNIT`); an overwrite within capacity bonds nothing;
  the signer pays within `storage_limit`, or the program with `SPONSOR`; refunds go to the
  recorded payer, exactly.
- Blobs: `size × price(used)` at upload (§2.2), refunded by a later `RemoveCode`. Program records:
  `RECORD_BYTES` (256) × `price(used)` at create, refunded by a later `CloseProgram` (A18).
- At the defaults a 64 KiB program (code and interface) bonds about 6.6 FANG and its record about
  0.026 FANG; storage is 0.1 FANG per KiB until the first eighth of the soft cap fills.

### 4.7 Schema, snapshots and loading (testnet-2 untouched)

- **`SCHEMA_VERSION` stays 9** (`state/codec.rs:55`), and so do the three equality checks that read
  it (`state/mod.rs:319-322`, `snapshot.rs:174-177`, `chain.rs:3899`). Its comment gains: "Records
  a feature adds when it first switches on, in namespaces and META keys no earlier state has, each
  value led by its own format byte, don't change it (programs, protocol v2); re-encoding a record
  that exists does."
- **Why that's enough.** A state without program records (testnet-2, a v2 chain with programs
  dormant) has exactly v0.5.2's records, encodings and root. A state with them has records only
  this binary knows, in namespaces `load_records` refuses on older binaries ("unknown record
  namespace 66", `state/mod.rs:440`), and those binaries can't run a v2 chain anyway (they halt at
  block 1, or refuse `blockula-1`'s genesis outright).
- **Snapshots** stream every record through `for_each_record` (`snapshot.rs:129-189`), so program
  records are included with no change, under the same schema-9 header; a snapshot taken before
  activation restores into a state that activates later.
- **The store** (`store.rs`) keeps `STORE_FORMAT = 3` (`store.rs:80`): records are records, and
  receipts ride in the block log (§8.1), not in `BlockMeta`.
- **Light-client proofs** (`/api/proof/{ns}/{key}`, `api.rs:1496-1513`) serve namespaces 66–69 and
  72 with no change (PRG 6.4).

## 5. Fuel and fees

### 5.1 One charge, known at admission

Compute is a second base-fee dimension, charged on what a transaction reserves, never refunded
(PRG 5.1, D5). Its price is META's `fuel_rate`, which congestion moves (§5.6, A6):

```
exec(tx)    = UploadCode:     upload_fuel(len(wasm), len(interface)) = UPLOAD_BASE + UPLOAD_PER_BYTE × (len(wasm) + len(interface))
            | CreateProgram:  fuel_limit                  (0 without init)
            | CallProgram:    fuel_limit
            | anything else:  0
load(tx)    = Σ over the distinct code hashes D(tx), in force at h: SCHEDULE_V0.load_fuel(size) = LOAD_BASE + LOAD_PER_BYTE × size
              D(CallProgram)   = the code of every call's program and every binding
              D(CreateProgram) = its wasm_hash and the bindings' code, if init runs; else none
              D(anything else) = none
reserved    = exec + load                                  ≤ MAX_RESERVATION (a const assertion, 5.3)
rate        = max(fuel_rate, program_fuel_floor_rate)      (META, as the state the tx executes on holds it, §5.6)
fuel_fee    = rate × reserved / 1000                       (u128, floored; < 2^64 since rate ≤ MAX_FUEL_RATE)
base        = econ::base_fee(h, size) + fuel_fee           (bytes, as today, plus fuel)
charge      = base + tip                                   ≤ tx.fee, at admission and execution
tip         ≤ tip_cap_multiple × base                      (audit F01, exact)
fee phase:    debit charge from `from`; burn base; tip to the proposer       (chain.rs:1352-1373, unchanged)
```

`charge = base + tip` is checked addition: a charge past 2^64 − 1 is refused (fee too low) and
`econ::charge` returns `None`, never a wrapped value; `max_tip` saturates at `u64::MAX`. At both
rate ceilings and the 1,000× tip cap a call (at most 41.2M fuel reserved) stays below 2^64 (about
70% of it); a large upload can pass it.

- **Where.** `econ::check_fee` (`econ.rs:268-275`) computes `base` as
  `base_fee(state, height, size) + programs::fuel_fee(state, tx, height)`, so `admit`'s charge
  (`chain.rs:1235`), the tip cap and the burn in `apply_j` (`chain.rs:1330`, `:1370`) include
  fuel with no change to `chain.rs`. `econ::charge` (`econ.rs:249-251`), which the API's
  `charged` uses for a pending transaction (`api.rs:666`), gets the same term.
- **`programs::fuel_fee` never fails.** A program it can't find contributes no load; admission
  then refuses the transaction with the precise reason (§2). It's 0 for every tag but 62–64, so
  every v1 transaction's charge is unchanged. Before META exists the rate is the floor's default
  (and admission refuses every program tag anyway). The schedule is W1's
  `meter::schedule(version_at(state, h))`: `None` below v2, where `fuel_fee` is 0, `SCHEDULE_V0`
  from v2.
- **Who pays.** The fee and `value` and deposits come from `from`. Programs pay only deposits they
  sponsor and creation fees of accounts they open with `OPEN_PAY` (PRG 5.4).
- **Nothing refunds.** Failed and out-of-fuel transactions pay their whole reservation, so every
  unit of work a proposer or verifier does on an admittable transaction is paid for, and agent
  budgets, preflight and the tip cap stay exact. A receipt's `fuel.used` equals the limit after
  any out-of-fuel, nested or not (W1's `exec.rs`), so it isn't a measure of work done.

### 5.2 Schedule v0 (`node/vm/src/meter.rs`, W1's `SCHEDULE_V0`)

Spike-vm F6's conservative schedule for soroban-wasmi, as W1 landed it (`meter.rs` gives each
number's basis), about 1.5–3× over the worst floor measurement on each item but upload, which has
1.22× (71 fuel per byte, the most that keeps the largest upload under `MAX_TX_FUEL`, against a
measured worst of 58: section 4.3 of [programs/phase0.md](programs/phase0.md), VM-5). It's the schedule of protocol v2
(`meter::schedule(2)`): frozen with v2's other rules from a supported release on (§9.2), and
then changed only by a later version.
On `blockula-1`, whose rules keep programs off under v2 (§1.7), programs would first run under
v3's schedule, which can carry what a re-run finds. Every other chain runs v0's from the version
v2 is in force.

| Item | Fuel |
|---|---|
| Wasm operators, by soroban-wasmi's five classes (base / entity / load / store / call) | 3 / 4 / 20 / 4 / 30 |
| Load, per distinct declared code (at admission, §5.1) | `LOAD_BASE` 10,000 + `LOAD_PER_BYTE` 40 per byte |
| Upload: validation, a test translation and hashing (at admission) | `UPLOAD_BASE` 50,000 + `UPLOAD_PER_BYTE` 71 per byte of code and interface |
| Instantiation, per frame (inside the transaction's `fuel_limit`, `Schedule::instantiate_fuel`) | 2,000 + 2,000 per page + 1 per data byte + 300 per import + 500 per export + 15 per function + 20 per global + 50 per element segment + 2 per table element a segment initializes + 20 per data segment |
| Storage (`HostCosts`) | `storage_write` 5,000 + 10 per byte; reads and removes as ABI v1 prices them |
| Host functions | ABI v1's (`HostCosts`), with `abort_v1` +1,000 and `debug_log_v1` priced as `emit_v1` |

**Calibration obligation**, each item a go/no-go with its fallback, every result recorded beside
W1's `CALIBRATION-*.md` (the floor runners' runs are in `node/vm/CALIBRATION-x86_64.md`,
`CALIBRATION-aarch64.md` and `CALIBRATION-node.md`: every item GO, the upload item at 1.73 and 1.89
ns per fuel against 2.5). PRG 4.5's rule: no pattern may run slower than **2.5 ns per counted
fuel** on either floor runner (x86-64 and the aarch64 GitHub runner), translation included, so a
`CALIBRATED_BLOCK_FUEL` block runs in ≤ 150 ms.
1. **The crate's patterns** (W1's `cargo bench --bench calibrate` under `SCHEDULE_V0`, through
   W1's dispatch-only `calibrate-vm.yml`). It needs nothing from the node (dispatching
   it is the maintainer's step on GitHub). Fallback: raise the item's price before a supported
   programs-on release (§10.6).
2. **Upload, reported on its own** (VM-5): at 71 fuel per byte, validation plus translation must
   take ≤ 177 ns per byte of code and interface on both floors. W1's bench prints it as one line
   ("Upload go/no-go: GO|NO-GO") and `calibrate-vm.yml` fails on NO-GO. The margin is the
   thinnest in the schedule and sensitive to load: an idle Mac measured 25.2 fuel per byte of wall
   time, a loaded one 33.2, which at phase 0's ~2.3× Mac-to-floor ratio projects 58 to 76 against
   the price's 71 (VM-V1), so only the floor dispatch decides. Fallback, before a supported programs-on release: lower
   `MAX_CODE_BYTES` (192 KiB leaves room for 93 fuel per byte under `MAX_TX_FUEL`) and raise
   `UPLOAD_PER_BYTE`, moving W1's `const` assertions with them; until then, a lower
   `program_max_block_fuel` is the only stopgap.
3. **The node's patterns** (`node/src/programs/bench.rs`, `#[ignore]`d), timed through
   `run_block`, `commit_block` and the writer's `persist`, on the aarch64 runner, as soon as the
   integration executes a block (stage S4a), not at the end:
   - **minimal calls:** a block of `MAX_BLOCK_TXS` (512, `consensus.rs:56`) one-instruction calls
     to the smallest valid code. A call counts about 17,100 fuel: `LOAD_BASE` 10,000 + 40 a byte
     of a ~50-byte module, and about 5,000 of instantiation (2,000 + 2,000 for one page + 500 an
     export + 15 a function) and the nop. The rule, ≤ 2.5 ns per counted fuel, applies to the VM's
     share of `run_block` + `commit_block` (the block's time minus that of a block of as many
     native transfers, without the writer's `persist`, which is reported apart), so the
     per-transaction setup fuel doesn't meter (a thread, an engine and its linker, a translation,
     §6.1) stays within what the transaction paid. The native per-transaction cost is reported
     beside it: `Tx::check_format` and `Tx::id` are pure functions every path runs for every
     transaction, not priced in fuel and bounded by `MAX_BLOCK_TXS`; they are made cheap (one hex
     decode per field; the id computed once and passed to `programs::exec::Run::new`) before the
     benches are read, with the replay gate as the proof nothing changed (S4c's review: on an M4
     Max they were ~58% of a minimal call, and the VM's share ~2.2 ns per counted fuel). Fallback,
     in this order: one thread and one engine per block pass (A3: no outcome or fuel changes, so
     no version); a per-block cap on program transactions; raising `LOAD_BASE` only if the floor
     runners still fail after both. **The first is taken** (A3): on the floor runners the
     minimal-call pattern moved from 10.4–12.0 to 1.50–1.66 ns per counted fuel (run 40763852590:
     x86-64 1.62–1.66, aarch64 1.50–1.51, worst repetition 1.78), every pattern GO, so the second
     and third aren't needed;
   - **write storm:** a block of calls each writing 256 distinct one-byte keys, and one of calls
     writing 16 KiB values: ≤ 2.5 ns per counted fuel including the state tree's updates and the
     database write, which phase 0 didn't measure (programs/phase0.md: "Storage costs here exclude the
     node's"). Fallback: raise `storage_write`, its per-byte price and `storage_remove` before a
     supported programs-on release.

The limits below assume every item holds.

### 5.3 Limits and constants

| Name | v0 | Kind | Why |
|---|---|---|---|
| `program_max_block_fuel` | 40,000,000 by default | parameter in [20M, `CALIBRATED_BLOCK_FUEL` = 60M] (W1) | 40M × 2.5 ns = 100 ms per run at the calibration rule's bound; ~50–70 ms at schedule v0's expected 1.2–1.7 ns per fuel on the aarch64 floor. **Per validator, a block runs at most four times**: once verified, once replayed as an ancestor (the replay cache keeps it from running again, §6.6), once executed, and once more when this validator leads (packing). That's ≤ 400 ms of the 1 s block when leading, 300 ms otherwise. Without the cache every proposal and every vote would replay the unexecuted ancestors again. A verifier first waits until it has executed the block's cited `exec_height` (`verify`), so what it replays is the stretch between what it has executed and the parent: for a block with transactions at most `FEE_LAG` − 1 = 16 blocks (`check_header` refuses transactions in a block whose cited state is more than `FEE_LAG` = 17 back), and usually the parent alone; an empty block may cite an older state, and a verifier abstains past `MAX_REPLAY` = 64 ancestors. A proposer replays whatever it hasn't executed itself, up to `MAX_REPLAY`, beyond which it proposes an empty block. The council raises the budget toward 60M once the re-run is in |
| `program_max_tx_fuel` | 10,000,000 by default | parameter in [1M, `MAX_TX_FUEL` = 20M] (W1) | A quarter of a default block, so one transaction can't fill it alone; with the largest load (21.2M) a reservation is 31.2M, which fits the default block but not one at the parameter's 20M floor ("larger than a block", §5.4) |
| `MAX_TX_CODE_BYTES` | 512 KiB of distinct declared code | constant (W1's `config.rs`) | Bounds `load` at 24 × 10,000 + 40 × 524,288 = 21,211,520 (`Schedule::max_load_fuel`) |
| `MAX_CODE_BYTES`, `MAX_INTERFACE_BYTES` | 256 KiB, 16 KiB | constants (W1's `config.rs`; §3) | Bound `upload_fuel` at 50,000 + 71 × 278,528 = 19,825,488 ≤ `MAX_TX_FUEL`, so the largest upload fits a block at the parameter's lower bound |
| `MAX_CODE_TX_BYTES` | 320 KiB | constant (`Tx::max_bytes`) | An upload's wire size |
| `MAX_RESERVATION` | 2^26 (67,108,864) | constant (W1's `meter.rs`), with W1's `const` assertions that `MAX_TX_FUEL + max load` (41,211,520) and the largest `upload_fuel` fit | Bounds every reservation, so `MAX_FUEL_RATE` keeps `rate × reserved` in a u64 |
| `MAX_FUEL_RATE` | `u64::MAX / MAX_RESERVATION` = 2^38 − 1 milli-drops per fuel | constant (`programs/mod.rs:148` at `296a466b`, since `check_invariants` and `params.rs`'s floor bound read it too; `fuel.rs` uses `super::MAX_FUEL_RATE`, never a second definition) | The controller's ceiling (§5.6), as `econ::MAX_RATE` is for bytes (`econ.rs:77`): W1 asks the node to keep the rate there. About 177 consecutive full blocks reach it from the default floor |
| `MAX_CALLS`, `MAX_BINDINGS`, `MAX_INPUT` | 8, 16, 32 KiB | constants | PRG 8.1 |
| `program_fuel_floor_rate` | 250 milli-drops per fuel | parameter [1, 1,000,000] | At the floor a typical call (300k reserved) costs 75,000 drops of fuel, about its byte fee, and a full block's fuel 0.01 FANG; congestion moves the rate above it (§5.6) |
| `RECORD_BYTES` | 256 | constant | PRG 6.3 |

`lowest_fuel_fee(tx) = exec(tx) × 1 / 1000` (the floor's lower bound) is the stateless lower
bound: `chain::check_fee` (`chain.rs:1838-1844`) adds it to `econ::lowest_base_fee`, so gossip,
HTTP and block verification refuse a program transaction whose fee can't cover its declared
compute under any parameters, and never refuse one some state would accept.

### 5.4 The block's fuel budget

A block's **counted fuel** is Σ `reserved` over its transactions that passed admission (applied
or failed). It must stay ≤ `program_max_block_fuel` of the state the block runs on.
- **Admission (Block mode):** `block.fuel + reserved > program_max_block_fuel` → `Err` ("the
  block's program fuel is spent"). In every mode, a transaction whose own `reserved` exceeds
  `program_max_block_fuel` is refused ("larger than a block").
- **Counting:** `apply_j`'s fee phase adds `reserved` to `state.programs.block.fuel` through the
  fee journal `fj` (one call, `programs::count_fuel`, beside `chain.rs:1369-1373`; it writes
  nothing for a reservation of 0), so it sticks exactly when the fee does, and a rolled-back
  trial restores `(height, fuel)` (§4.3).
- **Propose** (`consensus.rs:662-671`): an `Err` skips the transaction and holds its sender's
  later ones back, as today; a fault leaves it out and quarantines it (§6.3).
- **Verify** needs no check of its own: the sequential pass (`consensus.rs:848-853`) admits each
  transaction before it runs, and admission refuses one past the budget, so verifying a block
  runs at most `program_max_block_fuel` of program work whatever the block carries, and a block
  with such a transaction is rejected. (A stateless Σ `exec` pre-check would save nothing, and
  would have to live in P2's shared verify helpers, `consensus.rs` `check_txs`/`check_slot`, so
  that `blockula replay` runs it too.)
- **Execution:** an unadmittable transaction is skipped, and the proposer pays its byte base fee
  as junk (`staking::charge_junk`, `staking.rs:1287-1294`), never fuel: it never ran. A verified
  block never has one.
- **Mempool mode** doesn't count (a queue isn't a block, PRG 5.3).

Because load is counted per transaction (A3), every transaction's load is counted, also when an
earlier one in the block declared the same code. So the work a block does is bounded by what it
counted, and no cache or position in the block changes any transaction's fuel.

### 5.5 `/api/fees`

A `fuel` section, omitted while programs are `Dormant` (clients treat an absent section as unavailable). Existing endpoints keep their pre-programs key sets and bytes on v1 and v2 without META:

```json
"fuel": { "status": "on", "rate": 250, "floor_rate": 250, "suggested_rate": 1833,
          "rate_unit": "milli-drops per fuel", "block_fuel": 0,
          "max_tx_fuel": 10000000, "max_block_fuel": 40000000, "schedule": 0,
          "load_base": 10000, "load_per_byte": 40, "upload_base": 50000, "upload_per_byte": 71,
          "max_tx_code_bytes": 524288, "storage_price": "100000", "state_bytes": 12345,
          "state_soft_bytes": 67108864, "value_cap": "10000000000000000",
          "value_held": "0", "sponsored": "0", "value_headroom": "10000000000000000",
          "paused_until": null }
```

`rate` is what the next block charges; `suggested_rate` is the most it can reach in `FEE_LAG`
blocks of full fuel (§5.6: `rate` raised by a full block's step 17 times (block h+17 itself needs
16 steps; the extra one is margin), computed as
`econ::base_fee` extrapolates bytes, `econ.rs:225-246`), so a fee priced with it covers any of
them; `block_fuel` is the last executed block's counted fuel. `storage_price` is
`price(state_bytes)` now. `value_headroom` is `value_cap − (value_held + sponsored)`, the
control the cap still allows (§7.1), floored at 0. Clients price a program transaction as `max_fee(size) + suggested_rate
× reserved / 1000` (their own `reserved`, from the record sizes `/api/program` and `/api/code`
report), check `reserved ≤ max_block_fuel` before signing, and are charged only what §5.1
computes at the block that includes it. A wallet may sign less margin; if the rate overtakes it,
the transaction leaves the mempool at its post-block check ("fee too low") and is signed again.

### 5.6 The fuel rate, and proposal order

**The rate.** `programs::end_block(state, b)`, called from `State::end_block` right after
`crate::econ::end_block(self, b)` (`chain.rs:1000`), returns at once while META `programs` is
absent (v1, and v2 before programs first switch on). Otherwise:

```
used      = block.fuel if block.height == b.height else 0      (this block's counted fuel, §5.4)
limit     = program_max_block_fuel                             (as the block ran it: before this block's ParamChanges)
floor     = program_fuel_floor_rate
fuel_rate = clamp(next_rate(max(fuel_rate, floor), used, limit), floor, MAX_FUEL_RATE)
```

`next_rate` is `econ::next_slot`'s formula on a rate (`econ.rs:324-339`): with ChainParams'
`target_fullness_bps` (50%) and `base_fee_max_change_bps` (12.5%), a block whose counted fuel is
full raises it 12.5%, an empty one lowers it 12.5%, linearly in between, in u128, at least one
milli-drop up when above target. It's its own function in `programs/fuel.rs`, so `econ.rs` is
untouched. It is `next_slot`'s formula unclamped (it saturates at 0 where `next_slot` subtracts
plainly; the two agree for a step up to 10,000, its bound), so a property test checks
`next_slot(..).rate == next_rate(..).clamp(floor_rate, econ::MAX_RATE)` over the controller's
whole domain: they agree wherever `next_slot`'s clamp doesn't act. With a step of 0 the rate never
rises (the one-milli-drop minimum applies only to a non-zero step). A second property test runs
10,000 consecutive full blocks and checks neither the rate nor any charge overflows (PRG 5.2's).

Every path reads the same number: admission, the proposer's trial, verification and execution all
read `fuel_rate` from the state the transaction runs on (the parent's, replayed), so they agree
without a ring (A6). That is per state: the mempool's state is the executed head's, while a
proposal and a verification build on the parent's replayed state, which can be an unexecuted
block ahead with a higher rate. So a call priced for the head's rate can be admitted and then
refused in the next block; §5.5's post-block re-check drops it (fee too low) and the wallet signs
again. Sustained full blocks double the rate every 6 blocks and multiply it a
thousandfold in a minute, so filling program capacity costs exponentially more the longer it
lasts, as bytes do (`econ::end_block`); the council's floor only sets where it rests.

**Proposal order** (proposer policy, not consensus, A38): `TipRate` (`chain.rs:4078-4106`)
compares tip per share of the scarcer resource instead of tip per byte: `share = max(size ×
meter::CALIBRATED_BLOCK_FUEL, exec × consensus::MAX_BLOCK_BYTES)` (the codec's 4 MiB, not the
`max_block_bytes` parameter: `TipRate::of` stays a function of the transaction alone, and `exec`
comes from its own bytes, §5.1; load isn't in it), cross-multiplied in u128 (a tip times a share
is below 2^110). For a transaction that reserves no
fuel, `share = size × CALIBRATED_BLOCK_FUEL`, and two such compare exactly as `tip × size` does
today, so every v1 proposal keeps its order (a test compares the two orders over every v1 vector
transaction). It keeps a heavy call, whose tip cap grows with its fuel fee, from outranking light
ones by tip per byte (PRG 5.3).

## 6. Execution safety and the mempool

### 6.1 How a program transaction runs (Block mode)

`apply_action`'s dispatch (`chain.rs:1625-1631`, one arm per module) gains one:
`Action::UploadCode { .. } | Action::CreateProgram { .. } | Action::CallProgram { .. } |
Action::ProgramsPause {} => crate::programs::apply(self, j, tx, &sender, signer, height)?`.
`apply_action` keeps its signature (a multisig's approved action runs through it too,
`governance.rs:1333-1336`): in `Mempool` mode `apply_j` never reaches it for tags 62–64 (§6.4).
For tags 62–64 in `Block` mode:

1. **Re-check** admission's program checks (§2) on the state the action runs on. In v0 the
   action phase is only reached right after `admit` in the same `apply_j` (no nesting, A8), so
   this never fails; it's there so a later nested path can't skip them. A failure is the
   transaction's `Failed`.
2. **Resolve the declared set** from `&State`: each declared program's record, code bytes,
   decoded interface (`InterfaceRecord` holds it decoded, §4.2) and `version_digest`, into a
   `ProgramView` (`node/src/programs/view.rs`), W1's `vm::host::View` over the state as the
   action phase sees it: **after this transaction's fee phase** (its charge debited, its tip
   credited, its nonce bumped, `chain.rs:1352-1373`), so the signer's balance the VM checks
   `value` and deposits against is net of the fee, and commit's `j_debit` agrees with it. The
   methods: `height`, `balance` (by rendering the 32 bytes with the chain's prefix; a keyless
   system account, `chain::system_account`, that has no record yet reads as `Some(0)`, an existing
   account, so no program pays a creation fee for one, A30), `code_of` (a program with
   `created_at < h`, or the one this create is making), `storage` (namespace 69),
   `storage_price(used)` (§4.6; `None` past the ceiling), `state_bytes` (META's),
   `account_creation_fee`, `method(program, name) → Option<MethodInfo { kind, payable }>` (from
   the decoded interface) and `version_digest(program)` (§2.1, from the record in force). These
   are exactly the nine methods of W1's `View` trait.
3. **Which thread runs the VM is the node's business, not consensus (A3).** A *block pass* is one
   run of a block's transactions: executing a certified block, replaying an unexecuted ancestor, a
   proposer's packing, a verification's apply loop, the startup replay of the block log
   (`programs::block_pass`; `State::run_block`, `Chain::replay`, `propose`'s packing loop and
   `verify`'s apply loop each wrap their loop in one). A pass that holds at least one program
   transaction (tags 62–64) runs **on one thread**: `std::thread::scope(|s|
   std::thread::Builder::new().name("blockula-program-pass").stack_size(PROGRAM_STACK_BYTES)
   .spawn_scoped(s, …))`, with `PROGRAM_STACK_BYTES` = 64 MiB, joined when the loop ends. The
   loop's captures move to that thread (`State` is `Send` and `Sync`), and each program
   transaction's `ProgramView` is built and used on it. A pass without a program transaction runs
   on the caller's thread, with no thread and no engine. A spawn the OS refuses is tried twice more
   at once; a third refusal runs the pass on the caller's thread and each program transaction on a
   thread of its own, the first meeting the refusals and the `Setup` fault below. A panic that
   unwinds the pass (node code outside the VM's `catch_unwind`) is raised again on the caller's
   thread at the join, where it would have unwound before. Inside a pass there is no
   per-transaction thread, so the `Tx` fault for a thread that panicked past every backstop (§6.2)
   applies only outside a pass. Outside a pass (`/view`, a lone `State::apply`) each VM run gets a
   thread of its own, with the same retries and the same `Setup` fault. The mempool starts neither
   (§6.4). A node holds one such stack per pass running at once: a verification, a proposal, an
   execution and the startup replay can overlap. On a validator at most two run at once: each
   blocks one of the async runtime's two workers, and the chain lock keeps packing and execution
   apart. No limiter of the node's enforces it.
4. **On that thread** (the pass's, or the transaction's), under `catch_unwind` (a caught panic is a
   `Tx` fault): an engine, then load, then execute.
   - **The engine** is the pass's one engine, `vm::engine::new_block_engine(EngineKind::Soroban,
     schedule)` with `schedule = vm::meter::schedule(version_at(state, h))` (`SCHEDULE_V0`),
     built when the first program transaction that needs one runs, or a fresh one per VM run
     outside a pass. An `Err` is a `Setup` fault for that transaction.
   - **Load every distinct declared code before anything of the transaction runs**, in hash order
     (`BlockEngine::load`). Code the engine already holds is W1's no-op, so a code that several
     transactions of a pass declare is translated once. Load stays counted per transaction (A3,
     §5.4): admission counted it from the transaction's own declaration, and nothing reads fuel off
     an engine. Stored code passed validation and a test translation at upload, so a `LoadError`
     here is a `Tx` fault (`NodeFault::Load`), never the transaction's outcome.
   - `vm::exec::execute(&*engine, &view, signer, &calls, &bindings, TxLimits { fuel_limit,
     storage_limit }, schedule)` → `Result<Receipt, NodeFault>`. Each `vm::exec::Call` carries
     the program's 32 address bytes, the signed `version_digest`, method, input, value and
     `kind`: `MethodKind::Init` for a create's `init` (the digest the new record has),
     `MethodKind::Call` for tag 64. The VM refuses a top-level method of another kind
     (`TrapKind::NotCallable`), a value into a method that isn't payable (`NotPayable`) and a
     digest that isn't the one in force (`VersionMismatch`), each a failed receipt, though
     admission has already refused all three; `signer` is `from`'s 32 address bytes.
   - **The pass's engine is dropped**:
     - after any node fault or caught panic. It is taken out of its slot while a transaction runs
       and put back only if the run returned `Ok`, and `programs::apply` drops it when it records
       a fault, whichever step raised it;
     - when the code loaded into it would pass `ENGINE_CODE_BYTES` = 1 MiB, twice
       `MAX_TX_CODE_BYTES`. soroban-wasmi keeps every translated function until its engine is
       dropped. The bound is node policy, and changing it changes no outcome. It was chosen from
       the heap a loaded engine holds: under 32 bytes per code byte whatever the code. The code
       map is one `Vec<Instruction>`, with at most two 8-byte instructions per code byte, whose
       capacity is under twice what it holds. The most measured is 31.7 MiB at the bound
       (`br_table`-dense code; `node/vm/CALIBRATION-node.md`). A rebuild drops the engine it
       replaces before the new one translates anything;
     - when a transaction runs under a schedule that differs in value from the one it was built
       from (compared as values, not addresses);
     - and when the pass ends, on the thread that built it.

     So no engine, module or cache survives a fault, and none can change what a transaction does.
     A transaction reaches only its declared set (the `ProgramView` and `reachable`, never the
     engine). `info()`, instantiation cost and the frames' stores don't depend on what else is
     loaded: translation is eager and unmetered, the stack pool is reset per call, and the
     constant pool, function types and code map are only dedup tables and opaque handles.

   `UploadCode` runs `vm::upload::check` inline on the pass thread (or on a thread of its own
   outside a pass, §2.2 step 3), with its own throwaway engine either way. There a refusal is the
   transaction's failure (the code is new), and only a panic or an engine that won't build is a
   fault.
5. **A failed receipt** (a trap, an abort, a per-transaction bound, the VM's commit checks) is
   recorded (§8.1), and `programs::apply` returns `Err`, which `apply_j` turns into
   `Outcome::Failed` with the fee kept and the action journal rolled back (`chain.rs:1397-1401`).
6. **A successful receipt is committed** (`programs::commit`, panic-free, checked arithmetic
   returning `Err`), through the action journal `aj`, in canonical order:
   1. storage: each `diff.storage` change in its order (sorted by program, then key) writes or
      removes the namespace-69 entry `program ‖ key`, reading the entry it replaces first (W1's
      `StorageChange` carries only the new one); each `diff.programs` delta moves its record's
      `items`, `bytes` and `deposit`; META `state_bytes` moves by `diff.state_bytes` and `escrow`
      by `diff.escrow` (the deposit FANG itself moves in the balance deltas, folded in). **Sponsored
      deposits:** a program's entries are paid by the signer or, with `SPONSOR`, by the program
      itself (W1 `host.rs:249`: `payer` is the writing frame's program or the signer), so an entry
      is program-paid exactly when its `payer` is its own program's 32 bytes. Over the changes,
      each new program-paid entry's deposit is added and each replaced or removed one's
      subtracted, in the program's record's `sponsored` and META's `sponsored`;
   2. balance deltas: debits (negative), sorted by address, through `j_debit`, then credits,
      sorted, through `j_credit`. The VM's who-was-debited and conservation checks
      (`Diff::check`) already passed;
   3. accounts the diff opened: their creation fees are in `diff.burned`, burned with `j_burn`;
      each goes into `aj.programs.opened`, which `econ::charge_new_accounts` skips, so the
      signer isn't billed again for an account a program paid for (`econ.rs:548-565`, one filter
      term);
   4. `value_held` moves by the net delta of program accounts (a program this transaction
      created included). **The FANG programs control** is `value_held + sponsored` (§7.1): a
      program bonding its own balance into a sponsored entry, or getting that deposit back,
      moves FANG between the two terms and leaves it unchanged;
   5. exit-only: if programs are exit-only and the control rose, `Err` (`exit_only`, §7.3);
   6. the value cap: if the control rose and now exceeds `program_value_cap`, `Err`
      (`value_cap`, §7.1).
   Then the receipt is recorded and `apply_j` runs its existing post-action checks:
   `charge_new_accounts` and `check_locks` (`chain.rs:1380-1384`), which hold `value` and the
   signer's deposits to transferable FANG. If one fails, the transaction is `Failed` and the
   receipt says `native` (§8.1).

Execution order is the block's; calls run in order; diffs apply sorted; no hash collections
(PRG 4.3: W1's `clippy.toml` bans them in `node/vm`; the node's crate has no per-module Clippy
config, so `programs::tests::no_hash_collections` fails if a file under `node/src/programs/`
names `HashMap` or `HashSet`); no clock, randomness or floats reach a program.

### 6.2 Node faults

A **node fault** is anything no valid transaction can cause: a `vm::engine::NodeFault` from
`exec::execute` or `upload::check` (`Panic`, `NotLoaded`, `Load`, `Instantiate`, `Engine`, `Fuel`,
`Host`, W1's batch-shape faults among them: an empty batch, more than `MAX_CALLS` calls or
`MAX_BINDINGS` bindings, which the codec makes impossible), a `LoadError` on stored code, a panic
on the program thread or one that escaped to `join`, an engine that won't build, or a thread the
OS won't start. It is **never a transaction outcome** (A11): `programs::apply` records it in
`state.programs.fault` (§4.3) and returns `Err`; whatever outcome `apply_j` then reports is never
kept, and while `fault` is set every later program action on that `State` returns `Err` at once
and runs nothing.

Two classes (`FaultClass`):
- **`Tx`**, tied to the transaction: anything raised while validating, translating, loading or
  executing its code (every `NodeFault` from `execute`, `Panic` from `upload::check`, a
  `LoadError` on stored code, a panic on its thread).
- **`Setup`**, this node couldn't make what every program transaction needs: a thread the OS
  refused three times for a transaction (outside a pass, or after a pass thread was itself
  refused three times and the pass fell back to a thread per transaction, §6.1 step 3), or an
  engine that wouldn't build (`new_block_engine`'s `Err`, or
  `upload::check`'s `NodeFault::Engine`, its throwaway engine).

**What each path does.**

| Path | On a fault |
|---|---|
| **Proposing** (the packing trials, §6.3) | **Leave the transaction out and go on packing.** Nothing on this path is committed and no outcome is recorded, so leaving it out claims nothing about it. Roll back its trial, take the fault off the trial state (`fault.take()`: the state is the proposer's own copy, and the rollback left nothing of the transaction in it), and hold its sender's later transactions back for this proposal. `Tx`: quarantine it (below) and drop it, with its sender's later transactions, from this node's mempool ("not proposed: program execution faulted on this node (…)", `drop_from_mempool`). `Setup`: no quarantine; skip the proposal's other program transactions. Either way an error log naming the transaction, the height and the fault, and `blockula_program_node_faults_total{path, class}`. A fault while replaying the parent's unexecuted ancestors means this node can't judge the parent: no proposal (`return None`), as for a parent that doesn't extend its chain |
| **Verifying** | **Abstain** (no vote either way: `futures::future::pending().await`, as for a block it can't judge), with the log and the metric: right after the ancestors' replay if it faulted; on an `Err` in the apply loop while a fault is set (checked before rejecting: an earlier transaction's fault can make a later one unadmittable here); after the loop. Also abstain, before replaying anything, on a block that carries a quarantined transaction id (the id only: a block carrying other transactions from a held sender is judged as usual, below). A pass that ends with no fault records the block's digest as verified clean (below) |
| **Executing a certified block** (`Chain::apply_block`), **startup replay** (`Chain::replay`) | **Halt.** Right after `run_block` (`chain.rs:3082`) `apply_block` records the halt in the node's `HaltSignal` (which `node::watch_halts` turns into the exit, as for every halt) and returns the `Halt` itself as its error; `State::commit_block` (`state/mod.rs:363-379`) also refuses while `fault` is set, the backstop for every path that commits (`chain.rs:3104`, `:2974`). `Tx` → `Halt::ProgramFault { height, tx, fault }`, exit **13**, unless this node's own verification ran the block without a fault (below); `Setup`, and a `Tx` fault on a block verified clean here → `Halt::ProgramResources { height, fault }`, exit **14** |
| **The mempool** | Never runs a program (§6.4), so it can't fault. It refuses quarantined transactions at the door |
| **`/view`** (§8.2) | Its own engine on a state copy: an HTTP 500 and the metric, nothing else |

**The exit codes.**
- **13 (`EXIT_PROGRAM_FAULT`) stays down.** A certified block is one a quorum verified without a
  fault (a validator that faulted abstained). So a `Tx` fault while executing one means this node
  differs from that quorum (its binary, its build, its hardware), and a restart would replay the
  same block and most likely fault again: an operator looks first (OPERATIONS.md, "Incident
  response"), while the rest of the set goes on if a quorum is left. OPERATIONS.md's systemd unit lists 13 in
  `RestartPreventExitStatus` and `deploy/entrypoint.sh` keeps 13 down. A startup replay that faults exits 13 again:
  `run_validator` and `run_full_node` (`node.rs:778`, `:1041`) exit through `exit_halted` when
  `Chain::open`'s error is a `Halt` (one `downcast_ref`), not with 1.
- **The one exception: a block this node verified clean.** That argument holds for a node that
  abstained on the block, not for one whose own `verify` applied the same transactions to the
  same state without a fault a moment earlier: there the fault is this node's doing (a transient
  panic, an engine or instantiation error under memory pressure), and a restart most likely
  executes the block. So a verify pass that ends with no fault records the block's digest in a
  node-local set (`Chain`'s `verified_clean`, the last 64 digests, in memory), when the block
  carries a program transaction (only such a block can `Tx`-fault) and META `programs` exists
  in the state it was verified on, after its ancestors' replay (so v1 nodes run today's code,
  and program-free blocks don't evict the digests that matter), and a `Tx` fault
  while executing a block in it is classed with `Setup`: `Halt::ProgramResources` ("this node
  verified block H without a fault, then faulted executing it: restarting once"), exit 14. The
  set doesn't survive the restart, so a second fault on the same block exits 13.
- **14 (`EXIT_PROGRAM_RESOURCES`) restarts.** A thread the OS refused (thread limits,
  `vm.overcommit_memory = 2`, a container's memory cap counting the 64 MiB stack) is cured by a
  restart: the block wasn't committed, so consensus delivers it again. An engine that won't build
  is `Setup` too: the quorum's identical binary built one, so this node's environment is what
  differs, and a restart is the cheapest retry. 14 is in neither stay-down list (the unit's and
  the entrypoint's comments say so); a supervisor restarts it for as long as it recurs, so an operator
  stops the unit if it exits 14, or crashes, again at the same height (OPERATIONS.md). A full node exits 14 too:
  its sync loop, which parks a node on every other halt and keeps serving reads at its height,
  exits on this one, since nothing restarts a process that doesn't exit. OPERATIONS.md ("Programs (optional, off by
  default)") documents the thread and overcommit limits a validator needs.
- `Halt`'s two variants carry their kinds (`program_fault`, `program_resources`) and messages
  ("block H can't be executed here: a program execution fault in tx … (…); this node stopped
  rather than guess an outcome: see docs/OPERATIONS.md, Incident response"), which
  `/api/health`, `/api/status` and `/metrics` print as they do every halt (`api.rs:728`, `:937`,
  `:971`, `:1149`).

**The quarantine** (node-local, never consensus; `programs::Quarantine`, held by `Chain` beside
the mempool). It keys on what the faulting transaction's author controls, never on other
people's programs or accounts, and every entry expires. A `Tx` fault in a proposer's trial adds:
- **the transaction's id**, until the executed height passes its `valid_until` (at most an hour
  of blocks ahead, `chain.rs:1823-1830`), after which no block can carry it anywhere;
- **its sender, for program transactions only** (tags 62–64), for `QUARANTINE_HOLD_BLOCKS` =
  `MEMPOOL_TTL_BLOCKS` (600 blocks, `chain.rs:1898`: ten minutes at 1 s blocks, fifty at the
  5 s default; counted in blocks whatever the block time) from the fault. The
  sender's transfers and other native actions go through as before, so it can always replace
  the nonce the dropped transaction held;
- **for an upload, its new `wasm_hash`**, for a day of blocks: bytes the sender wrote, refused
  from anyone.

A call's or a create's declared code is never quarantined: it's the callee's and the bindings'
code, other people's, and one caller's input that trips an engine bug says nothing about anyone
else's call. Up to 1,024 ids, 1,024 senders and 256 code hashes, oldest out first; a restart
empties it (the trial marker refills what crashed the process). While an entry lasts,
`admit_to_mempool` refuses the quarantined transaction, a held sender's program transactions and
an upload of held bytes ("quarantined on this node after a program execution fault, until block
N: see its log"), so neither HTTP nor gossip brings them back and proposals never see them; and
verification abstains on a block carrying a quarantined id, and only that (a Byzantine proposer
that includes a held sender's other transaction gets it judged, and abstained on only if it
faults). A transaction that faults deterministically is tried at most once per validator; trying
again costs its author another funded account (at least `account_creation_fee` +
`MIN_NEW_ACCOUNT`) or the hold's 600 blocks, and new bytes for an upload. A fault on one node
alone (resources, a bad build) quarantines a valid transaction there only, for at most an hour
(an upload's bytes for a day); the others include it.

**The trial marker** (node-local). The marker holds one record for every trial in flight (a
proposal's packing and a verification each name one program transaction at a time, and the two
can overlap): its id, sender, kind (`upload_code`, `create_program` or `call_program`),
`valid_until` (so the id's expiry is exact at startup) and, for an upload, `wasm_hash`. It is
rewritten whole into the fixed 4 KiB `<home>/program-trial` by one `pwrite` whenever a trial
starts, names a transaction or ends (no `fsync`: a process that dies keeps what it wrote; a host
that dies loses only this hint). A guard that unwinds from a panic leaves the file as it was, so
a node-side panic that escapes the program thread's `catch_unwind` and ends the process (the
runtime doesn't catch task panics) is recorded like an abort. At startup the marker is read (at
most its fixed size; a damaged file is logged and ignored, never fatal) and a non-empty one goes
into the quarantine under the rule above, holds counted from the next block, with an error log
("the last run stopped while trying tx …: quarantined"). So a transaction that kills the process
(an abort, a stack overflow past the guard page, an out-of-memory kill, a panic outside the
program thread) crashes a validator at most once in a proposal or a verification, instead of
looping it through its own mempool and its peers' gossip.

The marker covers trials only: executing a certified block (`apply_block`), the startup replay and
the replay of ancestors mark nothing, so a transaction that kills the process there restarts
into the same crash. That matches the halt rule (such a node differs from the quorum that
certified the block, and a restart is a choice for its operator: OPERATIONS.md, "Programs
(optional, off by default)"). The marker can't tell a crash the transaction caused
from an unrelated kill: a kill of many validators while they verify the same program-carrying
block (a zone outage) quarantines that transaction on all of them, and its sender's program
transactions at each, which delays an honest transaction for up to its `valid_until` and holds
its sender's program calls for `QUARANTINE_HOLD_BLOCKS` at those nodes; it stalls nothing.
OPERATIONS.md says what that scope means for an operator (S4C-R9): the second crash, in a block
the quorum certified, has no marker to name its transaction, so the log's last lines and the
block's own transactions are what an operator reads.

**Strikes** (node-local). A leader that never faults on a transaction can still find that its
proposals carrying it never certify, because enough other validators fault on it and abstain
(below). So when a proposal is returned (not when packing starts: a proposal never made strikes
nothing), the proposer records its program transaction ids with its epoch and view; when this
node executes a finalized block with `parent_view` < v < `view` for a view v of that epoch it
proposed in (that view was nullified: `CBlock`'s own fields, `consensus.rs:120-122`), each
recorded id that is still queued gets a strike (one already included or dropped gets none).
Records are keyed by (epoch, view), since views restart every epoch. Counts are kept until the
transaction's `valid_until`. At two strikes the leader drops the transaction from its mempool
("not certified in two proposals: other validators may be faulting on it") and skips it, and holds
its sender's program transactions at this node's door for `QUARANTINE_HOLD_BLOCKS`, with its own
refusal text ("held on this node until block N: two of this node's proposals carrying this
sender's program transactions weren't certified; other nodes accept them"), not the fault
quarantine's; `blockula_program_uncertified_proposals_total` counts the strikes. Only this
node's own proposals strike, so a Byzantine leader strikes only its own mempool. A view can be
skipped for reasons that have nothing to do with the transaction (a slow link, a lagging or
overloaded proposer, withheld votes): then an innocent sender's program transactions are dropped
and held at that one node for `QUARANTINE_HOLD_BLOCKS`, and other nodes include them. That is the
price of the only defence against a fault some validators hit and others don't.

**What that leaves.** A deterministic fault can't stall or halt the chain: honest proposers leave
the transaction out, honest verifiers abstain on any block that carries it, so it's never
certified, and a Byzantine proposer only wastes its own views. Execution halts only a node that
differs from a quorum. A fault that some honest validators hit and others don't (a build- or
architecture-dependent engine behaviour) is within that while at most f hit it: the others
certify, and each that faulted (and so abstained) halts when it executes the block (13). Past f,
the leaders that don't fault keep proposing it and their views fail to certify, while the chain
advances in the faulting leaders' views; strikes end that after two views per such leader, and
each new attempt costs the author another funded account. Validators that disagree about the
engine are a consensus defect, not a node's problem: OPERATIONS.md's incident response reads the
faults and strikes metrics and answers with the pause (§7.2) and a fix (§7.4). What remains is a
defect that misbehaves *without* faulting: a wrong outcome (a validation hole, a metering error, a
host bug), a hang (an endless loop in a translator: nothing times out), or memory corruption.
Fuzzing is the defence against those (§10.4), and §10.6 makes it a gate of a supported release
that isn't waived for programs on at genesis. If one is found on a live chain, the response is the
pause (§7.2) and a fixed release, under a new version once the rules are frozen (§7.4, §9.2).

### 6.3 Proposers include failed program transactions, and leave out faulted ones

Today's proposer leaves out every transaction whose trial run is `Failed` or `Err`
(`consensus.rs:659-671`). With programs that would let anyone make every proposer run the VM for
free, every block. The rule becomes:

```rust
// Node-local: the trial marker names this tx until the next one, or until packing ends (§6.2).
if crate::programs::is_program_tx(tx) { trial.mark(tx); }
let outcome = state.apply_j(&mut tj, tx, &chain_id, &proposer, height, ApplyMode::Block);
// A node fault: this proposal commits nothing, so leaving the tx out claims nothing (§6.2).
if let Some(fault) = state.programs.fault.take() {
    state.rollback(tj);
    stuck.insert(tx.from.clone());
    faulted.push((tx.clone(), fault));
    continue;
}
match outcome {
    Ok(Outcome::Applied) => {}
    // A program transaction that ran and failed was charged for the work: include it.
    Ok(Outcome::Failed(_)) if crate::programs::is_program_tx(tx) => {}
    Ok(Outcome::Failed(_)) | Err(_) => { /* as today: roll back, hold the sender, continue */ }
}
```

A `Setup` fault also sets a flag that skips (and holds back the senders of) the proposal's other
program transactions. After the packing block, with the chain lock taken mutably,
`Chain::quarantine_faulted(&faulted, height)` quarantines and drops each `Tx` fault (§6.2), the
proposal's program transaction ids are recorded with its view for strikes (§6.2), and the
marker is emptied.

`is_program_tx` is tags 62, 63 and 64 (not 69: a pause that fails is left out, like any validator
duty). No v1 transaction carries those tags or faults, so v1's proposals are unchanged. `verify`
already accepts `Failed` outcomes (it rejects only `Err`, `consensus.rs:848-853`) and execution
already charges them, so this is proposer policy, not a consensus change. Packing still holds the
chain lock while it runs (`consensus.rs:635-681`), and since S4c it writes the trial marker
there (a 4 KiB `pwrite` per program transaction, to the page cache); the block's fuel budget
bounds the trials of the transactions it includes (§5.3). A faulted trial is rolled back, its
fuel with it, so the budget doesn't count it. A proposal stops trying program transactions after
`MAX_FAULTED_TRIALS` = 8 `Tx` faults, as it does after a `Setup` fault: the quarantine bounds
repeats of one id and one sender, not the number of distinct funded senders. The rest wait
queued, neither quarantined nor dropped, and the next proposal tries eight more. The trade: while
such a flood is queued (it needs a node-fault bug and a funded account per faulting
transaction), program transactions behind it in the tip order wait ⌈N/8⌉ of that leader's
proposals; without the cap every proposal would run all N trials under the chain lock.
Moving packing off the lock (PRG 12.2, P5) is later work.

### 6.4 The mempool never runs the VM

`State::apply_j`'s last parameter, `full_checks: bool` (`chain.rs:1322`), becomes
`mode: ApplyMode`:

```rust
pub enum ApplyMode {
    /// Executing or judging a block: proposals, verification, execution, replay. Runs
    /// `check_format` and programs.
    Block,
    /// The mempool's simulations (`MempoolCheck::run`, `dry_run`, `simulate_queue`) and the fuzz
    /// harness's dry run: the format was checked at the door, and programs never run.
    Mempool,
}
```

Its call sites today pass `true` from `apply` (`chain.rs:1317`) and the proposer
(`consensus.rs:662`), and `false` from `MempoolCheck::run` (`chain.rs:2247`), `dry_run`
(`:3624`), `simulate_queue` (`:3670`) and the fuzz harness (`node/fuzz/src/harness.rs:594`): a
mechanical change, and `ghost::apply_unsigned` takes the mode instead of the bool. In
`Mempool` mode:
- **Admission does everything that needs no VM**, the same function as in `Block` mode:
  `status` (version, feature, pause, exit-only), the signer rules, the fee with fuel, the nonce,
  each program's existence, age and `version_digest`, each method's kind and payability, the
  bindings, the code-size bound and "larger than a block". It skips only the block's fuel
  counter.
- **The action phase charges the worst case and runs nothing.** Where `apply_j` runs the action
  (`chain.rs:1379`), a program transaction in `Mempool` mode goes to
  `crate::programs::mempool_apply` instead of `apply_action`: `from` is debited (and the
  simulation burns) `Σ value + storage_limit` for a call, `init.value + storage_limit` plus
  `account_creation_fee` (when the address is new) for a create, and `storage_limit` for an
  upload. `check_locks` then holds that to transferable FANG as usual. The simulations' states
  are never committed, so this changes nothing real; it means a sender's later queued
  transactions are judged against the worst its program transaction could cost, and a program
  transaction whose worst case its sender can't cover never enters the mempool.
- **Cost.** Admission of a program transaction is O(declared programs) map reads, with no
  parsing (decoded interfaces are in the records, §4.2). So re-checking the whole mempool after
  every block (`MempoolCheck::run`, up to `MAX_MEMPOOL` transactions) costs what it costs today,
  and a flood of program transactions makes no node execute anything it isn't paid for.

- **The door.** `admit_to_mempool` (`chain.rs:3499`) refuses what the quarantine holds (§6.2)
  and, through admission, a create or call whose code or program isn't usable until a later
  block, saying from which block (A5, §2.1).

What the mempool can't know is whether a call will fail, or fault; it doesn't need to: a proposer
runs it, and it's included and charged if it fails, left out and quarantined if it faults
(§6.3).

### 6.5 What never changes for v1

Every rule in §6 is reached only through tags 62–64 and 69 or a state with program records. For a
v1 transaction: `ApplyMode` is the same bool under a name, the proposer's rule takes the same
branch, the fault checks find nothing, the quarantine, strikes and `verified_clean` stay empty and the trial marker is never
written, `commit_block` commits as before, proposal order is the same (§5.6), and the replay
cache is off (§6.6). P2's replay gate is the proof for everything that's consensus (§10.5); unit
tests cover the rest (§10.1).

### 6.6 The replay cache

`consensus::replay` runs the parent's unexecuted ancestors through `State::run_block` on a copy
of the executed tip, for every proposal and every vote, so every proposal and every vote re-runs
the blocks its node hasn't executed yet. A verifier first waits to have executed the block's
cited `exec_height`, so it replays at most the stretch between that and the parent (for a block
with transactions at most `FEE_LAG` − 1 = 16 blocks, and usually the parent alone; for an empty
block, which may cite an older state, up to `MAX_REPLAY` = 64, past which it abstains), and a
proposer replays what it hasn't executed itself (up to `MAX_REPLAY`; past it, an empty block).
With program blocks that work compounds for a node that is falling behind (safety F5, build F4).
The cache (`ReplayCache`, held
by `App` beside the chain handle, non-consensus) keeps the post-`run_block` state of the
ancestors `replay` ran, keyed by block digest, at most `MAX_REPLAY_CACHED` = 16 entries (O(1)
`imbl` clones that share structure):
- `replay` computes `pending` and checks its oldest link against the executed tip exactly as
  today, then starts from the newest block in `pending` whose post-state is cached (a digest
  commits to the block's whole ancestry, so that state is the same whichever tip computed it),
  runs the rest, and caches each block it ran;
- a state whose `programs.fault` is set is never cached; entries at or below the executed height
  are dropped each time `replay` runs;
- it's used only while META `programs` is present in the executed tip's state, so a v1 chain's
  proposers and verifiers run today's code exactly.

Each block then runs at most once as an ancestor per validator, which is what §5.3's budget
assumes. PRG's P4 caches verified and proposed blocks' post-states instead, which would save one
more run; but `verify`'s pass applies the block's transactions without its end-of-block steps
(`consensus.rs:848-853`), so its state isn't the block's post-state, and caching it would mean
running `end_block` in `verify` too. That's later work, as is adopting a verified post-state at
finalization (one execution per block), which is consensus-critical (PRG 12.2).

## 7. Training wheels

Three, each bounded (PRG 14.1, D4, minus the freeze, A1): a cap on the FANG programs hold, a pause
any f + 1 validators can pull, and exit-only when governance switches programs off. None moves
anyone's funds. They are configurable, experimental controls over the units that programs hold
(governance moves the cap within its bounds); the units have no monetary meaning on a kit chain,
and the cap carries no allocation or promise.

### 7.1 The value cap and the FANG programs control

**`value_held`** (META `programs`) is Σ balances of program accounts, deposits excluded (they're
`escrow`). It changes only through:
1. **a program commit** (§6.1 step 6.4): the net delta of program accounts in the diff, which
   covers `value` sent in, `transfer_v1` out and between programs, sponsored deposits bonded and
   refunded, and `OPEN_PAY` creation fees;
2. **a native `Transfer` to a program** (+ amount);
3. **adoption at create** (+ the adopted balance).

**`sponsored`** (META, and per program in its record) is the escrow programs paid themselves: Σ
the deposits of entries whose `payer` is their own program (§6.1 step 6.1). It changes only in a
program commit. A program can move its balance into such deposits (`storage_write` with
`SPONSOR`) and back (removing the entry refunds it), so `value_held` alone isn't what programs
control: **the FANG programs control is `value_held + sponsored`**, and it's what the cap and
exit-only (§7.3) measure (A40). Deposits a signer paid aren't in it: they refund only to that
signer, and no program can move them.

No other path can credit or debit a program account in v0: a program has no keys, so it never
signs anything (§2.1), and the native actions that pay a recipient someone else names are
guarded (below). `check_invariants` recomputes both (§4.4), but only tests and the fuzzer run that
(`State::debug_check`, `state/mod.rs:606-617`). In production `Chain::apply_block`, at each epoch
boundary, runs `programs::check_totals(&state)` (non-consensus, O(programs + blobs): Σ balances
of program accounts, Σ per-program `sponsored`, and Σ per-program `deposit`, `record_deposit`
and blob deposits, against `value_held`, `sponsored` and `escrow`); a mismatch logs an error and
sets `blockula_program_accounting_drift`, so a bookkeeping bug that moves the cap is an alarm,
not a silence (safety F12).

**The cap:** after any transaction that raised the control, `value_held + sponsored ≤
program_value_cap` (10M FANG by default), or the transaction fails (`value_cap`).
One that doesn't raise it is never refused by the cap: payouts lower it, and a sponsored deposit
or its refund leaves it unchanged, so exits and clean-ups at the cap keep working, and a program
can't take in more than the cap by parking its balance in storage it sponsors (without this, the
storage curve alone bounded that, at about 55M FANG of deposits under the default soft cap and
440M at its bound: Σ over the curve's 16 steps of `soft/8 × price × 2^k`). Griefing it is
accepted explicitly, as PRG 9.3 says (anyone can park FANG in their own program to fill it; it
costs them the capital's yield, and exits keep working); `/api/fees` publishes the headroom, and
the council can raise it.

**The native-credit guard**, `programs::native_credit(state, j, to, amount, kind, height) ->
Result<()>`, runs at each of the four credits that pay a recipient the transaction or proposal
names: right after the credit, in the same journal, so an `Err` rolls the credit back with the
rest of the action (for `Unshield`, before its effects run):

| Site | `kind` | A program recipient |
|---|---|---|
| `Transfer` (`chain.rs:1422-1423`) | `Transfer` | allowed while `status` is `On` and the program `accepts_transfers`; `value_held += amount` (journaled), then the cap on the control. Otherwise `Err` → `Failed` |
| `AwardHunt` (`chain.rs:1586-1587`) | `Other` | `Err` → the award fails, charged |
| `TreasurySpend` (`governance.rs:889-890`, and early in `prepare_kind`, `governance.rs:508-511`) | `Other` | `Err` → refused when proposed, and the proposal fails if it runs |
| `Unshield` (`ghost.rs:618-622`, once `to` is resolved) | `Other` | `Err` → unadmittable (an unsigned tx can't fail with a fee charged); Ghost Mode is refused on `blockula-1` anyway (W2) |

For a recipient that isn't a program (every recipient on testnet-2), the guard is one map lookup
and returns `Ok` (§1.2). A plain `Transfer` never runs code (PRG 8.3).

### 7.2 `ProgramsPause` (tag 69)

Follows `GhostPause` (`ghost.rs:535-556`), with limits `GhostPause` lacks, sized for a set as
small as four validators (A25):
- **Who signs:** a validator in the current epoch's active set, through its own account (its
  consensus key, which W2's float pays for: a duty, with a slot in the mempool's lane) or its
  owner's (an owner key, which may tip): `staking::validator_for_signer` (`staking.rs:1937-1951`).
  Never nested (§2.1).
- **When it's admitted:** version ≥ 2 and META `programs` present: in `On`, `ExitOnly` and while
  a pause is running (a signature then counts toward renewing it). The vote and window rules
  below are admission checks, so a refused signature is never charged.
- **Votes.** `votes` maps each validator to the height of its pending signature. A vote lives
  `PAUSE_VOTE_BLOCKS` (a day at the chain's block time, `params::blocks_per_day`,
  `params.rs:295`), across epoch boundaries; votes older than that, and those of validators no
  longer in the active set, are dropped before counting. A validator with a live vote can't sign
  again until it lapses.
- **Threshold:** f + 1 of the active set at `h`, `ghost::pause_threshold(n) = (n − 1) / 3 + 1`
  (`ghost.rs:249-251`). On reaching it, with no pause running, a new **run** starts: `pause = {
  from: h + 1, until: h + 1 + PROGRAMS_PAUSE_BLOCKS, started: h + 1, signers }`; with one
  running, it's **renewed**: `until = min(max(until, h + 1 + PROGRAMS_PAUSE_BLOCKS), started +
  PAUSE_MAX_SPAN)`, `started` unchanged, and `signers` are the new ones. Either way each counted
  voter's `pause_signed` becomes `h`, and `votes` clears. `PROGRAMS_PAUSE_BLOCKS` is 3 days at
  the chain's block time (259,200 at 1 s), a code constant.
- **The window** (`PAUSE_SIGN_WINDOW`, 30 days at the block time): a validator whose
  `pause_signed` is younger than the window can't sign **unless a fix is pending**:
  `programs::fix_pending(state, h)` is true while a `ScheduleUpgrade` or a `DeactivateFeature {
  programs }` proposal is voting or queued (`governance.open_proposals`), or an upgrade is
  scheduled above `h` (`upgrade::next_after`). Only a signature that starts or renews a pause
  spends the window: one that never reached the threshold costs its fee and nothing else.
  `pause_signed` entries older than the window are dropped when the next pause starts.
- **The cap** (the maintainer's decision, 2026-10-20): a run of pauses (one started, then renewed
  while it runs) never extends past `started + PAUSE_MAX_SPAN`, `PAUSE_MAX_SPAN` being 30 days at
  the chain's block time (2,592,000 at 1 s), a code constant; and a new run can't start while `h <
  started + 2 × PAUSE_MAX_SPAN` of the last run, whose `pause` record stays in META after it lapses
  for exactly this check (so no end-of-block hook is needed). Both are admission checks, never
  charged: with no pause running, a signature is refused while the last run's spacing holds
  ("programs can be paused again from block N"); with one running, a signature is refused once
  `until` has reached `started + PAUSE_MAX_SPAN` ("this pause has reached its 30-day cap"). The cap
  holds whether or not a fix is pending.
- **While `from ≤ h < until`** (`status` is `Paused`): tags 62, 63 and 64 are refused at
  admission (`Err`, never charged), so nothing new runs; native credits to programs are refused
  too (stricter than PRG, which let them through: during an incident no value should flow in);
  `/view` keeps working. It **lapses on its own** at `until`; there's no unpause.
- **Recorded on chain:** `pause` and its signers, `votes`, `pause_signed`, served by
  `/api/params` and the explorer.

What the limits allow. Without a pending fix, each validator takes part in at most one pause (or
renewal) per 30 days, so at most ⌊n / (f + 1)⌋ three-day pauses chain in any 30 days: two at
n = 4 (6 days), the same two as PRG's 32 seats. While a fix is pending, any f + 1 validators can
keep programs paused until it lands (§7.4), but never past 30 days from the run's start, and a
pause refuses value-free exits too, so they can hold the FANG in programs that long at most.
"Pending" is cheap to make true (anyone may open a `ScheduleUpgrade` proposal for its deposit, and
a scheduled upgrade counts until it activates, up to 365 days out, `MAX_UPGRADE_HORIZON_DAYS`), so
the cap is what bounds the time: program FANG can be held without a vote for at most 30 days in
any 60, as PRG's `FREEZE_MAX` bounds its freeze. On a set one party runs, who signs is that
party's power. The cap covers §7.4's fix path (9 days at the floors, 16 at the default periods)
with three weeks to spare at the floors and two at the default periods; its price is that a second, unrelated incident within 60 days of a run's start
gets exit-only and the operators' `--halt-height`, not a pause. The pause playbook (OPERATIONS.md,
"Programs") says to agree before signing, that a lone signature costs only its fee, and when the next run
can start.

### 7.3 Exit-only

`status` is `ExitOnly` while META `programs` exists and the feature is off (§1.4). Then:
- refused at admission: `UploadCode`, `CreateProgram`, and any `CallProgram` carrying `value > 0`;
- refused at commit (`exit_only`, the transaction fails, charged): any call whose diff raises
  **the FANG programs control**, `value_held + sponsored` (§7.1), the measure the cap uses too.
  Commit computes its change from the diff and the stored entries it replaces (W1's
  `StorageChange` carries only the new entry; commit reads the old one from namespace 69 before it
  writes): the net delta of program accounts (§6.1 step 6.4), plus, over `diff.storage`, each new
  program-paid entry's deposit, minus each replaced or removed one's (an entry is program-paid
  when its `payer` is its own program, §6.1 step 6.1). A signer's own deposit, within its
  `storage_limit` and refundable only to that signer, isn't programs' FANG, so a withdrawal that
  writes a record (a `claimed` marker, a balance for a holder who had none) works (A26); a
  program moving its own balance into or out of its deposits nets to zero;
- native credits to programs refused (§7.1);
- everything else runs: calls that pay users out, that move FANG between programs, that read;
  writes the signer bonds; removals; `ProgramsPause`; `program_*` parameter changes.

Switching back on is `ActivateFeature { programs }` (⅔, or the council while it lasts; on
`blockula-1` only from v3, §1.7).

### 7.4 What they can and can't do, in time

- A pause stops new program transactions from the block after f + 1 signatures: within seconds
  of the validators deciding. It never moves funds, and it holds them only while it runs.
- Exit-only and a parameter change (a lower cap, a lower `program_max_block_fuel`) take a
  governance vote: at the shortest a vote can set them to (`params.rs`'s `governance_band`: 3
  days of voting and a 1-day timelock, the "floors"; at 1-second blocks `voting_blocks = 259200`,
  `timelock_blocks = 86400`). `blockula-1`'s genesis must start inside that band
  (`check_governance_floors`); a kit chain's may start below it, as `blockula dev`'s and
  `summon --dev-durations`'s do (votes of a minute), and then no figure below in days holds for
  it. A chain's default periods are longer than the floors, 7 days of voting and 2 of timelock, and
  every figure below in days grows with them. The council passes at the close, not before
  (`governance.rs:771-785`).
- A fix to the engine or the integration takes a release and a `ScheduleUpgrade`: proposed at t,
  it runs at t + 4 days and may schedule the version no earlier than t + 4 days + 3 days + 66
  blocks (`governance.rs:547-557`: the timelock's end plus a `CancelUpgrade`'s vote plus
  `MIN_UPGRADE_LEAD` + 2).
- Exit-only can't stop an engine exploit that drains programs through calls that look like
  payouts; only the per-transaction conservation checks (`Diff::check`) and the pause can. So
  the pause has to cover the whole fix path.

**The fix path at n = 4** (f + 1 = 2), at the governance floors, with the incident found at t₀ and
the council proposing at once (the validators are the bootstrap council: GOVERNANCE.md):

| When | What happens | What stops programs |
|---|---|---|
| t₀ + 1 block | two validators sign `ProgramsPause` | pause 1, until t₀ + 3 d |
| t₀ (same hour) | the council proposes `DeactivateFeature { programs }` and `ScheduleUpgrade { 3, H }`, H ≥ t₀ + 7 d + 66 blocks; from here `fix_pending` holds | pause 1 |
| t₀ + 3 d | before pause 1 lapses, any two sign again (both proposals are queued: the window doesn't apply) | pause renewed until t₀ + 6 d |
| t₀ + 4 d | the `DeactivateFeature` runs: exit-only; the `ScheduleUpgrade` runs: v3 scheduled at H (still `fix_pending`) | pause + exit-only |
| t₀ + 6 d | any two renew again | pause until t₀ + 9 d, + exit-only |
| H ≈ t₀ + 7 d + 66 blocks | v3 activates: the fix runs (the release supporting it shipped before H) | exit-only; the pause until t₀ + 9 d |
| t₀ + 8 d or later | the council's `ActivateFeature { programs }`, proposed once v3 is scheduled (from t₀ + 4 d; it may run only at or after H, §1.7), runs after its vote and timelock | exit-only until it runs |

Under PRG's rule (and this file's first draft) the same arithmetic left t₀ + 6 d to H covered by
exit-only alone: a day and 66 blocks in which an exploit that looks like payouts runs on. If the
fix takes longer than a week to write, H is set later or a second `ScheduleUpgrade` is proposed:
`fix_pending` holds as long as one is open or scheduled, and the pause is renewed every 3 days, up to 30 days from pause 1's start (the cap, §7.2); past that, exit-only and the operators' `--halt-height` cover what's left.

At a chain's default periods (7 days of voting, 2 days of timelock) the same path takes 16 days
and 66 blocks (7 + 2 + 7 days, then 66 blocks) before the version may activate, and the 30-day cap
leaves 14 days to spare. At the floors it takes 7 days and 66 blocks, and the cap leaves 23.
The operators' coordinated `--halt-height` (`upgrade.rs:559-585`; OPERATIONS.md, "Incident
response") stays the last resort, and it stops the chain, not just programs. There's no
`programs_freeze` in v0 (A1).

## 8. Receipts, API and clients

### 8.1 Receipts

Every program transaction a block applied (tags 62, 63, 64; applied or failed, not skipped) gets
a receipt. `ProgramsPause` doesn't (its effect is META state).

```rust
/// node/src/programs/receipt.rs. Serde JSON, display only (A10): never hashed into the root.
pub struct ProgramReceipt {
    pub version: u8,                          // 1
    pub kind: ReceiptKind,                    // upload | create | call
    pub outcome: ReceiptOutcome,              // below
    pub fuel: FuelUse,                        // { limit, load, used, reserved, rate } (below)
    pub fee: FeeParts,                        // { bytes, fuel, tip } in drops: Σ is what was charged
    pub calls: Vec<CallResult>,               // { program, method, value, return_data } per call: return data empty unless ok
    pub events: Vec<Event>,                   // { program, name, data }: empty unless ok
    pub omitted: Option<Omitted>,             // { events, event_bytes, return_bytes }: past the claims budget (below)
    pub balance_changes: Vec<BalanceChange>,  // { account, change: decimal string of drops }: the program diff's net deltas, sorted (the fee is in `fee`)
    pub storage: StorageSummary,              // { bytes_delta: i64, bonded: u64, refunded: u64 }: informational
    pub outside_vm: OutsideVm,                // { record_deposit, upload_deposit, account_creation_fee, total }: debits outside the VM diff
    pub burned_by_programs: u64,              // drops the VM diff burned (`Diff.burned`): creation fees of accounts a program's transfer opened
    pub created: Option<String>,              // a create's program address
    pub upload: Option<UploadResult>,         // { wasm_hash, interface_hash, new_code, new_interface, deposit }
}
```

**`FuelUse`.** `limit` is the signed `fuel_limit`, `load` the declared code's load, `reserved`
their sum and `rate` the rate it was charged at (§5.1); `used` is W1's `Receipt::fuel_used`, which
equals `limit` after any `OutOfFuel` that fails the transaction, nested or not, even when only a
callee's smaller `call_v1` share ran out (W1's `exec.rs`, pinned by
`a_callee_out_of_its_share_fails_with_the_whole_limit`); under `try_call_v1` the caller runs on
and only the callee's share counts. An upload's `limit` and `used` are its `upload_fuel` (it
signs none), and a create without `init` has 0 for both. Nothing is refunded from `used`, and it
isn't a measure of the work done.

**`balance_changes`** is the VM's diff only, in drops as decimal strings (an `i128` as a JSON
number loses precision past 2^53); the signer's own write bonds (an initializer's or a call's
entries) are in it. What a program transaction debits outside the VM is in `outside_vm`: a
create's record deposit, an upload's deposit, and the account creation fee a create burns when
the program's address held no account (an adopted address pays none). `storage.bonded` is
informational (everything the transaction bonded, including what `balance_changes` already
counts). The signer's balance reconciles exactly: Δbalance(signer) = −(`fee` total +
`outside_vm.total`) + `balance_changes[signer]` (S6b's review, S6B-R1). FANG a program's own send
burns isn't the signer's: when a program's transfer opens an account, the creation fee
(`account_creation_fee`) is burned from the program's balance inside the diff, out of the amount
or on top of it (`Overlay::transfer`), and `econ::charge_new_accounts` skips the accounts a
program's commit opened. The receipt states it as `burned_by_programs`, so conservation can be
asserted rather than derived: Σ `balance_changes` + (`storage.bonded` − `outside_vm.record_deposit`
− `outside_vm.upload_deposit` − `storage.refunded`) + `burned_by_programs` = 0 (S7b's review). `CallResult.method` and `Event.name` are the program's own identifiers, stored plain in the
block; `/api/tx` serves them as `untrusted_method` / `untrusted_name` (§8.2) and the SDK keeps those
names. Trap kinds and validation rules print as `{ number, name }`; both are pinned, since
renaming either would change the record of every block already run.

`ReceiptOutcome` is `{ ok: true }` or a failure, numbered (stable now, so a later receipts root can
commit them unchanged). 1–3 are W1's `FailCode::number()` for a failed `vm::exec::Receipt`; 10
and up are the node's own:

| # | `code` | Detail |
|---|---|---|
| 1 | `trapped` | `trap`: W1's `TrapKind` number and name; `program`: where the failure began |
| 2 | `aborted` | `program`, `abort_code`, `untrusted_message` (≤ 256 bytes, sanitized by the VM's `host::sanitize`, which is consensus: a caller reads the same text after `try_call`, §9.2) |
| 3 | `commit` | `commit_error`: W1's `CommitError` number (a host bug, counted by a metric) |
| 10 | `value_cap` | the cap, and the control (`value_held + sponsored`) the transaction would have left |
| 11 | `exit_only` | |
| 12 | `storage_limit` | an upload's or a create's own deposits past `storage_limit` |
| 13 | `state_full` | an upload or create past the ceiling |
| 14 | `invalid_code` | `rule`: W1's `validate::Rule::number()` (`#[repr(u8)]`, pinned by `validate::tests::rules_keep_their_numbers`: 1 `Size`, 2 `Wasm`, 3 `Feature`, 4 `Float`, 5 `Import`, 6 `Memory`, 7 `MemoryGrow`, 8 `Table`, 9 `Limit`, 10 `Segment`, 11 `BodySize`, 12 `Export`, 13 `Start`, 14 `CustomSection`), and its `func` and `offset` when it has them; never the message |
| 15 | `translation_refused` | |
| 16 | `interface_mismatch` | |
| 20 | `native` | a native check failed (funds for `value` or deposits, locks, the creation fee): its wording is in the tx's `error`, as for any failed tx |

Never engine or host text: only numbers and the program's own sanitized abort message (PRG 6.5).

**The claims budget.** Return data costs 1 fuel a byte (`HostCosts::per_byte`) and event data 8,
because neither is meant to be kept; a block's receipts would otherwise let anyone write tens of
MB of hex to every full node's block log a second for a fraction of a FANG. So a block records at
most `MAX_BLOCK_CLAIM_BYTES` = 256 KiB of claims over all its receipts, in transaction order,
counting each event as its data plus its name plus 32 and each call's return data as its length.
A receipt whose claims don't fit what's left records none of them: it keeps every fact (outcome,
fuel, fee, balance changes, storage, created, upload) and says `omitted: { events, event_bytes,
return_bytes }` instead. The count goes on past it: a later receipt whose claims fit what's left
records them, so the receipts that omit claims need not be the block's last. Every node computes
the same receipts, so the cut is the same everywhere. Facts are bounded by fuel already (a balance change needs a `transfer_v1` at 10,000
fuel or a call's value). A later receipts root that commits claims prices them first (§9).

**Where receipts live.** `types::Block` (`types.rs:2259-2296`) gains
`programs: BTreeMap<usize, ProgramReceipt>`, keyed by index into `txs`, with
`#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]`, documented like `base_fee`:
"display only: replay recomputes it". So:
- `programs::apply` pushes each receipt, with its tx id, into `state.programs.block.receipts`;
- `Chain::apply_block` takes them after `run_block` (`chain.rs:3082`), if `BlockRun.height` is
  this block's (a block with no program transaction leaves an older height's there, which it
  ignores), and keys them by the tx's index in the block it builds (`chain.rs:3108`). Where the
  tx's outcome is `Failed` but its receipt is ok (a check after the program ran failed), the
  receipt's outcome becomes `native`;
- the block log stores them with the block (the writer serializes `Block` as it does today:
  `store.rs`, `Writer` and `persist` are untouched, and `STORE_FORMAT` stays 3);
- `Chain::replay` keeps the logged block's receipts. P2's replay gate (from `9bb65dc`) compares
  each replayed block's **whole record** with the recording, failure reasons aside
  (`replay::compare` on JSON values, after fates, skipped sizes, state root and base fee). So a
  v1 block's lack of a `programs` key is checked by the gate itself, and on a recorded chain that
  ran programs the receipts this binary computes must equal the recording node's: the gate holds
  receipts to determinism across binaries, though they're no part of the root. A later release
  that changes a receipt's JSON shape for blocks already run does so from an activation height,
  like any field of the block record, or the replay of older recordings fails (§9.2). A v1
  block's JSON line stays byte for byte what v0.5.2 writes (a unit test, §10.1: the gate compares
  values, not bytes);
- pruning drops them with their block (`410 pruned`, as today);
- `Outcome::Failed(reason)` for a program transaction is rendered from the receipt ("program
  boo1…xyz trapped: out of fuel", "program boo1…xyz aborted with code 7"), display text that
  `check_replayed` doesn't compare (`chain.rs:1809-1821`).

A receipt is what this node computed. Facts it reports (balances, storage, deposits) are also in
the state and provable (`/api/proof`); events and return data are the program's claims (PRG 8.4).

### 8.2 The node's API

New routes in `api.rs`'s router (`api.rs:470-512`), handlers in `node/src/programs/api.rs`, each
reading `n.live.view()` (a `ReadView`, never the chain lock):

| Route | Returns |
|---|---|
| `GET /api/program/{address}` | The record: `creator`, `created_at`, `code` (`wasm_hash`, size, `interface_hash`), `version` and `version_digest`, `authority`, `upgrade_delay_blocks`, `policy`, an `upgradability` sentence ("immutable", or "no upgrades exist yet; once a protocol version adds them, X may upgrade it with N blocks' notice"), `accepts_transfers`, `balance`, storage totals, `record_deposit`, the decoded methods with `untrusted_name`s, the proof path (`/api/proof/66/{hex(address)}`), and `height` |
| `GET /api/program/{address}/storage?after=&limit=` | Entries by key prefix (hex), a page at a time (should, not must) |
| `GET /api/code/{hash}` | `size`, `exports`, `uploader`, `deposit`, `refs`, `uploaded_at`, `last_used_at`; `?wasm=1` adds the code as hex |
| `GET /api/interface/{hash}` | The raw bytes as hex and the decoded JSON (§3.4) |
| `POST /api/program/{address}/view` | Body `{ method, input, bindings?, fuel? }`; runs a `View` method (§ below); returns `{ ok, outcome, return_data, fuel_used, height }` |
| `GET /api/tx/{id}` | A program transaction gains `"program"`: the receipt, split into `facts` (`balance_changes`, `storage`, `outside_vm`, `burned_by_programs`, `fuel`, `fee`, `created`, `upload`) and `claims` (`events`, `return_data`), with `untrusted_method` / `untrusted_name` (PRG 8.4's shape). `fee` and `fee_display` are the receipt's sum (`charged`, `api.rs:663-669`, reads it) |
| `GET /api/fees` | The `fuel` section (§5.5) |
| `GET /api/params` | `programs`: `status`, the parameters with bounds and class (§4.5), `fuel_rate`, `value_held`, `sponsored`, `escrow`, `state_bytes`, `programs`, `pause` and its signers, `votes`, `pause_signed`, `fix_pending` (bool: `pause::fix_pending(state, now)`, §7.2). The whole section is omitted while Dormant; pre-activation parameter availability and activation-version rules belong in the operating documents and the CLI's help (OPERATIONS.md, GOVERNANCE.md; §4.5, §1.7) |
| `GET /api/account/{id}` | For a program, a `program` summary and a link to `/api/program/…` |
| `GET /api/search/{q}` | `{ type: "program", id }` for a program address; `code` for a stored hash |
| `/api/health`, `/api/status` | `halted: "program_fault"` or `"program_resources"` (§6.2) |
| `/metrics` | Gauges, printed once programs exist (META `programs`; nothing while Dormant): `blockula_programs_status` (1 on, 2 exit-only, 3 paused), `blockula_program_block_fuel` (the last executed block's counted fuel), `blockula_program_fuel_rate` (milli-drops per fuel the next block charges), `blockula_program_value_held_drops`, `blockula_program_sponsored_drops`, `blockula_program_state_bytes`. The safeguards' series (§6.2, §7.1): `blockula_program_node_faults_total{path="propose"\|"verify"\|"execute"\|"startup",class="tx"\|"setup"}`, `blockula_program_quarantined{kind="tx"\|"sender"\|"code"}`, `blockula_program_uncertified_proposals_total`, `blockula_program_accounting_drift` and `blockula_replay_cache_hits_total` (§6.6). Views: `blockula_program_views_total{outcome="ok"\|"failed"\|"refused"\|"limited"\|"fault"}`. Rev 2 of this file also listed `blockula_program_txs_total{kind,outcome}`, `blockula_program_fuel_reserved_total` and `blockula_program_conservation_faults_total`; this release does not emit them (each program transaction's receipt carries the same facts, §8.1) |

**`/view`.** Reads only. While `status` is `Dormant` it answers 404 before doing anything (no
program can exist), as the other program routes do for an address with no record. Otherwise it
takes a copy of the newest executed state (an O(1) `imbl` clone, PRG
8.5), judges `h` = executed height + 1 as admission does, and refuses a method that isn't `View`.
It runs on `tokio::task::spawn_blocking` and, inside, on the same 64 MiB scoped thread with a
fresh engine as a transaction (§6.1), with the caller as 32 zero bytes, no value, and
`min(fuel, --view-max-fuel, program_max_tx_fuel)` (the flag defaults to `MAX_TX_FUEL`; the
chain's current parameter always bounds it, so a governance cut applies at once). A View frame
can't write, transfer, emit or call a non-`View` method (W1). Every client string is validated
before it's echoed or used (a binding's `program` is an address, a method an identifier, a digest
64 hex). The answer's `height` is the executed height of the state read; the VM judges at
`height + 1`. Limits: a per-IP budget of 60 views a minute (`VIEWS_PER_IP_PER_MINUTE`, loopback
exempt, taken before the body is parsed) through `Node::limits` (`api.rs:848`'s pattern;
`limit_reads` covers only GET and HEAD, `api.rs:296-309`), answered `429` with Retry-After past
it; and at most `MAX_CONCURRENT_VIEWS` (2) running per node: a view that finds both slots taken
waits up to 250 ms for one, with at most 16 waiting, else `429` (Retry-After 1, and the per-IP
hit is refunded). A slot is held until the VM's task ends, not the request, so a client that
disconnects can't run more than two VMs. A node fault here is a `500` and a metric, never a
halt. A view runs outside §6.2's trial marker: a program whose execution kills the process (an
abort, not a caught panic) crashes the serving node, and nothing records it for the restart.
Views are off on validators by default for that reason among others, and the fuzzing gate
(§10.4) covers the engine; a marker around views is a follow-up (S8). **It's off on validators** unless started with `--program-views`, and on by
default on full and RPC nodes (A24, PRG 9.5): a validator's HTTP is private only by convention
(docs, #44), and a view runs a VM for whoever asks.

**Submission.** `submit_inner` (`api.rs:876-913`) refuses early, before the signature check (like
Ghost Mode's early refusal, `api.rs:882-890`), exactly what `programs::admit` would refuse for
the status in force at the next height, tag by tag: 62 and 63 unless `On`; 64 unless `On`, or
`ExitOnly` with every call's `value` 0; 69 while `Dormant`; any of them while `Paused` but 69.
So exits and pauses get through the public node while programs are exit-only or paused. Its fee
check includes fuel through `econ::check_fee`.

**No list route in v0.** Nothing lists deployed programs: the explorers show the creates in a
window of recent blocks, a stopgap that any account can crowd with cheap creates, and an older
program is reached by its address. A follow-up (should, before a supported programs-on release):
`GET /api/programs?after=<address>&limit=<n>`, program addresses in creation order (`created_at`,
then address), each with `created_at`, `wasm_hash` and the creator, a page at a time (`next`),
from an index the node keeps beside the history rows; the explorers then list from it (§8.6).

### 8.3 The CLI: `blockula program`

`node/src/program_cli.rs`, with `Cmd::Program` in `main.rs` next to `Cmd::Name`
(`main.rs:256`, `:411`), away from P2's `Cmd::Replay`. It signs like `blockula name` and `stake`
(`contrib_cli.rs`): the same key files, `--network` pinning and confirmation rules.

| Command | Does |
|---|---|
| `upload <code.wasm> --interface <iface.json> [--storage-limit FANG] [--yes]` | Encodes the interface (§3.4), prints both hashes and the deposit at today's price, asks (the deposit is permanent in v0), signs tag 62 |
| `create <wasm_hash> <interface_hash> [--init METHOD --input HEX --value FANG] [--fuel N] [--storage-limit FANG] [--authority ADDR --delay BLOCKS --policy open\|additive] [--accepts-transfers] [--bind ADDR[@DIGEST]]…` | Prints the address it will get, with the warning that nothing may be sent there before the create is final (a failed create strands it, §2.3), the record deposit and, when the address holds no account yet, the account creation fee it burns; asks (`--yes` skips); signs tag 63 |
| `call <address> <method> [--input HEX] [--value FANG] [--fuel N] [--storage-limit FANG] [--bind ADDR[@DIGEST]]…` | Reads the record, prints the whole `version_digest` it signs (refusing one that isn't 64 hex), the most the fee can be and, without `--storage-limit`, that the call can't bond new storage; signs tag 64, waits and prints the receipt |
| `view <address> <method> [--input HEX] [--bind ADDR]…` | `POST /view` |
| `show <address>`, `code <hash>`, `interface <hash>` | The reads above |
| `pause` | For a validator: prints what it does, the live votes, whether this validator may sign (§7.2's window, a pending fix), asks, and signs tag 69 from the validator's own account with its consensus key (the duty form) |
| `interface encode <iface.json>` | Bytes and `interface_hash`, offline |

`--fuel` defaults to `program_max_tx_fuel` (there's no `/simulate` in v0, PRG 8.5); the most that
overpays is `max_tx_fuel × rate`, 0.0025 FANG at the default floor. `--storage-limit` defaults to
0 for calls (a cap is consent, and 0 consents to no new storage: a call whose writes bond a
deposit traps `storage_limit` and still pays its fee; `call` says so before it signs and again,
in words, if the call trapped for it; pass a limit for any method that writes) and to the deposit
at today's price plus 10% for uploads and creates. Node answers are checked before use (a status
height the clock rules out, a malformed digest): a lying node gets a refusal in words, never a
panic and never a signature. Every command
that signs refuses a reservation above `max_block_fuel` before signing (§4.5). Addresses only,
never names, for programs and bindings (PRG 8.1). `upload`, `create` and the first `call` are one
step per block (§2.1): `create` and `call` wait until the code or program is usable (the block
after the one that stored it) and say so, instead of being refused by the mempool.

### 8.4 The SDK (`@blockula/sdk`)

- **`sdk-ts/src/tx.ts`:** the four actions in `Action`, `encodeAction` for tags 62, 63, 64 and 69
  byte for byte (§2), `putBytes` (u32 length), and `versionDigest`, `programAddress`,
  `wasmHash`, `interfaceHash`, `methodSelector` (BLAKE3 with `context`, as `digestOf` does,
  `tx.ts:684-692`).
- **Vectors:** `types::tests` gains `program_vector_actions()` (every tag, every option set and
  unset, the bounds), written to `sdk-ts/test/tx-vectors.json` with the other vectors; a sibling
  of `state/proof_vectors.rs` writes digest vectors (addresses, version digests, selectors,
  hashes) that `sdk-ts/test/` checks. `tx-vectors.ts:15`'s tag list grows by 62–64 and 69.
- **`describe.ts`:** summaries from structure only ("alice.boo called program boo1…xyz with
  5 FANG attached"), never a method name (PRG 9.7); unknown future tags stay "can't read"
  (`describe.ts:350-351`). They land with the vectors, because `tx-vectors.ts:20` asserts every
  vector's action has a description.
- **`Wallet`:** `uploadCode(wasm, iface, { storageLimit })` (`iface` is §3.2's format-1 bytes, a
  `Uint8Array` or lowercase hex, as `blockula program interface encode` prints them or the guests'
  `.iface` files hold; an SDK encoder for the JSON form, §3.4, is a later release), `createProgram({ wasmHash,
  interfaceHash, init?, fuelLimit, storageLimit, bindings, authority?, … })`,
  `callProgram(calls, { fuelLimit, storageLimit, bindings })`, a convenience `call(program,
  method, input, { value, fuelLimit, … })` that reads the record and signs its `version_digest`,
  and `programsPause()`. They price fuel at `/api/fees`' `suggested_rate` (§5.5), refuse a
  reservation above `max_block_fuel` before signing, and wait, after an upload or a create, for
  its block to finalize and one more before resolving, so a following `createProgram` or `call`
  isn't refused (§2.1). `createProgram` returns the program's address only once its create has
  applied, never a predicted one to fund ahead (§2.3).
  The default `fuelLimit` is `min(program_max_tx_fuel, program_max_block_fuel − load)`, so the
  default never trips the refusal. An upload's default `storageLimit` is the deposit as if both
  blobs were new, plus the CLI's margin (a limit is a ceiling the chain bonds only what's new
  against, so one node's "already stored" can't lower it); the node's answer only displays the
  expected deposit. The tip cap is priced on a lower bound of the including block's base fee
  (bytes at `lowest`, fuel at `floor_rate`): a tip within it is never refused, and the node
  enforces the real cap. `programsPause()` signs tag 69 from the validator's own account and
  lets admission decide: it needs programs switched on (`/api/fees` has `fuel`), reads and prices
  no fuel (it reserves none), and a refused signature (§7.2's window, a live vote of its own,
  not in the active set) is an `ApiError` 400, never charged. The generic `build()`/`act()`
  refuse tags 62–64 (they price no fuel) and name the helper to use.
- **After submission:** once a program transaction is submitted, every later failure (a receipt
  or answer that doesn't check, a created address or upload fact that disagrees) says the
  transaction was sent, names its id and says not to send it again, with the reason as its
  cause, like the waits' errors; never "nothing from that answer was used: ask another node"
  alone. `ProgramFailedError`'s message holds only numbers and the chain's own words; a
  program's abort message is on the error as `untrusted_message`, never in `message`. Text a
  node or program chose that the SDK quotes is stripped of control, format (bidi, zero-width),
  private-use, surrogate and line/paragraph separator characters.
- **`Client`:** `program(address)`, `code(hash)`, `interface(hash)`, `view(address, method,
  input, opts)`, and `provenProgram(address, { witnesses })` over `provenRecord` (namespace 66),
  which decodes `ProgramRecord` format 1 (§4.2) and recomputes `version_digest` itself. Its
  refusals are tested one by one (a tampered value, a record that decodes but isn't the leaf, an
  inclusion proof claimed as absent, a wrong key or namespace, a witness root that differs, a
  head past `maxLagBlocks`), and a `call` with witnesses refuses, before signing, a node whose
  record differs from the proven one.

### 8.5 The MCP

Read-only in v0: `program_show` (the record, methods as `untrusted_method`, upgradability,
`version_digest`), `program_view` (a `View` method's return data, as the program's claim) and
`program_code`. No tool signs a program transaction: calls by agents need the confirmation flow
PRG 11.5 specifies (elicitation, an allowlist), a later release. Like the other read tools they
are titled and annotated read-only, and they read through the MCP's pinned client: a node on
another chain than the pinned one, or on `blockula-1` without the user's pin, is refused before
anything is read from it or a view is run on it. Method names, interface
identifiers, event names and abort messages go through `untrusted.ts` as `untrusted_*`, and tool
descriptions never include chain text. Every field a program or its deployer chose is named `untrusted_*`:
`untrusted_method`, `untrusted_argument`, `untrusted_exports`, `untrusted_return_data`, and in
receipts `untrusted_name`, `untrusted_message` and `untrusted_data` (the bytes of a return value or
event, always summarized past 64 bytes), whichever tool reaches them (`lookup` included). With
`BLOCKULA_WITNESSES`, `program_show` builds the record's fields from the proven record (code and
interface hashes, version, creator, `created_at`, authority, policy, delay, `accepts_transfers`,
storage totals, record deposit), refuses a node whose reading of any of them differs, says in
`proven` exactly what was proven, and marks every other field (balance, unless proven with
`provenAccount`; methods and types) as the node's word. `program_view` says that the serving
node ran the view and nothing proves its answer, and names the node.

### 8.6 The explorer

Read-only. The node's built-in explorer (`node/src/explorer.html`) and the web explorer
(`web/app`, `/program/{address}`) show a program: its record and upgradability, its methods as
"named by the deployer", its balance and storage totals, and its recent transactions (the account
history: tag 64's `addresses()` names each call's program, so the store's history rows index
it). A transaction page shows the receipt with facts apart from claims (PRG 8.4). "What was signed,
exactly" abridges only the blobs a program action carries (`wasm`, `interface`, `input`, data);
every v1 action prints exactly as before, keys and proofs whole. Until the list route exists
(§8.2) the Programs page lists the creates in a window of recent blocks, and says so.
`web/lib/explorer/programs.ts` lists deployed programs beside native ones (`Program.kind:
"deployed"`) with no `ActionSpec` forms yet (calls are CLI- and SDK-only in v0);
`web/lib/explorer/ledger.ts` learns what tags 62–64 move (fee, value, deposits, the creation
fee); the e2e mock node's world (`web/e2e/explorer-world.ts`) gains a program, a call and a failed
call, so `explorer-pages.spec.ts` visits the program page under the strict CSP.

## 9. Upgradability

Every later change to programs is a new protocol version, scheduled by `ScheduleUpgrade`;
binaries that don't run it halt cleanly at its height (`upgrade.rs:1-43`). Each binary keeps every
rule ever in force and picks by `version_at(h)`, so a node replays all of history (PRG 7.6, D16).
This release is built so that each piece has a way to change without touching a v0 format.

### 9.1 How a later version changes each part

| Part | How it changes later |
|---|---|
| **Actions** | New tags only: 65–68 and 70 with PRG's meanings, 71–79 for more. A v0 tag's encoding never changes; a new field means a new tag. Allowing nesting in a multisig (A8) is a version gate on `decode_at`'s refusal plus PRG 5.1's reservation rules |
| **Host ABI** | Add-only: new functions with new `_vN` names (PRG 10.1). Code importing a function not in force at upload is refused, so a program never meets a function that appears under it. ABI v1's functions keep their semantics forever |
| **Engine** | A renamed dependency per engine generation (`engine_v2 = { package = "soroban-wasmi", version = "=…" }`), chosen by `version_at`; never removed. Every stored module re-validated under the new one before release (PRG 7.6) |
| **Fuel schedule** | `SCHEDULE_V1` (and so on) from its version's height for every program: fuel is the chain's price, not a program's property, and no program can read it (no `fuel_left`, PRG 10.2) |
| **Limits and prices** | Parameters move within their bounds by governance; bounds and constants move only with a version, after re-measurement |
| **Fee model** | The controller's target and step (ChainParams' today) or a ring fixing the rate `FEE_LAG` ahead (PRG 5.2) arrive as new META fields at their version, beside `fuel_rate` |
| **Counting load once per block** | Counting each distinct code's load once per block is a counting rule keyed by version (A3); sharing an engine without it needs no version |
| **Records** | Each value's format byte: a version writes format 2 (on every write from its height, or with a bounded sweep it specifies); its readers accept 1 and 2; format 1's decoder is never removed |
| **Parameters** | New keys join the map; a missing key reads as its default, so no migration (§4.5) |
| **Interface** | Format 2 (events, errors, named types, docs) accepted at upload from its version; format 1 stays valid forever |
| **Receipts** | A receipts root (namespace 70) from its activation height; v0's receipt shape and outcome numbers are kept for it, and claims it commits get a price (or the claims budget becomes a consensus limit) at that version |
| **Native reach** | `program_names`, `program_hunts` and the rest come as ABI functions behind their own features (PRG 10.4); AwardHunt and TreasurySpend credits to programs (A9) as a guard rule keyed by version |
| **Schema** | The first version that re-encodes an existing record type bumps `SCHEMA_VERSION` to 10 with `SUPPORTED_SCHEMAS = 9..=10` (PRG 14.4 item 3's mechanism), on its own schedule |

### 9.2 What a supported release freezes

These are protocol v2's rules, the same on every chain that runs v2. **From a supported
programs-on release on** (one that has passed §10.6's gates: the external audit and the fuzz
campaign) they are frozen: they change only through a later protocol version, because every later
binary must replay the chains that ran them, and a rule found wrong is fixed by a new version
whose rule applies from its activation height. What they are:
- the encodings and JSON of tags 62, 63, 64 and 69 (§2), and the refusal to nest them;
- the digest domains and the address derivation (§2.1);
- interface format 1 and its rules (§3);
- the key layouts of namespaces 66, 67, 68, 69 and 72, the format-1 value layouts, META
  `programs` format 1 and the parameter keys, defaults and bounds (§4);
- `SCHEDULE_V0`, the load and upload prices, `MAX_TX_FUEL`, `CALIBRATED_BLOCK_FUEL`,
  `MAX_RESERVATION` (W1's `meter.rs`), `MAX_FUEL_RATE` and every constant in §5.3, and the fuel
  controller's formula (§5.6);
- W1's limits (`config.rs`: pages, tables, functions, locals, globals, call depth, key and value
  sizes, the per-transaction write, key, event and return caps, which span every call and frame
  of a transaction, distinct keys counted once, `MAX_METHODS`, `MAX_TX_CODE_BYTES`) and storage
  accounting (`storage.rs`: `ENTRY_OVERHEAD` 96, `CAPACITY_UNIT` 64, the price curve, priced at
  bytes once the write lands);
- W1's validation rules (`validate.rs`, wasmparser `=0.228.0`) and their `Rule` numbers, ABI v1's
  function list and semantics (`abi.rs`), the account-opening rule (A30), the engine pin
  (soroban-wasmi `=29.0.0`) and its configuration;
- **`host::sanitize`** (`host.rs:162-181`: its table of refused code points, `char::is_control`,
  `from_utf8_lossy` and the 256-byte cut): it isn't display only, because a caller reads the
  sanitized abort message of a callee after `try_call` (`host.rs:385-391`, `last_return_v1`), so
  its control flow can depend on it. A vector test pins it (§10.1);
- the semantics of §6 that decide outcomes (load counted per transaction, the declared set, the
  commit order, a fault never being an outcome) and of §7 (the cap and exit-only on their shared
  measure, `value_held + sponsored`, with program-paid meaning "paid by the entry's own program";
  the pause's threshold, vote life, window and its waiver, renewal and lapse);
- the receipt outcome numbers and W1's `FailCode`, `TrapKind` and `CommitError` numbers
  (display-only in v0, but a later receipts root will commit them).

Not frozen, because no outcome depends on them: how the node runs a transaction (an engine and a
thread per transaction or per block pass, A3), the quarantine, the trial marker, strikes and the
verified-clean retry (§6.2), the replay cache (§6.6), proposal order (§5.6), the claims budget's
place in the block log (§8.1) and the API. (Receipts aren't consensus either, but P2's gate
compares them with recorded history, so their JSON for blocks already run changes only from an
activation height, §8.1.)

A bug in a frozen rule, found after a supported release, is fixed by a new version whose rule
applies from its activation height; history keeps the old rule (PRG 7.6's exception, an exploited
memory-safety bug, needs a checkpoint and is a release decision).

**Until a supported release, nothing is frozen and nothing is promised.** v0's rules may be
corrected in place, in the code and in this file, and a chain made with `--programs` (or one that
activated programs by governance) before then comes with no compatibility promise: after an update
its history may not replay on the corrected binary, and it may have to be recreated. `--programs`
says so when it is used (`blockula dev --help`, `blockula summon --help` and `summon`'s output).

### 9.3 How the programs rules came to ship (history)

This section records why the code looks as it does at two points, so that a reader of it isn't
surprised. It is history, not a plan: the launch kit (§0) has no launch date, no go/no-go and no
`blockula-1`.

**What the first plan was.** The spec was first written for a chain, `blockula-1`, that would have
been born at v2 on a fixed date with its program rules still to be audited. No audit could clear
v2's program rules by the date its genesis was to be frozen, since the integration was still being
built, so the plan was that `blockula-1` starts with programs **off** and gets them under
protocol v3: a release that supports v3 carries the audit's fixes keyed on `version_at(h) ≥ 3`, a
`ScheduleUpgrade { 3 }` passes, and the council's `ActivateFeature { programs }` runs at or after
v3's height. Nodes that hadn't upgraded would halt at v3 (`upgrade_required`) instead of running
programs under rules they don't have. Two rules, stage L, make that unbreakable, and they are the
part that is code: `Genesis::check` refuses `programs` in `blockula-1`'s genesis (§1.5) and
`check_may_activate` refuses `ActivateFeature { programs }` there under v2 (§1.7). Without the
second, a council vote under v2 would switch on rules no audit had cleared (if the integration is
in the binary) or set a flag no code reads (if it isn't), which every later binary would find on
with META absent, a state §4.4 forbids. Without the first, a genesis listing `programs` would be
read with no META by a binary without the integration and with META by every binary that has it
(`Genesis::initial_state` calls `on_activate`): two genesis states. With both, every binary
computes the same `blockula-1`, and nothing in history changes: below v3 every program
transaction is refused at admission (§1.2).

**What became of it.** The launch kit has no such chain, so both rules are inert: they name an id
that no kit chain has. They stay, because lifting the genesis refusal would change no state (a
`blockula-1` genesis has `features = []`) and because raising or dropping the activation rule is
exactly the kind of change that splits a chain between old and new binaries (§1.7). The rest of
the first plan was process, not rules, and is dropped. Among it was the idea that the first binary
might carry programs dormant, with the node-local safeguards (the quarantine, trial marker,
strikes, the verified-clean retry, the replay cache and the totals check) and the tests of v2's
programs-on rules following in a later release. All of them are in this binary (§6.2, §6.6,
§7.1, §10.1); each is still required before a supported programs-on release (§10.6).

**What a chain with its own id gets.** Programs from block 1 if it is made with `--programs`;
otherwise a v1 chain that can schedule v2 and activate them by governance (§1.3). The activation
path on a chain with another id is the simulator's scenario 4 (§10.2).

## 10. Testing obligations

Each item names where it lives. "Rust writes, TS checks" vectors are regenerated by a Rust test
and committed; CI fails if they drift.

### 10.1 Unit and vector tests

- **Codec** (`types.rs` tests): `program_vector_actions()` round-trips every v0 tag through the
  wire form and JSON (options set and unset, bounds at and past their limits, non-ASCII refused
  in identifiers); `check_format` refusals one by one; nested 62–64 and 69 refused; 65–68 and 70
  unknown; `every_tag_round_trips_through_the_wire_form` at `(0..=54).chain(60..=64).chain([69])`;
  `json_number_fields_are_bounded` covers `fuel_limit` and `upgrade_delay_blocks`;
  `Tx::max_bytes` for an upload.
- **Vectors, Rust → TS:** tx signing bytes (`sdk-ts/test/tx-vectors.json`, `tx-vectors.ts`, with
  `describeAction` rendering each, `tx-vectors.ts:20`), the digests and addresses (a sibling of
  `state/proof_vectors.rs`), interface encodings (for the later TS port, written now), and
  `ProgramRecord` format 1 for `provenProgram`.
- **`stored_actions_move_with_the_schema`** (`state/tests.rs:474-497`) keeps its digest under
  schema 9: a v0 program action can't be stored in a multisig proposal (A8), and a new tag changes
  no existing record (its own comment, `state/tests.rs:471-472`). `program_vector_actions()` is a
  list of its own that this test doesn't iterate; when a later version lets them nest, it adds
  them and records the new digest.
- **State:** each record's format byte (decoding refuses 0 and 2); `ByteKey` order; META
  `programs` loads back from its records (`load_meta`'s hand-written list); a state with program
  records survives `for_each_record` → `from_records` and a snapshot round trip with the same
  root; `check_invariants` catches each broken invariant of §4.4 (one test per line); a
  rolled-back first trial at a new height leaves the counted fuel at 0 (§4.3).
- **Storage pricing** (§4.6): an entry, an upload's blob and a create's record each written across
  a price step pay the higher price (priced once the write lands); an upload of code and
  interface prices the interface on top of the code.
- **Fees and the fuel rate:** `fuel_fee` for each tag; the tip cap includes fuel; the stateless
  lower bound never refuses what some state accepts (property test over parameter bounds);
  `MAX_RESERVATION`'s `const` assertions; `next_rate` equals `econ::next_slot`'s rate on the same
  inputs (property test); 10,000 consecutive full blocks overflow neither the rate nor a charge;
  the rate is read as `max(fuel_rate, floor)` after a floor change; admission, the proposer's
  trial, verification and execution read the same rate; `suggested_rate` covers 17 full blocks.
- **Proposal order:** `TipRate` orders every v1 vector transaction (and a property-test sample of
  fuel-free ones) exactly as tip per byte did; a heavy call no longer outranks a light one that
  tips the same per share.
- **Admission matrix:** every v0 tag × {owner key, session key, consensus key acting for its
  owner, consensus key on the validator's own account, multisig owner key, a program as `from`} ×
  `status` {Dormant below v2 (META and the feature present, only the version short), Dormant at
  v2, On, ExitOnly, Paused}: exactly the cells §2 and §7 allow; `submit_inner`'s early refusal
  matches it tag by tag (§8.2; S6b). Cells that depend on more than the status are named too: tag
  64 with several calls where only a later one carries FANG (exit-only refuses it); a pause
  running in exit-only (Paused wins: a value-free call is refused); tag 69 while Paused, whose
  verdict depends on `pause_signed` (§7.2's window), so that cell is built through `pause::sign`.
- **The version gate** (P7's pattern, `a_version_gated_rule_flips_at_its_height_on_every_path`,
  `chain.rs:5708-5766`, W2 `:5717-5780`): on a chain scheduling v2 at height
  `H` with `programs` activated after, every program tag is unadmittable on every path (mempool
  admission and revalidation, propose, verify, execute, replay) before activation and admitted
  after, and nowhere else.
- **`blockula-1`'s activation rule** (§1.7), landed with W2's stage L (`06c35fd`) and its tests:
  `ActivateFeature { programs }` on `blockula-1` refused at proposal and when it runs under v2,
  a first activation and a reactivation after exit-only alike; accepted at proposal once its
  `runs_at` falls under a scheduled v3, and failing when it runs if v3 was withdrawn; on under v3;
  every other chain id (testnet, dry run, dev) activates under v2; `Genesis::check` refuses
  `programs` in `blockula-1`'s genesis and accepts it on a dry run born at v2 (W2's
  `mainnet_switches_programs_on_by_governance_only_from_v3`,
  `on_mainnet_programs_may_be_proposed_to_run_after_a_scheduled_v3`,
  `ghost_mode_and_v2_programs_stay_off_on_mainnet_whatever_the_council_votes`,
  `mainnet_is_born_at_v2_with_programs_off` and the rest named in §1.7). The integration adds one:
  on a `blockula-1` state that reached v3 (the state machine, as W2's tests run it), the
  activation creates META exactly as on a dev chain.
- **Mempool:** `MempoolCheck::run`, `dry_run` and `simulate_queue` never create an engine (a
  test-only counter in `programs::exec`, as `upgrade::test_gate` counts); the pessimistic debit
  holds a sender's queue to its worst case; the upload cap; a create queued behind its sender's
  pending upload is refused with the block it's usable from (§2.1); the quarantine's refusals
  and their limits (§6.2): the quarantined id; a held sender's program transactions, but not its
  transfer; an upload of held bytes, but not a call to a program whose code a faulting call
  declared; and each entry's expiry (an id once the chain passes its `valid_until`, a sender after
  `QUARANTINE_HOLD_BLOCKS`, bytes after a day).
- **Proposer:** a failing call is included and charged; a failing native tx is still left out;
  a block whose program fuel would pass the budget leaves the rest out; a transaction whose trial
  faults (a `Tx` fault injected through the test-build fault hooks, which cover calls in
  `ProgramView` and uploads in `exec::check_upload`) is left out, quarantined (its
  id, and its sender for program transactions; an upload's bytes), dropped from the mempool with
  its sender's later transactions, and the rest of the block is packed; a `Setup` fault (an
  injected spawn refusal, three times) leaves out every program transaction of that proposal and
  quarantines none; a spawn refused once then allowed is no fault; the trial marker names the
  transaction under trial and is empty after packing, and a non-empty marker at startup fills the
  quarantine under the same rule; strikes (a proposal whose view was nullified, read from a later
  finalized block's `parent_view` and `view`, strikes its program transactions; two strikes drop
  one and hold its sender; one strike doesn't).
- **Faults elsewhere:** `verify` abstains when the ancestors' replay faulted, when a transaction
  is `Err` with a fault set, after a fault in its loop, and on a block carrying a quarantined
  transaction id (and verifies one carrying a held sender's transfer); `apply_block` records
  `Halt::ProgramFault` (exit 13) for a `Tx` fault and `Halt::ProgramResources` (exit 14) for a
  `Setup` one, and for a `Tx` fault on a block this node's verify passed without a fault (a fault
  injected at execution only); `commit_block` refuses while `fault` is set; a node restarted after
  a `Tx` fault faults again on replay and `run_validator` exits 13, the verified-clean case
  included (the set is gone after the restart).
- **The replay cache** (§6.6): a cached and an uncached replay of the same ancestors give equal
  roots; an entry for one fork is never used on another; entries at or below the executed height
  go; a faulted replay isn't cached; the cache is never consulted on a chain without META.
- **Block passes** (A3, `programs::pass`): whole blocks, proposals and verifications run as a pass
  and as a thread and engine per transaction give the same outcomes, fuel, receipts, fault
  records, strikes, marker and root, with faults injected at the first, middle and last program
  transaction; a transaction reaches its declared set and no other code the engine holds; a fresh
  engine after a fault inside the VM and after one outside it, and after an engine that wouldn't
  build; the memory bound, including an excess of one byte; a pass mixing creates with `init` and
  calls builds one engine; the mempool starts no thread and no engine (`pass::counts`); a v1 block
  starts neither; the fallback when the OS refuses the pass thread; the marker left as it was by
  a panic on the pass thread; and S8f's twin check of the unrolled v2 block against
  `State::run_block`.
- **Training wheels:** the cap on the control (a call, a transfer and an adoption each refused
  past it; a payout never refused; a program at the cap that bonds its balance into sponsored
  entries can't then take in more; a sponsored entry's removal at the cap applies; a create whose
  `init` fails leaves a balance sent ahead to its address untouched, and the creator's next create
  gets another address, §2.3); exit-only (each rule of §7.3, including a withdrawal that writes a new key the
  signer bonds, which applies, and a program moving its balance into a sponsored deposit, which
  nets to zero); the pause (threshold at n = 1, 4, 7; two signatures in adjacent epochs within a
  day pause; a vote older than a day doesn't count; a lone signature spends no window; a
  validator that started a pause is refused for 30 days unless a fix is pending, under each of
  `fix_pending`'s three conditions; a renewal extends `until` and never shortens it; the cap: a
  renewal is clipped at `started + PAUSE_MAX_SPAN` and a signature once `until` is there is refused
  (uncharged), even with a fix pending; a new run is refused below `started + 2 × PAUSE_MAX_SPAN`
  of the last run and admitted at exactly that height, with a new `started`; the lapse at
  exactly `until`; a pause in exit-only; the consensus-key form is a lane duty, the owner-key
  form may tip); the credit guard at all four sites; `ParamChange` routing and classes (council
  passes all but the minimum delay; before META exists the key is unknown; `exec_set_param`
  reads the version at `econ.height`); `check_totals` flags a corrupted `value_held`,
  `sponsored` and `escrow`.
- **The node's own determinism and resource rules:** the deepest path the guests and the fuzz
  corpus reach (an 8-deep `call_v1` chain at the recursion limit) uses less than an eighth of
  `PROGRAM_STACK_BYTES` on the program thread (PRG 4.3); `no_hash_collections` (§6.1);
  `release_profile_covers_the_vm` (overflow checks on the VM's and the engine's crates, WBS S1);
  the `const` assertion tying `MAX_STATE_SOFT_BYTES` to `snapshot::MAX_STATE_BYTES` (§4.6).
- **In `node/vm`** (stage V): an opening transfer below `MIN_NEW_ACCOUNT` returns
  `ERR_TRANSFER` under both open flags; `host::sanitize` pinned by vectors (each refused range at
  its bounds, a 257-byte message, invalid UTF-8).
- **Receipts:** a failed call's receipt and the `native` override; the claims budget (a block of
  calls returning 4 KiB each records claims up to 256 KiB, then `omitted`); `Block::programs`
  absent from a v1 block's JSON (byte-identical serialization); `/api/tx`'s program section.

### 10.2 The simulator scenarios (`sim.rs`)

Beside the existing `#[test]`s (`sim.rs:245-1277`), in `node/src/sim/programs.rs`, on 4
validators and a dev chain id (never `blockula-1`'s rule):
1. **Programs on at genesis** (a `Genesis` with `upgrades: [{2, 1}]`, `features: ["programs"]`,
   written over `summon`'s `genesis.json` as `sim.rs:771-778` does). Through `Wallet`-like
   helpers it uploads the four guests from W1's `node/vm/tests/programs/` (counter, token,
   escrow, router) with their interfaces, a block apart from their creates and their first calls
   (§2.1); creates each (the token with `init`); calls them, including a router call that
   composes token and escrow through bindings; sends a failing call (charged, included by
   whoever leads); and a call past `program_max_block_fuel`'s remaining budget (left for the
   next block). Then:
   - every validator reaches the same root at every height, and the same receipts;
   - one validator restarts from its database and another from its block logs alone, replaying
     every block (the replay compares each receipt with the one recorded); a full node, and then
     a validator under its own key, start from a certified snapshot taken after the uploads
     (installed as `blockula join` does), so the snapshot carries the programs' code, interfaces,
     records and storage. All of them catch up to the same roots and receipts, and the validator
     started from the snapshot proposes and signs again;
   - full-fuel blocks raise `fuel_rate` on every validator alike, and empty ones lower it;
   - `DeactivateFeature { programs }` passes by council vote: exit-only holds (a payout works; a
     call with value, an upload and a create are refused), and `ActivateFeature` turns them back
     on. This comes before the pause because a pause outranks both exit-only and on
     (`Status::Paused` is checked first) and runs three days of blocks (`Spans::pause_blocks`:
     days at the chain's block time), which a simulator can't wait out and can't lift;
   - last, two validators sign `ProgramsPause`: every validator refuses program transactions from
     the next block; with a `ScheduleUpgrade` proposal voting (it needs no votes: `fix_pending`
     counts a proposal in its voting period), two sign again before it lapses and it's renewed
     (its lapse, 3 days of blocks later, is a unit test that sets heights directly, §10.1). The
     pause applies from the block after the second signature's, and a program transaction queued
     before it is left out of every block from then on.

   Two more runs sign the pause apart from scenario 1: once into blocks full of other
   transactions, where the signature lands through its duty lane (§2.5) within two blocks, with
   `max_block_bytes` at its floor, since blocks full of fuel don't fill the lane (the packer skips
   a program transaction past the fuel budget and goes on); and once in the owner form, by a
   validator whose owner account differs from its consensus key, which pays a tip.
2. **A transaction that faults** (a `Tx` fault injected for one transaction on every validator):
   it's never included, blocks keep coming (this much needs only the fault paths, WBS S4b; the
   rest needs the quarantine and strikes, S4c), every validator quarantines it once, its sender's
   next program transaction is refused while a transfer from it is included, a call to the same
   program from another account is included, and a `ProgramsPause` signed meanwhile lands. A
   variant injects the fault on two validators of four (past f): the other two leaders' views fail
   to certify until exactly two strikes each drop the transaction, after which every view
   certifies. A simulator loses view 2 of about a quarter of its epochs whatever the traffic
   (Commonware simplex's fast timeout, when a new epoch's engine hasn't yet seen a leader's vote),
   so the counts leave each epoch's view 2 out.
3. **A lagging verifier**: one validator's execution delayed under full-fuel blocks; its
   verification time stays bounded (replay-cache hits counted, and none on any node before the
   delay), it keeps voting, and it catches up. Run with blocks faster than a finalization round trip (20 ms blocks against about 80 ms), so
   that proposers' own tips trail their parents and ancestors really are replayed: at 1 s blocks a
   prompt proposer cites its parent's state, a verifier waits for the state a block cites (§6.6),
   and no replay runs at all.
4. **Programs off at genesis**, switched on by council vote at a height, with roots checked
   before and after.

### 10.3 Invariants

`State::debug_check` (`state/mod.rs:606-617`, every block in tests) runs
`programs::check_invariants`; the state-machine fuzzer checks it after every block
(`node/fuzz/src/harness.rs:605-607`). Production runs `check_totals` at epoch boundaries (§7.1).

### 10.4 Fuzzing

A deterministic fault can no longer stall or halt the chain (§6.2), so fuzzing's job is the
defects that don't fault: a wrong outcome, a hang, memory corruption, and a crash that
`catch_unwind` doesn't see.
- **The VM** (`node/vm/fuzz`): W1's `program_validate` (validation, then `upload::check`'s test
  translation of whatever validation accepts: the widest surface, attacker bytes into wasmparser
  and soroban-wasmi's translator; a refusal after acceptance is a finding too) and `program_exec`
  (batches against the guests, either engine), plus, from stage V, `interface_codec` (seeded with
  the guests' interfaces) and a `wasm-smith` generator restricted to v0's subset feeding
  `program_validate`'s path, since mutated bytes rarely get past validation. Run under
  AddressSanitizer as well as plain, with libFuzzer's `-timeout=10` (a hang is a finding) and
  `-rss_limit_mb=2048`, on x86-64 and aarch64. CI checks these targets' seeds on every PR (the
  `Fuzz harness` job) and runs each for five minutes nightly (`fuzz.yml`'s `fuzz-vm` job, a smoke
  run that is no part of the campaign); the campaign below runs them on dedicated machines
  (`node/vm/fuzz/README.md`).
- **A v2 fixture** for the state machine (`node/fuzz/src/harness/v2.rs`, the target
  `state_machine_programs`; v1's `state_machine` and its corpus are unchanged): genesis with
  `upgrades: [{2, 1}]` and `programs` on, the four guests uploaded and created, in four profiles
  (default parameters; a tight cap and the fuel floor; exit-only; a long block time with four
  validators, for the pause), and `supported_version(&genesis)` instead of
  `SUPPORTED_PROTOCOL_VERSION` in the halt check. Its dry run uses `ApplyMode::Mempool`.
- **Program ops** (ops 40–46 of that target): call a guest method with fuzzer-chosen input, value,
  fuel and storage limit; a batch of calls; a create with `init`; an upload of fuzzer-mutated
  bytes; a pause signature; a transfer to a program; a governance step (exit-only on or off, a
  parameter, blocks passing). Each block keeps the harness's checks: a rolled-back dry run
  changes nothing, the incremental root equals a full rebuild, the invariants hold, a quarter of
  inputs run twice for determinism (the second time through `State::run_block`), the snapshot
  round trip. And it adds oracles that don't call the code they check (`harness/oracle.rs`): each
  program transaction's charge, fuel and receipt against §5.1's and §5.6's formulas over raw
  state, every balance against the receipts' deltas, the guests' storage against models.
- **Node targets** (`TARGETS` in `node/fuzz/src/harness.rs`, and the nightly matrix of
  `fuzz.yml`): `program_tx_codec` (tags 62–64, 69 round trip: it runs on every gossiped
  transaction even while programs are off), `program_receipt` (JSON round trip, the claims budget,
  `/api/tx`'s section), `program_commit` (arbitrary well-formed `Diff`s through
  `programs::commit` on a small state: it returns `Err` or keeps every invariant, and never
  panics) and `state_machine_programs`.
- **Seeds:** the guests, their interfaces, one vector per tag.

### 10.5 The v1 replay obligation

The release that carries v2 replays testnet-2's archived history (P2's gate,
[REPLAY.md](REPLAY.md)) and reaches every recorded state root, tx fate (applied, failed, skipped),
skipped size and base fee, and then each block's whole record as a JSON value, failure reasons
aside: what P2's `replay::compare` checks (from `9bb65dc`), so a stray `programs` key on a v1
block fails the gate. A v1 block's JSON line is also byte-identical to v0.5.2's, which a value
comparison doesn't establish (key order, number form), so a unit test checks the bytes
(§10.1). What isn't consensus but changes code every v1 node runs (proposal order, the replay
cache's off switch, the quarantine) has its own v1 tests (§10.1). P2's CI job (and `replay.yml`'s nightly replay of the public testnet) is a required
check of this work: nothing here is mergeable while it fails.

### 10.6 Release gates

A **supported programs-on release** is one that tells people to run programs on a chain that
matters to them, and it has passed these gates. Until one exists, `--programs` and governance
activation are there as an experiment, with the controls of §7 and the operating duties of
OPERATIONS.md: v0's rules may still be corrected in place (§9.2), and a chain made with them
carries no compatibility promise. The gates:
- `cargo test` (node, `node/vm` with and without `ref038`), `npm test -w @blockula/mcp`,
  `npm run test:vectors -w @blockula/sdk`, the explorer e2e.
- Every calibration item of §5.2 within budget on both floor runners (or its fallback applied),
  each recorded beside W1's `CALIBRATION-*.md`. (Met: A3 took item 3's first fallback and the
  floor runners' records are in `node/vm/CALIBRATION-*.md`.)
- **The fuzz campaign.** Every target of §10.4, under ASan and plain, on x86-64 and aarch64, for
  at least **14 CPU-days per family in total** (VM validation and translation; VM execution; the
  node's targets and v2 fixture), summed over those four configurations with none under 2
  CPU-days, on one shared corpus per target, and the last 24 hours of wall-clock without a finding
  in any configuration. The state machine targets run with `-len_control=0` and a `-max_len`, so
  inputs grow past the shortest seed's length (S8f's review: every input of a first run stayed
  under 229 bytes); coverage and corpus size recorded in `docs/AUDIT_SCOPE.md`. **A supported
  release can't waive it for a chain that lists programs in its genesis** (`summon --programs`):
  validation and the test translation take attacker bytes for the price of a fee, and what
  fuzzing hasn't found becomes a validator crash or a wrong outcome from block 1. A chain made
  with `--programs` before then runs without the campaign, and says so. For a release that offers
  programs only by governance activation, the external audit's report sets the campaign's length,
  never below the 24-hour floor; the nightly runs continue either way. A release that only
  carries programs dormant needs the codec targets
  (`program_tx_codec`, `interface_codec`) 24 hours clean, since they're reachable there, and not
  the rest. (PRG asked for two weeks of wall-clock; CPU-days let the machines there are do it in
  days.) **Status: the campaign has not been run.** `docs/AUDIT_SCOPE.md` records what has: the
  local runs, the codec targets' 24-hour runs, and the plan with its CPU-day reading.
- The simulator scenarios of §10.2 and the replay gate of §10.5 green.
- Every node-local safeguard in the binary that runs them: the quarantine, trial marker, strikes
  and verified-clean retry (§6.2), the replay cache (§6.6) and `check_totals` (§7.1); and every
  test of §10.1.
- The external audit's scope: `node/vm` (the `HostPtr` `unsafe`, validation, `upload::check`,
  the overlay and commit checks, metering, `host::sanitize`), `node/src/programs/`, every hook
  the stages made in the node's existing files, the codec of tags 62–64 and 69, the version
  gating and `ApplyMode`, the fault paths (the proposer's exclusion, the quarantine and strikes,
  verify's abstentions, the halts, exit codes and the verified-clean retry), the fuel
  controller, the pause rules, the value cap and exit-only's shared measure (`value_held +
  sponsored`), §1.7's activation rule, the replay cache, and soroban-wasmi's enabled paths at the
  pin. `docs/AUDIT_SCOPE.md` lists it for an auditor.

---

## Appendix: Review log (revision 2)

Two adversarial reviews of the first draft, a safety review (S-F1 to S-F17) and a build review
(B-F1 to B-F13), plus the engine reviews' findings where this file is involved (VM-1 to VM-6 and
VM-R1 to VM-R4 from two reviews of the VM crate, VM-V1 and VM-V2 from its verification), and the
two verifiers of revision 2 (F1 to F5, safety; BV-1 to BV-6, build: the last section). Each
finding was checked against the code (`c940ead`, the base the line numbers in the first three
sections were taken from, unless named: W1 at `4182d21`, W2 at `e60f26d` (its `2da5f90` differs
only in a test helper), P2 at `c446442`; the verification's findings against W2 `2da5f90` (then
`06c35fd`), the operations branch `8f28cc0` (then `7d9c777`) and P2 `b437f0c` (then `b965526`))
and PRG before deciding. "Fixed" says where; "declined" says why. The planning documents these
findings were answered in (the build plan, with its dates and its launch runbook) are not part
of this tree; where a row below mentions a date or a launch, it is the plan that §9.3 records as
history.

### Safety review

| # | Verdict | Checked | Resolution |
|---|---|---|---|
| S-F1 (high) | Fixed | Packing runs under the chain lock over `proposal_txs()` (`consensus.rs:635-681`); expiry and the TTL are judged only after a block executes (`chain.rs:2241`, `:1824`, `MempoolCheck::run` from `execute_shared`, `:3042-3045`), so a tx that faults every leader never left | A fault on the propose path leaves the tx out, quarantines it and drops it from the mempool; packing goes on; verify abstains; only execution of a certified block halts (A11, A28, §6.2, §6.3); tests §10.1, sim scenario 2. The quarantine was narrowed by verification F1 (the id until `valid_until`, the sender's program transactions for 600 blocks, an upload's bytes for a day; never a call's declared code) |
| S-F2 (high) | Fixed | `econ::end_block` moves the byte rate on `tx_bytes` only (`econ.rs:344-356`); `TipRate` is tip per byte (`chain.rs:4078-4106`); `max_tip` scales with a base that includes fuel (`econ.rs:255-257`) | A per-block fuel rate in META moved by `next_slot`'s formula on counted fuel, floored by the council's parameter (A6, §5.6), so filling fuel costs exponentially more; proposal order by tip per share of the scarcer resource. PRG's 17-slot ring isn't needed: nothing checks the fuel fee statelessly |
| S-F3 (high) | Fixed, one part declined | `upload::check` runs attacker bytes through wasmparser and soroban-wasmi's translator; the first draft halted every node on a panic there | A panic in a proposer's trial is a `Tx` fault: the upload is left out and quarantined, and its `wasm_hash` and its sender's program transactions held (for a day and for 600 blocks, verification F1); verifiers abstain on a block carrying it; it can't halt the chain (§6.2). The fault log names tx and height. Mempool pre-validation **declined**: it would make every node translate every gossiped upload unpaid, and the maintainer's decisions keep the mempool VM-free; the trial costs one translation per validator per bad upload. Abort-class crashes get the trial marker; the fuzz campaign (ASan, hangs, x86-64 and aarch64) is a gate not waivable for programs on at genesis (§10.4, §10.6) |
| S-F4 (medium) | Fixed | `storage_write` is 5,000 + 10/B (W1 `meter.rs`); phase 0 excluded the node's commit cost | Write-storm and 16 KiB-value patterns timed through `run_block`, `commit_block` and `persist`, run as soon as S4a executes a block (WBS S4c), with a pre-freeze fallback (§5.2 item 3) |
| S-F5 (medium) | Fixed | `replay` re-runs unexecuted ancestors at every vote (`consensus.rs:836-847`, `:917-933`; `MAX_REPLAY` 64 at `:98`) | The replay cache lands (A29, §6.6, used only once programs exist); §5.3 states at most four runs per block per validator; lagging-verifier sim (§10.2 scenario 3) |
| S-F6 (medium) | Fixed | `exit_halted` with `RestartPreventExitStatus` keeps a node down (`node.rs:121-131`, `docs/OPERATIONS.md:251`) | `FaultClass`: a refused thread is retried twice, then `Setup`; `Setup` exits 14 (restarts), `Tx` exits 13 (stays down) (§6.2); OPERATIONS documents thread and overcommit limits; an engine per block pass is the A3 fallback |
| S-F7 (medium) | Fixed | W1's `Overlay::write` bonds whenever capacity grows, payer the signer or `SPONSOR` | Exit-only measures `value_held` plus program-paid deposits; signer-bonded writes allowed (A26, §7.3), computed at commit from the diff, no W1 change. Verification F2 made the same measure the value cap's and stores its second term (`sponsored`, A40) |
| S-F8 (medium) | Fixed, not as suggested | W2's `exec_activate_feature` checks only `known_features` and Ghost Mode on `blockula-1`; the council passes `ActivateFeature` in year 1 (`governance.rs:756-758`) | A constant a fix release raises would split the chain between old and new binaries at the first activation. Instead a rule keyed on chain id and version: on `blockula-1`, `ActivateFeature { programs }` only under v3+ (A27, §1.7), required in the launch binary either way (§9.3) |
| S-F9 (low) | Fixed | `rollback(fj)` on the consensus-key path (`chain.rs:1385-1401`) restores only what's journaled | `(height, fuel)` journaled together; `count_fuel` writes nothing for 0 (§4.3, §5.4); test §10.1 |
| S-F10 (low) | Fixed | Ghost Mode's early refusal pattern (`api.rs:876-890`) | `submit_inner` refuses per tag exactly what admission would at the next height (§8.2) |
| S-F11 (low) | Fixed | `duty_of` wants the consensus key (`staking.rs:2000-2001`); the tip ban applies only to a consensus key acting for an owner (`chain.rs:1259-1263`); the validator's own account holds its consensus key as the original key (`chain.rs:1078-1085`) | No tip rule of `ProgramsPause`'s own; the consensus-key form is a lane duty (`duty_of` gains `programs_pause`), the owner-key form may tip (A14, §2.5) |
| S-F12 (low) | Fixed | `check_invariants` runs only in tests and the fuzzer (`state/mod.rs:606-617`) | `check_totals` at every epoch boundary in production, error log and `blockula_program_accounting_drift` (§7.1) |
| S-F13 (low) | Fixed | `host::sanitize` (`host.rs:162-181`) feeds `last_return` after `try_call` (`host.rs:385-391`) | Frozen with v2's rules and pinned by vectors (§9.2, §10.1); §8.1 says so |
| S-F14 (low) | Fixed | GhostPause resets votes per epoch (`ghost.rs:544`); mainnet epochs are 3,600 blocks | Votes live a day across epochs; only a pause started or renewed spends the window (A25, §7.2), with build F2 |
| S-F15 (low) | Fixed | W1 `overlay.rs` `transfer` opens with `amount ≥ fee` only; native rules: `chain.rs:1412-1414`, `econ.rs:548-551` | `MIN_NEW_ACCOUNT` for both open flags (stage V) and keyless system accounts read as existing (A30, §6.1) |
| S-F16 (info) | Fixed | All four points hold | Heads refreshed (header, WBS §0.1, §3.2); `exec_set_param` reads the version at `econ.height` (§4.5, `chain.rs:1000-1002`); §5.4 says the proposer pays a skipped tx's byte fee (`staking.rs:1287-1294`); the view is over the post-fee state (§6.1 step 2) |
| S-F17 (info) | Fixed | WBS stages | Each fix has a stage (WBS §2); crate calibration runs now, the node's as soon as S4a executes a block; the fuzz campaign is a WBS §4 gate |

### Build review

| # | Verdict | Checked | Resolution |
|---|---|---|---|
| B-F1 (critical) | Fixed | As S-F1 | As S-F1. `propose` returns `None` only when the parent's ancestors fault in its replay (it can't judge the parent) |
| B-F2 (high) | Fixed: option (b), extended | ScheduleUpgrade's earliest activation (`governance.rs:547-557`) is the timelock's end plus a `CancelUpgrade`'s vote plus 66 blocks; mainnet floors 3-day votes, 1-day timelock (genesis spec example); the council passes at the close (`governance.rs:771-785`) | The window doesn't apply while a fix is pending; renewal while paused; votes live a day; unmet votes free (A25, §7.2); §7.4's table shows a pause in force every day of the fix path at n = 4. Option (a), an 8-day pause, declined: a fixed length can't follow a fix that takes longer. Option (c), no window, declined: the bound still matters when nothing is being fixed |
| B-F3 (medium) | Fixed | P2 at `c446442`: `consensus.rs` +89 after `:567`, verify rewritten (`:727-830`), `leader` (`:1222-1230`); `node.rs` `apply_batch_trusted` (`:1456-1477`) | WBS §3.2 recomputed. Verify's stateless Σ `exec` check dropped: the sequential pass already bounds verify's VM work (§5.4), so nothing enters P2's `check_txs`/`check_slot`. S4b's verify hooks are named by their text and written after P2 merges (they also merge as text: WBS §0.3's probe). No hook in `node.rs`'s sync region: synced blocks execute through `apply_block`, which halts like any execution. (P2 moved on to `b437f0c`: +91 after `:567`, `node::execute_batch` split out of `apply_batch_trusted`; WBS §3.2 and §0.3's probe are redone against it) |
| B-F4 (medium) | Fixed | As S-F5 | As S-F5 |
| B-F5 (medium) | Fixed | W1 builds an engine and linker per engine (`engine/soroban.rs:94-95`); `MAX_BLOCK_TXS` 512 (`consensus.rs:56`) | A3: load counted per transaction is the consensus rule; engine per transaction is an implementation choice, out of §9.2's freeze; a numeric gate on the minimal-call block (§5.2 item 3) with two fallbacks |
| B-F6 (medium) | Fixed | As S-F14 | As S-F14 |
| B-F7 (medium) | Fixed | Stage sizes against what each stage holds now, and the launch plan's dates (a code freeze, a spec freeze and a launch release in the last week of October, since dropped: §9.3) | WBS §1 re-estimated by stage with what runs in parallel, an audit hand-off milestone, a compressed path for a dormant launch binary with an Oct 28 go/no-go, and what's cut from it (§9.3, WBS §1.2–1.3) |
| B-F8 (low) | Fixed | `dry_run` simulates queued txs in Mempool mode (`chain.rs:3620-3630`), which writes no records | One step per block, said in §2.1; the mempool names the usable block; SDK and CLI wait (§8.3, §8.4); the sim spaces its steps |
| B-F9 (low) | Fixed | As S-F15 | As S-F15 |
| B-F10 (low) | Fixed | `tx-vectors.ts:20` asserts a description for every vector | `describe.ts` arms move to S7a (WBS); §8.4 says why |
| B-F11 (low) | Declined (documented) | `prepare_kind` checks a `ParamChange` at proposal (`governance.rs:504-507`) | Accepting `program_*` keys before META exists would change v2 while programs are off, which the architecture rule forbids. §4.5, `/api/params` and the operating documents (OPERATIONS.md, GOVERNANCE.md) say the defaults run for at least one vote and timelock after activation |
| B-F12 (low) | Fixed | A reservation of 10M + 21.2M load exceeds a 20M budget; W1's test shows 72 fuel/B breaks the upload assertion | §4.5 and §5.3 say a reservation can exceed the budget; clients check before signing (§5.5, §8.3, §8.4). The upload margin is a go/no-go with its fallback (192 KiB code, a higher price) before the freeze (§5.2 item 2); not lowered now, because the measurement decides and the change is free until then |
| B-F13 (info) | Fixed; (d) since done in the maintainer's decisions record | Each point holds | (a) A16 records the widths; (b) `pending`, `offered_to` and `status` dropped from `ProgramRecord` (A19, §4.2); (c) `/view` off on validators (A24, §8.2); (d) the decisions record's working rule should get the exit-only wording: outside this track's files, and it now has it (its exit-only rule, with value-free calls, `ProgramsPause` and `program_*` changes admissible) |

### Engine review and the engine track's fix pass

| Item | Resolution |
|---|---|
| VM-4 (storage priced before or after the write) | One convention, W1's: priced at program state's bytes once the write lands, the written bytes included, for entries, upload blobs and program records alike; stated in §2.2 step 5, §2.3 step 2 and §4.6 |
| VM-1, VM-R3b (a translator panic at upload) | W1's `upload::check` returns `Result<Result<ModuleInfo, Refusal>, NodeFault>` and `MemHost::upload` runs it; §2.2 step 3 and §6.2 map each arm (a panic is a `Tx` fault, an unbuildable engine `Setup`) |
| VM-R2 (rule numbers) | Landed in W1 (`#[repr(u8)]`, 1–14, `rules_keep_their_numbers`); §8.1 cites it, and stage V no longer adds it |
| VM-2 (batch shape) | W1's `execute` faults on an empty batch, more than `MAX_CALLS` calls or `MAX_BINDINGS` bindings; §6.2 counts those among `Tx` faults (the codec makes them impossible) |
| VM-R1 (bounds across calls and frames) | Frozen as consensus in §9.2 |
| VM-5 (upload margin) | §5.2 item 2 |
| VM-6 (`fuel_used` after out-of-fuel) | §5.1 and §8.1 say `used` equals the limit after any out-of-fuel |
| VM-V2 (the FuelUse note) | §8.1's `FuelUse` paragraph: `used == limit` after any `OutOfFuel` that fails the transaction, nested or not (W1's `exec.rs` `Receipt::fuel_used`, test `a_callee_out_of_its_share_fails_with_the_whole_limit`); under `try_call_v1` only the share counts |
| VM-V1 (upload go/no-go thin under load) | §5.2 item 2 quotes both Mac readings and their floor projections (58 to 76 fuel per byte against 71): the floor dispatch decides, with the fallback and the stopgap named |
| VM-3, VM-R3 (a, c, d) (engine lifetime and integrator docs) | The crate's docs now describe the per-transaction engine A3 specifies; §6.1 relies on exactly that (`Box<dyn BlockEngine>` isn't `Send`, made and dropped on the transaction's thread, loaded before `execute`, the node's `wasm_hash` as the engine's key). No spec change |
| VM-R4 (phase0.md's wording on wasmi 2.0) | Engine-track documentation only; nothing in this file depends on it |

### Read-through against the landed branches (before the verification)

Found by reading this file against PRG, W1 `4182d21`, W2 `2da5f90`, ops `327904f` (renamed
`8f28cc0` since, the same tree) and P2 `c446442`, and fixed in place:
- amendments PRG differed on without a row: the ABI subset (A31), record layouts (A32), the state
  soft cap (A33), the deposit's pricing point (A34), credits refused while paused (A35), the
  value check's place (A36), three stateless rules (A37), upload slots and `TipRate`'s
  constants (A38), the clients (A39); A6, A10, A15, A17 and A29 widened to every difference;
- an upload of a stored but mismatched pair now fails like any mismatch (§2.2);
- W2's tests that let `blockula-1` activate programs change with stage L (§1.7, §10.1; named in
  full after verification BV-1); how W2's
  owner-outranks-key council rule meets programs (§1.7);
- a launch binary without the integration must also refuse `programs` in `blockula-1`'s genesis,
  or two binaries compute two genesis states (§9.3);
- the pause waiver's reach stated plainly, with a bounded alternative for the maintainer (§7.2);
- W1's nightly fuzz matrix doesn't run the VM targets (§10.4); the node-side hash-collection
  test, the stack-depth test and the snapshot bound PRG asks for (§6.1, §10.1);
- exit 13 and 14 are already handled on the ops branch; `run_full_node` exits like
  `run_validator` on a startup fault (§6.2); the halt is the error `apply_block` returns;
- the replay cache's difference from P4 (A29, §6.6); the minimal-call bench's arithmetic (§5.2).

### Revision 2 verification

Two verifiers read revision 2 against the code: a safety lens (F1 to F5) and a build lens (BV-1
to BV-6). Each was checked against W1 `4182d21`, W2 `2da5f90`, ops `8f28cc0`, P2 `b437f0c` and
the base before deciding; all three branches moved while this pass ran, and the last section says
what that changed.

| # | Verdict | Checked | Resolution |
|---|---|---|---|
| F1 (medium) | Fixed | The quarantine held a faulting transaction's sender for every tag and a call's declared code (the callee's and the bindings', other people's: §5.1's `D(CallProgram)`) until restart, and verify abstained on any block "carrying one". The node's only mempool expiries are height-based (`MEMPOOL_TTL_BLOCKS` = 600, `chain.rs:1898`; `valid_until` at most an hour ahead, `chain.rs:1823-1830`) | The quarantine keys on what the author controls, and every entry expires (§6.2, A28): the id until its `valid_until` passes; the sender's program transactions, never its transfers, for `QUARANTINE_HOLD_BLOCKS` = 600 blocks; an upload's own bytes for a day; a call's or a create's declared code never. Verify abstains on a quarantined id and nothing else. The deterrent stays: another attempt needs another funded account or the hold to lapse. §2.2, §10.1, sim scenario 2 (a transfer from the sender and a call to the same program from another account are included) |
| F2 (medium) | Fixed | §7.1 capped `value_held` (balances only) while exit-only measured `value_held` plus program-paid escrow. W1's `Overlay::write` bonds a deposit from the payer's balance into escrow and `remove` refunds the recorded payer (`overlay.rs`); a `SPONSOR` write's payer is the writing frame's program (`host.rs:249`). The curve admits Σ_{k<16} soft/8 × price × 2^k ≈ 5.5 × 10^16 drops (55M FANG) at the default soft cap | A40: the cap bounds **the FANG programs control**, `value_held + sponsored`, the measure exit-only already used, so a program can't park its balance in sponsored storage and take in more, and a sponsored refund at the cap (neutral) applies. `sponsored` is stored in META and per program record (A32, §4.2), kept by commit with an entry program-paid exactly when its payer is its own program (§6.1 step 6.1), checked by `check_invariants` and `check_totals` (§4.4, §7.1), reported by `/api/fees`' headroom, `/api/params` and a metric (§5.5, §8.2); tests §10.1 |
| F3 (low) | Fixed (node-local) | Leaders pack from `proposal_txs()` with no memory of uncertified proposals (`consensus.rs:635-681`); `CBlock` carries `view` and `parent_view`, the views between nullified (`consensus.rs:120-122`) | **Strikes** (§6.2): a proposer records its proposal's program transaction ids with the view; a nullified view (seen in a later finalized block) strikes them; at two strikes the leader drops the transaction and holds its sender's program transactions for 600 blocks; `blockula_program_uncertified_proposals_total`. "What that leaves" now covers a fault on more than f validators, and OPERATIONS.md's incident response reads the metric (WBS S4c, S9; sim scenario 2's variant) |
| F4 (low) | Fixed (node-local) | Verify applies the block's transactions with the same VM to the same state (`consensus.rs:848-853`); W1's `NodeFault::Engine` and `Instantiate` can be resource-induced; 13 idles the node (`node.rs:123-131`, ops `entrypoint.sh`) | A **verified-clean retry** (§6.2, A11): verify records the digest of a block it passed without a fault (`verified_clean`, the last 64, in memory); a `Tx` fault executing such a block is `Halt::ProgramResources`, exit 14. The set is gone after the restart, so a second fault on the block exits 13. Test §10.1; WBS S4c |
| F5 (low) | Fixed: its second option | The address is `derive_key(creator ‖ nonce)` (§2.1); the fee phase spends the nonce whatever the action does (`chain.rs:1373`); PRG 1040-1044 invites sending FANG ahead | Adoption stays, so a balance sent ahead by mistake or to front-run a create can't block it, but §2.3 says it's no way to fund a program: a failed create strands it, and clients never send FANG to an address before its create is final (the CLI warns, the SDK returns an address only once the create applied, §8.3, §8.4); a test pins the behaviour (§10.1). The salted address is declined for v0: it changes tag 63's encoding against PRG for counterfactual funding, which nothing in v0 needs; a later tag can add it (§9.1) |
| BV-1 (medium) | Fixed | W2 `2da5f90`: `mainnet_tests.rs:224` and `:692` (`:748`) pass and execute `ActivateFeature { programs }` under v2 on `blockula-1`; `:436-440` and `:655` accept a `blockula-1` genesis listing programs; `upgrade.rs:813`; W2's own report names the first three | §1.7 and §10.1 named the three tests L rewrites (five under option B) and what replaces their assertions (refused at `Propose` under v2; accepted once v3 is scheduled under `runs_at`), and WBS stage L was re-estimated at 0.5 agent-days. Then L landed on W2's branch (`06c35fd`) with those tests rewritten, two new ones and the genesis refusal on every option: §1.5, §1.7, §9.3, §10.1 and WBS stage L now describe what landed, and L costs this track a review only |
| BV-2 (low) | Fixed | The operations branch was `8f28cc0` (`327904f` isn't an ancestor; `git diff` between them is empty); its OPERATIONS.md had the unit line at `:253` and the checklist at `:795`; the stay-down entrypoint commit was `83b5d2d` there | The citations named the operations head (`7d9c777` by the end of this pass: it moved again, changing only the launcher), `:253`, `:795` and `83b5d2d`; S-F6's `:251` stays, since it cites the base. (Those lines are now in this tree's OPERATIONS.md and `deploy/entrypoint.sh`.) |
| BV-3 (low) | Fixed | W2's last six `chain.rs` hunks: +10 from `:5710`, rising to +14 from `:5759` to the end | WBS §3.2's row restated; §10.1 cites the P7 test at `:5708-5766` (W2 `:5717-5780`) |
| BV-4 (low) | Fixed | `staking/tests.rs:1720` and `governance/tests.rs:2237` (W2 `:2292`) pass `apply_j`'s bool | S3's edit list and WBS §3.1 own them; §0.3's probe covers both lines |
| BV-5 (low) | Fixed | WBS §1.2's table put `/api/tx`'s program section before `receipt.rs`, verify's hooks before any rebase onto P2, and admission items in S4a's acceptance while S3 runs beside it | S4b writes `receipt.rs` first and `/api/tx`'s section moves to October 27; a rebase onto main once P2 merges (October 26) comes before verify's abstentions; S4a's acceptance keeps execution only (a wrong digest fails in the VM until S3), and the admission item moves to stage T |
| BV-6 (low) | Fixed by a split, not a longer S3 | S3's and S4a's acceptance lists against two days each | The tests of programs-on rules that a dormant launch binary doesn't need (the admission matrix's `On`, `ExitOnly` and `Paused` cells, the controller's property and overflow tests, the same-rate test, S4a's admission item) become stage T (1.5 agent-days, after S4b, beside S4c), a gate before any chain runs programs with value (§9.3, §10.6). S3 and S4a keep their launch halves at two days each, so the compressed path keeps its 7.5 days and gains an October 25 checkpoint; the total is about 22.5 agent-days |

Found on the way, and fixed in place:
- P2 moved to `9bb65dc`, then `b437f0c` (tests and docs only past `9bb65dc`), then was squashed
  into `b965526` (two lines of REPLAY.md past `b437f0c`), with a `Cargo.lock` bump on top
  (`74c2b24`): `replay::compare`
  now checks each block's whole record, so receipts are held to determinism by the gate (§8.1,
  §9.2, §10.5); `node::execute_batch` and `BatchWatch` split out of `apply_batch_trusted`. WBS
  §0.3's regions and §3.2's shifts are recomputed, and the probe re-run: 115 one-line hooks in 21
  files merge cleanly against `b437f0c` (and `c446442`).
- W2's report asks the maintainer to confirm §1.7 (it adds a version step to decision 2's "the
  council turns it on later"): WBS §1.3 lists the call.
- W2's run 7 landed stage L and W2-V2-1, then rewrote the branch (`06c35fd`; run 7 still
  verifying): `check_may_activate(feature, chain_id, version)` read at `runs_at` at `Propose`,
  `MAINNET_PROGRAMS_ACTIVATION_VERSION`, `Genesis::check` refusing `programs` on `blockula-1` on
  every option, its tests; and `known_features` read at the proposal's own block (`1653478`), so
  v1 and v2 binaries agree on every v1 block. §1.2, §1.3, §1.5, §1.7, §4.5 (the routing reads
  the proposal's block too), §9.3, §10.1 and WBS §0.1, L, §1.1–§1.4, §3.1, §3.2 follow it.
- the operations branch moved to `8f28cc0`, then `7d9c777` (the launcher only); its launch
  runbook still said the council turns programs on with `ActivateFeature`. That runbook belonged
  to the launch plan §9.3 records as history, and this file doesn't describe it.
- The maintainer's decisions record has had the exit-only wording since the last revision (§0 and
  B-F13 now say so), so that call is gone.

### Maintainer decisions (2026-10-20)

The maintainer took every recommendation of the build plan's open questions:
- the launch binary: whether it carried programs dormant (option A, on a compressed path with a
  go/no-go checkpoint, option B as the fallback). **Superseded** by the launch kit's scope
  (2026-10-22): there is no launch, so there is no launch binary, and §9.3 records this as
  history;
- §1.7 confirmed: on `blockula-1` programs switch on by governance only from v3, and its genesis
  can't list them (stage L, on W2's branch);
- the pause cap: a run lasts at most `PAUSE_MAX_SPAN` (30 days) from its start, and a new run
  can't start within `2 × PAUSE_MAX_SPAN` of the last run's start (§4.2's `Pause.started`, §7.2,
  §7.4, §10.1, A25);
- the fuzz campaign stays non-waivable for a chain that lists programs in its genesis (§10.6);
- host ABI v1 as W1's subset (A31); `program_state_soft_bytes` 64 MiB within 16–512 MiB (A33).
- Stage L's review (2026-10-20, wave 2a): no code defect. L-1: §1.7's last paragraph said later
  versions could add their own "only from vN" activation rule, which would be vacuous or a split;
  reworded (the code's doc comment was right, and the test now pins every version). L-2: the launch
  docs' two activation paths fixed on W2's branch (`df74f52c`).

### Brought up to the code after wave 2a (2026-10-21)

Each reading below is the code on the programs integration branch at `296a466b`, checked there
first; the stage reports raised them.
- §1.2 items 1 and 3, §4.5 "Routing" (S2-R1): the codec admits programs' seven keys and
  admission refuses them while `Dormant`, in v1's codec words; v1 refused them in the codec, not
  in `ChainParams` as these lines said. Same effect, different place; item 1 now lists it.
- §5.3: `MAX_FUEL_RATE` lives in `programs/mod.rs` (the skeleton's decision 2), not `fuel.rs`.
- §4.4 (S2-R3): an entry's capacity covers its rounded size, as W1's overlay keeps it on an
  overwrite that fits; it doesn't equal it.
- §3.2 (V-1): "4 deep" is four `option`/`vec` layers around a leaf; a fifth is refused.
- §2.1, §3.4 (V-2): the selector is defined once, in `node/vm`'s `interface.rs`; `digest.rs`
  re-exports it.
- §1.1 (S2's report): `status` counts only a running pause (`from ≤ h < until`).
