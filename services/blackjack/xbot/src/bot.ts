/**
 * The bot: one post in, at most a few replies out, and at most one relayed transaction.
 *
 * It acts only on the strict commands of `parse.ts`, for the account whose checked X link
 * names the post's author (`links.ts`), within the grant that account gave this relayer at the
 * table. Post text, handles and chain text are data: nothing else in them is ever read.
 *
 * **Exactly once.** A post is recorded by its id before anything that can't be taken back:
 *
 *   intent  → the bot decided to act (committed before a transaction is signed)
 *   sent    → a transaction is signed and its id recorded (committed before it's submitted)
 *   acted   → it ran (committed before the reply is written)
 *   done    → answered, refused or ignored
 *
 * A post already recorded is never handled again. After a restart `recover` finishes whatever
 * isn't done, from the chain: a signed transaction is looked up by its id; if no node knows
 * it, the table's own state says whether it ran (the open hand carries the post's id, the last
 * hand's seed is the post's, the hand moved on); and it is given up only once its last valid
 * block has passed, so it can never run later. An intent with nothing signed was never
 * submitted and is handled afresh. A reply is a folder named for its post and its number, so
 * writing it twice gives the same folder.
 */
import { Phase, dealers, relaySeed, snapshotOfGame, snapshotOfSettled, verifyHand, type Game, type LastHand, type PlayerRecord, type TableSnapshot } from "@blockula/blackjack";
import { balanceText, endedText, refusalText, settledText, stopText, turnText, waitingText, type Refusal } from "./copy.js";
import { Links } from "./links.js";
import { checkPost, parseCommand, type Command } from "./parse.js";
import type { Written } from "./sink.js";
import { DEFAULT_CONFIG, TableRefused, type BotConfig, type Clock, type Landed, type OnSigned, type OpenHand, type PostRecord, type Render, type Reply, type ReplyAction, type ReplySink, type Store, type TableDriver, type VerifyLink } from "./types.js";

export interface BotDeps {
  table: TableDriver;
  store: Store;
  verifyLink: VerifyLink;
  render: Render;
  sink: ReplySink;
  clock: Clock;
  config?: Partial<BotConfig>;
}

/** What handling a post came to: `ok`, `refused:<reason>`, `ignored:<reason>`, and the replies written. */
export interface Handled {
  result: string;
  replies: Written[];
}

/** A hand as the bot found it after a move. */
type HandState = { kind: "turn"; game: Game } | { kind: "waiting"; game: Game } | { kind: "settled"; last: LastHand | null; cards: { player: number[]; dealer: number[] } | null; balance: bigint };

/** What the chain's state alone says about an action that ran: enough to go on without its receipt. */
type Found = { gameId?: string };

const HOUR = 3_600_000;
const MINUTE = 60_000;

export class Bot {
  readonly config: BotConfig;
  readonly links: Links;
  private readonly table: TableDriver;
  private readonly store: Store;
  private readonly render: Render;
  private readonly sink: ReplySink;
  private readonly clock: Clock;
  private queue: Promise<unknown> = Promise.resolve();
  private written: Written[] = [];

  constructor(deps: BotDeps) {
    this.config = { ...DEFAULT_CONFIG, ...deps.config };
    this.table = deps.table;
    this.store = deps.store;
    this.render = deps.render;
    this.sink = deps.sink;
    this.clock = deps.clock;
    this.links = new Links(deps.table, deps.store, deps.verifyLink, deps.clock, this.config);
  }

  /** One thing at a time: posts, ticks and recovery never interleave. */
  private serial<T>(f: () => Promise<T>): Promise<T> {
    const run = this.queue.then(f, f);
    this.queue = run.catch(() => undefined);
    return run;
  }

  private take(): Written[] {
    const out = this.written;
    this.written = [];
    return out;
  }

  /** Handles one post. Anything that isn't a command, and any post seen before, is ignored without a reply. */
  handle(post: unknown): Promise<Handled> {
    return this.serial(() => this.handleNow(post));
  }

  /** Looks at every open hand: answers when the dealer's cards are out, and calls `timeout` on a hand whose dealer missed its deadline. */
  tick(): Promise<Written[]> {
    return this.serial(async () => {
      for (const h of Object.keys(this.store.state.hands).sort()) await this.look(h);
      return this.take();
    });
  }

