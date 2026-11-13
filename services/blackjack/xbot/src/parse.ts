/**
 * The bot's commands, parsed strictly. A post is a command only if it is nothing but mentions
 * and one command:
 *
 *   post     = ws* mention (ws+ mention)* ws+ command ws*
 *   mention  = "@" 1–15 of [A-Za-z0-9_]          at least one is the bot's handle (any case)
 *   command  = "deal" ws+ amount | "hit" | "stand" | "balance" | "stop"      (any case)
 *   amount   = ("0" | [1-9][0-9]{0,8}) ("." [0-9]{1,9})?                     more than zero
 *   ws       = space | tab | line feed | carriage return
 *
 * Every other post is not a command and gets no answer: words before, between or after, two
 * commands, a handle that only looks like the bot's, any character outside printable ASCII
 * (so no look-alike letters, no zero-width characters, no full-width "＠"), an amount with a
 * sign, an exponent, a comma, a leading zero or more than nine decimals. The leading mentions
 * are the ones X puts in front of a reply; the bot's own may be there more than once.
 *
 * Nothing here reads a post for meaning: the text is a stranger's, and this is the only thing
 * the bot ever does with it.
 */

/** The bot's X handle, without the "@". */
export const BOT_HANDLE = "BlockulaChain";

export const DROPS_PER_FANG = 1_000_000_000n;

export type Command = { kind: "deal"; amount: bigint } | { kind: "hit" } | { kind: "stand" } | { kind: "balance" } | { kind: "stop" };
export type CommandKind = Command["kind"];

/** The longest post the parser looks at: longer than any command with X's most mentions. */
export const MAX_COMMAND_CHARS = 1_000;
/** X puts at most 50 mentions in front of a reply. */
export const MAX_MENTIONS = 50;

const MENTION = /^@[A-Za-z0-9_]{1,15}$/;
const AMOUNT = /^(0|[1-9][0-9]{0,8})(?:\.([0-9]{1,9}))?$/;
/** Printable ASCII and the four kinds of whitespace: a post with anything else isn't a command. */
const PLAIN = /^[\x20-\x7e\t\n\r]*$/;

/** A FANG amount as a command writes it, in drops; `null` unless it's a plain decimal above zero. Whole-number maths only. */
export function parseAmount(s: string): bigint | null {
  const m = AMOUNT.exec(s);
  if (!m) return null;
  const drops = BigInt(m[1]) * DROPS_PER_FANG + BigInt((m[2] ?? "").padEnd(9, "0"));
  return drops > 0n ? drops : null;
}

/** The command a post's text is, or `null`: see the grammar above. */
export function parseCommand(text: unknown, botHandle: string = BOT_HANDLE): Command | null {
  if (typeof text !== "string" || text.length > MAX_COMMAND_CHARS || !PLAIN.test(text)) return null;
  const tokens = text.split(/[ \t\n\r]+/).filter((t) => t !== "");
  const bot = "@" + botHandle.toLowerCase();
  let i = 0;
  let named = false;
  while (i < tokens.length && tokens[i].startsWith("@")) {
    if (!MENTION.test(tokens[i])) return null;
    if (tokens[i].toLowerCase() === bot) named = true;
    i++;
  }
  if (!named || i > MAX_MENTIONS) return null;
  const rest = tokens.slice(i);
  if (rest.length === 0) return null;
  const word = rest[0].toLowerCase();
  if (word === "deal") {
    if (rest.length !== 2) return null;
    const amount = parseAmount(rest[1]);
    return amount === null ? null : { kind: "deal", amount };
  }
  if (rest.length !== 1) return null;
  return word === "hit" || word === "stand" || word === "balance" || word === "stop" ? { kind: word } : null;
}

/** A post as the bot is handed it. Everything in it is a stranger's: data, never instructions. */
export interface Post {
  /** The post's id: a decimal u64. */
  id: string;
  /** Who posted it, without the "@". */
  authorHandle: string;
  authorId?: string;
  text: string;
  /** The post it replies to, if any. */
  inReplyToId?: string | null;
  createdAt: string;
}

const POST_ID = /^[1-9][0-9]{0,19}$/;
const HANDLE = /^[A-Za-z0-9_]{1,15}$/;
/** A reply's target: a post id, or an id this build gave one of its own (unposted) replies. */
const REPLY_TO = /^[A-Za-z0-9-]{1,64}$/;
const U64_MAX = (1n << 64n) - 1n;

/** Whether `s` is a post id: a decimal u64 above zero, no leading zero. */
export const isPostId = (s: unknown): s is string => typeof s === "string" && POST_ID.test(s) && BigInt(s) <= U64_MAX;
/** Whether `s` is an X handle (without the "@"). */
export const isHandle = (s: unknown): s is string => typeof s === "string" && HANDLE.test(s);

/** `value` as a post, or `null` when it isn't shaped like one (a bad id, a handle that isn't one). */
export function checkPost(value: unknown): Post | null {
  if (!value || typeof value !== "object") return null;
  const o = value as Record<string, unknown>;
  if (!isPostId(o.id) || !isHandle(o.authorHandle) || typeof o.text !== "string") return null;
  const reply = o.inReplyToId;
  if (reply !== undefined && reply !== null && !(typeof reply === "string" && REPLY_TO.test(reply))) return null;
  const post: Post = { id: o.id, authorHandle: o.authorHandle, text: o.text, inReplyToId: (reply as string | null | undefined) ?? null, createdAt: typeof o.createdAt === "string" ? o.createdAt : "" };
  if (typeof o.authorId === "string" && /^[0-9]{1,20}$/.test(o.authorId)) post.authorId = o.authorId;
  return post;
}
