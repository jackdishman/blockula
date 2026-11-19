// The proven table reader, offline: records read from Merkle proofs made by this directory's own
// prover (written from the tree's definition, checked by the SDK's verifier), state roots
// confirmed by witnesses, and every way an answer can be wrong refused.
// docs/proposals/blackjack-card-privacy.md, section 7.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { keyHash, rootOf, valueHash, verifyProof, type ProofResponse } from "@blockula/sdk";
import { DEFAULT_LIMITS, Phase, Table, bytesAddress, chainFromTop, decodeGame, decodeHouse, decodeLimits, decodeOpenList, hexBytes, storageKey, toHex, type Game, type House } from "../src/index.js";
import { ProofError, ProvenTable, RootWitnesses, storageRecordKey, storedValue, type ProofFailure, type ProofNode, type WitnessNode } from "../src/proven.js";
import { StateTree, encodeGame, encodeHouse, encodeLimits, encodeOpenList, storageRecords, storedEntry, tableStorage } from "../src/testing.js";

const A = (n: number) => bytesAddress(new Uint8Array(32).fill(n), "boo");
const PROGRAM = A(9);
const OTHER = A(10);
const FAST = { waitMs: 120, retryMs: 5, attemptMs: 60, headTtlMs: 0 };

async function refused(p: Promise<unknown>, kind: ProofFailure, what: string): Promise<ProofError> {
  try {
    await p;
  } catch (e) {
    assert.ok(e instanceof ProofError, `${what}: threw ${String(e)}, not a ProofError`);
    assert.equal(e.kind, kind, `${what}: ${e.message}`);
    // No message carries a whole root or record: the dealer's log scrubs long hex anyway.
    assert.ok(!/[0-9a-f]{32,}/.test(e.message), `${what}: the message holds a long run of hex`);
    return e;
  }
  assert.fail(`${what}: wasn't refused`);
}

// ---- the table whose storage these tests read ----
const chain = chainFromTop("11".repeat(32));
const chain2 = chainFromTop("22".repeat(32));
const house: House = { owner: A(1), dealer: A(2), bankroll: 500n, reserved: 30n, stakes: 20n, table: 7n, nextCommit: 12n, nextGame: 10n, paused: false, tripped: false, open: 2 };
const waiting: Game = { id: 8n, player: A(3), stake: 10n, clientSeed: null, seedHash: "5a".repeat(32), link: chain[0], deadline: 140n, postId: 0n, phase: Phase.Seed, table: false, dealt: 0, playerCards: [], dealerCards: [], playerTotal: 0, dealerTotal: 0 };
const playing: Game = { id: 9n, player: A(4), stake: 10n, clientSeed: "c1".repeat(32), seedHash: null, link: chain2[3], deadline: 150n, postId: 77n, phase: Phase.Player, table: true, dealt: 3, playerCards: [9, 20], dealerCards: [5], playerTotal: 18, dealerTotal: 6 };
const commitments = new Map<bigint, string>([
  [10n, "c0".repeat(32)],
  [11n, "c1".repeat(32)],
]);
const storage = tableStorage({ house, limits: DEFAULT_LIMITS, open: [8n, 9n], games: [waiting, playing], commitments });

// ---- the test encoders are the decoders' inverses ----
assert.deepEqual(decodeHouse(encodeHouse(house), "boo"), house);
assert.deepEqual(decodeLimits(encodeLimits(DEFAULT_LIMITS)), DEFAULT_LIMITS);
assert.deepEqual(decodeGame(8n, encodeGame(waiting), "boo"), waiting);
assert.deepEqual(decodeGame(9n, encodeGame(playing), "boo"), playing);
assert.deepEqual(decodeOpenList(encodeOpenList([8n, 9n]), 2), [8n, 9n]);
console.log("✓ the test encoders write what the decoders read");