  /** Open hands that wait on the dealer. */
  waiting(): number {
    return Object.values(this.store.state.hands).filter((h) => h.awaiting).length;
  }

  /** Finishes every post a previous run left unfinished. Call it before the first post. */
  recover(): Promise<Written[]> {
    return this.serial(async () => {
      await this.recoverNow();
      return this.take();
    });
  }

  // ---- a post ----

  private async handleNow(post: unknown): Promise<Handled> {
    const ignored = (reason: string): Handled => ({ result: `ignored:${reason}`, replies: this.take() });
    const p = checkPost(post);
    if (!p) return ignored("not a post");
    const h = p.authorHandle.toLowerCase();
    if (h === this.config.handle.toLowerCase()) return ignored("own post");
    const cmd = parseCommand(p.text, this.config.handle);
    if (!cmd) return ignored("not a command");
    if (this.config.blocked.includes(h)) return ignored("blocked");
    const s = this.store.state;
    if (s.posts[p.id]) return ignored("duplicate");
    const now = this.clock.now();
    if (this.config.maxPostAgeMs > 0 && !(now - Date.parse(p.createdAt) <= this.config.maxPostAgeMs)) return ignored("old");

    const rec: PostRecord = { id: p.id, handle: h, shown: p.authorHandle, command: cmd.kind, amount: cmd.kind === "deal" ? String(cmd.amount) : null, inReplyTo: p.inReplyToId ?? null, state: "done", account: null, gameId: null, txId: null, validUntil: null, height: null, before: null, sent: null, result: null, replies: 0, at: now };
    // A move under someone else's hand is that player's to make: no answer.
    if ((cmd.kind === "hit" || cmd.kind === "stand") && rec.inReplyTo !== null) {
      const owner = Object.keys(s.hands).find((k) => s.hands[k].thread.includes(rec.inReplyTo as string));
      if (owner !== undefined && owner !== h) return this.skip(rec, "not the hand's player");
    }
    if (s.stopped[h] && cmd.kind !== "deal") return this.skip(rec, "stopped");
    if (!this.allow(h, now)) return this.skip(rec, "rate");
    try {
      await this.dispatch(rec, cmd);
    } catch (e) {
      // Once a post is recorded as begun it is finished from the chain, never begun again.
      const begun = s.posts[rec.id] !== undefined && s.posts[rec.id].state !== "done";
      if (!begun) throw e;
      await this.recoverNow();
    }
    return { result: s.posts[p.id]?.result ?? "ignored:failed", replies: this.take() };
  }

  /** Records a post as seen and unanswered. */
  private skip(rec: PostRecord, reason: string): Handled {
    rec.state = "done";
    rec.result = `ignored:${reason}`;
    this.store.state.posts[rec.id] = rec;
    this.store.commit();
    return { result: rec.result, replies: this.take() };
  }

  /** Whether `h` may have a command handled now: the cooldown, the hour's count, and everyone's minute. Counts it if so. */
  private allow(h: string, now: number): boolean {
    const r = this.store.state.rate;
    r.global = r.global.filter((t) => now - t < MINUTE);
    if (Object.keys(r.commands).length > 1_000) for (const k of Object.keys(r.commands)) if (!r.commands[k].some((t) => now - t < HOUR)) delete r.commands[k];
    const mine = (r.commands[h] ?? []).filter((t) => now - t < HOUR);
    const ok = r.global.length < this.config.globalPerMinute && mine.length < this.config.perHour && (mine.length === 0 || now - mine[mine.length - 1] >= this.config.cooldownMs);
    if (ok) {
      mine.push(now);
      r.global.push(now);
    }
    if (mine.length > 0) r.commands[h] = mine;
    else delete r.commands[h];
    return ok;
  }

