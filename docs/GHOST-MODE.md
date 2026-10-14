# Ghost Mode (shielded transfers)

> **NOT AUDITED. DISABLED AT GENESIS.** This is a new shielded pool on a new proof system. A
> soundness bug in a shielded pool is a silent infinite mint (Zcash Sprout 2018, Orchard
> CVE-2026-54496). Nodes must reject every ghost transaction until governance runs
> `ActivateFeature { ghost }` after the gates in section 9. Nothing here has had external review.

Ghost Mode is an Orchard-style shielded pool: notes are Poseidon2 commitments in a Merkle tree,
spending one publishes a nullifier, a hash-based STARK proves the spend is valid, and the note
contents are encrypted to the recipient with ML-KEM-768. Everything is post-quantum: no
pairings, no elliptic curves.

This document is the design contract for MAINNET-DESIGN.md §6 (DEC-2). Its library is the
`blockula-ghost` crate in `node/ghost/` (P-GHOST-CORE). Node integration (tags 50–54, state,
turnstile, fees) is a later package; section 8 is its plan.

| | |
|---|---|
| Proof system | Plonky3 `=0.8.0`, uni-stark, KoalaBear (p = 2³¹ − 2²⁴ + 1) with a degree-4 challenge extension, hiding FRI |
| Hash | Poseidon2, width 16, Plonky3's KoalaBear parameters (8 full + 20 partial rounds, x³) |
| Circuit | One Action: 2 spends + 2 outputs, dummies allowed, 128 rows × 261 columns, 453 constraints, degree 3 |
| Security | 100 bits proven (list-decoding regime), 110 conjectured, from Plonky3's calculator |
| Proof | 126–132 KiB; prove ~64 ms, verify ~6.3 ms (one core, Apple M4 Max) |
| Note encryption | ML-KEM-768 (`ml-kem =0.3.2`) + XChaCha20-Poly1305; 1,481 bytes per output |

## 1. Keys and addresses

