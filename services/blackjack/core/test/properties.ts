// Properties of the rules, the payouts, the fairness check and the dealer's helper, over
// thousands of generated hands and stakes. Offline; the generator is a hash chain, so a failure
// names a hand that fails again. Nothing here uses a vector the Rust side wrote: the oracle is
// the rules as a player would read them, written a second time.
import assert from "node:assert/strict";
import { blake3 } from "@noble/hashes/blake3.js";
import { CHAIN_LEN, Hand, MAX_DEALER_CARDS, MAX_PLAYER_CARDS, Outcome, Phase, bytesAddress, card, chainFromTop, dealers, exposure, hexBytes, linksFromNewest, natural, owedLinks, payout, replayCounted, replayMoves, seedHashOf, toHex, total, verifyHand, type Game, type HandProof } from "../src/index.js";

// ---------------------------------------------------------------------------------- a generator

class Rng {
  private state: Uint8Array;
  constructor(label: string) {
    this.state = blake3(new TextEncoder().encode(label));
  }
  bytes32(): string {
    this.state = blake3(this.state);
    return toHex(this.state);
  }
  u64(): bigint {
    return BigInt("0x" + this.bytes32().slice(0, 16));
  }
  below(n: number): number {
    return Number(this.u64() % BigInt(n));
  }
}

// ---------------------------------------------------------------------------------- an oracle

/** A hand's value with every ace at 11, brought down one ace at a time while it's over 21. */
function value(cards: readonly number[]): number {
  let aces = 0;
  let sum = 0;
  for (const c of cards) {
    const r = c % 13;
    if (r === 0) {
      aces++;
      sum += 11;
    } else sum += r >= 9 ? 10 : r + 1;
  }
  while (sum > 21 && aces > 0) {
    sum -= 10;
    aces--;
  }
  return sum;
}

function oracle(player: readonly number[], dealer: readonly number[]): Outcome {
  const [p, d] = [value(player), value(dealer)];
  const pn = player.length === 2 && p === 21;
  const dn = dealer.length === 2 && d === 21;
  if (p > 21) return Outcome.Bust;
  if (pn) return dn ? Outcome.Push : Outcome.Blackjack;
  if (dn) return Outcome.Lose;
  if (d > 21 || p > d) return Outcome.Win;
  return p === d ? Outcome.Push : Outcome.Lose;
}

/** The hand a player and a dealer would play from `cards` (in chain order), by the book. */
function playByTheBook(cards: readonly number[], wants: (player: number[]) => boolean): { player: number[]; dealer: number[]; outcome: Outcome; used: number; moves: string } {
  let at = 0;
  const player = [cards[at++]];
  const dealer = [cards[at++]];
  player.push(cards[at++]);
  let moves = "";
  if (value(player) === 21) {
    if ([0, 9, 10, 11, 12].includes(dealer[0] % 13)) dealer.push(cards[at++]);
    return { player, dealer, outcome: oracle(player, dealer), used: at, moves };
  }
  while (value(player) < 21) {
    if (!wants(player)) {
      moves += "S";
      break;
    }
    moves += "H";
    player.push(cards[at++]);
  }
  if (value(player) <= 21) {
    dealer.push(cards[at++]);
    while (value(dealer) < 17) dealer.push(cards[at++]);
  }
  return { player, dealer, outcome: oracle(player, dealer), used: at, moves };
}

// ---------------------------------------------------------------------------------- payouts

