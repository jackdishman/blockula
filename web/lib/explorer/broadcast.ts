// One signed transaction, sent through several nodes at once. It is the same transaction at every
// node (one nonce, one signature), so it runs once and is charged once however many of them take
// it. Used where a single node keeping a transaction to itself costs the sender something: the
// blackjack table's seed reveal (docs/BLACKJACK_REVIEW.md, BJ-04).

/**
 * Posts to every node in `nodes` at once and resolves with the first acceptance; the other posts
 * still run to their end. When no node accepts: the error of a node that may have received it
 * (anything `refused` doesn't call a refusal) comes first, since the transaction may still run;
 * only when every node refused it is a refusal thrown (the first node's, the selected one).
 */
export function firstAccepted<T>(nodes: readonly string[], post: (node: string) => Promise<T>, refused: (e: unknown) => boolean): Promise<T> {
  if (!nodes.length) return Promise.reject(new Error("no node to send through"));
  return new Promise<T>((resolve, reject) => {
    const errors: unknown[] = new Array(nodes.length);
    let left = nodes.length;
    nodes.forEach((node, i) => {
      let sent: Promise<T>;
      try {
        sent = post(node);
      } catch (e) {
        sent = Promise.reject(e);
      }
      sent.then(resolve, (e) => {
        errors[i] = e;
        if (--left === 0) reject(errors.find((x) => !refused(x)) ?? errors[0]);
      });
    });
  });
}
