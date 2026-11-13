/**
 * Where replies go in this build: a directory. One folder a reply, named for the post it
 * answers and which of that post's replies it is:
 *
 *   <out>/<post id>-<n>/reply.txt        the text (at most 280 as X counts)
 *   <out>/<post id>-<n>/image.png        the hand's picture, on a reply about a hand
 *   <out>/<post id>-<n>/image.alt.txt    its alt text (always there when the picture is)
 *   <out>/<post id>-<n>/meta.json        what it answers, for whom (data), the hand, the transaction
 *
 * Nothing is posted anywhere. A folder is written under another name and renamed into place,
 * so it's whole or absent; writing the same reply again replaces it.
 */
import { mkdirSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { replyId, type Reply, type ReplySink } from "./types.js";

/** A reply as it was written: with the id this build gave it. */
export type Written = Reply & { id: string };

export class DirSink implements ReplySink {
  constructor(readonly dir: string) {
    mkdirSync(dir, { recursive: true });
  }

  write(reply: Reply): string {
    const name = `${reply.meta.inReplyTo}-${reply.seq}`;
    const tmp = join(this.dir, `.${name}.tmp`);
    const final = join(this.dir, name);
    rmSync(tmp, { recursive: true, force: true });
    mkdirSync(tmp);
    writeFileSync(join(tmp, "reply.txt"), reply.text + "\n");
    if (reply.image) {
      if (reply.image.png) writeFileSync(join(tmp, "image.png"), reply.image.png);
      writeFileSync(join(tmp, "image.alt.txt"), reply.image.alt + "\n");
    }
    writeFileSync(join(tmp, "meta.json"), JSON.stringify({ id: replyId(reply.meta.inReplyTo, reply.seq), ...reply.meta, image: reply.image ? Boolean(reply.image.png) : false, posted: false }, null, 2) + "\n");
    rmSync(final, { recursive: true, force: true });
    renameSync(tmp, final);
    return replyId(reply.meta.inReplyTo, reply.seq);
  }
}

/** Keeps replies in memory: the tests'. */
export class MemorySink implements ReplySink {
  readonly replies: Written[] = [];
  write(reply: Reply): string {
    const id = replyId(reply.meta.inReplyTo, reply.seq);
    const at = this.replies.findIndex((r) => r.id === id);
    if (at >= 0) this.replies[at] = { ...reply, id };
    else this.replies.push({ ...reply, id });
    return id;
  }
}
