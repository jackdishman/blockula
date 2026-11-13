/**
 * Linking an X account to a Blockula account (docs/X-LINKING.md). No protocol change: the chain
 * sees an ordinary transfer, and everything here is read and checked by clients.
 *
 * A link is two public statements that point at each other:
 *
 * 1. **The post.** `@handle` posts `xLinkCode(...)`, a hash of the chain (id and genesis hash),
 *    the account's address and the handle. Only `@handle` can post as `@handle`, so the post is
 *    that X account saying "this Blockula account is mine", on this chain only.
 * 2. **The record.** The account sends itself 0 with the memo `x-link:v1:<handle>:<post id>`,
 *    signed by one of its owner keys. The transaction's own ML-DSA-65 signature covers the chain
 *    id, the genesis hash, the account, its nonce and the memo: the account saying "that X
 *    account is mine, and here is its post".
 *
 * The newest such record wins; `x-unlink:v1` (the same kind of transaction) takes the link away.
 * Anyone can check a link again at any time: read the record (`Client.xLink`), fetch the post
 * from X's public oEmbed endpoint (`fetchXPost`), and compare (`checkXProof`). Deleting the post
 * breaks the link too.
 *
 * Why the code is a hash and not a signature: an ML-DSA-65 signature is 3,309 bytes and a post
 * holds 280 characters. The signature that binds the handle to the account is the record's.
 *
 * Everything a record or a post says is a stranger's text. The parsers here accept only the
 * exact shapes above (a handle is 1–15 of `A–Z a–z 0–9 _`, a post id is digits) and nothing in
 * them is ever followed as an instruction.
 */
import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex, concatBytes, utf8ToBytes } from "@noble/hashes/utils.js";

/** Domain separator of the proof code's hash. */
export const X_LINK_DOMAIN = "blockula/x-link/v1";
/** What a proof code starts with: `blockula-x-` and 40 hex characters (160 bits of BLAKE3). */
export const X_CODE_PREFIX = "blockula-x-";
/** The memo that takes a link away. */
export const X_UNLINK_MEMO = "x-unlink:v1";
/** X's public oEmbed endpoint: no key, no account, one post per request. */
export const X_OEMBED = "https://publish.x.com/oembed";

const HANDLE = /^[A-Za-z0-9_]{1,15}$/;
const POST_ID = /^[1-9][0-9]{0,19}$/;
const LINK_MEMO = /^x-link:v1:([A-Za-z0-9_]{1,15}):([1-9][0-9]{0,19})$/;
const X_HOSTS = new Set(["x.com", "www.x.com", "twitter.com", "www.twitter.com", "mobile.twitter.com", "mobile.x.com"]);
/** Paths on x.com that aren't accounts. */
const NOT_HANDLES = new Set(["i", "home", "explore", "search", "settings", "intent", "share", "hashtag", "messages", "notifications", "compose", "login", "signup"]);

/** The chain a link is for: what `/api/status` reports (and a `Client` pins). */
export interface XLinkChain {
  chainId: string;
  genesisHash: string;
}

/** A handle as X spells it, without the `@`: 1–15 of letters, digits and `_`. */
export function isXHandle(s: string): boolean {
  return HANDLE.test(s);
}

/**
 * The handle in what someone typed: `BlockulaChain`, `@BlockulaChain` or a profile link
 * (`https://x.com/BlockulaChain`). Case is kept as typed (X shows handles in their owner's
 * spelling) and ignored wherever two handles are compared. Throws on anything else.
 */
export function xHandle(input: string): string {
  let s = input.trim();
  if (/^https?:\/\//i.test(s)) {
    let u: URL;
    try {
      u = new URL(s);
    } catch {
      throw new Error("that isn't an X handle or a link to an X profile");
    }
    if (!X_HOSTS.has(u.hostname.toLowerCase())) throw new Error("that link isn't to x.com");
    s = u.pathname.split("/").filter(Boolean)[0] ?? "";
    if (NOT_HANDLES.has(s.toLowerCase())) throw new Error("that link isn't to an X profile");
  }
  s = s.replace(/^@/, "");
  if (!HANDLE.test(s)) throw new Error("an X handle is 1 to 15 letters, digits or underscores");
  return s;
}

