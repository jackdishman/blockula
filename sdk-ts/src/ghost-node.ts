/// <reference types="node" />
// `@blockula/sdk/ghost/node`: what `@blockula/sdk/ghost` needs a file system for. Node only.
import { chmod, mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import type { GhostNoteState, GhostNoteStore } from "./ghost.js";

const HEX32 = /^[0-9a-f]{64}$/;

/**
 * A `GhostWallet`'s note store as files in `dir`: one per chain instance and key,
 * `<instance hex>.<first 16 hex of the owner digest>.json`, mode 600 in a directory made mode 700.
 * It holds the tree's leaves and, for each of the wallet's notes, its position, value, memo and
 * ciphertext: no key and no note seed, but what the wallet holds and was sent, so it's private.
 * It's a cache: deleting a file loses nothing, the next sync rebuilds it from the chain.
 *
 * It isn't the CLI's note store (`~/.blockula/ghost/<instance>.json`, another format): give it a
 * directory of its own.
 */
export class FileNoteStore implements GhostNoteStore {
  constructor(readonly dir: string) {}

  /** The file a key's notes on a chain are kept in. */
  path(instance: string, owner: string): string {
    if (!HEX32.test(instance) || !HEX32.test(owner)) throw new Error("a note store is keyed by a chain instance and an owner digest, 64 hex characters each");
    return join(this.dir, `${instance}.${owner.slice(0, 16)}.json`);
  }

  async load(instance: string, owner: string): Promise<GhostNoteState | null> {
    let text: string;
    try {
      text = await readFile(this.path(instance, owner), "utf8");
    } catch (e) {
      if ((e as NodeJS.ErrnoException).code === "ENOENT") return null;
      throw e;
    }
    try {
      return JSON.parse(text);
    } catch {
      // Unreadable: a cache gone bad, rebuilt by the next sync.
      return null;
    }
  }

  /** Written whole, mode 600, replacing the old one in one rename. */
  async save(state: GhostNoteState): Promise<void> {
    const file = this.path(state.instance, state.owner);
    await mkdir(this.dir, { recursive: true, mode: 0o700 });
    await chmod(this.dir, 0o700).catch(() => undefined);
    const tmp = `${file}.${process.pid}.tmp`;
    await rm(tmp, { force: true });
    // Created mode 600 ("wx": never through a file someone else put there), so it's never readable by others.
    await writeFile(tmp, JSON.stringify(state), { mode: 0o600, flag: "wx" });
    await rename(tmp, file);
  }
}