// ---- the prover agrees with the SDK's definition of the tree and its verifier ----
{
  const records = storageRecords(PROGRAM, storage);
  const tree = new StateTree(records);
  assert.equal(tree.root, toHex(rootOf(records.map((r): [Uint8Array, Uint8Array] => [keyHash(r.namespace, r.key), valueHash(r.value)]))));
  for (const r of records) assert.ok(verifyProof(tree.root, r.namespace, r.key, r.value, tree.prove(r.namespace, r.key)), "an inclusion proof verifies");
  for (const absent of [storageRecordKey(PROGRAM, storageKey.game(123n)), storageRecordKey(OTHER, storageKey.house), Uint8Array.of(1, 2, 3)]) {
    assert.equal(tree.value(69, absent), null);
    assert.ok(verifyProof(tree.root, 69, absent, null, tree.prove(69, absent)), "an exclusion proof verifies");
    assert.ok(!verifyProof(tree.root, 69, absent, storedEntry("00"), tree.prove(69, absent)), "and proves no value");
  }
  // The smallest trees: nothing, and one record.
  const none = new StateTree([]);
  assert.ok(verifyProof(none.root, 69, Uint8Array.of(1), null, none.prove(69, Uint8Array.of(1))));
  const one = new StateTree([records[0]]);
  assert.ok(verifyProof(one.root, records[0].namespace, records[0].key, records[0].value, one.prove(records[0].namespace, records[0].key)));
  assert.ok(verifyProof(one.root, 69, Uint8Array.of(1), null, one.prove(69, Uint8Array.of(1))));
  console.log(`✓ the prover's ${records.length} proofs (and proofs of absence) check against the SDK's verifier and root`);
}

// ---- a stored entry ----
{
  assert.equal(toHex(storedValue(storedEntry("a1b2c3"))), "a1b2c3");
  assert.equal(storedValue(storedEntry("")).length, 0);
  const good = storedEntry("a1b2c3");
  assert.throws(() => storedValue(Uint8Array.from([2, ...good.slice(1)])), /format 2/);
  assert.throws(() => storedValue(good.slice(0, -1)), /length/);
  assert.throws(() => storedValue(Uint8Array.from([...good, 0])), /length/);
  assert.throws(() => storedValue(new Uint8Array(10)), /too short/);
  // A length that points past the end.
  const long = Uint8Array.from(good);
  new DataView(long.buffer).setUint32(1, 4_000_000_000, true);
  assert.throws(() => storedValue(long), /length/);
  assert.deepEqual([...storageRecordKey(PROGRAM, "48")], [...new Uint8Array(32).fill(9), 0x48]);
  assert.throws(() => storageRecordKey(PROGRAM, ""), /empty/);
  console.log("✓ a stored entry gives up its value only in the one format known; its tree key is the program's bytes then the key");
}

// ---- the node's own bytes: test/storage-vectors.json is written by the node's tests ----
{
  interface Answered {
    key: string;
    answer: ProofResponse;
  }
  const v = JSON.parse(readFileSync(new URL("./storage-vectors.json", import.meta.url), "utf8")) as { program: string; entries: (Answered & { value: string; payer: string; deposit: string; capacity: number })[]; absent: Answered };
  /** A node that answers from the file, and a witness that has the file's root at its height. */
  const answers = new Map([...v.entries, v.absent].map((e) => [e.answer.key, e.answer]));
  const node: ProofNode = { proof: async (_namespace, key) => answers.get(toHex(key))! };
  const witness = (root: string): WitnessNode => ({ status: async () => ({ height: 0 }), block: async () => ({ state_root: root }) });
  const table = new ProvenTable(v.program, node, [witness(v.absent.answer.root)], FAST);
  assert.equal(v.entries.length, 2);
  for (const e of v.entries) {
    // The tree key is the program's 32 address bytes then the key, and the stored entry gives up the program's value.
    assert.equal(toHex(storageRecordKey(v.program, e.key)), e.answer.key);
    assert.deepEqual(await table.entry(e.key), { value: e.value, height: 0 });
    // And the test prover writes an entry as the node does (the payer, the deposit and the capacity included).
    const made = storedEntry(e.value, hexBytes(e.payer, 32), BigInt(e.deposit));
    new DataView(made.buffer).setUint32(made.length - 4, e.capacity, true);
    assert.equal(toHex(made), e.answer.value);
  }
  assert.deepEqual(await table.entry(v.absent.key), { value: null, height: 0 });
  // Under any other root, the node's own answers are refused.
  await refused(new ProvenTable(v.program, node, [witness("00".repeat(32))], FAST).entry(v.entries[0].key), "disagreement", "the node's answer under a root the witness doesn't have");
  console.log("✓ the node's own answers for program storage (storage-vectors.json, written by its tests) are read: the tree key, the stored entry, an absence");
}

// ---- nodes ----
/** A node over one program's storage: it serves proofs, its head, and its blocks' state roots. */
class Node implements ProofNode, WitnessNode {
  rpc: string;
  height = 100;
  /** Blocks this node is behind the chain's `height` (as a witness). */
  behind = 0;
  down = false;
  hangs = false;
  asked = { proof: 0, status: 0, block: 0 };
  /** What it does to an honest answer before sending it. */
  bend: (r: ProofResponse) => ProofResponse = (r) => r;
  readonly roots = new Map<number, string>();
  private tree!: StateTree;

