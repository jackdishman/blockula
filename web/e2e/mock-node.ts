// A small in-memory Blockula node behind Playwright routes: enough of the HTTP API for the
// wallet's flows (status, accounts, keys, names, faucet, tx submit/lookup). Submitted txs are
// checked like the real node does it: ML-DSA signature over the SDK's signing bytes, the signing
// key authorized on `from` (owner keys for key changes), nonce and balance.
import { formatFang, keyId as keyIdOf, parseFang, signingBytes, type Tx } from "@blockula/sdk";
import { hexToBytes } from "@noble/hashes/utils.js";
import { ml_dsa65 } from "@noble/post-quantum/ml-dsa.js";
import type { BrowserContext, Page, Route } from "@playwright/test";
import { bech32m } from "@scure/base";
import { CHAIN_ID, GENESIS, RPC } from "./env";

type Role = { type: "owner" } | { type: "session"; limit: string; period_blocks: number; expires_at: number };
interface KeyRec { label: string; role: Role; added_at: number }
interface Acct { balance: bigint; nonce: number; keys?: Map<string, KeyRec> }
interface TxRec { id: string; tx: Tx | null; kind: string; from: string; to: string | null; amount: bigint | null; fee: bigint; memo: string; height: number; summary: string }

const FAUCET = addressOf("fa".repeat(32));
const VALIDATOR = addressOf("7a".repeat(32));

export function addressOf(keyIdHex: string) {
  return bech32m.encode("boo", bech32m.toWords(hexToBytes(keyIdHex)));
}

export function keyIdOfAddress(address: string) {
  const { words } = bech32m.decode(address as `${string}1${string}`);
  return Buffer.from(bech32m.fromWords(words)).toString("hex");
}

class HttpError extends Error {
  constructor(readonly status: number, message: string) {
    super(message);
  }
}

export class MockNode {
  chainId = CHAIN_ID;
  genesisHash = GENESIS;
  /** Extra /api/status fields (e.g. a v1 node's address_prefix). */
  statusExtra: Record<string, unknown> = {};
  faucet = true;
  height = 100;
  accounts = new Map<string, Acct>();
  names = new Map<string, string>();
  txs = new Map<string, TxRec>();
  /** Every signed tx accepted, in order. */
  submitted: Tx[] = [];
  /** Every request body the wallet sent to the node, to prove secrets never leave the page. */
  bodies: string[] = [];

  /** Answer the wallet's RPC calls in this page or browser context. */
  async install(target: Page | BrowserContext) {
    await target.route(`${RPC}/api/**`, (route) => this.handle(route));
  }

  acct(address: string): Acct {
    let a = this.accounts.get(address);
    if (!a) this.accounts.set(address, (a = { balance: 0n, nonce: 0 }));
    return a;
  }

  /** The account's keys; an untouched account has just its own ("original") key. */
  keysOf(address: string): Map<string, KeyRec> {
    return this.acct(address).keys ?? new Map([[keyIdOfAddress(address), { label: "original key", role: { type: "owner" }, added_at: 0 }]]);
  }

  credit(address: string, fang: string) {
    this.acct(address).balance += parseFang(fang);
  }

  private label(address: string) {
    const name = [...this.names].find(([, a]) => a === address)?.[0];
    return { address, name: name ?? (address === VALIDATOR ? "🧛 validator" : null), short: `${address.slice(0, 10)}…${address.slice(-4)}` };
  }

  private record(r: Omit<TxRec, "id" | "height">) {
    this.height += 1;
    const id = (this.txs.size + 1).toString(16).padStart(64, "0");
    this.txs.set(id, { ...r, id, height: this.height });
    return id;
  }

  private txView(r: TxRec) {
    return {
      id: r.id,
      summary: r.summary,
      kind: r.kind,
      status: "finalized",
      height: r.height,
      from: this.label(r.from),
      to: r.to ? this.label(r.to) : null,
      amount: r.amount?.toString() ?? null,
      amount_display: r.amount !== null ? formatFang(r.amount) : null,
      fee: r.fee.toString(),
      fee_display: formatFang(r.fee),
      memo: r.memo,
      action: r.tx?.action ?? null,
    };
  }

  private status() {
    return {
      chain_id: this.chainId,
      genesis_hash: this.genesisHash,
      token: "FANG",
      decimals: 9,
      height: this.height,
      block_time_ms: 1000,
      validator: this.label(VALIDATOR),
      faucet: this.faucet,
      supply_display: "1,000,000 FANG",
      burned_display: "0 FANG",
      ...this.statusExtra,
    };
  }

  private resolve(id: string) {
    const q = id.toLowerCase();
    if (q.startsWith("boo1")) return q;
    const name = q.replace(/\.boo$/, "");
    const owner = this.names.get(name);
    if (!owner) throw new HttpError(404, `no one has claimed ${name}.boo yet`);
    return owner;
  }

  private account(id: string) {
    const address = this.resolve(id);
    const a = this.acct(address);
    const keys = [...this.keysOf(address)].map(([key_id, k]) => ({ key_id, label: k.label, role: k.role, added_at: k.added_at, spent_this_period: "0", window_start: 0 }));
    const recent = [...this.txs.values()].filter((t) => t.from === address || t.to === address).reverse().map((t) => this.txView(t));
    return {
      account: this.label(address),
      balance: a.balance.toString(),
      balance_display: formatFang(a.balance),
      nonce: a.nonce,
      next_nonce: a.nonce,
      names: [...this.names].filter(([, o]) => o === address).map(([n]) => `${n}.boo`),
      keys,
      recent_txs: recent,
    };
  }

