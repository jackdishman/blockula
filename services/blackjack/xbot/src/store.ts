/**
 * The bot's store: one JSON file, replaced whole on every commit. A commit writes a new file
 * beside it, syncs it to disk and renames it over the old one, so the file is always one
 * complete state: the one before the commit or the one after, never half of each.
 *
 * It holds no key and no secret: post ids, handles (data), accounts, hand ids, transaction ids.
 */
import { closeSync, existsSync, fsyncSync, mkdirSync, openSync, readFileSync, renameSync, writeSync } from "node:fs";
import { dirname } from "node:path";
import { emptyState, type State, type Store } from "./types.js";

/**
 * Ties a store to one chain and one table: the first time, it remembers them; after that it
 * refuses any other. Post ids recorded for one table say nothing about another.
 */
export function pinStore(store: Store, chain: { chainId: string; genesisHash: string; table: string }): void {
  const s = store.state;
  if (s.chain === null) {
    s.chain = { ...chain };
    store.commit();
    return;
  }
  if (s.chain.chainId !== chain.chainId || s.chain.genesisHash !== chain.genesisHash || s.chain.table !== chain.table) throw new Error(`this store was used on ${s.chain.chainId} at table ${s.chain.table}: use another home for another chain or table`);
}

/** A store that lives only as long as the process: the tests'. */
export class MemoryStore implements Store {
  readonly state: State;
  commits = 0;
  constructor(state: State = emptyState()) {
    this.state = state;
  }
  commit(): void {
    this.commits++;
  }
}

/** The store's file as a state. Throws on a file that isn't one (it is never repaired by guessing). */
function parse(text: string, path: string): State {
  let s: unknown;
  try {
    s = JSON.parse(text);
  } catch {
    throw new Error(`${path} isn't JSON: the store is never half-written, so something else changed it. Move it away to start empty`);
  }
  const o = s as Partial<State> | null;
  if (!o || typeof o !== "object" || o.version !== 1 || typeof o.posts !== "object" || typeof o.hands !== "object") throw new Error(`${path} isn't a version 1 store`);
  return { ...emptyState(), ...o } as State;
}

export class JsonStore implements Store {
  readonly state: State;

  private constructor(
    readonly path: string,
    state: State,
  ) {
    this.state = state;
  }

  /** Opens the store at `path`, empty if there's no file yet. */
  static open(path: string): JsonStore {
    mkdirSync(dirname(path), { recursive: true, mode: 0o700 });
    return new JsonStore(path, existsSync(path) ? parse(readFileSync(path, "utf8"), path) : emptyState());
  }

  commit(): void {
    const tmp = `${this.path}.tmp`;
    const fd = openSync(tmp, "w", 0o600);
    try {
      writeSync(fd, JSON.stringify(this.state));
      fsyncSync(fd);
    } finally {
      closeSync(fd);
    }
    renameSync(tmp, this.path);
  }
}