{
  const rng = new Rng("payouts");
  const TOP = (1n << 64n) - 1n;
  const stakes: bigint[] = [0n, 1n, 2n, 3n, 5n, 999_999_999n, 1_000_000_000n, 1_000_000_001n, 10n ** 18n, TOP];
  for (let i = 0; i < 5_000; i++) stakes.push(rng.u64() >> BigInt(rng.below(63)));
  const outcomes = [1, 2, 3, 4, 5, 6, 7, 8] as Outcome[];
  for (const stake of stakes) {
    // Exactly 1.5 × and 2.5 ×, the half drop dropped.
    assert.equal(exposure(stake), (stake * 3n) / 2n);
    assert.equal(payout(stake, Outcome.Blackjack), (stake * 5n) / 2n);
    assert.equal(payout(stake, Outcome.ForfeitDeal), payout(stake, Outcome.Blackjack));
    assert.equal(payout(stake, Outcome.Win), 2n * stake);
    assert.equal(payout(stake, Outcome.Forfeit), payout(stake, Outcome.Win));
    assert.equal(payout(stake, Outcome.Push), stake);
    for (const o of [Outcome.Lose, Outcome.Bust, Outcome.NoSeed]) assert.equal(payout(stake, o), 0n);
    // What a hand sets aside covers every ending, and is exactly the dearest one.
    const aside = stake + exposure(stake);
    let most = 0n;
    for (const o of outcomes) {
      const pays = payout(stake, o);
      assert.ok(pays >= 0n && pays <= aside, `outcome ${o} of ${stake}`);
      if (pays > most) most = pays;
    }
    assert.equal(most, aside);
    // The house keeps the half drop of an odd stake's natural, and only that.
    assert.equal(stake * 5n - 2n * payout(stake, Outcome.Blackjack), stake % 2n);
  }
  // A stake cut in two never pays more than it whole, and at most the half drop less.
  for (let i = 0; i + 1 < stakes.length; i += 2) {
    const [a, b] = [stakes[i] / 2n, stakes[i + 1] / 2n];
    for (const o of outcomes) {
      const [whole, parts] = [payout(a + b, o), payout(a, o) + payout(b, o)];
      assert.ok(whole >= parts && whole - parts <= 1n, `outcome ${o}: ${a} + ${b}`);
    }
  }
  console.log(`✓ payouts: ${stakes.length} stakes round the half drop down, fit their reserve, and gain nothing from being split`);
}

// ---------------------------------------------------------------------------------- hands

interface Dealt {
  id: bigint;
  seed: string;
  chain: string[];
  cards: number[];
  book: ReturnType<typeof playByTheBook>;
}

/** A random hand: its chain, the player's seed, and how it plays by the book under a random policy. */
function deal(rng: Rng): Dealt {
  const chain = chainFromTop(rng.bytes32());
  const seed = rng.bytes32();
  const id = rng.below(3) === 0 ? rng.u64() : BigInt(rng.below(1_000_000));
  const cards = Array.from({ length: CHAIN_LEN }, (_, k) => card(chain[k + 1], seed, id, k + 1));
  const style = rng.below(4);
  const book = playByTheBook(cards, (player) => {
    const t = value(player);
    if (style === 0) return false;
    if (style === 1) return true;
    return rng.below(100) < (t < 12 ? 95 : t < 17 ? 60 : 15);
  });
  return { id, seed, chain, cards, book };
}