  /**
   * Ends a post with a refusal. The reply is written unless this handle got one within
   * `refusalCooldownMs`, or `refusalsPerHour` this hour, or said `stop` (a stopped player hears
   * nothing until a deal goes through): the bot can't be made to post over and over.
   */
  private refuse(rec: PostRecord, r: Refusal): void {
    const s = this.store.state;
    const now = this.clock.now();
    rec.state = "done";
    rec.result = `refused:${r.reason}${r.reason === "table" ? `:${r.code ?? "unknown"}` : ""}`;
    s.posts[rec.id] = rec;
    const times = (s.rate.refusals[rec.handle] ?? []).filter((t) => now - t < HOUR);
    const quiet = (s.stopped[rec.handle] === true && rec.command === "deal") || times.length >= this.config.refusalsPerHour || (times.length > 0 && now - times[times.length - 1] < this.config.refusalCooldownMs);
    if (!quiet) {
      times.push(now);
      this.reply(rec, "refused", refusalText(r), null, { reason: r.reason, gameId: "gameId" in r ? String(r.gameId) : rec.gameId });
    }
    if (times.length > 0) s.rate.refusals[rec.handle] = times;
    else delete s.rate.refusals[rec.handle];
    this.store.commit();
  }

  /** Writes a reply to `rec`'s post and returns its id. The caller commits. */
  private reply(rec: PostRecord, action: ReplyAction, text: string, image: Reply["image"], extra: { reason?: string | null; gameId?: string | null; txId?: string | null; height?: string | null } = {}): string {
    const reply: Reply = {
      seq: rec.replies,
      text,
      image,
      meta: { inReplyTo: rec.id, handle: rec.shown, account: rec.account, action, reason: extra.reason ?? null, gameId: extra.gameId ?? rec.gameId, txId: extra.txId ?? rec.txId, height: extra.height ?? rec.height, at: new Date(this.clock.now()).toISOString() },
    };
    const id = this.sink.write(reply);
    rec.replies++;
    this.written.push({ ...reply, id });
    return id;
  }

  private finish(rec: PostRecord, result = "ok"): void {
    rec.state = "done";
    rec.result = result;
    this.store.state.posts[rec.id] = rec;
    this.store.commit();
  }

  /** Why the bot can't sign for this player now, or `null`. */
  private grantRefusal(pr: PlayerRecord, height: bigint): Refusal | null {
    if (!pr.grant || pr.grant.relayer !== this.table.relayer) return { reason: "no_grant" };
    // The call runs in a later block than the one read, so the grant's last block is already too late.
    if (height >= pr.grant.expiresAt) return { reason: "expired" };
    return null;
  }

  private async dispatch(rec: PostRecord, cmd: Command): Promise<void> {
    const s = this.store.state;
    // Bring this player's open hand up to date first: it may have ended since the bot last looked.
    if (s.hands[rec.handle]) await this.look(rec.handle);
    const link = await this.links.resolve(rec.handle);
    if (link.kind !== "ok") return this.refuse(rec, { reason: link.kind === "none" ? "no_link" : link.kind });
    const account = link.account;
    rec.account = account;
    switch (cmd.kind) {
      case "deal":
        return this.deal(rec, account, cmd.amount);
      case "hit":
      case "stand":
        return this.move(rec, account);
      case "balance":
        return this.balance(rec, account);
      case "stop":
        return this.stop(rec, account);
    }
  }

  private async deal(rec: PostRecord, account: string, amount: bigint): Promise<void> {
    const [house, limits, pr, height] = [await this.table.house(), await this.table.limits(), await this.table.player(account), await this.table.height()];
    const refusal = this.grantRefusal(pr, height);
    if (refusal) return this.refuse(rec, refusal);
    const grant = pr.grant!;
    if (house.paused) return this.refuse(rec, { reason: "paused" });
    if (pr.openGame !== null) return this.refuse(rec, { reason: "hand_open", gameId: pr.openGame, here: this.store.state.hands[rec.handle]?.gameId === String(pr.openGame) });
    const max = [limits.maxBet, limits.relayMaxBet, grant.maxBet].reduce((a, b) => (b < a ? b : a));
    if (amount < limits.minBet || amount > max) return this.refuse(rec, { reason: "limits", min: limits.minBet, max });
    const left = leftToday(pr, limits.dailyCap, height / limits.dayBlocks);
    if (amount > left) return this.refuse(rec, { reason: "daily", left });
    if (amount > pr.balance) return this.refuse(rec, { reason: "balance", balance: pr.balance });
    this.begin(rec);
    await this.send(rec);
  }

