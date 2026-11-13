/**
 * Every sentence the blackjack pages say to a player (app/blackjack, components/blackjack), in
 * one place, so lib/blackjack.test.mjs can hold all of it to the copy rules: plain English, short
 * sentences, "programs", a game played with FANG, and nothing about FANG being worth anything.
 *
 * Pure, so `node --test` reads it.
 */
import { POSTER_ALT, fang, type Limits } from "@blockula/blackjack";
import { blocksWords } from "./blackjack.ts";

/** The note every blackjack page shows, word for word. */
export const NOTE = "FANG has no value and can't be bought; no prizes.";

export const COPY = {
  note: NOTE,
  title: "Blackjack",
  eyebrow: "program · a game played with FANG",
  sub: "One hand against the dealer, played with FANG. Every card comes from a hash chain the dealer locked in before your bet, mixed with a seed your browser makes. You can check each hand yourself.",
  noTable: {
    title: "No table on this chain yet",
    body: "Nobody has set up a blackjack table for this wallet's chain. If someone gave you a table's address, open this page with ?table= and that address.",
  },
  badTable: {
    title: "That isn't a table's address",
    fromLink: "The link's table isn't an address this chain uses.",
    fromBuild: "This wallet was built with a table address that this chain doesn't use.",
  },
  notATable: "This program isn't a blackjack table that's set up. Check the address.",
  gate: {
    title: "Before you play",
    label: "I am 18 or older.",
    why: "You can watch the table without this. Betting and moving need it. Your answer is kept in this browser only.",
    needed: "Confirm you are 18 or older to play.",
  },
  signIn: {
    none: "Sign in or create a wallet to play. You can watch without one.",
    locked: "Unlock your wallet to play.",
    agent: "This device signs with an agent key, and agent keys only send transfers. Play from an owner device.",
    shared: "This is a shared account, and a shared account can't call a program yet. Play from a personal account.",
  },
  bet: {
    title: "Place a bet",
    label: "Your bet",
    review: "Review the bet",
    seed: "Your browser makes 32 random bytes for this bet, your seed. It is sent with the bet and kept in this browser so you can check the hand later.",
    deposit: "A bet also bonds a small storage deposit. It comes back when the hand is settled.",
  },
  moves: {
    hit: "Hit",
    stand: "Stand",
    claim: "Claim the hand",
    hitDoes: "Take one more card",
    standDoes: "Take no more cards and let the dealer play",
    claimDoes: "The dealer missed its deadline, so this settles the hand in the player's favor",
    relayed: "This hand was bet from your table balance. What it returns goes back there.",
  },
  hand: {
    title: "Your hand",
    none: "You have no open hand.",
    settling: "The hand was just settled. Reading the result…",
    lastTitle: "Your last hand",
    noLast: "No settled hand yet.",
    dealer: "Dealer",
    player: "You",
    playerOther: "Player",
    faceDown: "one card face down",
  },
  review: {
    eyebrow: "Read it twice",
    digest: "The program's record right now. The call runs only while the program still has exactly this code, interface and version. If it changed, the call is refused and nothing is charged.",
    fee: "at most, fuel included",
    total: "at most leaves your account",
    sign: "Sign & send",
    back: "← Back",
    exact: "The exact transaction you'll sign",
  },
  sent: {
    waiting: "Sent. Waiting for a block…",
    applied: "Done.",
    refused: "Your node refused it before it ran, so nothing was charged.",
    failed: "It ran and was refused by the table. Its fee was charged.",
    unsure: "Check the transaction before doing anything else, and don't send it again.",
    see: "See the transaction",
    ok: "OK",
  },
  verify: {
    title: "Check this hand",
    lede: "Your browser recomputes every card from the chain's own records. Nothing here trusts what a server drew.",
    commitment: "Commitment",
    commitmentMatches: "It is the commitment your bet took.",
    commitmentUnknown: "This browser doesn't hold the commitment from the bet (the bet was placed elsewhere, or its storage was cleared), so it can't compare.",
    seed: "Seed",
    seedYours: "This browser made it for this hand.",
    seedUnknown: "Recorded by the chain for this hand.",
    links: "Each link must hash to the one before it. The first hashes to the commitment.",
    noLinks: "No card has been revealed yet.",
    forfeited: "The dealer missed its deadline, so the hand ended by timeout, not by the cards.",
    run: "Check it",
    again: "Check again",
  },
  balance: {
    title: "Table balance (for play from X)",
    lede: "A table balance is FANG you keep at the table so a relayer can play it for you from posts on X. You don't need one to play here.",
    holds: "Your table balance",
    deposit: "Add to it",
    withdraw: "Take it back",
    grantTitle: "Let a relayer play it",
    grant: "A grant lets the relayer place bets and make moves with your table balance, within these limits, until the block you set. It can only ever send your balance back to you. It can't touch your account.",
    relayer: "Relayer's address",
    maxBet: "Most per bet",
    dailyCap: "Most per day",
    blocks: "For how many blocks",
    make: "Review the grant",
    revoke: "End the grant",
    current: "Current grant",
    none: "No relayer has a grant.",
  },
  embed: {
    open: "Play at the table",
    watching: "Watching a hand",
    nothing: "Nothing to show. This link needs a table and a player or a hand.",
    noHand: "This player has no hand to show yet.",
    gone: "That hand is settled. Its cards are on the player's own page.",
  },
  share: {
    title: "Blockula Blackjack",
    description: "One hand against the dealer, played with FANG. Every card can be checked. FANG has no value and can't be bought; no prizes.",
    alt: POSTER_ALT,
    open: "Open the table",
  },
  log: { title: "This hand's log", empty: "Nothing yet." },
  nav: { title: "Blackjack", tag: "program · game", blurb: "One hand against the dealer, played with FANG. Every card can be checked." },
} as const;