{
  const rng = new Rng("hands");
  const seen = new Map<number, number>();
  const longest = { player: 0, dealer: 0 };
  const N = 4_000;
  for (let n = 0; n < N; n++) {
    const h = deal(rng);
    const { book } = h;
    const links = h.chain.slice(1, book.used + 1);

    // The state machine, the moves and the links agree with the book.
    const r = replayMoves(h.chain[0], h.seed, h.id, links, book.moves);
    assert.deepEqual([r.player, r.dealer, r.outcome, r.phase], [book.player, book.dealer, book.outcome, Phase.Done], `hand ${n}`);
    assert.deepEqual([r.playerTotal, r.dealerTotal], [value(book.player), value(book.dealer)], `hand ${n}`);
    assert.deepEqual([total(book.player).total, total(book.dealer).total], [value(book.player), value(book.dealer)]);
    assert.ok(book.player.length <= MAX_PLAYER_CARDS && book.dealer.length <= MAX_DEALER_CARDS && book.used <= CHAIN_LEN);
    seen.set(book.outcome, (seen.get(book.outcome) ?? 0) + 1);
    longest.player = Math.max(longest.player, book.player.length);
    longest.dealer = Math.max(longest.dealer, book.dealer.length);
    // Knowing only how many cards the player took is enough to replay it the same way.
    const counted = replayCounted(h.chain[0], h.seed, h.id, links, book.player.length);
    assert.deepEqual([counted.player, counted.dealer, counted.outcome, counted.moves], [book.player, book.dealer, book.outcome, book.moves], `hand ${n}`);

    // ---- the fairness check: the truth verifies, and a changed record doesn't
    const stake = 1n + (rng.u64() % 50_000_000_000n);
    const player = rng.bytes32();
    const proof: HandProof = { game: h.id, clientSeed: h.seed, link: links[links.length - 1], dealt: links.length, nPlayer: book.player.length, commit: h.chain[0], outcome: book.outcome, stake, payout: payout(stake, book.outcome) };
    const ok = verifyHand(proof);
    assert.deepEqual([ok.player, ok.dealer, ok.outcome, ok.forfeited, ok.unrevealed, ok.links], [book.player, book.dealer, book.outcome, false, false, links], `hand ${n}`);
    // Every other outcome, for the same cards, is refused: with its own payout and with the true one.
    for (const other of [1, 2, 3, 4, 5, 6, 7, 8] as Outcome[]) {
      if (other === book.outcome) continue;
      assert.throws(() => verifyHand({ ...proof, outcome: other, payout: payout(stake, other) }), `hand ${n}: outcome ${other} for cards that give ${book.outcome}`);
      assert.throws(() => verifyHand({ ...proof, outcome: other }), `hand ${n}: outcome ${other}, the true payout`);
    }
    // A payout a drop off, either way.
    assert.throws(() => verifyHand({ ...proof, payout: proof.payout! + 1n }), /payout/);
    if (proof.payout! > 0n) assert.throws(() => verifyHand({ ...proof, payout: proof.payout! - 1n }), /payout/);
    // Another chain's link, or this chain's at another depth.
    assert.throws(() => verifyHand({ ...proof, link: rng.bytes32() }), /commitment/);
    assert.throws(() => verifyHand({ ...proof, dealt: proof.dealt + 1 }), /commitment/);
    assert.throws(() => verifyHand({ ...proof, link: h.chain[links.length + 1], dealt: links.length + 1 }), `hand ${n}: a link past the hand's last card`);
    // A seed that isn't the bet's, when the bet's hash is known.
    assert.equal(verifyHand({ ...proof, seedHash: seedHashOf(player, h.seed), player: playerAddress(player) }).seedChecked, true);
    assert.throws(() => verifyHand({ ...proof, seedHash: seedHashOf(player, rng.bytes32()), player: playerAddress(player) }), /committed/);
    // Another seed or another game deals another hand: the recorded outcome and count rarely fit,
    // and when they do, the cards are still the ones that seed deals, never the original's.
    for (const changed of [{ ...proof, clientSeed: rng.bytes32() }, { ...proof, game: h.id ^ 1n }]) {
      try {
        const v = verifyHand(changed);
        assert.notDeepEqual(v.cards, ok.cards);
      } catch {
        // Refused: the usual case.
      }
    }

    // ---- a hand cut short verifies only as the forfeit the program would have paid
    for (let cut = 0; cut < links.length; cut++) {
      const part = links.slice(0, cut);
      const nPlayer = cut === 0 ? 0 : cut === 1 ? 1 : cut === 2 ? 1 : Math.min(book.player.length, 2 + Math.max(0, Math.min(cut - 3, book.player.length - 2)));
      const open: HandProof = { game: h.id, clientSeed: h.seed, link: cut === 0 ? h.chain[0] : part[cut - 1], dealt: cut, nPlayer, commit: h.chain[0] };
      // Every prefix of a hand's links is a state the hand passed through: it verifies as open.
      const v = verifyHand(open);
      assert.equal(v.outcome, null, `hand ${n} cut at ${cut}: unfinished`);
      const most = v.phase === Phase.Deal || natural(v.player) ? Outcome.ForfeitDeal : Outcome.Forfeit;
      assert.equal(verifyHand({ ...open, outcome: most, stake, payout: payout(stake, most) }).forfeited, true);
      for (const other of [1, 2, 3, 4, 5, 8, most === Outcome.Forfeit ? Outcome.ForfeitDeal : Outcome.Forfeit] as Outcome[]) {
        assert.throws(() => verifyHand({ ...open, outcome: other, stake, payout: payout(stake, other) }), `hand ${n} cut at ${cut}: ended as ${other}`);
      }
      // A forfeit is never worth less to the player than the way the cards did end.
      assert.ok(payout(stake, most) >= payout(stake, book.outcome), `hand ${n} cut at ${cut}`);
    }

    // ---- the dealer's helper: exactly what each turn owes, never a card early
    const hand = new Hand();
    let revealed = 0;
    let m = 0;
    const game = (): Pick<Game, "id" | "clientSeed" | "phase" | "dealt" | "playerCards" | "dealerCards"> => ({ id: h.id, clientSeed: h.seed, phase: hand.phase, dealt: hand.dealt, playerCards: [...hand.player], dealerCards: [...hand.dealer] });
    // Before the seed is revealed, nothing: no link leaves the dealer.
    assert.deepEqual(owedLinks({ ...game(), phase: Phase.Seed, clientSeed: null }, h.chain), []);
    assert.throws(() => owedLinks({ ...game(), clientSeed: null }, h.chain), /has its seed/);
    while (hand.phase !== Phase.Done) {
      if (hand.phase === Phase.Player) {
        // The player's turn: the dealer owes nothing, so the next card stays unknown to everyone.
        assert.deepEqual(owedLinks(game(), h.chain), [], `hand ${n}: a link on the player's turn`);
        if (book.moves[m++] === "H") hand.hit();
        else hand.stand();
        continue;
      }
      assert.ok(dealers(hand.phase));
      const owed = owedLinks(game(), h.chain);
      assert.ok(owed.length >= 1 && owed.length <= 12, `hand ${n}: ${owed.length} links in one reveal`);
      assert.deepEqual(owed, h.chain.slice(revealed + 1, revealed + 1 + owed.length), `hand ${n}: the chain's next links, in order`);
      const before = hand.phase;
      owed.forEach((link, i) => {
        // No link is revealed once the hand is the player's to move, or over.
        assert.ok(dealers(hand.phase), `hand ${n}: link ${revealed + i + 1} revealed with no card owed`);
        hand.deal(card(link, h.seed, h.id, hand.dealt + 1));
      });
      revealed += owed.length;
      // It stops exactly where the player must choose, or the hand ends.
      const now = hand.phase as Phase;
      assert.ok(now === Phase.Player || now === Phase.Done, `hand ${n}`);
      // A hit shows one card unless it made 21 (then the dealer's hand follows): the card after
      // a hit that leaves the player choosing is never out.
      if (before === Phase.Hit && now === Phase.Player) assert.equal(owed.length, 1, `hand ${n}: a hit reveals one card`);
      if (before === Phase.Deal && now === Phase.Player) assert.equal(owed.length, 3, `hand ${n}: the deal reveals three`);
    }
    assert.equal(revealed, book.used, `hand ${n}: every link the hand used, and no more`);
    assert.equal(m, book.moves.length);
    assert.deepEqual(linksFromNewest(h.chain[revealed], revealed), links);
  }
  for (const o of [1, 2, 3, 4, 5]) assert.ok((seen.get(o) ?? 0) > 20, `outcome ${o} barely happened`);
  assert.ok(longest.player >= 6 && longest.dealer >= 5);
  console.log(`✓ ${N} generated hands: the rules match a second reading of them, verifyHand accepts the truth and refuses every changed record, and the dealer never owes a card early`);
}