  private async move(rec: PostRecord, account: string): Promise<void> {
    const hand = this.store.state.hands[rec.handle];
    if (!hand || hand.account !== account) return this.refuse(rec, { reason: "no_hand" });
    if (rec.inReplyTo === null || !hand.thread.includes(rec.inReplyTo)) return this.refuse(rec, { reason: "thread" });
    const id = BigInt(hand.gameId);
    rec.gameId = hand.gameId;
    let game = await this.table.game(id);
    if (game && dealers(game.phase)) {
      // The dealer owes a card: the move waits for it, a little.
      const state = await this.awaitDealer(id, account);
      if (state.kind === "waiting") {
        hand.awaiting = true;
        return this.refuse(rec, { reason: "waiting" });
      }
      game = state.kind === "turn" ? state.game : null;
    }
    // Over already (the next `tick` answers with how it ended).
    if (!game) return this.refuse(rec, { reason: "table", code: 54 });
    const refusal = this.grantRefusal(await this.table.player(account), await this.table.height());
    if (refusal) return this.refuse(rec, refusal);
    rec.before = { nPlayer: game.playerCards.length, dealt: game.dealt };
    this.begin(rec);
    await this.send(rec);
  }

  private async balance(rec: PostRecord, account: string): Promise<void> {
    const [limits, pr, height] = [await this.table.limits(), await this.table.player(account), await this.table.height()];
    const live = this.grantRefusal(pr, height) === null;
    const grant = live ? { leftToday: leftToday(pr, limits.dailyCap, height / limits.dayBlocks), maxBet: [limits.maxBet, limits.relayMaxBet, pr.grant!.maxBet].reduce((a, b) => (b < a ? b : a)), blocksLeft: pr.grant!.expiresAt - height } : null;
    this.reply(rec, "balance", balanceText({ balance: pr.balance, grant, open: pr.openGame !== null }), null);
    this.finish(rec);
  }

  private async stop(rec: PostRecord, account: string): Promise<void> {
    const s = this.store.state;
    const [pr, height] = [await this.table.player(account), await this.table.height()];
    if (s.hands[rec.handle] || pr.openGame !== null) return this.refuse(rec, { reason: "finish_first" });
    // With nothing to send, or no grant to send it with, the player is still marked stopped.
    const cant: Refusal | null = pr.balance === 0n ? { reason: "empty" } : this.grantRefusal(pr, height) ? { reason: "stop_no_grant" } : null;
    if (cant) {
      this.refuse(rec, cant);
      s.stopped[rec.handle] = true;
      this.store.commit();
      return;
    }
    this.begin(rec);
    await this.send(rec);
  }

  // ---- a transaction ----

  /** Records the intent: from here the post is finished from the chain, never started over blindly. */
  private begin(rec: PostRecord): void {
    rec.state = "intent";
    rec.txId = null;
    rec.validUntil = null;
    this.store.state.posts[rec.id] = rec;
    this.store.commit();
  }

  /** The relayed call a record stands for. */
  private call(rec: PostRecord, onSigned: OnSigned): Promise<Landed> {
    switch (rec.command) {
      case "deal":
        return this.table.relayBet(rec.account as string, BigInt(rec.amount as string), BigInt(rec.id), onSigned);
      case "hit":
        return this.table.relayHit(BigInt(rec.gameId as string), BigInt(rec.id), onSigned);
      case "stand":
        return this.table.relayStand(BigInt(rec.gameId as string), BigInt(rec.id), onSigned);
      case "stop":
        return this.table.relayWithdraw(rec.account as string, BigInt(rec.id), onSigned);
      case "timeout":
        return this.table.timeout(BigInt(rec.gameId as string), onSigned);
      case "balance":
        throw new Error("a balance signs nothing");
    }
  }

  /** Signs and submits `rec`'s call (its id is committed before it's submitted), then answers. */
  private async send(rec: PostRecord): Promise<void> {
    let landed: Landed;
    try {
      landed = await this.call(rec, (txId, validUntil) => {
        rec.txId = txId;
        rec.validUntil = String(validUntil);
        rec.state = "sent";
        this.store.commit();
      });
    } catch (e) {
      if (e instanceof TableRefused) return this.refuse(rec, { reason: "table", code: e.code });
      throw e;
    }
    this.landed(rec, landed);
    await this.report(rec);
  }