  constructor(name: string, entries: ReadonlyMap<string, string>, private readonly program = PROGRAM) {
    this.rpc = `https://${name}.example`;
    this.set(entries);
  }

  /** The state after the current block. */
  set(entries: ReadonlyMap<string, string>): void {
    this.tree = new StateTree(storageRecords(this.program, entries));
    this.roots.set(this.height, this.tree.root);
  }

  /** A new block with this storage. */
  mine(entries: ReadonlyMap<string, string>, blocks = 1): void {
    for (let i = 1; i < blocks; i++) this.roots.set(this.height + i, this.tree.root);
    this.height += blocks;
    this.set(entries);
  }

  private async up(): Promise<void> {
    if (this.hangs) await new Promise(() => undefined);
    if (this.down) throw new Error("connection refused");
  }

  async proof(namespace: number, key: Uint8Array): Promise<ProofResponse> {
    this.asked.proof++;
    await this.up();
    return this.bend(this.tree.answer(namespace, key, this.height));
  }

  async status(): Promise<{ height: number }> {
    this.asked.status++;
    await this.up();
    return { height: this.height - this.behind };
  }

  async block(height: number): Promise<{ state_root: string }> {
    this.asked.block++;
    await this.up();
    const root = this.roots.get(height);
    if (root === undefined || height > this.height - this.behind) throw Object.assign(new Error("no such block"), { status: 404 });
    return { state_root: root };
  }
}

/** A plain `Table` over the same storage, through a stand-in for the node's storage route. */
function plainTable(entries: ReadonlyMap<string, string>, height: number): Table {
  return new Table(PROGRAM, async (path) => {
    const after = new URL(`http://x${path}`).searchParams.get("after") ?? "";
    const keys = [...entries.keys()].sort();
    const next = keys.find((k) => Buffer.compare(Buffer.from(k, "hex"), Buffer.from(after, "hex")) > 0);
    return { entries: next === undefined ? [] : [{ key: next, value: entries.get(next) }], height };
  });
}

