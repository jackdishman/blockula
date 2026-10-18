// Protocol v1 vectors for the explorer's ledger and preflight (web/lib/v1.test.mjs): real
// transactions on a dev chain, what the node answered before each one was signed (the reads the
// wallet's preflight takes), what it said about each once it ran (/api/tx), and what each did to
// the balances of the accounts it touched. Transactions the chain refuses are kept too, with the
// node's refusal, so the preflight can be held to refusing them first.
//
//   blockula dev --reset --port 7340 &
//   npm run build -w @blockula/sdk
//   node scripts/explorer-vectors.mjs http://localhost:7340 > web/lib/v1-vectors.json
import { Client, proposal, proposalDigest, Wallet } from "@blockula/sdk";

const rpc = process.argv[2] || "http://localhost:7070";
const client = new Client(rpc);
const status = await client.status();
if (!status.faucet) throw new Error(`${rpc} has no faucet; run this against a dev chain (blockula dev)`);
await client.addressPrefix();

const get = async (path) => {
  const res = await fetch(`${rpc}/api${path}`);
  const json = await res.json();
  if (!res.ok) throw Object.assign(new Error(json.error ?? res.statusText), { status: res.status });
  return json;
};
const maybe = (path) => get(path).catch((e) => (e.status === 404 ? null : Promise.reject(e)));
const balance = async (who) => BigInt((await get(`/account/${who}`)).balance);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const log = (...a) => console.error(...a);

/**
 * What the wallet's checkTx reads before it signs `tx` (web/lib/explorer/check.ts), trimmed to
 * what the preflight reads: an account's history to its pending transactions (they reserve what
 * they'll take), the validator set to its admins.
 */
async function reads(tx) {
  const a = tx.action;
  const pays = a.type === "transfer" || a.type === "award_hunt";
  const huntId = ["fund_hunt", "award_hunt", "cancel_hunt"].includes(a.type) ? a.hunt_id : null;
  const pendingOnly = (acct) => ({ ...acct, recent_txs: acct.recent_txs.filter((t) => t.status === "pending") });
  return {
    status: await get("/status").then((s) => ({ height: s.height, block_time_ms: s.block_time_ms })),
    account: pendingOnly(await get(`/account/${tx.from}`)),
    multisig: await maybe(`/multisig/${tx.from}`),
    recipient: pays ? pendingOnly(await get(`/account/${encodeURIComponent(a.to)}`)) : null,
    fees: pays ? await get("/fees").then((f) => ({ account_creation_fee: f.account_creation_fee })) : null,
    hunt: huntId != null ? ((await maybe(`/hunt/${huntId}`)) ?? { id: huntId, missing: true }) : null,
    name: a.type === "register_name" || a.type === "renew_name" ? await get(`/name/${a.name}`) : null,
    validators: a.type === "cancel_hunt" ? await get("/validators").then((v) => ({ admins: v.admins })) : null,
  };
}

const vectors = [];
/** The signed tx without its key and signature (several KB each, and not what the tests read). */
const slim = (tx) => ({ ...tx, public_key: `${tx.public_key.slice(0, 16)}…`, signature: `${tx.signature.slice(0, 16)}…` });

/**
 * Build `action` as `wallet` (actAs'd), read what checkTx reads, submit, and record what happened:
 * the node's refusal, or the sealed tx and the balance change of each account in `watch`.
 */
async function vector(name, wallet, action, { watch = [], tip, note } = {}) {
  const tx = await wallet.build(action, "", tip ? { tip } : {});
  const r = await reads(tx);
  const accounts = [...new Set([tx.from, ...watch])];
  const before = Object.fromEntries(await Promise.all(accounts.map(async (a) => [a, (await balance(a).catch(() => 0n)).toString()])));
  const v = { name, note, signer: wallet.keyId, tx: slim(tx), reads: r, before };
  const res = await fetch(`${rpc}/api/tx`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(tx) });
  const sent = await res.json();
  if (!res.ok) v.refused = sent.error ?? res.statusText;
  else v.result = await settled(sent.id);
  if (!v.refused) {
    await sleep(50);
    v.after = Object.fromEntries(await Promise.all(accounts.map(async (a) => [a, (await balance(a).catch(() => 0n)).toString()])));
  }
  vectors.push(v);
  log(`${v.refused ? "✗" : "✓"} ${name}${v.refused ? `: ${v.refused}` : ""}`);
  return v;
}
/** A submitted tx once it's out of the mempool. */
async function settled(id) {
  for (let i = 0; i < 120; i++) {
    const t = await get(`/tx/${id}`).catch(() => null);
    if (t && t.status !== "pending") return t;
    await sleep(250);
  }
  throw new Error(`${id} is still pending`);
}

const funded = async (name) => {
  const w = Wallet.generate(client);
  await w.faucet();
  if (name) await w.registerName(name);
  return w;
};
const tag = Math.random().toString(36).slice(2, 7);
const nm = (n) => `${n}${tag}`;

const vlad = await funded(nm("vlad"));
const mina = await funded(nm("mina"));
const lucy = await funded(nm("lucy"));
await vlad.faucet();
const fresh = () => Wallet.generate(client).address;
const validator = (await get("/validators")).active[0].validator.address;

// Payments.
await vector("transfer", vlad, { type: "transfer", to: mina.address, amount: "1500000000" }, { watch: [mina.address] });
const opened = fresh();
await vector("transfer opening an account", vlad, { type: "transfer", to: opened, amount: "1000000000" }, { watch: [opened] });
await vector("transfer with a tip", mina, { type: "transfer", to: lucy.address, amount: "250000000" }, { watch: [lucy.address], tip: "0.0001" });
await vector("transfer signed to a name", vlad, { type: "transfer", to: `${nm("lucy")}.boo`, amount: "2000000000" }, { watch: [lucy.address] });

