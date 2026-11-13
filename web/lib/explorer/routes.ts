/** In-app explorer paths. The node's own explorer stays at `{rpc}/#/…` (see `nodeExplorer`). */
export const to = {
  explore: () => "/explore",
  /** The directory of accounts and names. */
  boos: () => "/boos",
  /** A boo's profile, by its name (`vlad.boo` or `vlad`): the short link people share. */
  boo: (name: string) => `/boo/${encodeURIComponent(name.toLowerCase().replace(/\.boo$/, ""))}`,
  /** The guestbook: signatures paid to guestbook.boo (lib/explorer/guestbook.ts). */
  guestbook: () => "/guestbook",
  /** The leaderboards: guestbook signers, trick or treat players, Soul (lib/explorer/leaderboard.ts). */
  leaderboard: () => "/leaderboard",
  /** Link, change or remove this wallet's X account (docs/X-LINKING.md). */
  x: () => "/x",
  block: (height: number | string) => `/block/${encodeURIComponent(String(height))}`,
  tx: (id: string) => `/tx/${encodeURIComponent(id)}`,
  account: (nameOrAddress: string) => `/account/${encodeURIComponent(nameOrAddress)}`,
  /** The money graph around an account; `follow` lists accounts already followed (addresses). */
  trace: (nameOrAddress: string, follow?: string) => `/trace/${encodeURIComponent(nameOrAddress)}${follow ? `?follow=${encodeURIComponent(follow)}` : ""}`,
  name: (name: string) => `/name/${encodeURIComponent(name.replace(/\.boo$/, ""))}`,
  programs: () => "/programs",
  program: (id: string) => `/programs/${id}`,
  /** A program anyone deployed, by its address (or a stored code, by its hash). */
  deployed: (address: string) => `/program/${encodeURIComponent(address)}`,
  hunt: (id: number | string) => `/hunt/${encodeURIComponent(String(id))}`,
  claim: (id: number | string) => `/claim/${encodeURIComponent(String(id))}`,
  seance: (id: number | string) => `/seance/${encodeURIComponent(String(id))}`,
};

/** Where a `/api/search` hit lives in the app. */
export const searchHref = (hit: { type: string; id: string }) =>
  hit.type === "block" ? to.block(hit.id) : hit.type === "tx" ? to.tx(hit.id) : hit.type === "account" ? to.account(hit.id) : hit.type === "program" || hit.type === "code" ? to.deployed(hit.id) : to.name(hit.id);

/** Explorer pages are wide (charts, tables); the wallet's own pages are a narrow column. */
export function isExplorerPath(path: string) {
  return /^\/(explore|boos?|guestbook|leaderboard|block|tx|account|trace|programs?|hunt|claim|seance)(\/|$)/.test(path) || /^\/name\/[^/]+/.test(path);
}