// ---- proven reads are the plain reads, and "not there" is proven too ----
{
  const node = new Node("node", storage);
  const witness = new Node("witness", storage);
  const proven = new ProvenTable(PROGRAM, node, [witness], FAST);
  const plain = plainTable(storage, node.height);
  assert.deepEqual(await proven.house(), await plain.house());
  assert.deepEqual(await proven.house(), { ...house, height: 100 });
  assert.deepEqual(await proven.limits(), await plain.limits());
  assert.deepEqual(await proven.openGames(), [8n, 9n]);
  assert.deepEqual(await proven.game(8n), waiting);
  assert.deepEqual(await proven.game(9n), await plain.game(9n));
  assert.equal(await proven.game(7n), null, "a settled hand is provably gone");
  assert.equal(await proven.commitment(10n), "c0".repeat(32));
  assert.equal(await proven.commitment(9n), null, "a taken commitment is provably gone");
  assert.equal(await proven.relayerApproved(A(5)), false);
  assert.deepEqual(await proven.player(A(5)), await plain.player(A(5)));
  assert.equal(proven.proofs, node.asked.proof);
  // One round of witness requests for the one block all of it was read at.
  assert.equal(proven.roots.rounds, 1);
  assert.equal(witness.asked.block, 1);
  // The witness was asked for a header and its head, and nothing else exists to ask it.
  assert.equal(witness.asked.proof, 0);

  // A new block: the new state is read, and its root confirmed once.
  const later = tableStorage({ house: { ...house, open: 1 }, limits: DEFAULT_LIMITS, open: [9n], games: [playing], commitments });
  node.mine(later);
  witness.mine(later);
  assert.deepEqual(await proven.openGames(), [9n]);
  assert.equal(await proven.game(8n), null);
  assert.equal((await proven.house()).height, 101);
  assert.equal(proven.roots.rounds, 2);

  // A table that isn't one: its house record is provably absent.
  const nothing = new ProvenTable(OTHER, node, [witness], FAST);
  await assert.rejects(nothing.house(), /isn't a blackjack table/);
  console.log("✓ proven reads give what the storage route gives, an absent record is proven absent, and a block's root is confirmed once");
}

// ---- every wrong answer is refused ----
{
  const lie = tableStorage({ house, limits: DEFAULT_LIMITS, open: [8n, 9n], games: [waiting, { ...playing, phase: Phase.Dealer }], commitments });
  const witness = new Node("witness", storage);
  const read = (node: Node, opts = FAST) => new ProvenTable(PROGRAM, node, [witness], opts).game(9n);

  // A node whose whole state is a consistent lie: its proofs verify against its own root, which no witness has.
  const liar = new Node("liar", lie);
  assert.equal((await new ProvenTable(PROGRAM, liar, [new Node("accomplice", lie)], FAST).game(9n))?.phase, Phase.Dealer, "(the lie is a well-formed one: a witness in on it confirms it)");
  const e = await refused(read(liar), "disagreement", "a consistent lie");
  assert.match(e.message, /witness\.example/);

  // An honest proof with the value changed.
  const node = new Node("node", storage);
  node.bend = (r) => ({ ...r, value: toHex(storedEntry(encodeGame({ ...playing, phase: Phase.Dealer }))) });
  await refused(read(node), "bad_proof", "a changed value");
  // The value dropped: "that hand doesn't exist".
  node.bend = (r) => ({ ...r, value: null });
  await refused(read(node), "bad_proof", "a record claimed absent");
  // The lie's proof and value, under the honest root.
  node.bend = (r) => ({ ...new StateTree(storageRecords(PROGRAM, lie)).answer(69, storageRecordKey(PROGRAM, storageKey.game(9n)), r.height), root: r.root });
  await refused(read(node), "bad_proof", "a lie's proof under the real root");
  // A real proof of another record: another hand, another table, another namespace.
  const real = new StateTree(storageRecords(PROGRAM, storage));
  node.bend = (r) => real.answer(69, storageRecordKey(PROGRAM, storageKey.game(8n)), r.height);
  await refused(read(node), "bad_proof", "another hand's proof");
  node.bend = (r) => ({ ...r, key: toHex(storageRecordKey(OTHER, storageKey.game(9n))) });
  await refused(read(node), "bad_proof", "another table's key");
  node.bend = (r) => ({ ...r, namespace: 66 });
  await refused(read(node), "bad_proof", "another namespace");
  // Nonsense.
  for (const bent of [{ ...real.answer(69, storageRecordKey(PROGRAM, storageKey.game(9n)), 100), height: -1 }, { ...real.answer(69, storageRecordKey(PROGRAM, storageKey.game(9n)), 100), proof: { siblings: ["zz"], leaf: null } }, null as unknown as ProofResponse]) {
    node.bend = () => bent;
    await refused(read(node), "bad_proof", "a malformed answer");
  }
  // A real, proven record in a format this reader doesn't know.
  const odd = new Node("odd", storage);
  const oddWitness = new Node("odd-witness", storage);
  const key = storageRecordKey(PROGRAM, storageKey.game(9n));
  const records = storageRecords(PROGRAM, storage).map((r) => (toHex(r.key) === toHex(key) ? { ...r, value: Uint8Array.from([2, ...r.value.slice(1)]) } : r));
  const oddTree = new StateTree(records);
  odd.bend = (r) => oddTree.answer(69, key, r.height);
  oddWitness.roots.set(100, oddTree.root);
  await refused(new ProvenTable(PROGRAM, odd, [oddWitness], FAST).game(9n), "bad_proof", "an entry of another format");

  // The right root at the wrong height: the witness has another root there.
  const moved = new Node("moved", storage);
  const ahead = new Node("ahead", storage);
  ahead.mine(lie);
  moved.bend = (r) => ({ ...r, height: 101 });
  await refused(new ProvenTable(PROGRAM, moved, [ahead], FAST).game(9n), "disagreement", "a root moved to another block");

  // The node contradicting itself about one block.
  const two = new Node("two", storage);
  const table = new ProvenTable(PROGRAM, two, [witness], FAST);
  await table.game(9n);
  two.set(lie);
  await refused(table.game(9n), "disagreement", "two roots for one block");
  console.log("✓ a lie, a changed or dropped value, another record's proof, a moved root, an unknown format and nonsense are each refused");
}

// ---- old state ----
{
  const node = new Node("node", storage);
  const witness = new Node("witness", storage);
  // The chain moves on; the node keeps serving block 100, with its genuine proof and root.
  witness.mine(storage, 10);
  const table = new ProvenTable(PROGRAM, node, [witness], FAST);
  assert.equal((await table.house()).height, 100, "10 blocks behind is accepted by default");
  witness.mine(storage);
  const e = await refused(table.house(), "stale", "11 blocks behind");
  assert.match(e.message, /#100.*#111/);
  assert.equal((await new ProvenTable(PROGRAM, node, [witness], { ...FAST, maxLagBlocks: 11 }).house()).height, 100);
  await refused(new ProvenTable(PROGRAM, node, [witness], { ...FAST, maxLagBlocks: 0 }).house(), "stale", "any lag at all, when none is allowed");
  // The newest head any witness reports is the one that counts.
  const slow = new Node("slow", storage);
  await refused(new ProvenTable(PROGRAM, node, [slow, witness], FAST).house(), "stale", "one witness far ahead of another");
  console.log("✓ a genuine proof of old state is refused past maxLagBlocks");
}

// ---- witnesses: behind, down, a quorum, and a disagreement that is never outvoted ----
{
  const node = new Node("node", storage);
  const mk = (name: string) => new Node(name, storage);

  // Behind by a block: waited for.
  const behind = mk("behind");
  behind.behind = 1;
  setTimeout(() => (behind.behind = 0), 30);
  const t0 = Date.now();
  assert.equal((await new ProvenTable(PROGRAM, node, [behind], FAST).house()).height, 100);
  assert.ok(Date.now() - t0 >= 20 && behind.asked.block > 1, "the witness was asked again until it had the block");

  // Never catches up, or down, or hanging: nothing is made of the record.
  const stuck = mk("stuck");
  stuck.behind = 1;
  await refused(new ProvenTable(PROGRAM, node, [stuck], FAST).house(), "unconfirmed", "a witness that stays behind");
  const down = mk("down");
  down.down = true;
  await refused(new ProvenTable(PROGRAM, node, [down], FAST).house(), "unconfirmed", "a witness that is down");
  const hung = mk("hung");
  hung.hangs = true;
  await refused(new ProvenTable(PROGRAM, node, [hung], FAST).house(), "unconfirmed", "a witness that never answers");
  // All of them by default: one down among three is too few.
  await refused(new ProvenTable(PROGRAM, node, [mk("a"), mk("b"), down], FAST).house(), "unconfirmed", "one of three down, all required");
  // A quorum of two: one may be down, or behind, or hanging.
  for (const third of [down, stuck, hung]) assert.equal((await new ProvenTable(PROGRAM, node, [mk("a"), third, mk("b")], { ...FAST, quorum: 2 }).house()).height, 100);
  await refused(new ProvenTable(PROGRAM, node, [mk("a"), down, stuck], { ...FAST, quorum: 2 }).house(), "unconfirmed", "two of three down, two required");

  // A disagreement is never outvoted: two confirm, the third (the slowest to answer) has another root.
  const other = new Node("dissenter", tableStorage({ house: { ...house, paused: true }, limits: DEFAULT_LIMITS, open: [8n, 9n], games: [waiting, playing], commitments }));
  const slowBlock = other.block.bind(other);
  other.block = async (h) => {
    await new Promise((r) => setTimeout(r, 25));
    return slowBlock(h);
  };
  for (const quorum of [1, 2, 3]) {
    const e = await refused(new ProvenTable(PROGRAM, node, [mk("a"), mk("b"), other], { ...FAST, quorum }).house(), "disagreement", `a dissenting witness, quorum ${quorum}`);
    assert.match(e.message, /dissenter\.example/);
  }

  // A failed confirmation isn't remembered: the same block is asked about again, and can pass.
  const flaky = mk("flaky");
  flaky.down = true;
  const table = new ProvenTable(PROGRAM, node, [flaky], FAST);
  await refused(table.house(), "unconfirmed", "a witness down for now");
  flaky.down = false;
  assert.equal((await table.house()).height, 100);

  // Reads at once share one confirmation.
  const w = mk("w");
  const many = new ProvenTable(PROGRAM, node, [w], { ...FAST, headTtlMs: 1000 });
  await Promise.all([many.house(), many.limits(), many.game(8n), many.game(9n), many.commitment(10n)]);
  assert.deepEqual([many.roots.rounds, w.asked.block, w.asked.status], [1, 1, 1]);

  // Settings that make no sense.
  assert.throws(() => new ProvenTable(PROGRAM, node, []), /at least one witness/);
  assert.throws(() => new RootWitnesses([w], { quorum: 2 }), /quorum is 1 to 1/);
  assert.throws(() => new RootWitnesses([w], { quorum: 0 }), /quorum/);
  assert.throws(() => new RootWitnesses([w], { maxLagBlocks: -1 }), /maxLagBlocks/);
  console.log("✓ a witness behind is waited for; down, stuck or silent ones count against the quorum; a dissenting one refuses the read whatever the quorum");
}

console.log("✅ the proven reader tests passed");