// Names.
await vector("register a name", lucy, { type: "register_name", name: nm("tomb") });
await vector("renew a name", lucy, { type: "renew_name", name: nm("lucy"), years: 1 });
await vector("offer a name", lucy, { type: "transfer_name", name: nm("tomb"), to: mina.address }, { watch: [mina.address] });
await vector("accept a name", mina, { type: "transfer_name", name: nm("tomb"), to: mina.address });
await vector("release a name", mina, { type: "release_name", name: nm("tomb") });

// Staking and governance.
await vector("delegate", vlad, { type: "delegate", validator, amount: "20000000000" });
await vector("undelegate an amount", vlad, { type: "undelegate", validator, shares: "0", amount: "5000000000" });
const deposit = (await get("/params")).params.proposal_deposit;
await vector("propose", vlad, { type: "propose", kind: proposal.paramChange("max_validators", 40), title: `Room for more validators ${tag}`, url: "", deposit });
const prop = (await get("/proposals")).proposals.find((p) => p.title === `Room for more validators ${tag}`);
const voter = await funded();
await vector("vote", voter, { type: "vote", proposal_id: prop.id, aye: true, amount: "60000000000", conviction: 1, proposal_digest: proposalDigest(prop.kind, prop.title, prop.url ?? "") });
await vector("pay more than a vote lock leaves", voter, { type: "transfer", to: mina.address, amount: "50000000000" }, { watch: [mina.address], note: "a vote at conviction 1 locks 60 FANG of 100" });
await vector("stake locked FANG", voter, { type: "delegate", validator, amount: "70000000000" });

// Hunts and the pool.
await vector("fund the pool", mina, { type: "fund_pool", amount: "4000000000" });
const { height } = await client.status();
await vector("create a hunt", vlad, { type: "create_hunt", repo: "jackdishman/blockula", issue: 14, amount: "12000000000", expires_at: height + 200_000, title: "Balance journal endpoint" });
const hunt = (await get("/hunts")).hunts.find((h) => h.creator.address === vlad.address && h.issue === 14);
await vector("fund a hunt", lucy, { type: "fund_hunt", hunt_id: hunt.id, amount: "3000000000" });
await vector("cancel a hunt before it expires", vlad, { type: "cancel_hunt", hunt_id: hunt.id });
await vector("award a hunt as someone else", lucy, { type: "award_hunt", hunt_id: hunt.id, to: lucy.address, pr: 7 });
const winner = fresh();
await vector("award a hunt to a new account", vlad, { type: "award_hunt", hunt_id: hunt.id, to: winner, pr: 7 }, { watch: [winner] });

// Opening an account the sender can't pay the creation fee for.
const poor = await funded();
const left = await balance(poor.address);
await vector("can't pay the creation fee", poor, { type: "transfer", to: fresh(), amount: String(left - 60_000_000n) }, { note: "the amount and fee fit, the 0.1 FANG creation fee doesn't" });

// An agent on a budget: the creation fee counts against it.
const agent = Wallet.generate(client);
await mina.addSessionKey(await agent.keyGrant(mina.address), "claude", { limit: "1", periodBlocks: 1000, expiresInBlocks: 100_000 });
await agent.actAs(mina.address);
await vector("an agent pays within its budget", agent, { type: "transfer", to: lucy.address, amount: "300000000" }, { watch: [lucy.address] });
await vector("an agent opening an account goes over its budget", agent, { type: "transfer", to: fresh(), amount: "600000000" });

// A 2-of-2 multisig: its owner keys only propose and approve.
const coven = await funded();
const other = Wallet.generate(client);
await coven.addOwnerKey(await other.keyGrant(coven.address), "the other witch");
await coven.multisigCreate([coven.keyId, other.keyId], 2);
await vector("a multisig's owner key pays", coven, { type: "transfer", to: mina.address, amount: "1000000000" });
await vector("a multisig's owner key proposes", coven, { type: "multisig_propose", action: { type: "transfer", to: mina.address, amount: "1000000000" } });

// A race: the offer is withdrawn while it's accepted, and one of the two is dropped unrun.
await lucy.registerName(nm("crypt"));
await lucy.transferName(nm("crypt"), mina.address);
const withdraw = await lucy.build({ type: "transfer_name", name: nm("crypt"), to: lucy.address });
const accept = await mina.build({ type: "transfer_name", name: nm("crypt"), to: mina.address });
const ids = await Promise.all([withdraw, accept].map((t) => fetch(`${rpc}/api/tx`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(t) }).then((r) => r.json()).then((x) => x.id)));
let dropped = null;
for (let i = 0; i < 40 && !dropped; i++) {
  await sleep(500);
  for (const id of ids) {
    const t = await get(`/tx/${id}`);
    if (t.status === "failed") dropped = t;
  }
}
if (dropped) {
  vectors.push({ name: "dropped in a race", result: dropped });
  log(`✓ dropped in a race: ${dropped.error}`);
} else log("- the race didn't drop either (both ran in order)");

// One vector a line, so a regenerated file diffs by vector.
process.stdout.write(`{"chain_id": ${JSON.stringify(status.chain_id)}, "made_with": "scripts/explorer-vectors.mjs", "vectors": [\n${vectors.map((v) => JSON.stringify(v)).join(",\n")}\n]}\n`);
