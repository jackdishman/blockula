// The Ghost Mode prover's timings, for Node (test/ghost-bench.ts) and a browser page
// (test/ghost-bench-browser.ts) alike: `g` is the wasm glue's exports, already initialized.
// Every proof is of a full Action that spends two real notes from a tree (the trace is the same
// size whatever is spent) and is checked by the same build's verifier. Keys are random and thrown
// away; nothing here is printed but times and sizes.
const median = (xs) => {
  const s = [...xs].sort((a, b) => a - b);
  return s.length % 2 ? s[(s.length - 1) / 2] : (s[s.length / 2 - 1] + s[s.length / 2]) / 2;
};

export function benchGhost(g, proofs = 12, random = (n) => crypto.getRandomValues(new Uint8Array(n))) {
  const now = () => performance.now();
  const key = g.GhostKey.fromMaster(random(32), 0);
  const address = key.address();
  const tree = new g.GhostTree();
  // Two notes of the key's own, as a shield makes them.
  const fund = new g.GhostAction(tree.root());
  fund.output(key, address, 600n, new Uint8Array());
  fund.output(key, address, 400n, new Uint8Array());
  fund.seal();
  const leaves = new Uint8Array(64);
  leaves.set(fund.commitment(0), 0);
  leaves.set(fund.commitment(1), 32);
  const encs = [fund.enc(0), fund.enc(1)];
  tree.append(leaves);
  fund.free();

  const scanner = new g.GhostScanner(key);
  const t0 = now();
  const SCANS = 200;
  for (let i = 0; i < SCANS; i++) scanner.tryDecrypt(leaves.subarray(0, 32), encs[0], 0)?.free();
  const decryptMs = (now() - t0) / SCANS;

  const prove = [];
  const verify = [];
  const sizes = [];
  for (let i = 0; i < proofs; i++) {
    const a = new g.GhostAction(tree.root());
    a.spend(key, tree, 0, encs[0]);
    a.spend(key, tree, 1, encs[1]);
    a.output(key, address, 700n, new Uint8Array());
    a.output(key, address, 297n, new Uint8Array());
    a.seal();
    const sighash = random(32);
    const t1 = now();
    const proof = a.prove(sighash);
    prove.push(now() - t1);
    const t2 = now();
    const ok = g.verifyAction(a.anchor(), a.nullifier(0), a.nullifier(1), a.commitment(0), a.commitment(1), a.valueBalance(), sighash, proof);
    verify.push(now() - t2);
    if (!ok) throw new Error("a proof this build made didn't verify");
    sizes.push(proof.length);
    a.free();
  }
  scanner.free();
  tree.free();
  key.free();
  const round = (x) => Math.round(x * 10) / 10;
  return {
    proofs,
    prove_ms: { median: round(median(prove)), min: round(Math.min(...prove)), max: round(Math.max(...prove)), first: round(prove[0]) },
    verify_ms: { median: round(median(verify)) },
    trial_decrypt_ms: Math.round(decryptMs * 1000) / 1000,
    proof_bytes: { min: Math.min(...sizes), max: Math.max(...sizes) },
  };
}
