/**
 * From an X handle to the account the bot may play for.
 *
 * The chain's record goes the other way: an account names a handle (docs/X-LINKING.md), and
 * no node indexes records by handle. So the bot keeps a registry. The accounts in it are the
 * ones whose grant at the table names this relayer (read from the table), plus any an operator
 * enrolled. For each, the account's link claim is read from the chain and checked against its
 * proof post by the verifier the bot was given. A checked link is trusted for `linkTtlMs`,
 * then read and checked again.
 *
 * A handle resolves to an account only when exactly one account's checked link names it:
 * - none checked, none claimed: no linked account;
 * - claimed but the proof didn't check out: not verified;
 * - two or more checked: refused for all of them (the handle's owner posted proofs for
 *   several accounts, and the bot won't pick).
 * An unchecked claim never blocks a checked one: otherwise anyone could lock a player out by
 * naming their handle.
 */
import type { BotConfig, Clock, LinkEntry, Store, TableDriver, VerifyLink } from "./types.js";

export type Resolved = { kind: "ok"; account: string } | { kind: "none" } | { kind: "unverified" } | { kind: "conflict" };

export class Links {
  constructor(
    private readonly table: TableDriver,
    private readonly store: Store,
    private readonly verify: VerifyLink,
    private readonly clock: Clock,
    private readonly config: Pick<BotConfig, "linkTtlMs" | "listEveryMs">,
  ) {}

  /** The account `handle` may play, checked within the last `linkTtlMs`. */
  async resolve(handle: string): Promise<Resolved> {
    const h = handle.toLowerCase();
    const now = this.clock.now();
    const links = this.store.state.links;
    const fresh = (e: LinkEntry) => now - e.checkedAt < this.config.linkTtlMs;
    const named = () => Object.entries(links.accounts).filter(([, e]) => e.handle === h);
    const since = now - links.listedAt;
    const known = named();
    // List again when the listing is old, when a link for this handle is, or (not more often
    // than `listEveryMs`) when nothing checked names the handle: its account may be new.
    const stale = since >= this.config.linkTtlMs || known.some(([, e]) => !fresh(e));
    if (stale || (!known.some(([, e]) => e.verified) && since >= this.config.listEveryMs)) await this.refresh();
    const after = named();
    const checked = after.filter(([, e]) => e.verified && fresh(e)).map(([account]) => account);
    if (checked.length === 1) return { kind: "ok", account: checked[0] };
    if (checked.length > 1) return { kind: "conflict" };
    return after.length > 0 ? { kind: "unverified" } : { kind: "none" };
  }

  /** Lists the accounts again and checks every link that's new, changed, unverified or older than `linkTtlMs`. */
  async refresh(): Promise<void> {
    const now = this.clock.now();
    const links = this.store.state.links;
    const accounts = [...new Set([...(await this.table.granted()), ...this.store.state.enrolled])].sort();
    const next: Record<string, LinkEntry> = {};
    for (const account of accounts) {
      const claim = await this.table.link(account);
      if (!claim) {
        next[account] = { handle: null, postId: null, verified: false, checkedAt: now };
        continue;
      }
      const handle = claim.handle.toLowerCase();
      const old = links.accounts[account];
      if (old && old.verified && old.handle === handle && old.postId === claim.post_id && now - old.checkedAt < this.config.linkTtlMs) {
        next[account] = old;
        continue;
      }
      // A check that can't be made (the verifier threw) is "not verified", and is tried again next time.
      const verified = await this.verify(claim).then(
        (c) => c.verified === true,
        () => false,
      );
      next[account] = { handle, postId: claim.post_id, verified, checkedAt: now };
    }
    links.accounts = next;
    links.listedAt = now;
    this.store.commit();
  }
}
