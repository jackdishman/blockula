#!/usr/bin/env node
// A Halloween house bot: knock with a payment, get a trick (nothing back), a treat (your stake
// back) or a jackpot (twice your stake). It is a toy for worthless coins.
//
//   node examples/trick-or-treat-bot.mjs [--name trickortreat] [--rpc URL] [--network local|demo|testnet]
//        [--key FILE] [--faucet] [--stake 1] [--max-plays-per-day 10] [--floor 10]
//        [--state FILE] [--from HEIGHT]
//
// --key FILE: the bot's key file (JSON with a hex `seed` field, as `blockula wallet new` writes). If it doesn't exist it is created (mode 600).
//             Default ./trickortreat-key.json.
// --faucet:   fund the bot from the chain's faucet when it can't afford its name (blockula dev and
//             test chains only).
// --stake:    FANG each play costs, and the unit of every payout (default 1).
// --floor:    FANG the bot always keeps. Below it, it stops paying and says so (default 10).
//
// The house rules, in the order they're applied to each payment to the bot:
//   - A payment from the bot itself, or of less than the stake, isn't a play: it's kept and logged.
//   - A sender gets at most --max-plays-per-day plays per UTC day (by block time). Further
//     payments are kept and logged.
//   - If paying would take the bot's transferable FANG (after the fee) under the floor, the bot
//     keeps the payment and logs that it has stopped paying. Anything above the stake is a tip.
//   - Otherwise it draws: 45% trick (pays 0), 45% treat (1 x stake), 10% jackpot (2 x stake). The
//     expected payout is 0.65 x stake, so the house wins over time and can't be drained by volume.
//
// Safety, as in docs/INTEGRATION.md:
//   - Each incoming tx is handled once, by its tx id, kept in --state next to the block height it
//     resumes from (as receive-payments.mjs does).
//   - The payout is signed once and stored with the play before it's sent. If it comes back
//     pending or unknown, the stored bytes are rebroadcast (never signed again), and the bot
//     won't take another play until every earlier payout is settled (as pay-from-a-script.mjs).
//   - It pays the sending account by address (the tx's `from`), never a name or text in a memo.
//   - The sender's memo is chain text from a stranger: it is never read for meaning, never copied
//     into the bot's memos and never used to pick the outcome. The bot's memos are fixed strings.
import { existsSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { randomBytes, randomInt } from "node:crypto";
import { ApiError, Client, TxFailedError, TxPendingError, Wallet, formatFang, parseFang, txId, watchPayments } from "@blockula/sdk";

const { flags } = parseArgs(process.argv.slice(2));
const name = (flags.name ?? "trickortreat").replace(/\.boo$/, "");
const nameFull = `${name}.boo`;
const rpc = flags.rpc ?? process.env.BLOCKULA_RPC;
// A network pins its chain (id, genesis, prefix): a node on another chain is refused.
const client = flags.network ? new Client(rpc, { network: flags.network }) : new Client(rpc ?? "http://localhost:7070");
const stake = parseFang(flags.stake ?? "1");
const floor = parseFang(flags.floor ?? "10");
const maxPlays = Number(flags["max-plays-per-day"] ?? 10);
if (stake <= 0n || !Number.isSafeInteger(maxPlays) || maxPlays < 1) {
  console.error("--stake must be above 0 and --max-plays-per-day a whole number of at least 1");
  process.exit(2);
}
const keyFile = flags.key ?? "./trickortreat-key.json";
const stateFile = flags.state ?? `./trickortreat-${name}.json`;

// Randomness: the operator's own RNG is trusted. Nothing here is verifiable by a player, and
// nothing needs to be: this is a toy with worthless coins. Don't copy it for anything real.
// (crypto.randomInt is a CSPRNG, so at least a player can't predict the next draw.)
const OUTCOMES = [
  { weight: 45, kind: "trick", multiple: 0n, memos: ["trick! the candy turned to dust", "trick! boo!", "trick! a bat ate your treat"] },
  { weight: 45, kind: "treat", multiple: 1n, memos: ["treat! fun-size, same as you gave", "treat! thanks for knocking", "treat! a sweet for a sweet"] },
  { weight: 10, kind: "jackpot", multiple: 2n, memos: ["JACKPOT! double the candy", "JACKPOT! the full-size bar", "JACKPOT! the vampire's treasure"] },
];
const draw = () => {
  let n = randomInt(100);
  const o = OUTCOMES.find((o) => (n < o.weight ? true : ((n -= o.weight), false)));
  return { kind: o.kind, multiple: o.multiple, memo: o.memos[randomInt(o.memos.length)] };
};

// 1. The bot's key.
let keyHex;
if (existsSync(keyFile)) keyHex = JSON.parse(readFileSync(keyFile, "utf8")).seed;
else {
  keyHex = randomBytes(32).toString("hex");
  writeFileSync(keyFile, JSON.stringify({ ["seed"]: keyHex }), { mode: 0o600 });
  console.log(`A new key, written to ${keyFile}`);
}
const wallet = Wallet.fromSeed(keyHex, client);
await client.addressPrefix();
const me = wallet.address;
wallet.feeHeadroom = 2; // room for the base fee to rise while a payout waits
console.log(`The house is ${nameFull} at ${me} on ${client.rpc}`);

// 2. The name: use it if this key owns it, register it if it's free, stop if someone else has it.
await claimName();

// 3. State: where we got to, and what became of every payment we've seen.
//    handled[txid] = { outcome, from, amount, day, status, payout?, tx?, payout_id? }
//    status: "kept" | "trick" | "paying" | "paid" | "payout_failed" | "payout_expired"
const state = existsSync(stateFile) ? JSON.parse(readFileSync(stateFile, "utf8")) : { next: undefined, handled: {}, plays: {} };
if (flags.from) state.next = Number(flags.from);
const save = () => {
  writeFileSync(`${stateFile}.tmp`, JSON.stringify(state, null, 2));
  renameSync(`${stateFile}.tmp`, stateFile); // never a half-written file
};

const stop = new AbortController();
process.on("SIGINT", () => stop.abort());
let stoppedPaying = false;

// A payout left unsettled by an earlier run (or a node that didn't answer) comes first.
await settleAll();

console.log(`Stake ${formatFang(stake)}, floor ${formatFang(floor)}, at most ${maxPlays} plays a sender a day. ${state.next ? `Resuming from block #${state.next}` : "Watching from the next block"} (Ctrl-C to stop)`);
const next = await watchPayments(client, me, {
  fromHeight: state.next,
  onPayment: play,
  onProgress: (n) => {
    state.next = n;
    save();
  },
  onError: (e) => console.error(`(retrying: ${e.message})`),
  signal: stop.signal,
}).catch((e) => {
  console.error(`Stopped: ${e.message}`);
  process.exitCode = 1;
});
if (next !== undefined) state.next = next;
save();
console.log(`Stopped. Next run resumes from block #${state.next}.`);

async function play(p) {
  if (state.handled[p.id]) return; // already dealt with
  await settleAll(); // never take a play on top of an unsettled payout
  const who = p.from_name ?? p.from; // for the log only; the payout goes to p.from
  const day = new Date(p.timestamp_ms ?? Date.now()).toISOString().slice(0, 10);
  const keep = (why) => {
    state.handled[p.id] = { outcome: "kept", from: p.from, amount: p.amount.toString(), day, status: "kept", why };
    console.log(`  kept ${p.amount_display} from ${who} (tx ${p.id}): ${why}`);
    save();
  };
  if (p.from === me) return keep("it's from the house itself");
  if (p.amount < stake) return keep(`below the stake of ${formatFang(stake)}`);
  const dayKey = `${p.from} ${day}`;
  if ((state.plays[dayKey] ?? 0) >= maxPlays) return keep(`${who} has played ${maxPlays} times today`);

  const o = draw();
  const payout = stake * o.multiple;
  // The play is counted and the draw recorded before anything is sent: a restart can't redraw it.
  state.plays[dayKey] = (state.plays[dayKey] ?? 0) + 1;
  if (payout === 0n) {
    state.handled[p.id] = { outcome: o.kind, from: p.from, amount: p.amount.toString(), day, status: "trick", payout: "0" };
    console.log(`  TRICK  ${who} knocked with ${p.amount_display}: nothing back`);
    save();
    return;
  }
  // The floor: what the bot could still move after this payout and its largest fee.
  const q = await wallet.quote(p.from, payout, { memo: o.memo });
  if (!q.enough || q.transferableAfter < floor) {
    state.plays[dayKey] -= 1;
    if (!stoppedPaying) console.log(`  The house is at its floor (${formatFang(floor)}): it has stopped paying and keeps what it's given until it's topped up.`);
    stoppedPaying = true;
    return keep("the house is at its floor and has stopped paying");
  }
  stoppedPaying = false;
  const tx = await wallet.build({ type: "transfer", to: q.recipient.signed, amount: payout.toString() }, o.memo);
  const payout_id = txId(tx);
  // Store the signed tx before it leaves: whatever happens next is recoverable.
  state.handled[p.id] = { outcome: o.kind, from: p.from, amount: p.amount.toString(), day, status: "paying", payout: payout.toString(), payout_id, tx };
  save();
  console.log(`  ${o.kind.toUpperCase()}  ${who} knocked with ${p.amount_display}: paying ${formatFang(payout)} (tx ${payout_id})`);
  await settle(p.id);
}

/** Every payout still "paying" is settled now, or the bot stops with a message. */
async function settleAll() {
  for (const id of Object.keys(state.handled)) {
    if (state.handled[id].status === "paying") await settle(id);
  }
}

/** Gets one stored payout to a known end by its stored bytes, never by signing it again. */
async function settle(id) {
  const h = state.handled[id];
  const { tx, payout_id } = h;
  const end = (status, line) => {
    h.status = status;
    delete h.tx; // the bytes are no longer needed
    console.log(`  ${line}`);
    save();
  };
  // Does the node already know it?
  const known = await client.tx(payout_id).catch(() => null);
  if (known?.status === "finalized") return end("paid", `paid ${formatFang(h.payout)} to ${h.from} (block #${known.height})`);
  if (known?.status === "failed" && !known.dropped) return end("payout_failed", `payout ${payout_id} failed in a block (its fee was charged; nothing was paid)`);
  try {
    await client.post(tx); // the same bytes: a node answers with the same id, and it runs at most once
  } catch (e) {
    if (e instanceof ApiError && e.status >= 400 && e.status < 500 && e.status !== 408) return unclear(`refused: ${e.message}`);
    return unclear(`unclear: ${e.message}`);
  }
  try {
    const done = await client.waitForTx(payout_id, 30_000, tx.valid_until);
    return end("paid", `paid ${formatFang(h.payout)} to ${h.from} (block #${done.height})`);
  } catch (e) {
    if (e instanceof TxFailedError && !e.dropped) return end("payout_failed", `payout ${payout_id} failed in a block: ${e.reason} (its fee was charged; nothing was paid)`);
    if (e instanceof TxFailedError || e instanceof TxPendingError) return unclear(e.message);
    throw e;
  }

  /** Not known yet: only the account's nonce can say whether it ran, and the bot stops until it's clear. */
  async function unclear(why) {
    let account, status;
    try {
      [account, status] = await Promise.all([client.account(tx.from), client.status()]);
    } catch (e) {
      throw new Error(`payout ${payout_id} to ${h.from} is unsettled (${why}) and the node can't say more (${e.message}). It stays stored; run again later. Never pay it by hand until it's clear.`);
    }
    if (account.next_nonce > tx.nonce) {
      // The nonce is used: this tx ran, or another did. Look the tx up again before deciding.
      const again = await client.tx(payout_id).catch(() => null);
      if (again?.status === "finalized") return end("paid", `paid ${formatFang(h.payout)} to ${h.from} (block #${again.height})`);
      throw new Error(`payout ${payout_id}: nonce ${tx.nonce} is used but this node can't show the tx (${why}). It may have paid ${h.from}: check an archive node's /api/tx/${payout_id} before doing anything else.`);
    }
    if (Number(status.height) > tx.valid_until) {
      return end("payout_expired", `payout ${payout_id} never ran: its nonce is unused and the chain is past block ${tx.valid_until}. ${h.from} is owed ${formatFang(h.payout)}: pay it by hand if you like.`);
    }
    throw new Error(`payout ${payout_id} to ${h.from} is unsettled (${why}); it can still run until block ${tx.valid_until}. Run again to rebroadcast the same bytes.`);
  }
}

/** The house's name: ours, or registered now. */
async function claimName() {
  let info = await client.name(nameFull).catch((e) => (e instanceof ApiError && e.status === 404 ? null : Promise.reject(e)));
  if (info?.status === "available" || !info) {
    if (flags.faucet) {
      const a = await client.account(me).catch(() => null);
      if (!a || BigInt(a.transferable ?? a.balance ?? 0) < parseFang(info?.price_display?.replace(" FANG", "") ?? "5") * 2n) {
        await wallet.faucet();
        console.log("Funded by the faucet");
      }
    }
    const done = await wallet.registerName(nameFull);
    console.log(`Registered ${nameFull}: ${done.summary}`);
    info = await client.name(nameFull);
  }
  const owner = typeof info.owner === "string" ? info.owner : info.owner?.address;
  if (owner !== me) {
    console.error(`${nameFull} belongs to ${owner ?? "someone else"} (${info.status}), not to this key. Pick another --name or use that key.`);
    process.exit(1);
  }
}

function parseArgs(argv) {
  const positional = [];
  const flags = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--faucet") flags[a.slice(2)] = true;
    else if (a.startsWith("--")) flags[a.slice(2)] = argv[++i];
    else positional.push(a);
  }
  return { positional, flags };
}