  /** The call ran: what that changes, committed before any reply. `landed` is its receipt, or what the table's state shows without one. */
  private landed(rec: PostRecord, landed: Landed | Found): void {
    const s = this.store.state;
    const receipt = "events" in landed ? landed : null;
    if (receipt) {
      rec.txId = receipt.txId;
      rec.height = String(receipt.height);
    }
    rec.state = "acted";
    switch (rec.command) {
      case "deal": {
        let gameId = receipt ? undefined : (landed as Found).gameId;
        if (receipt) {
          // The table's own `relayed` event names the post and the hand it opened.
          const ev = receipt.events.find((e) => e.name === "relayed" && e.action === "bet" && e.postId === BigInt(rec.id) && e.player === rec.account);
          if (!ev || ev.name !== "relayed") throw new Error(`transaction ${receipt.txId} ran without a relayed bet for post ${rec.id}`);
          gameId = String(ev.game);
        }
        rec.gameId = gameId as string;
        s.hands[rec.handle] = { gameId: rec.gameId, account: rec.account as string, shown: rec.shown, thread: [rec.id], lastPost: rec.id, awaiting: true };
        // A deal that went through is how a stopped player comes back.
        delete s.stopped[rec.handle];
        break;
      }
      case "hit":
      case "stand": {
        const hand = s.hands[rec.handle];
        if (hand && hand.gameId === rec.gameId) {
          if (!hand.thread.includes(rec.id)) hand.thread.push(rec.id);
          hand.lastPost = rec.id;
          hand.awaiting = true;
        }
        break;
      }
      case "stop": {
        const ev = receipt?.events.find((e) => e.name === "withdraw" && e.player === rec.account);
        rec.sent = ev && ev.name === "withdraw" ? String(ev.amount) : null;
        s.stopped[rec.handle] = true;
        break;
      }
      default:
        break;
    }
    s.posts[rec.id] = rec;
    this.store.commit();
  }

  /** Answers a post whose call ran. */
  private async report(rec: PostRecord): Promise<void> {
    const s = this.store.state;
    if (rec.command === "stop") {
      this.reply(rec, "stop", stopText(rec.sent === null ? null : BigInt(rec.sent)), null);
      return this.finish(rec);
    }
    if (rec.command === "deal" || rec.command === "hit" || rec.command === "stand") {
      const hand = s.hands[rec.handle];
      if (hand && hand.gameId === rec.gameId) {
        const state = await this.awaitDealer(BigInt(hand.gameId), hand.account);
        this.show(rec, hand, state, rec.command, rec.command === "deal");
      }
    }
    this.finish(rec);
  }

  // ---- a hand ----

  /** Waits (at most `revealWaitMs`) until the dealer owes nothing on hand `id`. */
  private async awaitDealer(id: bigint, account: string): Promise<HandState> {
    const until = this.clock.now() + this.config.revealWaitMs;
    for (;;) {
      const game = await this.table.game(id);
      if (!game) return this.settled(id, account);
      if (!dealers(game.phase)) return { kind: "turn", game };
      if (this.clock.now() >= until) return { kind: "waiting", game };
      await this.clock.sleep(this.config.pollMs);
    }
  }

  /** A settled hand from the player's record, its cards recomputed from the links (`verifyHand`): nothing is taken from the node's reading of them. */
  private async settled(id: bigint, account: string): Promise<HandState & { kind: "settled" }> {
    const pr = await this.table.player(account);
    const last = pr.last && pr.last.game === id ? pr.last : null;
    let cards: { player: number[]; dealer: number[] } | null = null;
    if (last) {
      try {
        const v = verifyHand({ game: id, clientSeed: last.clientSeed, link: last.link, dealt: last.dealt, nPlayer: last.nPlayer, outcome: last.outcome, stake: last.stake, payout: last.payout });
        cards = { player: v.player, dealer: v.dealer };
      } catch {
        cards = null;
      }
    }
    return { kind: "settled", last, cards, balance: pr.balance };
  }

