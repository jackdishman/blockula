/**
 * What the bot is built over: the table it plays at, the store it remembers in, the check of
 * an X link, the picture of a hand, the place its replies go, and the clock. Each is an
 * interface so the bot runs the same over a real local chain and over the in-memory table the
 * tests use. None of them is an X client: this build has none.
 */
import type { Game, House, Limits, PlayerRecord, TableEvent, TableSnapshot } from "@blockula/blackjack";
import type { CommandKind } from "./parse.js";

/** A relayed transaction that ran and wasn't refused. */
export interface Landed {
  txId: string;
  /** The block it ran in. */
  height: bigint;
  /** The table's events in its receipt. */
  events: TableEvent[];
}

/** What the chain says about a transaction the bot signed. */
export type TxState =
  | { status: "landed"; landed: Landed }
  /** It ran and the table refused it (an abort code), or the chain failed it. */
  | { status: "refused"; code: number | undefined; txId: string }
  /** A node holds it; it hasn't run. */
  | { status: "pending" }
  /** No node the bot asks knows it (or it was dropped unrun): it may still run until its `validUntil`. */
  | { status: "unknown" };

/** The table refused a call: the program's abort code (`ABORTS` in `@blockula/blackjack`). */
export class TableRefused extends Error {
  constructor(
    readonly code: number | undefined,
    readonly txId?: string,
  ) {
    super(`the table refused the call (abort ${code ?? "unknown"})`);
    this.name = "TableRefused";
  }
}

/** Called with a transaction's id and the last block it can run in, after it's signed and before it's submitted. */
export type OnSigned = (txId: string, validUntil: bigint) => void | Promise<void>;

/** An account's X link as its chain record claims it (`client.xLink`): unchecked. */
export interface LinkClaim {
  handle: string;
  post_id: string;
  /** The code the post must contain. */
  code: string;
}

/** The table, as the relayer sees it: reads, and the relayed calls its key may sign. */
export interface TableDriver {
  /** The relayer's own address. */
  readonly relayer: string;
  height(): Promise<bigint>;
  house(): Promise<House>;
  limits(): Promise<Limits>;
  player(account: string): Promise<PlayerRecord>;
  /** An open hand, or `null` once it's settled. */
  game(id: bigint): Promise<Game | null>;
  /** The accounts whose grant names this relayer. */
  granted(): Promise<string[]>;
  /** The account's X link claim, or `null`. */
  link(account: string): Promise<LinkClaim | null>;
  relayBet(player: string, amount: bigint, postId: bigint, onSigned: OnSigned): Promise<Landed>;
  relayHit(game: bigint, postId: bigint, onSigned: OnSigned): Promise<Landed>;
  relayStand(game: bigint, postId: bigint, onSigned: OnSigned): Promise<Landed>;
  relayWithdraw(player: string, postId: bigint, onSigned: OnSigned): Promise<Landed>;
  /** `timeout` on a hand whose dealer missed its deadline. */
  timeout(game: bigint, onSigned: OnSigned): Promise<Landed>;
  tx(id: string): Promise<TxState>;
}

/**
 * Checks a link claim against its proof post. The real check fetches the post from X; this
 * build never does: the tests and the dry run pass a stand-in that reads local posts.
 */
export type VerifyLink = (claim: LinkClaim) => Promise<{ verified: boolean }>;

/** A picture of a hand. `png` is `null` from a renderer that draws nothing (the tests'). */
export type Render = (snapshot: TableSnapshot) => { png: Uint8Array | null; alt: string };

export interface Clock {
  /** Milliseconds since the epoch. */
  now(): number;
  sleep(ms: number): Promise<void>;
}

export const systemClock: Clock = { now: () => Date.now(), sleep: (ms) => new Promise((r) => setTimeout(r, ms)) };

/** What a reply was for. */
export type ReplyAction = CommandKind | "update" | "timeout" | "settled" | "refused";

export interface ReplyMeta {
  /** The post this replies to. */
  inReplyTo: string;
  /** The player's handle: data, shown and never acted on. */
  handle: string;
  account: string | null;
  action: ReplyAction;
  /** Why, for a refusal. */
  reason: string | null;
  gameId: string | null;
  txId: string | null;
  height: string | null;
  /** When the bot wrote it (ISO 8601, from its clock). */
  at: string;
}

export interface Reply {
  /** Which of the post's replies this is, from 0. */
  seq: number;
  text: string;
  /** Set on a reply about a hand. */
  image: { png: Uint8Array | null; alt: string } | null;
  meta: ReplyMeta;
}

/** Where replies go. In this build: files in a directory. Nothing is posted. */
export interface ReplySink {
  /** Writes a reply and returns its id. Writing the same reply again replaces it. */
  write(reply: Reply): string;
}

