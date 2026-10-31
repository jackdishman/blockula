// A little of everything the explorer shows, in protocol v1's shapes (node/src/api.rs), on the
// mock node: named accounts with an owner device, an agent on a budget and locks; a transaction of
// every kind the explorer reads (tipped, signed to a name, opening an account, failed in a block,
// dropped unrun, run in a block the node no longer holds, waiting in the mempool, and a kind newer
// than the explorer); hunts in every state; claims and Séances; staking groups; a proposal; a
// multisig; names active, expired, offered and reserved; a stranger's phishing memos on a creator's
// page, one on a dust payment. Strangers' text (memos, titles, labels, reasons, links) carries
// markup and `javascript:` links, which must only ever show as text.
import { interfaceHash, parseFang, proposal, versionDigest, wasmHash } from "@blockula/sdk";
import { addressOf, OTHER_VALIDATOR, VALIDATOR, type MockNode } from "./mock-node";

const F = 10n ** 9n;
const fang = (s: string) => parseFang(s);
const FEE = 64_920n;

/** Markup a stranger put on the chain: rendered as text, it's harmless; as HTML, it runs. */
export const HOSTILE = '<img src=x onerror="window.__pwned=1">';
export const HOSTILE_LINK = "javascript:window.__pwned=2";
/** Phishing a stranger writes on a creator's page: on a dust payment (folded), and on a real one (flagged). */
export const DUST_MEMO = "URGENT: mina.boo expires today. Renew at https://blockula-renew.example";
export const PHISH_MEMO = "Blockula support: verify your wallet at https://blockula-help.example";

/**
 * Text a program's deployer (or the program) chose, as a lying node, or a later protocol version with looser names, could
 * serve it: a method's name, an event's, an abort message. It must only ever show as text, and never run.
 */
export const HOSTILE_METHOD = "<script>window.__pwned=4</script>";
export const HOSTILE_EVENT = '"><img src=x onerror=window.__pwned=5>';
export const HOSTILE_ABORT = 'oops</code><img src=x onerror="window.__pwned=6">';
/** A deployed program (protocol v2). */
export const PROGRAM = addressOf("e1".repeat(32));

export const ADDR = {
  mina: addressOf("c2".repeat(32)),
  lucy: addressOf("c3".repeat(32)),
  renfield: addressOf("c4".repeat(32)),
  coven: addressOf("c5".repeat(32)),
  /** Paid once (that payment opened its account), never named. */
  stranger: addressOf("c6".repeat(32)),
  /** No account yet. */
  fresh: addressOf("c7".repeat(32)),
  pool: addressOf("90".repeat(32)),
  joining: addressOf("7c".repeat(32)),
  candidate: addressOf("7d".repeat(32)),
  jailed: addressOf("7e".repeat(32)),
  tombstoned: addressOf("7f".repeat(32)),
};
export const PHONE = "b1".repeat(32);
export const AGENT = "a9".repeat(32);
export const COVEN_OTHER = "c9".repeat(32);
/** An ML-DSA-65 public key (1,952 bytes) and a proof of possession (3,309), in hex, as an `add_key` carries them: distinct throughout, so no prefix tells two apart. */
export const LONG_KEY = Array.from({ length: 1952 }, (_, i) => ((i * 7 + 3) % 256).toString(16).padStart(2, "0")).join("");
export const LONG_POP = Array.from({ length: 3309 }, (_, i) => ((i * 11 + 5) % 256).toString(16).padStart(2, "0")).join("");

export type World = ReturnType<typeof seedWorld>;

/**
 * Fill `node` with the world. `vlad` is the account whose page, keys and hunts the tests look at:
 * pass the wallet's own account to see it all from its holder's side.
 */