  /** Writes the reply that shows a hand: its text, its picture and the picture's alt text. A settled hand leaves the store. */
  private show(rec: PostRecord, hand: OpenHand, state: HandState, action: ReplyAction, opened: boolean, tx: { txId?: string | null; height?: string | null } = {}): void {
    const extra = { txId: tx.txId ?? rec.txId ?? undefined, player: `@${hand.shown}` };
    const meta = { gameId: hand.gameId, txId: tx.txId ?? rec.txId, height: tx.height ?? rec.height };
    const picture = (snapshot: TableSnapshot) => this.render(snapshot);
    if (state.kind === "settled") {
      if (state.last && state.cards) {
        const text = settledText({ gameId: state.last.game, stake: state.last.stake, playerCards: state.cards.player, dealerCards: state.cards.dealer, outcome: state.last.outcome, payout: state.last.payout, balance: state.balance, holeHidden: state.cards.dealer.length < 2 });
        this.reply(rec, action, text, picture(snapshotOfSettled(state.last, state.cards, extra)), meta);
      } else {
        this.reply(rec, action, endedText(BigInt(hand.gameId), state.balance), null, meta);
      }
      delete this.store.state.hands[rec.handle];
      return;
    }
    const view = { gameId: state.game.id, stake: state.game.stake, playerCards: state.game.playerCards, dealerCards: state.game.dealerCards };
    hand.awaiting = state.kind === "waiting";
    const id = this.reply(rec, action, state.kind === "turn" ? turnText(view, opened) : waitingText(view, opened), picture(snapshotOfGame(state.game, extra)), meta);
    // The bot's reply about the hand is part of the hand's thread: the player's next move may answer it.
    if (!hand.thread.includes(id)) hand.thread.push(id);
  }

  /** Looks at `h`'s open hand: answers if it moved on without a reply, and times out a dealer past its deadline. */
  private async look(h: string): Promise<void> {
    const s = this.store.state;
    const hand = s.hands[h];
    if (!hand) return;
    const rec = s.posts[hand.lastPost];
    if (!rec || rec.state !== "done") return;
    const id = BigInt(hand.gameId);
    let game = await this.table.game(id);
    if (game && dealers(game.phase)) {
      if ((await this.table.height()) <= game.deadline) {
        if (!hand.awaiting) {
          hand.awaiting = true;
          this.store.commit();
        }
        return;
      }
      await this.timeout(h, hand);
      game = await this.table.game(id);
      if (game) return;
    }
    if (!game) {
      // The bot's own timeout ended it, in this run or the one before: its record says so.
      const timedOut = s.posts[`timeout-${hand.gameId}`]?.state === "done" ? s.posts[`timeout-${hand.gameId}`] : null;
      const state = await this.settled(id, hand.account);
      this.show(rec, hand, state, timedOut ? "timeout" : hand.awaiting ? "update" : "settled", false, timedOut ? { txId: timedOut.txId, height: timedOut.height } : {});
      this.store.commit();
      return;
    }
    if (hand.awaiting) {
      this.show(rec, hand, { kind: "turn", game }, "update", false);
      this.store.commit();
    }
  }

  /** Calls `timeout` on a hand whose dealer missed its deadline: the player is paid. Recorded like a post's call, under the hand's id. */
  private async timeout(h: string, hand: OpenHand): Promise<PostRecord | null> {
    const s = this.store.state;
    const rec: PostRecord = { id: `timeout-${hand.gameId}`, handle: h, shown: hand.shown, command: "timeout", amount: null, inReplyTo: null, state: "intent", account: hand.account, gameId: hand.gameId, txId: null, validUntil: null, height: null, before: null, sent: null, result: null, replies: 0, at: this.clock.now() };
    this.begin(rec);
    try {
      const landed = await this.call(rec, (txId, validUntil) => {
        rec.txId = txId;
        rec.validUntil = String(validUntil);
        rec.state = "sent";
        this.store.commit();
      });
      this.landed(rec, landed);
      this.finish(rec);
      return rec;
    } catch (e) {
      if (!(e instanceof TableRefused)) throw e;
      // Not past the deadline after all, or over already: the next look decides.
      delete s.posts[rec.id];
      this.store.commit();
      return null;
    }
  }