/** An address for 32 bytes of hex, on the `boo` chain. */
function playerAddress(hex: string): string {
  return bytesAddress(hexBytes(hex, 32), "boo");
}

// ---------------------------------------------------------------------------------- why a chain's links are secret

{
  // The whole game rests on one thing: nobody but the dealer knows a link before it is revealed.
  // This is what a player does who does know a hand's first three links before betting (because
  // the dealer committed, on another chain or table, a chain it had already dealt from): the
  // seed is theirs to choose, so they try seeds until the deal is a natural the dealer's upcard
  // can't match. A few dozen tries; every hand pays 3:2. The dealer's store is tied to one table
  // on one chain for that reason (services/blackjack/dealer/src/store.ts, `bindChain`).
  const rng = new Rng("known links");
  let tries = 0;
  const HANDS = 50;
  for (let n = 0; n < HANDS; n++) {
    const chain = chainFromTop(rng.bytes32());
    const id = BigInt(n);
    const known = chain.slice(1, 4);
    let seed = "";
    for (;;) {
      tries++;
      seed = rng.bytes32();
      const [p1, up, p2] = known.map((link, k) => card(link, seed, id, k + 1));
      if (natural([p1, p2]) && ![0, 9, 10, 11, 12].includes(up % 13)) break;
    }
    const r = replayCounted(chain[0], seed, id, known, 2);
    assert.equal(r.outcome, Outcome.Blackjack, "settled at the deal, with nothing left for the dealer to do");
    assert.equal(payout(10n, r.outcome!), 25n);
  }
  assert.ok(tries / HANDS < 200, "a natural is a few dozen seeds away");
  // Without the links, the same player has nothing to search: the cards of every seed are unknown.
  console.log(`✓ a player who knows a hand's first links picks a natural in about ${Math.round(tries / HANDS)} seeds: links must never be public before their hand`);
}

console.log("✅ the property tests passed");
