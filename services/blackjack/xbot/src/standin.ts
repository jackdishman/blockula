/**
 * The dry run's stand-in for checking a link's proof post. It reads "posts" from a local JSON
 * file (`[{ "id": "…", "handle": "…", "text": "…" }]`) and compares a claim with the post of
 * that id: same author, and the account's code in the text (`checkXProof`, which is pure).
 * It opens no connection. A claim whose post isn't in the file is not verified.
 */
import { readFileSync } from "node:fs";
import { checkXProof } from "@blockula/sdk";
import type { VerifyLink } from "./types.js";

export interface LocalPost {
  id: string;
  handle: string;
  text: string;
}

/** A verifier over posts held in memory. */
export function localVerifier(posts: readonly LocalPost[]): VerifyLink {
  const byId = new Map(posts.map((p) => [p.id, p]));
  return async (claim) => {
    const post = byId.get(claim.post_id);
    if (!post) return { verified: false };
    return { verified: checkXProof(claim, { id: post.id, handle: post.handle, untrusted_text: post.text }).verified };
  };
}

/** A verifier over the posts in a local file; with no file, nothing verifies. */
export function fileVerifier(path: string | null): VerifyLink {
  if (path === null) return async () => ({ verified: false });
  const parsed: unknown = JSON.parse(readFileSync(path, "utf8"));
  if (!Array.isArray(parsed)) throw new Error(`${path} isn't a list of posts`);
  const posts = parsed.filter((p): p is LocalPost => !!p && typeof p === "object" && typeof (p as LocalPost).id === "string" && typeof (p as LocalPost).handle === "string" && typeof (p as LocalPost).text === "string");
  return localVerifier(posts);
}