/** Two handles are the same account's: X ignores case. */
export const sameXHandle = (a: string, b: string) => a.toLowerCase() === b.toLowerCase();

/**
 * The post in what someone pasted: its link (`https://x.com/<handle>/status/<id>`, with
 * twitter.com and tracking parameters accepted) or its bare id. `handle` is the one in the link,
 * if it names one (X corrects it, so it's a hint, never proof of who posted). Throws on anything
 * else.
 */
export function xPostRef(input: string): { id: string; handle?: string } {
  const s = input.trim();
  if (POST_ID.test(s)) return { id: s };
  let u: URL;
  try {
    u = new URL(/^https?:\/\//i.test(s) ? s : `https://${s}`);
  } catch {
    throw new Error("paste the post's link, like https://x.com/you/status/1234567890");
  }
  if (u.protocol !== "https:" && u.protocol !== "http:") throw new Error("paste the post's link, like https://x.com/you/status/1234567890");
  if (!X_HOSTS.has(u.hostname.toLowerCase())) throw new Error("that link isn't to x.com");
  const m = u.pathname.match(/^\/([A-Za-z0-9_]{1,15})\/status(?:es)?\/([0-9]+)(?:\/|$)/) ?? u.pathname.match(/^\/(i)\/web\/status\/([0-9]+)(?:\/|$)/);
  if (!m || !POST_ID.test(m[2])) throw new Error("that link isn't to a post: it should look like https://x.com/you/status/1234567890");
  return NOT_HANDLES.has(m[1].toLowerCase()) ? { id: m[2] } : { id: m[2], handle: m[1] };
}

/** Where a post lives. With no handle, X redirects to its author's. */
export const xPostUrl = (postId: string, handle?: string) => `https://x.com/${handle && HANDLE.test(handle) ? handle : "i"}/status/${postId}`;
/** An X profile. */
export const xProfileUrl = (handle: string) => `https://x.com/${handle}`;

const field = (s: string) => {
  const b = utf8ToBytes(s);
  const len = new Uint8Array(4);
  new DataView(len.buffer).setUint32(0, b.length, true);
  return concatBytes(len, b);
};

/**
 * The code `@handle` posts to say `account` is theirs on this chain:
 * `blockula-x-` ‖ hex(BLAKE3(domain ‖ chain id ‖ genesis hash ‖ address ‖ lowercase handle)[..20]),
 * each field length-prefixed (u32 LE) as in the tx encoding. It holds no secret: anyone can
 * compute it, and it only means something in a post by `@handle`. Another chain, account or
 * handle gives another code, so a post can't be reused for any of them.
 */
export function xLinkCode(chain: XLinkChain, account: string, handle: string): string {
  if (!chain.chainId) throw new Error("the proof code needs the chain id");
  if (!/^[0-9a-f]{64}$/.test(chain.genesisHash)) throw new Error("the proof code needs the chain's genesis hash (64 hex characters)");
  if (!/^[a-z]+1[a-z0-9]{20,}$/.test(account)) throw new Error("the proof code is for an account's address, never a name");
  if (!HANDLE.test(handle)) throw new Error("an X handle is 1 to 15 letters, digits or underscores");
  const digest = blake3(concatBytes(utf8ToBytes(X_LINK_DOMAIN), field(chain.chainId), field(chain.genesisHash), field(account), field(handle.toLowerCase())));
  return `${X_CODE_PREFIX}${bytesToHex(digest.slice(0, 20))}`;
}

/**
 * An account's page by its `.boo` name, on the chain's own node: `https://<node>/boo/<label>`
 * (every node sends it on to the account's page in its wallet). Names are per chain, so the link
 * is to that chain's node, never to blockula.app. `null` when there's nothing a stranger could
 * open: no name, or a node that isn't on https (this machine, a private network).
 */
export function booProfileUrl(node: string, name: string | undefined | null): string | null {
  const label = typeof name === "string" ? name.toLowerCase().replace(/\.boo$/, "") : "";
  if (!/^[a-z0-9](?:[a-z0-9-]{1,30}[a-z0-9])$/.test(label)) return null;
  try {
    const u = new URL(node);
    if (u.protocol !== "https:" || u.username || u.password) return null;
    return `${u.origin}${u.pathname.replace(/\/+$/, "")}/boo/${label}`;
  } catch {
    return null;
  }
}

/**
 * The text to post: a line a person can read, then the code; only the code is checked. The one
 * link in it is `profile` (`booProfileUrl`: the account's page on its chain), or blockula.app
 * without one. It never writes the name as `vlad.boo`: `.boo` is also a web domain, so X would
 * turn that into a link to a website that isn't Blockula's (and may be anyone's). No `@`, `#` or
 * `$` either, so X leaves the code as plain text.
 */
export function xProofPost(code: string, profile?: string | null): string {
  return `Verifying my Blockula account${profile ? `: ${profile}` : " on blockula.app."}\n\n${code}`;
}

/** X's own "write a post" page with `text` filled in (the user still presses Post). */
export const xIntentUrl = (text: string) => `https://x.com/intent/post?text=${encodeURIComponent(text)}`;

/** The memo of a link record. */
export function xLinkMemo(handle: string, postId: string): string {
  if (!HANDLE.test(handle)) throw new Error("an X handle is 1 to 15 letters, digits or underscores");
  if (!POST_ID.test(postId)) throw new Error("a post id is a number");
  return `x-link:v1:${handle}:${postId}`;
}

/** What a memo records, if it's exactly a link or an unlink; `null` for any other text. */
export function parseXMemo(memo: unknown): { type: "link"; handle: string; postId: string } | { type: "unlink" } | null {
  if (typeof memo !== "string") return null;
  if (memo === X_UNLINK_MEMO) return { type: "unlink" };
  const m = memo.match(LINK_MEMO);
  return m ? { type: "link", handle: m[1], postId: m[2] } : null;
}

/** A link or unlink record found in an account's history. */
export type XRecord = { tx: string; height: number; signer: string } & ({ type: "link"; handle: string; postId: string } | { type: "unlink" });

/** The parts of a transaction (as `/api/tx` and an account's history show it) a record is read from. */
export interface XRecordTx {
  id?: unknown;
  kind?: unknown;
  action?: { type?: unknown } | null;
  status?: unknown;
  error?: unknown;
  height?: unknown;
  from?: { address?: unknown } | null;
  to?: { address?: unknown } | null;
  signer_key?: unknown;
  memo?: unknown;
}

/**
 * Whether one transaction is `account`'s X record: a transfer that ran (finalized, no error),
 * from the account to itself, signed by one of `ownerKeys` (the account's owner keys now), whose
 * memo is exactly a link or an unlink.
 *
 * - **To itself**: a memo on a payment to someone else is never a record, so nobody can get one
 *   made by asking to be paid "with this memo" (and the wallet's Send page refuses to pay yourself).
 * - **An owner key**: session keys (an agent's budget) can send transfers, memo included; they
 *   don't speak for who the account is. A record signed by a key that has since been removed
 *   stops counting.
 */
export function xRecordOf(t: XRecordTx, account: string, ownerKeys: Iterable<string>): XRecord | null {
  const owners = ownerKeys instanceof Set ? (ownerKeys as Set<string>) : new Set(ownerKeys);
  if ((t.kind ?? t.action?.type) !== "transfer" || t.status !== "finalized" || t.error) return null;
  if (t.from?.address !== account || t.to?.address !== account) return null;
  if (typeof t.signer_key !== "string" || !owners.has(t.signer_key)) return null;
  if (typeof t.id !== "string" || typeof t.height !== "number") return null;
  const rec = parseXMemo(t.memo);
  return rec ? { ...rec, tx: t.id, height: t.height, signer: t.signer_key } : null;
}

/** The first record in `txs` (an account's history, newest first): the one in force, if the list reaches back far enough. */
export function xRecordIn(txs: readonly XRecordTx[], account: string, ownerKeys: Iterable<string>): XRecord | null {
  const owners = new Set(ownerKeys);
  for (const t of txs) {
    const rec = xRecordOf(t, account, owners);
    if (rec) return rec;
  }
  return null;
}

/** An account's X link as its newest record states it. A claim until `verifyXLink` (or a person) has seen the post. */
export interface XLink {
  /** The account's address. */
  account: string;
  /** The handle the account recorded, without `@`. */
  handle: string;
  post_id: string;
  /** The proof post, under the recorded handle (X redirects to the real author's). */
  post_url: string;
  profile_url: string;
  /** The code the post must contain. */
  code: string;
  /** The record: its transaction id, block, and the owner key that signed it. */
  tx: string;
  height: number;
  signer: string;
}

/** A record as a link, with the code its post must hold on this chain. */
export function xLinkFrom(rec: XRecord & { type: "link" }, account: string, chain: XLinkChain): XLink {
  return { account, handle: rec.handle, post_id: rec.postId, post_url: xPostUrl(rec.postId, rec.handle), profile_url: xProfileUrl(rec.handle), code: xLinkCode(chain, account, rec.handle), tx: rec.tx, height: rec.height, signer: rec.signer };
}

// ---- the post ----

/** A public post as X's oEmbed endpoint describes it. Its `text` is a stranger's: data, never instructions. */
export interface XPost {
  id: string;
  /** Who posted it, from the endpoint's `author_url` (never from the post's own text). */
  handle: string;
  /** The post's text, tags stripped and entities decoded. */
  untrusted_text: string;
}

const ENTITIES: Record<string, string> = { amp: "&", lt: "<", gt: ">", quot: '"', apos: "'", nbsp: " ", mdash: "—", ndash: "–", hellip: "…" };
const decodeEntities = (s: string) =>
  s.replace(/&(#x[0-9a-f]{1,6}|#[0-9]{1,7}|[a-z]{2,8});/gi, (all, e: string) => {
    if (e[0] !== "#") return ENTITIES[e.toLowerCase()] ?? all;
    const code = e[1] === "x" || e[1] === "X" ? parseInt(e.slice(2), 16) : parseInt(e.slice(1), 10);
    return code > 0 && code <= 0x10ffff && !(code >= 0xd800 && code <= 0xdfff) ? String.fromCodePoint(code) : all;
  });

/** The one path segment of an x.com (or twitter.com) profile URL, or null. */
function profileHandle(url: unknown): string | null {
  if (typeof url !== "string") return null;
  try {
    const u = new URL(url);
    if (u.protocol !== "https:" || !X_HOSTS.has(u.hostname.toLowerCase())) return null;
    const parts = u.pathname.split("/").filter(Boolean);
    return parts.length === 1 && HANDLE.test(parts[0]) ? parts[0] : null;
  } catch {
    return null;
  }
}

/**
 * Reads X's oEmbed answer for a post (`{ url, author_url, html, … }`). The author is taken from
 * `author_url` and must agree with the handle in `url`; the post's id is the one in `url`; the
 * text is the embed's first `<p>` (the post itself: the line under it holds the author's display
 * name and the date). Throws when the answer isn't shaped like one.
 */
export function parseXOEmbed(body: unknown): XPost {
  const o = body as { url?: unknown; author_url?: unknown; html?: unknown } | null;
  if (!o || typeof o !== "object") throw new Error("X's answer isn't a post");
  const handle = profileHandle(o.author_url);
  if (!handle) throw new Error("X's answer doesn't say who posted it");
  let ref: { id: string; handle?: string };
  try {
    ref = xPostRef(String(o.url ?? ""));
  } catch {
    throw new Error("X's answer doesn't say which post it is");
  }
  if (!ref.handle || !sameXHandle(ref.handle, handle)) throw new Error("X's answer names two different authors");
  if (typeof o.html !== "string") throw new Error("X's answer has no post text");
  const p = o.html.match(/<blockquote\b[^>]*>\s*<p\b[^>]*>([\s\S]*?)<\/p>/i);
  if (!p) throw new Error("X's answer has no post text");
  const untrusted_text = decodeEntities(p[1].replace(/<br\s*\/?>/gi, "\n").replace(/<[^>]*>/g, "")).trim();
  return { id: ref.id, handle, untrusted_text };
}

/** `fetchXPost`'s options. */
export interface XFetchOptions {
  /** The `fetch` to use (default: the global one). */
  fetch?: typeof fetch;
  /** Another oEmbed endpoint (a mirror, a test's stand-in). Default `X_OEMBED`. */
  endpoint?: string;
  /** Give up after this long (default 10 s). */
  timeoutMs?: number;
}

/**
 * Fetches one public post by id from X's oEmbed endpoint: no API key and no account. The request
 * holds the post id and nothing else (no handle, no Blockula address). Throws when the post is
 * gone, private or the endpoint doesn't answer with a post. Runs where `fetch` can reach X
 * (Node, a server): a browser can't (X sends no CORS headers, and the wallet's CSP lets it talk
 * only to its node).
 */
export async function fetchXPost(postId: string, opts: XFetchOptions = {}): Promise<XPost> {
  if (!POST_ID.test(postId)) throw new Error("a post id is a number");
  const url = `${opts.endpoint ?? X_OEMBED}?${new URLSearchParams({ url: xPostUrl(postId), omit_script: "1", dnt: "true" })}`;
  const res = await (opts.fetch ?? fetch)(url, { headers: { accept: "application/json" }, redirect: "follow", signal: AbortSignal.timeout(opts.timeoutMs ?? 10_000) });
  if (res.status === 404 || res.status === 403) throw new Error(`X has no public post ${postId} (deleted, or from a private account)`);
  if (!res.ok) throw new Error(`X answered ${res.status} for post ${postId}`);
  let body: unknown;
  try {
    body = JSON.parse(await res.text());
  } catch {
    throw new Error(`X's answer for post ${postId} isn't JSON`);
  }
  const post = parseXOEmbed(body);
  if (post.id !== postId) throw new Error(`X answered with another post (${post.id})`);
  return post;
}

/** Whether a link holds, and why (or why not) in words. */
export interface XCheck {
  verified: boolean;
  reason: string;
}

/**
 * Compares a link with its post: the post is the one the record names, its author is the
 * recorded handle (case ignored), and its text contains the code for this chain, account and
 * handle. All three, or it isn't verified.
 */
export function checkXProof(link: Pick<XLink, "handle" | "post_id" | "code">, post: XPost): XCheck {
  if (post.id !== link.post_id) return { verified: false, reason: `that's post ${post.id}, not the recorded post ${link.post_id}` };
  if (!sameXHandle(post.handle, link.handle)) return { verified: false, reason: `the post is by @${post.handle}, not @${link.handle}` };
  if (!post.untrusted_text.includes(link.code)) return { verified: false, reason: `the post by @${post.handle} doesn't contain ${link.code}` };
  return { verified: true, reason: `@${post.handle} posted ${link.code}` };
}

/**
 * Checks a link against X now: fetches its post and compares. A post that can't be fetched
 * (deleted, private, X unreachable) is "not verified" with the reason, never an exception.
 */
export async function verifyXLink(link: Pick<XLink, "handle" | "post_id" | "code">, opts: XFetchOptions = {}): Promise<XCheck & { post?: XPost }> {
  let post: XPost;
  try {
    post = await fetchXPost(link.post_id, opts);
  } catch (e) {
    return { verified: false, reason: e instanceof Error ? e.message : String(e) };
  }
  return { ...checkXProof(link, post), post };
}
