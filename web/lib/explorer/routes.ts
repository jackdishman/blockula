/** In-app explorer paths. The node's own explorer stays at `{rpc}/#/…` (see `nodeExplorer`). */
export const to = {
  explore: () => "/explore",
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
  return /^\/(explore|block|tx|account|trace|programs?|hunt|claim|seance)(\/|$)/.test(path) || /^\/name\/[^/]+/.test(path);
}