  private keyAccounts(keyId: string) {
    const accounts = [...this.accounts.keys()]
      .filter((addr) => this.acct(addr).keys?.has(keyId))
      .map((addr) => {
        const k = this.keysOf(addr).get(keyId) as KeyRec;
        return { account: this.label(addr), role: k.role, label: k.label };
      });
    const own = addressOf(keyId);
    if (!accounts.some((a) => a.account.address === own) && this.keysOf(own).has(keyId)) {
      accounts.unshift({ account: this.label(own), role: { type: "owner" }, label: "original key" });
    }
    return { key_id: keyId, accounts };
  }

  private submit(tx: Tx) {
    if (tx.chain_id !== this.chainId) throw new HttpError(400, `wrong chain id ${tx.chain_id}`);
    const pk = hexToBytes(tx.public_key);
    if (!ml_dsa65.verify(hexToBytes(tx.signature), signingBytes(tx), pk)) throw new HttpError(400, "bad signature");
    const signer = keyIdOf(pk);
    const key = this.keysOf(tx.from).get(signer);
    if (!key) throw new HttpError(400, `this key isn't authorized on ${tx.from}`);
    const a = this.acct(tx.from);
    if (tx.nonce !== a.nonce) throw new HttpError(400, `bad nonce: expected ${a.nonce}, got ${tx.nonce}`);
    const fee = BigInt(tx.fee);
    const act = tx.action;
    const amount = act.type === "transfer" ? BigInt(act.amount) : 0n;
    if (a.balance < amount + fee) throw new HttpError(400, `insufficient balance: has ${formatFang(a.balance)}, needs ${formatFang(amount + fee)}`);
    let to: string | null = null;
    let summary: string;
    if (act.type === "add_key" || act.type === "remove_key") {
      if (key.role.type !== "owner") throw new HttpError(400, `only owner keys can ${act.type === "add_key" ? "add" : "remove"} keys`);
      const keys = new Map(this.keysOf(tx.from));
      if (act.type === "add_key") {
        keys.set(act.key_id, { label: act.label, role: act.role, added_at: this.height + 1 });
        summary = `${this.label(tx.from).name ?? this.label(tx.from).short} added key ${act.key_id.slice(0, 8)}… (${act.label}) as ${act.role.type === "owner" ? "an owner" : "a session key"}`;
      } else {
        if (![...keys].some(([id, k]) => id !== act.key_id && k.role.type === "owner")) throw new HttpError(400, "can't remove the last owner key");
        keys.delete(act.key_id);
        summary = `${this.label(tx.from).name ?? this.label(tx.from).short} revoked key ${act.key_id.slice(0, 8)}…`;
      }
      a.keys = keys;
    } else if (act.type === "transfer") {
      to = act.to;
      this.acct(to).balance += amount;
      summary = `${this.label(tx.from).short} sent ${formatFang(amount)} to ${this.label(to).short}`;
    } else if (act.type === "register_name") {
      this.names.set(act.name, tx.from);
      summary = `${this.label(tx.from).short} claimed ${act.name}.boo`;
    } else throw new HttpError(400, `the mock node doesn't do ${act.type}`);
    a.balance -= amount + fee;
    a.nonce += 1;
    this.submitted.push(tx);
    return this.record({ tx, kind: act.type, from: tx.from, to, amount: act.type === "transfer" ? amount : null, fee, memo: tx.memo, summary });
  }

  private async handle(route: Route) {
    const req = route.request();
    const path = decodeURIComponent(new URL(req.url()).pathname.replace(/^\/api/, ""));
    const body = req.postData();
    if (body) this.bodies.push(body);
    const cors = { "access-control-allow-origin": "*", "access-control-allow-headers": "content-type", "access-control-allow-methods": "GET, POST" };
    const reply = (status: number, json: unknown) => route.fulfill({ status, contentType: "application/json", headers: cors, body: JSON.stringify(json) });
    if (req.method() === "OPTIONS") return route.fulfill({ status: 204, headers: cors });
    try {
      let m: RegExpMatchArray | null;
      if (req.method() === "GET" && path === "/status") return reply(200, this.status());
      if (req.method() === "GET" && (m = path.match(/^\/account\/(.+)$/))) return reply(200, this.account(m[1]));
      if (req.method() === "GET" && (m = path.match(/^\/key\/([0-9a-f]{64})$/))) return reply(200, this.keyAccounts(m[1]));
      if (req.method() === "GET" && (m = path.match(/^\/name\/(.+)$/))) {
        const n = m[1].toLowerCase().replace(/\.boo$/, "");
        const owner = this.names.get(n);
        return reply(200, owner ? { name: `${n}.boo`, available: false, owner: this.label(owner) } : { name: `${n}.boo`, available: true, price: parseFang("5").toString(), price_display: "5 FANG" });
      }
      if (req.method() === "GET" && (m = path.match(/^\/tx\/([0-9a-f]{64})$/))) {
        const r = this.txs.get(m[1]);
        return r ? reply(200, this.txView(r)) : reply(404, { error: "unknown tx" });
      }
      if (req.method() === "POST" && path === "/faucet") {
        if (!this.faucet) throw new HttpError(400, "this chain has no faucet");
        const to = (JSON.parse(body ?? "{}") as { to: string }).to;
        this.credit(to, "100");
        return reply(200, { id: this.record({ tx: null, kind: "transfer", from: FAUCET, to, amount: parseFang("100"), fee: 0n, memo: "faucet", summary: `faucet sent 100 FANG to ${this.label(to).short}` }) });
      }
      if (req.method() === "POST" && path === "/tx") return reply(200, { id: this.submit(JSON.parse(body ?? "{}") as Tx) });
      return reply(404, { error: `mock node: no route for ${req.method()} ${path}` });
    } catch (e) {
      return reply(e instanceof HttpError ? e.status : 500, { error: e instanceof Error ? e.message : String(e) });
    }
  }
}