Everything derives from one 32-byte seed (a wallet seed or a passkey PRF output, through the
wallet's own KDF; `SpendingKey::from_master(seed, account)` gives independent accounts).

```text
seed ──BLAKE3──> sk (8 field elements) ──P2──> ak ─┐
                                      └─P2──> nk ─┴─P2──> owner
seed ──BLAKE3──> ML-KEM seed (64 B) ──ML-KEM-768 KeyGen──> (ek 1,184 B, dk)
seed ──BLAKE3──> ovk (32 B)
address = owner (32 B) ‖ ek (1,184 B) = 1,216 B
```

| Key | Derivation | Lets the holder |
|---|---|---|
| `sk` spending key | `BLAKE3-KDF("blockula ghost v1 spending key", seed)` → 8 elements | spend (the proof shows knowledge of `sk`) |
| `ak` | `H_ak(sk)` | nothing alone; part of `owner` |
| `nk` nullifier key | `H_nk(sk)` | compute nullifiers, so see which notes are spent |
| `owner` | `H_owner(ak ‖ nk)` | goes in every note commitment; public in the address |
| KEM key pair | `ML-KEM-768.KeyGen(BLAKE3-KDF("blockula ghost v1 kem seed", seed))` | decrypt incoming notes |
| `ovk` | `BLAKE3-KDF("blockula ghost v1 outgoing viewing key", seed)` | decrypt notes this wallet sent |

**Which KDF and why.** Derivations the circuit never sees (seed → `sk`, KEM seed, `ovk`, account
seeds, note `rcm`) use BLAKE3 in key-derivation mode with versioned context strings: it's
standard, fast, and what the rest of Blockula already uses. Derivations the circuit must
recompute (`ak`, `nk`, `owner`) use the Poseidon2 sponge, because BLAKE3 inside a STARK over a
31-bit field costs far more constraints. Bytes become field elements by reading little-endian
u64s from the BLAKE3 XOF and reducing mod p (distance from uniform < 2⁻³² per element).

**Viewing keys.** Full viewing key `(ak, nk, kem seed, ovk)`, 161 bytes: everything, including
spent status; it can't spend because that needs a preimage of `ak`. Incoming viewing key
`(owner, kem seed)`, 97 bytes: incoming notes only. Outgoing viewing key, 32 bytes. Each
encoding starts with a version byte (1). A holder of the full key can check it matches an
address (`owner = H(ak ‖ nk)`). These are user-controlled; a protocol auditor key would be a
policy and legal decision, not a technical one.

**Addresses.** `owner ‖ ek`, 1,216 bytes; decoding requires a canonical `owner` and an
encapsulation key that passes FIPS 203 validation. That's unusable as text, so `SetGhostAddress`
publishes it under the account's `.boo` name and UIs show names. People compare a fingerprint:
80 bits of `BLAKE3-KDF("blockula ghost v1 address fingerprint", address)` as `xxxx-xxxx-xxxx-xxxx-xxxx`.
ML-KEM keys can't be diversified, so each unlinkable address is its own account.

**Session keys and Familiars can't spend shielded funds.** Spending needs `sk` and the circuit
has no spend limit; agents get viewing keys at most.

## 2. Hashing

`P` is Plonky3's width-16 KoalaBear Poseidon2 permutation (`default_koalabear_poseidon2_16`).
Digests are 8 elements (248 bits, ~124-bit collision resistance), encoded as 8 little-endian
u32s; decoding rejects any element ≥ p, so every digest has one encoding.

- **Sponge** `H_d(x)` for a fixed-length input: the capacity (state[8..16]) starts at the domain
  IV, each 8-element chunk overwrites the rate (state[0..8], zero-padded), then `P`; the output is
  the rate after the last chunk. The IV is
  `BLAKE3-KDF("blockula ghost v1 poseidon2 sponge iv", "<name>/<input length>")`.

  | Domain | Input | Chunks |
  |---|---|---|
  | `ak` | `sk` (8) | 1 |
  | `nk` | `sk` (8) | 1 |
  | `owner` | `ak ‖ nk` (16) | 2 |
  | `note-commitment-v1` | `owner ‖ value limbs ‖ rcm` (20) | 3 |
  | `nullifier` | `nk ‖ cm ‖ pos_lo ‖ pos_hi` (18) | 3 |

- **Compression** (tree nodes): `C(l, r) = Trunc₈(P(l ‖ r) + (l ‖ r)) = P(l ‖ r)[0..8] + l`,
  Poseidon2's compression mode. The feed-forward means inverting `P` gives neither preimages nor
  collisions.

Every name carries a version, so a later pool can change any of it without ambiguity. The
Poseidon2 instance is the stock one; whether its round margin is enough given the 2026
cryptanalysis (eprint 2026/1692 reaches 28 of 31 rounds of a KoalaBear width-24 instance in the
CICO setting) is an activation gate (section 9).

## 3. Notes, commitments, nullifiers

A note is `(value: u64, owner, rseed: 32 bytes)`, note format version 1 (in the plaintext and in
the commitment domain).

- `rcm = BLAKE3-KDF("blockula ghost v1 note rcm", rseed)` → 8 elements (~248 bits of hiding).
- `cm = H_note-commitment-v1(owner ‖ limbs(value) ‖ rcm)`, where `limbs(v)` is four 16-bit limbs,
  least significant first (a u64 doesn't fit the field).
- `nf = H_nullifier(nk ‖ cm ‖ pos_lo ‖ pos_hi)`, the position as two 16-bit halves.

Nullifiers are position-bound (Sapling's fix): a sender can create two notes with the same
commitment, and both stay spendable with different nullifiers ("faerie gold"). Without `nk`
nobody can link a nullifier to its commitment.

## 4. Commitment tree

Depth 32 (2³² notes), leaves are note commitments, empty leaves are
`BLAKE3-KDF("blockula ghost v1 uncommitted leaf")`, and empty subtrees hash up from it.
Bit `h` of a leaf's position says whether its ancestor at height `h` is a right child.

- `Frontier` (what the node stores): the leaf count and one pending left node per level, O(32)
  work per append. Its encoding (`8 + 32 × 32` bytes) zeroes unused slots, so it's canonical.
- `RootHistory`: the last `ghost_anchor_window` roots; a spend's anchor must be one of them.
- `FullTree` (wallets, tests): keeps every node and serves authentication paths.

## 5. The Action circuit

### 5.1 Statement

One proof covers two spends and two outputs. Public values (54 field elements, in this order):

| Field | Elements | Meaning |
|---|---|---|
| `anchor` | 8 | tree root the real spends are in |
| `nf[0], nf[1]` | 16 | spend nullifiers |
| `cm[0], cm[1]` | 16 | output note commitments |
| `vb_neg` | 1 | 1 if the value balance is negative |
| `vb` | 4 | \|value balance\| as 16-bit limbs |
| `sighash` | 8 | `BLAKE3-KDF("blockula ghost v1 sighash", sighash bytes)` |
| `circuit id` | 1 | `CIRCUIT_ID` = 1 |

The value balance is `Σ spent − Σ output`: the fee for a transfer, `amount + fee` for an
unshield, `−amount` for a shield. The API type `ValueBalance` is `±(2⁶⁴ − 1)` with a canonical
zero (`vb_neg = 0`).

The prover knows, for each spend, `sk`, a note `(value, rseed)`, a position and 32 siblings, and
a dummy flag; for each output, `(owner, value, rseed)`; such that:

- **C1** every row is one Poseidon2 permutation (`out = P(in)`);
- **C2** `ak = H_ak(sk)`, `nk = H_nk(sk)`, `owner = H_owner(ak ‖ nk)` with the same `sk`;
- **C3** `cm = H_note-commitment-v1(owner ‖ limbs ‖ rcm)` for spends and outputs;
- **C4** the spend's `cm` hashes up its path to `anchor`, unless the spend is a dummy;
- **C5** `nf = H_nullifier(nk ‖ cm ‖ position)`, with the position exactly the path bits;
- **C6** every value limb is 16 bits, so every value is a u64; a dummy spends exactly 0;
- **C7** `Σ spent = Σ output + vb` over the integers;
- **C8** output commitments and nullifiers equal the public ones;
- **C9** the sighash binds the proof to one transaction.

### 5.2 Layout

Every row proves one permutation with Plonky3's `Poseidon2Air` (columns 0..164: input, round
states, output) through a `SubAirBuilder`, so the permutation constraints are Plonky3's own,
reused rather than rewritten. A fixed schedule says what each row's permutation is for:

```text
rows   0..42   spend 0   NK AK OWN_A OWN_B CM_A CM_B CM_C MERKLE×32 NF_A NF_B NF_C
rows  42..84   spend 1   (same)
rows  84..90   outputs   CM_A CM_B CM_C (×2)
rows  90..128  padding   input 0; row 127 also checks the value balance
```

The schedule is encoded in 35 periodic columns of period 128 (one full trace), which the
verifier evaluates itself: nothing about the schedule is committed or chosen by the prover. The
97 extra trace columns are the Merkle path bit, spend registers (`nk`, `cm`, position
accumulators, dummy flag) that are constant (or accumulate) within a spend block, the four
balance accumulators, 64 value bits and 9 carry bits.

Hiding needs `height ≥ 2 × (queries + 4 × opening points) = 112`, so 90 used rows pad to 128.

### 5.3 Constraints

Every constraint is a selector (a periodic column, degree 1) times an expression of degree ≤ 2,
except the Poseidon2 S-box constraints, which are degree 3 and unselected. Max degree 3.

| | Constraint (row selector: expression = 0) |
|---|---|
| Sponge | START: `in[8+j] = IV_j`; CONT_NEXT: `next.in[8+j] = out[8+j]` |
| C2 | NK: `reg_nk = out[0..8]` and `next.in[0..8] = in[0..8]` (AK hashes the same `sk`); AK: `next.in[0..8] = out[0..8]`; OWN_B: `in[0..8] = reg_nk`, `next.in[0..8] = out[0..8]` (owner into the commitment) |
| C3 | CM_C: `in[4..8] = 0` (padding of the 20-element input) |
| C4 | spend CM_C: `reg_cm = out`, and level 0's input holds `out` on the side the bit picks: `next.in[j] − out[j] + bit'·(next.in[8+j] − next.in[j])`; MERKLE (levels 0..30): the same with node `out[j] + in[j]`; ROOT: `(1 − dummy)·(out[j] + in[j] − anchor[j])`; `bit ∈ {0,1}`, `bit = 0` off Merkle rows |
| C5 | NF_A: `in[0..8] = reg_nk`; NF_B: `in[0..8] = reg_cm`; NF_C: `in[0] = pacc_lo`, `in[1] = pacc_hi`, `in[2..8] = 0`; NF_C of spend i: `out[0..8] = nf[i]` |
| Registers | `(1 − SPEND)·reg = 0`; CARRY: `next.reg = reg`; block start: `pacc = 0`; CARRY: `next.pacc = pacc + w·bit` with weights 2⁰..2¹⁵ for each half; `dummy ∈ {0,1}` |
| C6 | `vbits ∈ {0,1}`; `Σᵢ vbits[16k+i]·2ⁱ = VALUE·in[k]` (so zero bits off value rows); spend value row: `dummy·in[k] = 0` |
| C7 | first row: `acc = 0`; transitions: `next.acc[k] = acc[k] + VSIGN·in[k]`; `cbits ∈ {0,1}`, zero off the balance row; balance row: `acc[k] − (1 − 2·vb_neg)·vb[k] + c[k−1] − c[k]·2¹⁶ = 0` with `c[k] = bits − 4 ∈ [−4, 3]`, `c[−1] = c[3] = 0` |
| C8 | output CM_C j: `out[0..8] = cm[j]`; padding: `in = 0` |
| Public | `vb_neg ∈ {0,1}`; `circuit id = 1` |

Constraints that read the next row are gated by selectors that are zero on the last row (a unit
test checks this), so nothing wraps around to row 0.

**Why the balance can't wrap (C7).** Limbs are proven 16-bit, so `acc[k] ∈ [−2(2¹⁶−1), 2(2¹⁶−1)]`
and `vb[k] ∈ [0, 2¹⁶)` (public values come from a u64). With carries in `[−4, 3]`, every
balance-row equation has magnitude below 2²⁰ ≪ p, so it holds over the integers, not just mod p.
Summing them with weights `2^(16k)` gives `Σ spent − Σ output − vb = c[3]·2⁶⁴ − c[−1] = 0`.
Honest carries lie in `[−3, 3]`.

**Dummies.** The dummy flag is per spend block. A dummy skips C4 only, and only if all four of
its limbs are zero (checked limb by limb: gating on the value as one field element would let a
value ≡ 0 mod p through). Its nullifier is computed as usual from a random key, so it looks like
any other. Dummy outputs are zero-value notes to a throwaway address (`dummy_output`). A
non-boolean dummy flag only makes things stricter (≠ 1 enforces C4, ≠ 0 forces value 0); it's
constrained anyway.

**C9.** The sighash isn't used by any constraint. Plonky3 absorbs every public value into the
Fiat–Shamir transcript before drawing the first challenge, so a proof for one sighash fails for
any other (tested), and producing a new proof needs the witness. The proof is the spend
authorization; there is no separate signature.

**Outside the circuit** (the verifier or the node): `nf[0] ≠ nf[1]` (`verify` checks it), each
nullifier unspent, the anchor a recent root (this also pins it for an action whose spends are
both dummies, which the circuit leaves free), the value balance matching the tag, the turnstile.

### 5.4 Degrees of freedom

With the anchor, value balance and sighash fixed, the only cells a prover may choose are listed
in `degrees_of_freedom` (`src/circuit/tests.rs`), and the sweep proves the list is exact:

- a real spend: none. Its note, position and therefore its nullifier are determined by the tree.
- an output: the owner and the 8 `rcm` elements (the note it creates).
- a dummy spend: `sk`, `rcm`, the siblings and the path bits.

### 5.5 Proof system and parameters

`StarkConfig<HidingFriPcs<KoalaBear, Radix2DitParallel, MerkleTreeHidingMmcs<…>, ExtensionMmcs<…>, StdRng>, BinomialExtensionField<KoalaBear, 4>, DuplexChallenger<Poseidon2, 16, 8>>`.
Commitments use Poseidon2 (`PaddingFreeSponge` leaves, `TruncatedPermutation` nodes) with 4
salt elements per leaf.

| Parameter | Value |
|---|---|
| FRI blowup | 2⁴ (rate 1/16) |
| Queries | 48 |
| Proof of work | 16 bits before queries, 16 before the batching challenge, 16 before the out-of-domain point |
| Folding | binary, stop at degree 32 |
| Random codewords (hiding) | 4 |
| Quotient chunks | 8 |
| Batched functions | 602 |
| **Proven security** | **100 bits** (list decoding; unique decoding 59) |
| **Conjectured security** | **110 bits** |

Numbers from `p3_uni_stark::{ProvenSecurity, ConjecturedSecurity}` over the real AIR shape
(`config::security_report`, checked by a test). The ceiling is the ~124-bit challenge field:
across blowups 2²–2⁵, query counts up to 100 and grinding up to 20 bits, nothing exceeded ~110
proven or ~111 conjectured. 128 bits would need a degree-8 extension (bigger proofs).

### 5.6 Proof encoding and verification

Proof bytes are the postcard encoding of `p3_uni_stark::Proof`. `verify(public, bytes)`:

1. rejects more than `MAX_PROOF_BYTES` (192 KiB) and equal nullifiers;
2. decodes strictly: field elements must be canonical, and the bytes must re-encode to exactly
   themselves (no trailing bytes, no overlong varints), and the height must be ours;
3. runs Plonky3's verifier under `catch_unwind`. Plonky3 says its verifier "may panic on certain
   malformed proofs"; 0.8's transcript drivers yield to an in-flight panic, so it can be caught.
   The node's build must keep `panic = "unwind"`.

Never panics on any input: 20,000 random and 20,000 mutated proofs, a bit flipped in every
byte of a proof (section 10), none panicked. `prove` checks every constraint before proving, so
an invalid witness gets an error, not an invalid proof. It draws the masks from OS randomness
each time; `prove_with_seed` exists for tests only.

## 6. Note encryption

```text
(ct_kem, ss)  = ML-KEM-768.Encaps(ek)                                   1,088 B
key ‖ nonce   = BLAKE3-KDF("blockula ghost v1 note encryption", ss ‖ ct_kem ‖ cm)
enc           = ct_kem ‖ XChaCha20-Poly1305(key, nonce, aad = cm,
                         version ‖ value ‖ rseed ‖ memo (256 B))        1,401 B
ock ‖ nonce'  = BLAKE3-KDF("blockula ghost v1 outgoing cipher key", ovk ‖ cm ‖ ct_kem)
out           = XChaCha20-Poly1305(ock, nonce', aad = cm, owner ‖ ss)      80 B
```

- Each key encrypts one message, so the nonce is derived, not sent. The memo is fixed-size so
  lengths leak nothing. Without an `ovk` (dummy outputs), `out` is random bytes.
- The ciphertext carries nothing about the recipient (no key ids or tags). Whether ML-KEM's
  anonymity proofs for Kyber (eprint 2022/1696) carry over to FIPS 203 is for the auditors.
- **Robustness.** Poly1305 isn't key-committing, so a recipient accepts a note only after
  recomputing its commitment from the plaintext and finding the on-chain `cm`.
- **Trial decryption.** `TrialDecryptor` keeps the expanded ML-KEM key and tries
  `(cm, enc)`: decapsulate (implicit rejection gives a foreign ciphertext a pseudorandom secret),
  derive, open, parse, recompute `cm`. ~36 µs per output natively.
- **Outgoing recovery.** `try_decrypt_outgoing(ovk, cm, enc, out)` recovers `(owner, ss)`, then
  the note.
- **Scanning cost** is the main UX risk: every wallet downloads ~1.5 KB and decapsulates once per
  output. At the per-block cap (section 8) that's ~14 outputs a second. Later mitigations:
  symmetric channels after first contact, opt-in delegated detection.

## 7. API (crate `blockula-ghost`)

| Item | |
|---|---|
| `SpendingKey`, `FullViewingKey`, `IncomingViewingKey`, `OutgoingViewingKey`, `Address` | keys and encodings (section 1) |
| `Note`, `nullifier` | notes (section 3) |
| `Frontier`, `RootHistory`, `FullTree`, `MerklePath` | tree (section 4) |
| `ActionWitness`, `SpendWitness::{new, dummy}`, `check_witness`, `prove`, `verify`, `PublicInputs`, `ValueBalance` | the circuit (section 5) |
| `encrypt_note`, `TrialDecryptor`, `try_decrypt_incoming`, `try_decrypt_outgoing`, `dummy_output` | encryption (section 6) |
| `circuit::config::security_report` | the soundness numbers |

The crate is its own workspace; the node will depend on it by path. Plonky3 is `=`-pinned.

## 8. Node integration plan (later package)

**State** (all in the authenticated store, so in the state root):

| Namespace | Key → value | Notes |
|---|---|---|
| `ghost/cm` | position → `cm` | append-only, for wallet sync |
| `ghost/frontier` | () → `Frontier` bytes | O(32) per append |
| `ghost/root` | root → height | the last `ghost_anchor_window` (e.g. 1,024) blocks' roots |
| `ghost/nf` | nullifier → height | grows forever; spends prepay it through fees |
| `ghost/pool` | () → u128 | the turnstile |
| `features/ghost` | () → `{ active, paused, circuit_id, pool_cap, max_block_bytes, verify_fee, pause_votes }` | governance and validators |
| account | + optional `ghost_address` (1,216 B) | `SetGhostAddress` |

**Actions** (tags reserved in MAINNET-DESIGN §3.1; 55–59 stay reserved for a pool migration and
The Crypt):

| Tag | Action | Signed by | Value balance | Effect |
|---|---|---|---|---|
| 50 | `Shield { amount, bundle }` | `from` (normal Tx v3) | `−amount` | debit `amount` + fee transparently; `ghost_pool += amount`; fails above `pool_cap` |
| 51 | `PrivateTransfer { bundle }` | nobody: the proof authorizes | `fee + verify_fee` | fee paid from the pool: `ghost_pool −= fee` |
| 52 | `Unshield { to, amount, bundle }` | nobody | `amount + fee` | `ghost_pool −= amount + fee` (fails if it would go negative); credit `to` |
| 53 | `SetGhostAddress { address }` | account owner key | | publish the 1,216-byte address under the name |
| 54 | `GhostPause {}` | an active validator | | once f+1 distinct validators of the epoch have signed, ghost actions fail from the next block until `ActivateFeature { ghost }` |

`bundle = { anchor, nf[2], cm[2], enc[2] (1,401 B each), out[2] (80 B each), proof }`. The node
derives the public `ValueBalance` from the tag and fees, never from the sender.

**Sighash** (proposal for P-WIRE): `BLAKE3-KDF("blockula ghost v1 sighash", chain_id ‖ tag ‖
valid_until ‖ fee ‖ tip ‖ tag fields (from, nonce, amount, to) ‖ anchor ‖ nf ‖ cm ‖ enc ‖ out ‖
CIRCUIT_ID)`: everything in the transaction except the proof. Binding the ciphertexts stops a
relayer from swapping them (which would make a note unfindable).

**Unsigned transactions (51, 52).** No `from`, nonce, public key or signature; `check_format`
accepts exactly these tags unsigned. Replay protection is the nullifiers; `valid_until` lives in
the sighash. The mempool de-duplicates by nullifier, keeping the higher tip per byte. A proposer
that includes a ghost tx that then fails pays `base_fee × size` (CV-5), which is what stops free
junk.

**Limits.** Ghost txs (tags 50–52) up to `MAX_GHOST_TX_BYTES` = 256 KiB (a full Action is
~135 KiB: proof ≤ 132 KiB + 2,962 bytes of ciphertexts + header, + 5.3 KB of ML-DSA for a
shield); at most `ghost.max_block_bytes` = 1 MiB of ghost txs per block, i.e. ~7 per block. That
bounds verification at ~45 ms of one core per block and keeps transparent txs from being crowded
out. The API needs the binary wire form on `/api/tx` (JSON hex would double the size).

**Fees.** The usual byte fee (base burned, tip to the proposer) plus a flat `ghost.verify_fee`
for verifier CPU (PLAN.md's "verification premium"). For 51–52 all of it comes out of shielded
value through the value balance.

**Pipeline.**
- Admission: ghost active and not paused; size limits; `verify` once (on a blocking thread),
  cached by tx id; anchor in `ghost/root`; nullifiers distinct, not in `ghost/nf` or the mempool;
  value balance matches the tag; turnstile and pool cap.
- Consensus `verify`: proofs not already cached. Stateful checks run at execution and through
  CV-5's ancestor replay.
- Execution: re-check, append both output commitments (dummies too) to the frontier, insert both
  nullifiers, move value, update `ghost_pool`, record the root at end of block.
- `State::check_invariants()`: `supply = Σ balances + escrows + stakes + pools + ghost_pool`.

**Turnstile and pause.** The turnstile (ZIP 209's rule) bounds a soundness bug to theft of
honest shielded deposits rather than unbounded supply; the pool cap bounds those. `GhostPause`
gives a fast stop without an admin key: f+1 validators can already halt the chain, so it adds
no power, and it can only stop the pool, never move funds. Recovery from a suspected bug follows
Zcash: freeze the pool to withdraw-only and migrate to a new versioned pool through the
turnstile.

**Clients.** CLI `blockula ghost shield|send|unshield|scan|export-viewing-key`; SDK
`@blockula/sdk/ghost` via a WASM build of this crate; MCP `ghost_balance`, `ghost_send` (capped by
`BLOCKULA_MAX_SEND`). No delegated proving by default (the prover sees the witness). The explorer
shows "👻 a ghost transaction happened" with nullifiers, fee, proof size and the pool balance.

## 9. Activation gates

From MAINNET-DESIGN §6. `ActivateFeature { ghost }` only after all of:

1. A dedicated external ZK and cryptography audit of this crate, the Plonky3 0.8 components it
   uses and the node integration, with no open high or critical findings.
2. A clean under-constraint sweep and verifier fuzzing on the audited code (section 10), plus an
   LLM-assisted adversarial circuit review (what found the Orchard bug).
3. A cryptanalyst's sign-off on the Poseidon2 instance and its round margin.
4. At least 4 weeks on a public testnet with a public bug bounty on the pool.
5. Pool cap, turnstile and validator pause live and tested.
6. Counsel sign-off (EU AMLR anonymity-enhancing-coin rules from July 2027).

Activation pins `CIRCUIT_ID`; any later circuit change is a `ScheduleUpgrade`.

## 10. Measured numbers

`cargo run --release --example measure` (Apple M4 Max, one core; Plonky3 without its
`parallel` feature):

| | |
|---|---|
| Trace | 128 rows × 261 columns, 453 constraints, max degree 3 |
| Proof size | 129,319–135,016 bytes (126–132 KiB) over 20 proofs; opened values 8.9 KB, FRI and Merkle paths ~120 KB |
| Prove | 64 ms median (constraint check included) |
| Verify | 6.3 ms median |
| Security | 100 bits proven, 110 conjectured |
| Note encryption | 34 µs |
| Trial decryption | 36 µs (not ours), 38 µs (ours) |
| Frontier append / root | 0.55 µs / 18 µs |
| Full ghost tx | ~135 KiB (fits 256 KiB; ~7 per 1 MiB block) |

Parameter study (`explore_security_parameters`, ignored test): blowup 2⁵ with fewer queries
tops out at 98 bits proven; folding arity 4 or 8 saved 7–12 KiB but cost 2–3 proven bits; stopping
FRI at degree 32 instead of 1 saved ~10 KiB for free. Most of the proof is the 261-column trace
opened at 48 queries; a narrower layout (one round per row) would roughly halve it at the cost
of a more complex AIR.

## 11. Tests

`cargo test --manifest-path node/ghost/Cargo.toml` (~15 s):

- **Known answers** (`tests/kat.rs`): Plonky3's Poseidon2 vector, the domain IVs, keys, address,
  note commitment, nullifier, empty and populated tree roots, the sighash map, note encryption
  with a fixed RNG, and a full proof with fixed masks. A second implementation must reproduce
  them; a change here is a consensus change.
- **Completeness**: every dummy pattern, zero-value and u64::MAX notes, shields, unshields, limb
  carries, duplicate commitments, random actions.
- **Soundness, negative**: inflated output (by 1 and by 2⁴⁰), value balance off by one or with
  the wrong sign, limb of 2¹⁶, limb p − 1 (field wrap) with and without non-boolean bits, 2⁶³ +
  2⁶³ wrapping to 0, minting exactly p drops with field carries, accumulator not starting at 0,
  wrong sibling at levels 0/7/31, flipped position bit, wrong anchor, wrong nullifier (another
  position, off by one), wrong output commitment, someone else's note, spending with a viewing
  key, dummy with value, real note marked dummy, the same note or dummy twice, non-boolean path
  bit at a duplicated leaf, non-boolean sign, wrong circuit id, sighash and every public input
  swapped or changed. Each builds the cheating prover's most consistent trace, checks the
  constraints reject it, then forces a proof out of it and checks the verifier rejects that.
- **Under-constraint sweeps**: (a) every cell of valid traces (two real spends; dummies) changed
  by +1 and by a random amount: every change breaks a constraint on its row or the one before;
  (b) every permutation input and register cell changed with everything downstream re-derived
  (as a cheating prover would), the whole trace checked: only the declared degrees of freedom
  (5.4) survive, and each of those really is free; (c) every public value is pinned.
- **Mutation analysis** (`tools/mutation_check.py`, not in CI): each of 37 constraint groups is
  deleted in turn and the suite rerun. 36 deletions are caught. The survivor, dummy booleanity,
  is unexploitable (5.3, Dummies) and stays as defense in depth.
- **Proof robustness** (`tests/proof_robustness.rs`): bit flips (every byte of the head and tail,
  600 random), truncation at 70 lengths, extension, an overlong varint, oversize, proofs swapped
  between statements, random byte strings and structured mutations. `GHOST_FUZZ_ITERS=20000`
  soaks: 20,000 + 20,000 inputs, 0 panics. `every_byte_flip_is_rejected` (ignored, slow) flips a
  bit in every byte of a proof.
- **Encryption** (`tests/encryption.rs`): round trip, sizes, other keys, tampering in each
  region, wrong `cm`, outgoing recovery, no-ovk noise, wrong recipient, freshness, random garbage.
- **cargo-fuzz** (`node/ghost/fuzz`): `verify_proof` and `decoders`. The nightly installed while
  writing this (2026-10-13) hits a compiler panic in `p3-monty-31`, so they haven't run yet.

## 12. Known limitations

- **Not audited**, and Plonky3's only audit (Least Authority, 2024) predates 0.8 by many breaking
  releases. Its zero-knowledge is opt-in and "statistical", and Plonky3 says compositions like
  this one "need their own complete protocol analysis".
- **Poseidon2 margin**: the stock instance, no extra rounds (section 2).
- **Security** is 100 bits proven / 110 conjectured, capped by the challenge field; 128 would need
  a degree-8 extension.
- **2-in/2-out only.** Larger payments need several actions (and the node a bundle format whose
  value balances add up).
- **Scanning** costs every wallet ~1.5 KB and one decapsulation per output.
- **Memory hygiene**: secrets aren't zeroized.
- **No WASM or TypeScript yet**; the KATs are the contract for them.
- **Timing side channels** in proving and trial decryption haven't been analyzed.