/** The id this build gives a reply it wrote (and didn't post). */
export const replyId = (inReplyTo: string, seq: number) => `dry-${inReplyTo}-${seq}`;

// ---- the store ----

/** How far a command got. Written before each step that can't be taken back. */
export type PostState =
  /** The bot decided to act; nothing is signed. */
  | "intent"
  /** A transaction is signed (its id is recorded) and is being, or was, submitted. */
  | "sent"
  /** The transaction ran; the reply isn't written yet. */
  | "acted"
  /** Finished: answered, refused or ignored. */
  | "done";

export interface PostRecord {
  id: string;
  /** Lowercase. */
  handle: string;
  /** The handle as its author writes it. */
  shown: string;
  command: CommandKind | "timeout";
  /** A deal's bet, drops. */
  amount: string | null;
  inReplyTo: string | null;
  state: PostState;
  account: string | null;
  gameId: string | null;
  txId: string | null;
  validUntil: string | null;
  height: string | null;
  /** A hit's or a stand's hand before it: the player's cards and the cards dealt. */
  before: { nPlayer: number; dealt: number } | null;
  /** What a `stop` sent back, drops. */
  sent: string | null;
  /** `ok`, `refused:<reason>` or `ignored:<reason>`, once done. */
  result: string | null;
  /** Replies written to this post. */
  replies: number;
  at: number;
}

/** A hand the bot opened and hasn't seen settle. */
export interface OpenHand {
  gameId: string;
  account: string;
  shown: string;
  /** The deal's post, the player's moves and the bot's replies: a move must reply to one of them. */
  thread: string[];
  /** The player's newest post in the thread: where an update goes. */
  lastPost: string;
  /** The dealer owed a card when the bot last looked: `tick` watches it. */
  awaiting: boolean;
}

export interface LinkEntry {
  /** Lowercase; `null` for an account with no link. */
  handle: string | null;
  postId: string | null;
  verified: boolean;
  checkedAt: number;
}

export interface State {
  version: 1;
  /** The chain and table this store's posts were played on, once known: it is never used for another. */
  chain: { chainId: string; genesisHash: string; table: string } | null;
  posts: Record<string, PostRecord>;
  /** Open hands by lowercase handle. */
  hands: Record<string, OpenHand>;
  /** Handles that said `stop` and haven't dealt since. */
  stopped: Record<string, true>;
  rate: {
    /** Times of each handle's commands in the last hour. */
    commands: Record<string, number[]>;
    /** Times of each handle's refusal replies in the last hour. */
    refusals: Record<string, number[]>;
    /** Times of every command in the last minute. */
    global: number[];
  };
  links: {
    /** When the accounts were last listed. */
    listedAt: number;
    /** By account. */
    accounts: Record<string, LinkEntry>;
  };
  /** Accounts an operator enrolled by hand, besides the ones the table lists. */
  enrolled: string[];
}

export const emptyState = (): State => ({ version: 1, chain: null, posts: {}, hands: {}, stopped: {}, rate: { commands: {}, refusals: {}, global: [] }, links: { listedAt: 0, accounts: {} }, enrolled: [] });

/**
 * The bot's memory. `state` is changed in place and `commit` makes it durable: the bot commits
 * before every step that can't be taken back, so a restart knows how far each command got.
 */
export interface Store {
  readonly state: State;
  commit(): void;
}

// ---- configuration ----

export interface BotConfig {
  /** The bot's handle, without the "@". */
  handle: string;
  /** The least time between one handle's commands. */
  cooldownMs: number;
  /** The most commands one handle gets an hour. */
  perHour: number;
  /** The most commands a minute, over everyone. */
  globalPerMinute: number;
  /** The least time between refusal replies to one handle. */
  refusalCooldownMs: number;
  /** The most refusal replies one handle gets an hour. */
  refusalsPerHour: number;
  /** How long a checked link is trusted before it's checked again. */
  linkTtlMs: number;
  /** The least time between two listings of the table's accounts (an unknown handle asks for one). */
  listEveryMs: number;
  /** How long to wait for the dealer's cards before answering "waiting". */
  revealWaitMs: number;
  /** How often to look while waiting. */
  pollMs: number;
  /** Posts older than this are ignored; 0 turns the check off. */
  maxPostAgeMs: number;
  /** Lowercase handles the bot never answers. */
  blocked: string[];
}

export const DEFAULT_CONFIG: BotConfig = {
  handle: "BlockulaChain",
  cooldownMs: 3_000,
  perHour: 60,
  globalPerMinute: 120,
  refusalCooldownMs: 60_000,
  refusalsPerHour: 6,
  linkTtlMs: 10 * 60_000,
  listEveryMs: 30_000,
  revealWaitMs: 20_000,
  pollMs: 1_000,
  maxPostAgeMs: 0,
  blocked: [],
};