export function seedWorld(node: MockNode, vlad: string = addressOf("c1".repeat(32))) {
  const { mina, lucy, renfield, coven, stranger, pool } = ADDR;
  node.systemLabels.set(pool, "🪦 contribution pool");
  for (const [name, a] of Object.entries({ vlad, mina, lucy, renfield, coven })) {
    node.names.set(name, a);
    node.credit(a, "250");
  }
  node.names.set("castle", vlad);
  node.nameInfo.set("castle", { offered_to: mina, registered_at: 120 });
  node.names.set("tomb", vlad);
  node.nameInfo.set("tomb", { status: "grace", registered_at: 2, expires_at: node.height - 40, grace_until: node.height + 2_591_960 });
  node.reserved.add("crypt");

  // vlad: its own key, a phone, and an agent with a daily budget that's also a Familiar.
  const own = node.keysOf(vlad);
  node.acct(vlad).keys = new Map([
    ...own,
    [PHONE, { label: "Vlad's phone", role: { type: "owner" as const }, added_at: 105 }],
    [AGENT, { label: `claude ${HOSTILE}`, role: { type: "session" as const, limit: (10n * F).toString(), period_blocks: 86_400, expires_at: 2_000_000 }, added_at: 106 }],
  ]);
  node.spent.set(AGENT, fang("1.9"));
  node.acct(coven).keys = new Map([...node.keysOf(coven), [COVEN_OTHER, { label: "the other witch", role: { type: "owner" as const }, added_at: 140 }]]);
  node.locks.set(lucy, [
    { id: "vote:0", reason: "vote on proposal #0", amount: fang("5"), until: node.height + 900, vesting: false },
    { id: "vesting:3", reason: "contribution reward", amount: fang("2"), until: node.height + 9_000, vesting: true },
  ]);

  const tx = (kind: string, from: string, action: Record<string, unknown>, o: Partial<Parameters<MockNode["add"]>[0]> = {}) =>
    node.add({ kind, from, to: null, amount: null, fee: FEE, memo: "", summary: `${node.label(from).name ?? node.label(from).short} ${kind.replace(/_/g, " ")}`, action: { type: kind, ...action }, signer: o.signer ?? null, ...o });
  const pay = (from: string, to: string, amount: string, o: Partial<Parameters<MockNode["add"]>[0]> = {}) =>
    tx("transfer", from, { to: o.toName ?? to, amount: fang(amount).toString() }, { to, amount: fang(amount), summary: `${node.label(from).name ?? node.label(from).short} sent ${amount} FANG to ${o.toName ?? node.label(to).name ?? node.label(to).short}`, ...o });

  // A program anyone deployed: its code and interface stored by hash, its record, and the receipts of what ran.
  const wasmBytes = Uint8Array.from({ length: 560 }, (_, i) => (i < 8 ? [0, 97, 115, 109, 1, 0, 0, 0][i] : i % 251));
  const ifaceBytes = Uint8Array.from([1, 2, 3, 103, 101, 116, 1, 0, 0, 1, 4]);
  const [wasm_hash, interface_hash] = [wasmHash(wasmBytes), interfaceHash(ifaceBytes)];
  const toHex = (b: Uint8Array) => Buffer.from(b).toString("hex");
  const section = (kind: "upload" | "create" | "call", o: { fee: [bigint, bigint]; outcome?: Record<string, unknown>; outside?: Record<string, string>; fuel?: [number, number, number]; created?: string | null; upload?: Record<string, unknown> | null; changes?: { account: string; change: string }[]; storage?: { bytes_delta: number; bonded: string; refunded: string }; returns?: { program: string; untrusted_method: string; value: string; data: string }[]; events?: { program: string; untrusted_name: string; data: string }[]; omitted?: Record<string, number> | null; burned?: string }) => {
    const outside = { record_deposit: "0", upload_deposit: "0", account_creation_fee: "0", ...o.outside };
    const [limit, load, used] = o.fuel ?? [0, 0, 0];
    return {
      version: 1, kind, outcome: o.outcome ?? { ok: true },
      facts: {
        balance_changes: o.changes ?? [], storage: o.storage ?? { bytes_delta: 0, bonded: "0", refunded: "0" },
        outside_vm: { ...outside, total: (BigInt(outside.record_deposit) + BigInt(outside.upload_deposit) + BigInt(outside.account_creation_fee)).toString() },
        burned_by_programs: o.burned ?? "0",
        fuel: { limit, load, used, reserved: limit + load, rate: 250 }, fee: { bytes: o.fee[0].toString(), fuel: o.fee[1].toString(), tip: "0" }, created: o.created ?? null, upload: o.upload ?? null,
      },
      claims: { return_data: o.returns ?? [], events: o.events ?? [], omitted: o.omitted ?? null },
    };
  };
  const short = (a: string) => `${a.slice(0, 10)}…${a.slice(-6)}`;
  const call = { program: PROGRAM, version_digest: versionDigest(PROGRAM, wasm_hash, interface_hash, 1), method: HOSTILE_METHOD, input: "", value: "0" };
  const callAction = { calls: [call], fuel_limit: 10_000_000, storage_limit: "1000000000", bindings: [] };

  const ids = {
    sent: pay(vlad, mina, "12.5", { memo: "for the castle repairs", signer: null }),
    received: pay(mina, vlad, "3", { memo: HOSTILE }),
    tipped: pay(mina, lucy, "0.25", { fee: FEE + 100_000n, maxFee: FEE + 100_000n, tip: 100_000n, memo: "for the proposer too" }),
    opened: pay(vlad, stranger, "1", { memo: "a new account" }),
    byName: pay(vlad, lucy, "2", { toName: "lucy.boo" }),
    agent: pay(vlad, renfield, "0.4", { signer: AGENT, memo: "a tip" }),
    phone: pay(vlad, mina, "2", { signer: PHONE, memo: "sent from my phone" }),
    registered: tx("register_name", vlad, { name: "castle" }, { summary: "vlad.boo claimed castle.boo" }),
    renewed: tx("renew_name", lucy, { name: "lucy", years: 1 }, { summary: "lucy.boo renewed lucy.boo for 1 year" }),
    offered: tx("transfer_name", vlad, { name: "castle", to: mina }, { to: mina, summary: "vlad.boo offered castle.boo to mina.boo" }),
    released: tx("release_name", renfield, { name: "bat" }, { summary: "renfield.boo released bat.boo" }),
    // A v1 action carrying a long key and its proof of possession: "What was signed, exactly" shows every character of them.
    keyAdded: tx("add_key", vlad, { key_id: AGENT, label: "claude", role: { type: "session" }, public_key: LONG_KEY, pop: LONG_POP }, { summary: `vlad.boo added key ${AGENT.slice(0, 8)}… (claude) as a session key` }),
    keyRemoved: tx("remove_key", vlad, { key_id: "d0".repeat(32) }, { summary: "vlad.boo revoked key d0d0d0d0…" }),
    hunt: tx("create_hunt", vlad, { repo: "jackdishman/blockula", issue: 14, amount: fang("25").toString(), expires_at: node.height + 200_000, title: HOSTILE }, { amount: fang("25"), summary: "vlad.boo put 25 FANG on jackdishman/blockula#14" }),
    funded: tx("fund_hunt", lucy, { hunt_id: 0, amount: fang("5").toString() }, { amount: fang("5"), summary: "lucy.boo added 5 FANG to hunt #0" }),
    awarded: tx("award_hunt", mina, { hunt_id: 2, to: "lucy.boo", pr: 7 }, { to: lucy, toName: "lucy.boo", summary: "mina.boo awarded hunt #2 to lucy.boo for PR #7" }),
    cancelled: tx("cancel_hunt", vlad, { hunt_id: 3 }, { summary: "vlad.boo cancelled hunt #3" }),
    delegated: tx("delegate", renfield, { validator: VALIDATOR, amount: fang("20").toString() }, { to: VALIDATOR, amount: fang("20"), summary: "renfield.boo delegated 20 FANG to validator" }),
    undelegated: tx("undelegate", renfield, { validator: VALIDATOR, shares: "0", amount: fang("5").toString() }, { summary: "renfield.boo started unbonding 5 FANG" }),
    withdrew: tx("withdraw_unbonded", renfield, {}, { summary: "renfield.boo withdrew their unbonded stake" }),
    proposed: tx("propose", vlad, { kind: proposal.paramChange("max_validators", 40), title: "Room for more validators", url: "https://github.com/jackdishman/blockula", deposit: fang("10").toString() }, { amount: fang("10"), summary: "vlad.boo proposed #0" }),
    voted: tx("vote", lucy, { proposal_id: 0, aye: true, amount: fang("5").toString(), conviction: 1, proposal_digest: "ab".repeat(32) }, { amount: fang("5"), summary: "lucy.boo voted aye on proposal #0 with 5 FANG" }),
    unlocked: tx("unlock", renfield, {}, { summary: "renfield.boo unlocked their expired vote locks" }),
    claimed: tx("claim_distribution", mina, { distribution_id: 0, amount: fang("50").toString(), proof: [] }, { amount: fang("50"), summary: "mina.boo claimed 50 FANG from distribution #0" }),
    multisig: tx("multisig_create", coven, { owners: [node.keysOf(coven).keys().next().value, COVEN_OTHER], threshold: 2 }, { summary: "coven.boo became a 2-of-2 multisig" }),
    multisigPropose: tx("multisig_propose", coven, { action: { type: "transfer", to: mina, amount: fang("2").toString() } }, { summary: "coven.boo proposed sending 2 FANG to mina.boo" }),
    pool: tx("fund_pool", mina, { amount: fang("4").toString() }, { to: pool, amount: fang("4"), summary: "mina.boo gave 4 FANG to the contribution pool" }),
    vouched: tx("vouch", lucy, { account: renfield }, { to: renfield, amount: 10n * F, summary: "lucy.boo vouched for renfield.boo" }),
    familiar: tx("register_familiar", vlad, { key_id: AGENT, label: "claude" }, { summary: "vlad.boo registered claude as a Familiar" }),
    github: tx("link_github", vlad, { handle: "vlad", gist_id: "abc" }, { summary: "vlad.boo linked GitHub @vlad" }),
    commission: tx("update_validator", VALIDATOR, { commission_bps: 500, p2p_addr: "", http_addr: "" }, { summary: "validator set its commission to 5%" }),
    newer: tx("summon_bats", vlad, { bats: 3 }, { amount: fang("7"), summary: "vlad.boo did something this explorer doesn't know yet" }),
    failed: pay(vlad, mina, "1", { status: "failed", error: "insufficient balance: has 0.5 FANG, needs 1.00006492 FANG" }),
    waiting: pay(mina, lucy, "1", { status: "pending", memo: "on its way" }),
    dust: pay(stranger, mina, "0.000000001", { memo: DUST_MEMO }),
    phish: pay(stranger, mina, "0.5", { memo: PHISH_MEMO }),
    programUpload: tx("upload_code", vlad, { wasm: toHex(wasmBytes), interface: toHex(ifaceBytes), storage_limit: "65010000" }, { fee: 93_260n, maxFee: 238_834n, summary: `${short(vlad)} uploaded program code ${wasm_hash.slice(0, 10)}… (560 bytes) with interface ${interface_hash.slice(0, 10)}… (2 methods), bonding at most 0.06501 FANG`, program: section("upload", { fee: [70_270n, 22_990n], fuel: [91_961, 0, 91_961], outside: { upload_deposit: "59100000" }, storage: { bytes_delta: 591, bonded: "59100000", refunded: "0" }, upload: { wasm_hash, interface_hash, new_code: true, new_interface: true, deposit: "59100000" } }) }),
    programCreate: tx("create_program", vlad, { wasm_hash, interface_hash, authority: null, upgrade_delay_blocks: 0, policy: "open", accepts_transfers: false, init: null, fuel_limit: 0, storage_limit: "28160000", bindings: [] }, { fee: 65_820n, maxFee: 65_820n, summary: `${short(vlad)} created program ${short(PROGRAM)} (immutable) from code ${wasm_hash.slice(0, 10)}…`, program: section("create", { fee: [65_820n, 0n], outside: { record_deposit: "25600000", account_creation_fee: "100000000" }, storage: { bytes_delta: 256, bonded: "25600000", refunded: "0" }, created: PROGRAM }) }),
    programCall: tx("call_program", vlad, callAction, { fee: 2_574_060n, maxFee: 18_455_665n, names: [PROGRAM], summary: `${short(vlad)} called program ${short(PROGRAM)}`, program: section("call", { fee: [65_910n, 2_508_150n], fuel: [10_000_000, 32_600, 76_273], changes: [{ account: vlad, change: "-12800000" }, { account: HOSTILE_LINK, change: "0" }], storage: { bytes_delta: 128, bonded: "12800000", refunded: "0" }, returns: [{ program: PROGRAM, untrusted_method: HOSTILE_METHOD, value: "0", data: "0100000000000000" }], events: [{ program: PROGRAM, untrusted_name: HOSTILE_EVENT, data: "ab".repeat(200) }] }) }),
    programFailed: tx("call_program", vlad, callAction, { status: "failed", error: `program ${short(PROGRAM)} aborted with code 7`, fee: 2_574_060n, maxFee: 18_455_665n, names: [PROGRAM], summary: `${short(vlad)} called program ${short(PROGRAM)}`, program: section("call", { fee: [65_910n, 2_508_150n], fuel: [10_000_000, 32_600, 76_123], outcome: { ok: false, number: 2, code: "aborted", program: PROGRAM, abort_code: 7, untrusted_message: HOSTILE_ABORT }, returns: [{ program: PROGRAM, untrusted_method: HOSTILE_METHOD, value: "0", data: "" }] }) }),
    // From mina, not the account the tests sign as (a pending transaction of its own would hold its next nonce).
    programWaiting: tx("call_program", mina, callAction, { status: "pending", fee: 2_574_060n, maxFee: 18_455_665n, names: [PROGRAM], summary: `${short(mina)} called program ${short(PROGRAM)}`, program: null }),
    dropped: "dd".repeat(32),
    ranUnseen: "de".repeat(32),
  };
  node.failures.set(ids.dropped, { dropped: true, error: "dropped from the mempool: castle.boo belongs to vlad.boo", height: node.height - 3 });
  node.failures.set(ids.ranUnseen, { dropped: false, error: "hunt #9 is closed", height: 40 });

  const label = (a: string) => node.label(a);
  // The program's record and what it's made of, as /api/program, /api/code and /api/interface serve them (node/src/programs/api.rs).
  const created = node.txs.get(ids.programCreate)?.height ?? node.height;
  const uploaded = node.txs.get(ids.programUpload)?.height ?? node.height;
  node.credit(PROGRAM, "1");
  node.programs.set(PROGRAM, {
    address: PROGRAM, creator: label(vlad), created_at: created, code: { wasm_hash, size: wasmBytes.length, interface_hash }, version: 1, version_digest: call.version_digest,
    authority: null, upgrade_delay_blocks: 0, policy: "open", upgradability: "immutable", accepts_transfers: false, balance: (1n * F).toString(), balance_display: "1 FANG",
    storage: { items: 1, bytes: 128, deposit: "12800000", sponsored: "0" }, record_deposit: "25600000", record_payer: label(vlad),
    methods: [
      { untrusted_name: "get", selector: "1d3fce7875d1cf4c", kind: "view", payable: false, args: [], returns: "u64" },
      { untrusted_name: HOSTILE_METHOD, selector: "0000000000000000", kind: "call", payable: true, args: [{ untrusted_name: HOSTILE_EVENT, type: "u64" }, { untrusted_name: "to", type: { vec: ["address", 8] } }], returns: null },
    ],
    proof_path: `/api/proof/66/${Buffer.from(PROGRAM).toString("hex")}`,
  });
  node.accountExtra.set(PROGRAM, { program: { creator: label(vlad), created_at: created, version_digest: call.version_digest, upgradability: "immutable", accepts_transfers: false, link: `/api/program/${PROGRAM}` } });
  node.codes.set(wasm_hash, { wasm_hash, size: wasmBytes.length, untrusted_exports: ["get", HOSTILE_METHOD], uploader: label(vlad), deposit: "56000000", refs: 1, uploaded_at: uploaded, last_used_at: uploaded });
  node.interfaces.set(interface_hash, { interface_hash, size: ifaceBytes.length, hex: toHex(ifaceBytes), untrusted_interface: { format: 1, methods: [] }, uploader: label(vlad), deposit: "3000000", refs: 1, uploaded_at: uploaded, last_used_at: uploaded });

  // Hunts in every state (hunt_json), the creator vlad's.
  const hunt = (id: number, o: Record<string, unknown>) => ({
    id, repo: "jackdishman/blockula", issue: 10 + id, issue_url: HOSTILE_LINK, title: `Hunt ${id}`, amount: (25n * F).toString(), amount_display: "25 FANG", creator: label(vlad),
    funders: [{ account: label(vlad), amount: (20n * F).toString(), amount_display: "20 FANG" }, { account: label(lucy), amount: (5n * F).toString(), amount_display: "5 FANG" }],
    status: "open", created_at: 110, expires_at: node.height + 200_000, awarded_to: null, awarded_by: null, awarded_at: null, pr: null, pr_url: null, familiar: null, self_awarded: false, cancelled_at: null, ...o,
  });
  node.hunts = [
    hunt(0, { title: HOSTILE }),
    hunt(1, { title: "Expired, its escrow still held", status: "expired", expires_at: node.height - 5 }),
    hunt(2, { title: "Won", status: "awarded", creator: label(mina), awarded_to: label(lucy), awarded_by: label(mina), awarded_at: 125, pr: 7, pr_url: HOSTILE_LINK, familiar: "claude" }),
    hunt(3, { title: "Called off", status: "cancelled", cancelled_at: 128, repo: 'evil/x"><img src=x onerror=window.__pwned=3>' }),
  ];

  // The contribution pool: claims in every state, an open Séance and a closed one.
  const claim = (id: number, o: Record<string, unknown>) => ({
    id, claimant: label(lucy), handle: "lucy", repo: "jackdishman/blockula", pr: 30 + id, pr_url: HOSTILE_LINK, familiar: null, bond: F.toString(), bond_display: "1 FANG", status: "pending", submitted_at: 100 + id,
    attestations: [], merge_sha: null, verified_at: null, eligible_at: null, expires_at: 700, seance: null, score_ppm: null, score_display: null, payout: "0", payout_display: "0 FANG", soul_awarded_display: "0", closed_at: null, reason: null,
    revert_window_until: null, revert_attestations: null, ...o,
  });
  const attest = { validator: label(VALIDATOR), ok: true, merge_sha: "f00d".repeat(10), note: "", at: 112 };
  node.claims = [
    claim(5, { status: "rejected", bond: "0", reason: HOSTILE, closed_at: 140 }),
    claim(4, { status: "spam", bond: "0", seance: 1, closed_at: 135 }),
    claim(3, { status: "scored", bond: "0", seance: 1, score_ppm: 620_000, score_display: "62%", payout: (12n * F).toString(), payout_display: "12 FANG", soul_awarded_display: "3", attestations: [attest], verified_at: 112, merge_sha: "f00d".repeat(10), closed_at: 135 }),
    claim(2, { status: "in_seance", seance: 2, attestations: [attest], verified_at: 112, merge_sha: "f00d".repeat(10), familiar: "claude" }),
    claim(1, { status: "verified", attestations: [attest, { ...attest, validator: label(OTHER_VALIDATOR), ok: false, note: HOSTILE }], verified_at: 112, merge_sha: "f00d".repeat(10), revert_window_until: node.height + 50, revert_attestations: { attestations: [{ validator: label(VALIDATOR), revert_sha: "beef".repeat(10), at: 140, in_current_set: true }], quorum: 2, set: 2, by_revert: [{ revert_sha: "beef".repeat(10), validators: 1 }] } }),
    claim(0, {}),
  ];
  const juror = (a: string, o: Record<string, unknown>) => ({ juror: label(a), pairs: 4, committed: true, revealed: false, agreement_ppm: null, agreement_display: null, reward_display: "0 FANG", soul_delta_display: "0", assigned_pairs: [[2, 3]], commitment: "ab".repeat(32), votes: null, spam: [], ...o });
  const closed = { id: 1, phase: "closed", opened_at: 120, commit_until: 180, reveal_until: 210, seed: "5eed".repeat(16), budget: (40n * F).toString(), budget_display: "40 FANG", claims: [node.claims[2], node.claims[1]], jurors: [juror(mina, { revealed: true, agreement_ppm: 800_000, agreement_display: "80%", reward_display: "2 FANG", soul_delta_display: "+1" }), juror(renfield, { committed: false, soul_delta_display: "−1" })], results: { accepted: 1, below_threshold: 0, spam: 1, carried_over: 0, no_shows: 1, paid: (12n * F).toString(), returned: (28n * F).toString() }, results_display: { paid: "12 FANG", returned: "28 FANG" } };
  const open = { id: 2, phase: "sealing", opened_at: node.height - 10, commit_until: node.height + 50, reveal_until: node.height + 80, seed: "5eed".repeat(16), budget: (48n * F).toString(), budget_display: "48 FANG", claims: [node.claims[3]], jurors: [juror(mina, { committed: false }), juror(renfield, {})], results: null, results_display: null };
  node.seances = [closed, open];
  node.contributions = {
    active: true,
    pool: { account: label(pool), balance: (191n * F).toString(), balance_display: "191 FANG", next_budget_display: "47.75 FANG" },
    repos: [{ repo: "jackdishman/blockula", weight: 100 }],
    open_seance: { ...open, jurors: open.jurors.map(({ assigned_pairs, commitment, votes, spam, ...j }) => j) },
    next_seance_at: node.height + 90,
    recent_seances: [{ id: 1, opened_at: 120, closed_at: 210, claims: 2, jurors: 2, budget_display: "40 FANG", results: closed.results }],
    claims: { pending: 1, verified: 1, in_seance: 1, scored: 1, spam: 1, rejected: 1 },
    jurors: { pool: 2, active: 2 },
    rules: { seance_blocks: 90, commit_blocks: 60, reveal_blocks: 30, claim_maturity_blocks: 0, claim_ttl_blocks: 600, vesting_blocks: 600, vouch_lock_blocks: 120, soul_half_life_blocks: 31_536_000, claim_bond_display: "1 FANG", vouch_bond_display: "10 FANG", juror_bond_display: "10 FANG", vouch_min_soul: "10", juror_min_soul: "5", jury_size: 7, min_jury: 3, pairs_per_juror: 24, max_claims_per_seance: 64, pool_share_per_seance: "25%", juror_share: "10%", min_score: "15%", full_budget_people: 16, min_people: 2, claim_rank_divisor: 4 },
  };
  node.souls = [mina, lucy, renfield].map((a, i) => ({ account: label(a), soul: { points: (12 - i * 3) * 1000, display: String(12 - i * 3), earned_display: String(14 - i * 3) }, github: i ? null : "mina", juror: i < 2, vouched_by: null }));

  // Accounts beyond balance and keys.
  node.accountExtra.set(vlad, { github: { handle: `vlad${HOSTILE}`, gist_id: "abc", linked_at: 130, verify_url: "/api/github/vlad" }, familiars: [{ key_id: AGENT, label: "claude" }], hunts_created: [0, 1, 3], hunts_won: [] });
  node.accountExtra.set(lucy, {
    hunts_won: [{ id: 2, repo: "jackdishman/blockula", issue: 12, amount_display: "25 FANG", pr: 7, self_awarded: false }],
    contribution: {
      soul: { points: 9000, display: "9", earned_display: "11" },
      sybil: null,
      vouched_by: { voucher: label(mina), at: 100, bond_display: "10 FANG" },
      vouching_for: [{ account: label(renfield), at: 150, unlocks_at: 270 }],
      vouches_withdrawn: [],
      vouch_bonds_held: [],
      juror: { bond_display: "10 FANG", active: true, served: 1, joined_at: 101 },
      claims: node.claims,
      next_claim_bond: F.toString(),
      next_claim_bond_display: "1 FANG",
      max_open_claims: 4,
      rewards: { total_display: "12 FANG", withdrawable: (4n * F).toString(), withdrawable_display: "4 FANG", unvested_display: "8 FANG", grants: [] },
    },
  });
  node.multisigs.set(coven, { account: label(coven), threshold: 2, owners: [...node.keysOf(coven).keys()].map((key_id, i) => ({ key_id, label: i ? "the other witch" : "original key" })), created_at: 141, pending: [{ id: 0, action: { type: "transfer", to: mina, amount: fang("2").toString() }, summary: `coven.boo would send 2 FANG to mina.boo ${HOSTILE}`, approvals: [COVEN_OTHER], valid_approvals: 1, expires_at: node.height + 500, expired: false }] });

  // Staking: v1's groups around the two pools, and a set of one or two's admission.
  const row = (a: string, o: Record<string, unknown>) => node.validatorRow(a, { stake: (60_000n * F).toString(), stake_display: "60000 FANG", self_bond_display: "50000 FANG", ...o });
  node.validatorsExtra = {
    joining: [row(ADDR.joining, { status: "active" })],
    leaving: [row(OTHER_VALIDATOR, { status: "active" })],
    candidates: [row(ADDR.joining, {}), row(ADDR.candidate, { stake_display: "51000 FANG" })],
    jailed: [row(ADDR.jailed, { jailed: { at: 90, until: node.height + 3600, reason: `missed 51 of its last 100 leader slots ${HOSTILE}` }, liveness: { window: 100, recorded: 100, missed: 51, max_missed: 50, led: 49, missed_total: 51 } })],
    tombstoned: [row(ADDR.tombstoned, { status: "tombstoned", tombstoned_at: 77, slashed_display: "3000 FANG" })],
    admission: { members: [label(VALIDATOR)], quorum: 1, ready_from: node.height + 3240 },
    admins: [label(VALIDATOR)],
  };
  node.proposals = [
    {
      id: 0, title: "Room for more validators", url: "https://github.com/jackdishman/blockula", kind: proposal.paramChange("max_validators", 40), summary: "set max_validators to 40", proposer: label(vlad), deposit: (10n * F).toString(), deposit_display: "10 FANG",
      status: "voting", created_at: node.height - 30, voting_end: node.height + 5000, execute_at: null, supermajority: false, ayes: { amount: { drops: (5n * F).toString(), display: "5 FANG" }, weight: { drops: (5n * F).toString(), display: "5 FANG" } }, nays: { amount: { drops: "0", display: "0 FANG" }, weight: { drops: "0", display: "0 FANG" } }, voters: 1, council: [], tally: null, outcome: "",
    },
  ];
  /**
   * A payout to an account that didn't exist, made on request (not part of the world: the activity scan reads a bounded number of
   * blocks, and a world with one more would push the oldest ones past it): the creation fee (0.1 FANG) comes out of the amount,
   * inside the programs' own diff (burned_by_programs).
   */
  const addPayout = () =>
    tx("call_program", vlad, callAction, { fee: 2_574_060n, maxFee: 18_455_665n, names: [PROGRAM], summary: `${short(vlad)} called program ${short(PROGRAM)}`, program: section("call", { fee: [65_910n, 2_508_150n], fuel: [10_000_000, 32_600, 41_000], changes: [{ account: vlad, change: "19200000" }, { account: PROGRAM, change: "-1000000000" }, { account: mina, change: "900000000" }], storage: { bytes_delta: -128, bonded: "0", refunded: "19200000" }, burned: "100000000" }) });

  return { vlad, ids, wasm_hash, interface_hash, addPayout, ...ADDR };
}