/** The rules as the page states them, with the table's limits read from the chain. One paragraph per entry. */
export function rulesText(limits: Limits, blockMs = 1000): { title: string; body: string[] }[] {
  const f = (d: bigint) => `${fang(d)} FANG`;
  return [
    {
      title: "How it's played",
      body: [
        "This is a game played with FANG. " + NOTE,
        "You play one hand against the dealer. Get closer to 21 than the dealer without going over. An ace counts 1 or 11. Jacks, queens and kings count 10.",
        "You get two cards and the dealer shows one. Hit to take a card. Stand to stop. Go over 21 and you bust.",
        "When you stand, the dealer draws until it has 17 or more. It stands on every 17. There is no split, no double and no insurance.",
        "Cards come from an endless deck, so the same card can come up again.",
      ],
    },
    {
      title: "What comes back",
      body: [
        "Win, and twice your bet comes back. Blackjack (an ace and a ten-card as your first two cards) returns two and a half times your bet.",
        "A push (the same total as the dealer) returns your bet. A loss or a bust returns nothing.",
        "A hand played from your account is paid to your account as soon as it's settled.",
      ],
    },
    {
      title: "This table's limits",
      body: [
        `A bet is from ${f(limits.minBet)} to ${f(limits.maxBet)}, and never more than 1/${limits.bankrollDivisor} of the house's free bankroll.`,
        `An account can bet up to ${f(limits.dailyCap)} a day here. A day is ${limits.dayBlocks.toLocaleString("en-US")} blocks.`,
        `Up to ${limits.maxOpenGames} hands can be open at once, and each account plays one hand at a time.`,
        "The program holds these limits. This page reads them from the chain.",
      ],
    },
    {
      title: "Time limits",
      body: [
        `You have ${blocksWords(limits.idleBlocks, blockMs)} to move. After that your hand stands by itself. Anyone can trigger that, and the dealer then finishes the hand.`,
        `The dealer has ${blocksWords(limits.revealBlocks, blockMs)} to reveal each card it owes. If it doesn't, you can claim the hand. It then returns two and a half times your bet if blackjack was still possible, and twice your bet if not.`,
      ],
    },
    {
      title: "Why the cards are fair",
      body: [
        "Before any bet, the dealer picks a secret and hashes it 32 times. It publishes only the last hash, the commitment. That fixes the whole chain.",
        "When you bet, your browser adds 32 random bytes of its own, your seed. Each card is made from the next link of the dealer's chain and your seed together, so neither side can choose a card.",
        "To deal a card the dealer reveals a link. The program checks that it hashes to the link before it. The dealer's hidden card is a link nobody has seen until you stand.",
        "“Check this hand” redoes all of it in your browser: the links, the cards, the outcome and what it returned.",
      ],
    },
    {
      title: "Fees",
      body: ["Each bet, hit and stand is a transaction from your account, signed by your passkey. Each pays the chain's fee. You see the most it can cost before you sign."],
    },
  ];
}
