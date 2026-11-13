/**
 * The dry run: posts in as JSON lines, replies out through the bot's sink. Nothing is read
 * from X and nothing is posted.
 */
import type { Bot } from "./bot.js";
import type { Clock } from "./types.js";

export interface RunSummary {
  /** Lines read that held something. */
  posts: number;
  /** Lines that weren't JSON. */
  unreadable: number;
  replies: number;
  /** How each post ended, counted: `ok`, `refused:<reason>`, `ignored:<reason>`. */
  results: Record<string, number>;
  /** Hands still waiting on the dealer when the run ended. */
  waiting: number;
}

export interface RunOptions {
  clock: Clock;
  /** After the last post, how long to keep watching hands that wait on the dealer. */
  drainMs?: number;
  /** How often to look while draining. */
  tickMs?: number;
  log?: (line: string) => void;
}

/** Finishes what a previous run left, then handles each line's post in order, looking at the open hands after each. */
export async function runPosts(bot: Bot, lines: AsyncIterable<string> | Iterable<string>, o: RunOptions): Promise<RunSummary> {
  const sum: RunSummary = { posts: 0, unreadable: 0, replies: 0, results: {}, waiting: 0 };
  const log = o.log ?? (() => undefined);
  const wrote = (n: number) => {
    sum.replies += n;
  };
  wrote((await bot.recover()).length);
  for await (const line of lines) {
    if (line.trim() === "") continue;
    sum.posts++;
    let post: unknown;
    try {
      post = JSON.parse(line);
    } catch {
      sum.unreadable++;
      continue;
    }
    const r = await bot.handle(post);
    sum.results[r.result] = (sum.results[r.result] ?? 0) + 1;
    wrote(r.replies.length);
    // The line logged holds the post's id (digits) and the bot's own words for the result: never the post's text.
    const id = (post as { id?: unknown } | null)?.id;
    log(`${typeof id === "string" && /^[0-9]{1,20}$/.test(id) ? id : "?"} ${r.result} (${r.replies.length} ${r.replies.length === 1 ? "reply" : "replies"})`);
    wrote((await bot.tick()).length);
  }
  const until = o.clock.now() + (o.drainMs ?? 0);
  while (bot.waiting() > 0 && o.clock.now() < until) {
    await o.clock.sleep(o.tickMs ?? 1_000);
    wrote((await bot.tick()).length);
  }
  sum.waiting = bot.waiting();
  return sum;
}