  // ---- after a restart ----

  private async recoverNow(): Promise<void> {
    const s = this.store.state;
    const open = Object.values(s.posts)
      .filter((r) => r.state !== "done")
      .sort((a, b) => a.at - b.at || (a.id < b.id ? -1 : 1));
    for (const rec of open) {
      if (rec.state === "acted") {
        await this.report(rec);
        continue;
      }
      let landed: Landed | Found | null;
      try {
        // An intent has no transaction: none was submitted. The table is asked all the same.
        landed = rec.state === "sent" ? await this.resolve(rec) : await this.found(rec);
      } catch (e) {
        if (!(e instanceof TableRefused)) throw e;
        if (rec.command === "timeout") {
          delete s.posts[rec.id];
          this.store.commit();
        } else this.refuse(rec, { reason: "table", code: e.code });
        continue;
      }
      if (landed) {
        this.landed(rec, landed);
        if (rec.command === "timeout") this.finish(rec);
        else await this.report(rec);
      } else if (rec.command === "timeout") {
        delete s.posts[rec.id];
        this.store.commit();
      } else if (rec.state === "sent") {
        // Signed, never ran, and past its last valid block: it can't run now.
        this.refuse(rec, { reason: "not_sent" });
      } else {
        await this.dispatch(rec, rec.command === "deal" ? { kind: "deal", amount: BigInt(rec.amount as string) } : ({ kind: rec.command } as Command));
      }
    }
  }

  /**
   * What became of a signed transaction: its receipt, what the table's state shows, or `null`
   * once it can never run. Waits while it still could. Throws `TableRefused` if it ran and was refused.
   */
  private async resolve(rec: PostRecord): Promise<Landed | Found | null> {
    for (;;) {
      // Read the height first: a transaction not seen after its last valid block has passed never ran.
      const expired = (await this.table.height()) > BigInt(rec.validUntil as string);
      const tx = await this.table.tx(rec.txId as string);
      if (tx.status === "landed") return tx.landed;
      if (tx.status === "refused") throw new TableRefused(tx.code, tx.txId);
      const found = await this.found(rec);
      if (found) return found;
      if (expired) return null;
      await this.clock.sleep(this.config.pollMs);
    }
  }

  /** Whether the table's own state shows `rec`'s call ran. */
  private async found(rec: PostRecord): Promise<Found | null> {
    const account = rec.account as string;
    switch (rec.command) {
      case "deal": {
        const pr = await this.table.player(account);
        if (pr.openGame !== null) {
          const g = await this.table.game(pr.openGame);
          if (g && g.table && g.postId === BigInt(rec.id)) return { gameId: String(g.id) };
        }
        // Settled already: the last hand's seed is this post's and no other's.
        if (pr.last && pr.last.clientSeed === relaySeed(account, BigInt(rec.id))) return { gameId: String(pr.last.game) };
        return null;
      }
      case "hit":
      case "stand": {
        const id = BigInt(rec.gameId as string);
        const before = rec.before as { nPlayer: number; dealt: number };
        const g = await this.table.game(id);
        if (g) {
          if (rec.command === "hit") return g.phase === Phase.Hit || g.playerCards.length > before.nPlayer ? {} : null;
          return g.phase === Phase.Dealer ? {} : null;
        }
        const pr = await this.table.player(account);
        if (pr.last?.game !== id) return null;
        return rec.command === "stand" || pr.last.nPlayer > before.nPlayer ? {} : null;
      }
      case "stop":
        return (await this.table.player(account)).balance === 0n ? {} : null;
      case "timeout":
        return (await this.table.game(BigInt(rec.gameId as string))) ? null : {};
      case "balance":
        return null;
    }
  }
}

/** What a player can still bet from X today (drops): the table's daily cap and the grant's, whichever is less. */
function leftToday(pr: PlayerRecord, dailyCap: bigint, day: bigint): bigint {
  const grant = pr.grant;
  if (!grant) return 0n;
  const table = dailyCap - (pr.day === day ? pr.wagered : 0n);
  const granted = grant.dailyCap - (grant.day === day ? grant.wagered : 0n);
  const left = table < granted ? table : granted;
  return left > 0n ? left : 0n;
}
