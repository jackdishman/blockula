/**
 * Payment links: `<wallet>/send?to=<name or address>&amount=<FANG>&memo=<text>`. Send reads the
 * same three parameters, fills the form and shows a "a link filled in this payment" warning; the
 * payer still reviews and signs it themselves. A link never chooses the node (see lib/config).
 * No imports, so lib/qr.test.mjs can run it under plain Node.
 */

/** Builds a payment link. Empty amount or memo are left out. */
export function payLink(walletUrl: string, to: string, amount?: string, memo?: string): string {
  const q = new URLSearchParams({ to });
  if (amount?.trim()) q.set("amount", amount.trim());
  if (memo) q.set("memo", memo);
  return `${walletUrl.replace(/\/$/, "")}/send?${q}`;
}
