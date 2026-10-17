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
`blockula-ghost` crate in `node/ghost/` (P-GHOST-CORE). The node integrates it in
`node/src/ghost.rs` (P-GHOST-INT: tags 50–54, state, turnstile, fees, pause), switched off until
governance activates it; section 8 describes what was built. Background, prior art and the
choice of stack: `.context/research/ghost-mode.md`.

| | |
|---|---|
| Proof system | Plonky3 `=0.8.0`, uni-stark, KoalaBear (p = 2³¹ − 2²⁴ + 1) with a degree-4 challenge extension, hiding FRI |
| Hash | Poseidon2, width 16, Plonky3's KoalaBear parameters (8 full + 20 partial rounds, x³) |
| Circuit | One Action: 2 spends + 2 outputs, dummies allowed, 128 rows × 261 columns, 453 constraints, degree 3 |
| Security | 100 bits proven (list-decoding regime), 110 conjectured, from Plonky3's calculator |
| Proof | 126–132 KiB; prove ~60 ms, verify ~6.3 ms (one core, Apple M4 Max) |
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

Every IV, KDF context and domain is versioned (`v1`), so a later pool can change any of it
without ambiguity. The
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

- `Frontier` (what the node stores): the leaf count and one pending left node per height
  0..=32, O(32) work per append. Slot `h` is set exactly when bit `h` of the count is, so slot
  32 is set only in a full tree (2³² leaves) and holds its root: the last append folds every
  pending node into it. Its encoding (`8 + 33 × 32` = 1,064 bytes) requires the unused slots to
  be zero, so it's canonical; a count above 2³² is rejected.
- **A full tree.** Every action appends exactly two commitments (dummies included), so once the
  tree is full the pool is frozen: every further action fails with `TreeFull`. A migration to a
  new pool through the turnstile has to happen well before that (2³¹ actions). The frontier
  still reports the full tree's true root rather than refusing the last leaf, because that
  keeps node and wallets on one rule: `Frontier` and `FullTree` hold the same 2³² leaves and
  agree on the root at every size (an earlier version dropped the root on the last append and
  reported the empty tree's). And since 2³² is even, an action's two appends either both fit
  or the first fails, so an action is never half-appended. Refusing leaf 2³² − 1 instead would
  make the usable size odd: the last action could fit one output but not the other.
- `RootHistory`: the last N roots; a spend's anchor must be one of them. (The node keeps the
  equivalent by age instead: a root stays usable for `ghost_anchor_window` blocks after a newer one
  replaces it, section 8.)
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
(`config::security_report`, checked by a test). The ceiling comes from the ~124-bit challenge
field: across blowups 2²–2⁵, query counts up to 100 and grinding up to 20 bits, nothing exceeded
110 proven or 112 conjectured (100 queries would buy 110 proven bits for roughly twice the
proof). 128 bits would need a degree-8 extension (bigger proofs).

### 5.6 Proof encoding and verification

Proof bytes are the postcard encoding of `p3_uni_stark::Proof`. `verify(public, bytes)`:

1. rejects more than `MAX_PROOF_BYTES` (192 KiB) and equal nullifiers;
2. decodes strictly: field elements must be canonical, and the bytes must re-encode to exactly
   themselves (no trailing bytes, no overlong varints), and the height must be ours;
3. runs Plonky3's verifier under `catch_unwind`. Plonky3 says its verifier "may panic on certain
   malformed proofs"; 0.8's transcript drivers yield to an in-flight panic, so it can be caught.
   The node's build must keep `panic = "unwind"`: the crate refuses to compile otherwise
   (`compile_error!` on `cfg(panic)`; WASM, used only by wallets, is exempt).

No input tried so far makes it panic: 20,000 random and 20,000 mutated proofs and a bit flipped
in every byte of a proof (section 11). `prove` checks every constraint before proving, so
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

The crate is its own workspace; the node will depend on it by path. Plonky3 is `=`-pinned, and
so is `zeroize`.

**Build profile.** Cargo applies only the root workspace's profiles, so the crate's own
`[profile.release] overflow-checks = true` stops applying the moment the node depends on it:
the node's profile governs `blockula-ghost` and every `p3-*` crate, and the node today turns
overflow checks on for `blockula` only. Every robustness number in section 11 was measured with
overflow checks on everywhere, Plonky3 included, where an overflow on a malformed proof panics
and `verify` returns `Panicked`; without them it would wrap and run on down an untested path.
So `node/Cargo.toml` must have, when it adds the dependency:

```toml
[profile.release]
overflow-checks = true   # blockula, blockula-ghost and Plonky3; at minimum
                         # [profile.release.package.blockula-ghost] plus each p3-* crate
# panic stays "unwind" (the default); blockula-ghost won't compile with "abort"
```

and the proof-robustness soaks should be re-run from the node workspace. The node now has this,
per crate (`[profile.release.package.blockula-ghost]` and one entry for each of the 20 `p3-*`
crates in its lockfile), and `ghost::tests::release_profile_covers_plonky3` fails if a Plonky3
upgrade brings a crate that isn't listed or if `panic = "abort"` appears. The crate's own
correctness doesn't depend on the profile: its arithmetic on untrusted input (tree sizes and
positions, value balances, proof and ciphertext lengths) is explicit (`try_from`, `i128` sums,
length checks before slicing), so nothing in it can wrap in either profile.

## 8. Node integration (built, disabled)

`node/src/ghost.rs`, with hooks in `types.rs` (encodings), `chain.rs` (admission, execution,
mempool), `consensus.rs` (size limits, proofs before voting), `state/` (records) and `api.rs`.
Every ghost action is refused ("Ghost Mode is off") until governance runs
`ActivateFeature { ghost }` (upgrade.rs); a mainnet genesis can't list the feature
(`Genesis::check`), and `ghost_pool_cap` starts at 0, so an activated pool still takes no
deposits until governance raises the cap too.

**State** (all in the authenticated store, so in the state root; schema 3):

| Namespace | Key → value | Notes |
|---|---|---|
| 48 `GHOST_COMMITMENTS` | position → `cm` | append-only, for wallet sync |
| 49 `GHOST_ROOTS` | replaced root → height it was replaced | usable anchors besides the current one |
| 50 `GHOST_RETIRED` | height → root replaced then | the same records by age, for expiry |
| 51 `GHOST_NULLIFIERS` | nullifier → height spent | grows forever; spends prepay it through fees |
| 52 `GHOST_ADDRESSES` | account → 1,216-byte address | `SetGhostAddress` |
| 53–55 | | reserved for Ghost Mode; 56–63 kept for a migration to a new pool |
| META `ghost` | `GhostMeta { instance, frontier, root, pool, pause, votes }` | the chain instance, the `Frontier` bytes, the current anchor, the turnstile (u64 drops), the validators' pause |

The governance knobs are `ChainParams` (params.rs), not a feature record: `ghost_pool_cap`,
`ghost_max_tx_bytes` (256 KiB), `ghost_max_block_bytes` (1 MiB), `ghost_anchor_window` (1,024
blocks) and `ghost_verify_fee` (0.001 FANG). The flag itself is upgrade.rs's `features` map.

**Actions:**

| Tag | Action | Signed by | Value balance | Effect |
|---|---|---|---|---|
| 50 | `Shield { amount, bundle }` | `from`'s owner key (normal Tx v3) | `−amount` | debit `amount` + `ghost_verify_fee` (burned) besides the usual charge; `pool += amount`; fails (charged) above `ghost_pool_cap` |
| 51 | `PrivateTransfer { bundle }` | nobody: the proof authorizes | `fee` | `pool −= fee`; tip to the proposer, the rest burned |
| 52 | `Unshield { to, amount, bundle }` | nobody | `amount + fee` | `pool −= amount + fee`; credit `to` (address or name); opening `to`'s account burns `account_creation_fee` out of the fee (DEC-6) |
| 53 | `SetGhostAddress { address }` | the account's owner key | | the address is filed under the account, so wallets find it under each of its `.boo` names (and it follows the account, not a name that changes hands) |
| 54 | `GhostPause {}` | an active validator (its consensus key or owner) | | once f + 1 distinct validators of the epoch's active set have signed, ghost actions fail from the next block |

`bundle = { instance, anchor, nf[2], cm[2], enc[2] (1,401 B each), out[2] (80 B each), proof }`
(`types::GhostBundle`; hex in JSON, raw in the wire form, the proof after a u32 length). Digests
must be canonical and the nullifiers distinct (`check_format`). Ghost actions can't sit inside a
multisig proposal. Session keys can't shield (they only send transfers), and nothing they hold can
spend notes.

**Unsigned transactions.** 51 and 52 are the same `Tx` type with `from`, `memo`, `public_key`
and `signature` empty and `nonce` 0, in a wire form of their own: `BLOCKULA_GHOST_V1 ‖ chain_id ‖
genesis hash ‖ fee ‖ tip ‖ valid_until ‖ action` (`Tx::signing_bytes`, and the whole wire form). `Tx::from_bytes`
dispatches on the domain and accepts exactly these two tags unsigned (and neither in the signed
form). Keeping one type means blocks, gossip, the mempool, the API and the explorer carry them
unchanged, and no signed-tx path changed: `State::apply_j` hands unsigned txs to
`ghost::apply_unsigned` before it looks for a sender, `verify_signature` refuses them, and the
stateless checks skip the signature for them only. An unsigned tx's id is the hash of its wire
form. Replay protection is the nullifier set; `valid_until` is in the sighash.

**Sighash** (`Tx::ghost_sighash`, mirrored in `sdk-ts` `ghostSighash`, shared vectors):
`BLAKE3-KDF("blockula ghost v1 tx sighash", chain_id ‖ genesis hash ‖ tag ‖ valid_until ‖ fee ‖
tip ‖ tag fields (Shield: from, nonce, amount; Unshield: to, amount) ‖ instance ‖ anchor ‖ nf ‖
cm ‖ enc ‖ out ‖ CIRCUIT_ID)`. The genesis hash (the 32 raw bytes, as signed txs carry it since
audit #7) and `instance` (`ghost::chain_instance`: BLAKE3-KDF over the consensus genesis seed's
inputs) both identify the chain, so chains that share a chain id (a restarted testnet, a fork)
don't accept each other's proofs. The tx carries it, so a proof verifies without state;
admission and execution check it equals the chain's (`/api/ghost` serves it).

**Limits.** Ghost txs (50–52) up to `MAX_GHOST_TX_BYTES` = 1 MiB in the codec, and
`ghost_max_tx_bytes` at admission and in blocks; at most `ghost_max_block_bytes` of them per
block (`ghost::BlockBudget`, in consensus `propose` and `verify`). In blocks, both limits are the
ones the block's fee slot fixed 17 blocks ahead (`econ::FeeSlot`, like the base fee and
`max_block_bytes`), so a `ParamChange` takes effect 17 blocks after it executes and every proposer
within the execution lag knows them exactly; `verify` checks them before any proof. At most 256 queued in the
mempool. A tx spending a nullifier a queued tx spends replaces it, and a full ghost share gives up its
lowest tip per byte (the newest among equals), only for a tip at least the displaced one's plus the larger
of a tenth of it and the base fee of the newcomer's bytes (rounded up to the fee grid), and a higher tip
per byte: a higher tip always gets in, and every displacement pays for relaying itself (BIP-125's rules 3
and 4), so re-proving one note with tips one drop apart buys nothing. Any queued ghost tx but the
newcomer's sender's own can be given up. A queued shield is always its sender's last queued tx: nothing
queues behind it (any later tx from the account, a validator duty it pays included, is refused: "your
shield (nonce n) is waiting in the mempool, and nothing queues behind a shield"), so giving one up never
leaves a nonce gap, and nobody can make shields unevictable by covering each with a cheap tx of their own
(review of F36, round 3: 256 such shields locked the share against any tip). Until it's included, its
sender may replace it with its own shield at the same nonce, for a tip that displaces it (the only
replacement of a signed tx by its sender); wallets shield last, and send the account's next tx once the
shield is in a block. Were a signed tx ever displaced with later txs of its sender behind it, they'd go
with it and the newcomer would outbid them together. The refusal names the tip that gets in, unless
that's past the newcomer's tip cap (for an unsigned tx, both on the fee grid). Then it says to wait:
a queued tx (a shield, or another spend of the same note) tipping so near its own cap that outbidding
it takes more than the cap at the current base fee can't be replaced until the base fee rises, so its
sender waits until it's included (it sorts near the top of proposals) or leaves the mempool.
`POST /api/tx` takes the wire form as `application/octet-stream` (and JSON up to twice
`MAX_GHOST_TX_BYTES`).

**Fees.** A shield pays the usual byte fee and tip transparently, plus `ghost_verify_fee`. An
unsigned tx's `fee` is exact (it's in the value balance) and must cover the base fee for its
size (`econ::base_fee`), its tip, `ghost_verify_fee` and, for an unshield to an address with no
account yet, `account_creation_fee` (opening an account burns it, however it's opened); the tip
goes to the proposer, the rest is burned. Its `fee` and `tip` are public, so a fee priced to the
drop would tell wallets, and the moment they priced, apart: both must be whole multiples of
`ghost::FEE_GRID` (0.001 FANG), and wallets pay the canonical fee `/api/ghost` serves
(`canonical_fee`, `canonical_fee_new_account`: `/api/fees`' suggested base fee for a
`ghost_max_tx_bytes` tx, since the fee is in the sighash and fixed before the proof's size is
known, plus the verify fee and, for a new account, the creation fee, rounded up to the grid), tip
0 unless the mempool's ghost share is full, and `valid_until` = the height + `max_tx_validity_blocks`.
A ghost tx that can't be included is skipped, and P-STAKE charges the proposer for it (CV-5).

**Pipeline.**
- **Before the chain lock** (HTTP submission, on a blocking thread, after a cheap "is Ghost Mode
  on" check; gossip; consensus `verify`): `check_tx_stateless` checks format, prefix, fee floor,
  the signature (not for 51 and 52) and the proof against the statement derived from the tx
  alone (`ghost::statement`: digests from the bundle, the value balance from the tag and fields
  in `i128` through `ValueBalance::new`, the sighash). A block with an invalid proof is
  permanently invalid and gets a no.
- **Gossip** screens cheapest-first (audit F30, `consensus::screen_gossip`): the size before
  anything is copied, a muted peer or bytes already found invalid, then (parsed) a tx already in
  the mempool, finalized or being verified, the base fee and Ghost Mode being on, and only then
  the signature. A ghost proof is verified off the ingest loop, within a per-peer budget (8 a
  second, bursts of 32; 16 in flight over all peers), once per tx however many peers relay it. A
  peer that relays a bad signature or proof (honest nodes verify every tx before they send it) has
  its mempool gossip dropped unread for 5 minutes; its votes and blocks are unaffected.
- **Admission and execution**, deterministically: the feature on and unpaused, the size limit,
  the instance, the fee floor, the turnstile (`pool ≥ value leaving`), the recipient, a usable
  anchor, unspent nullifiers, room for two leaves (`size + 2 ≤ CAPACITY`), and the proof again.
  Verdicts are remembered by a hash of the verifier's whole input (public values, circuit id,
  proof bytes), and `verify` is a pure function of it, so a hit is exactly a repeat: the cache
  changes how long execution takes, never what it does. Valid ones (4,096) are kept least
  recently used first, and the mempool check re-runs every queued ghost tx each block, so theirs
  stay the freshest; failures (1,024) are kept apart, so junk that fails cheaply can't flush them. For a signed shield these are
  admission checks (can't be included) except the cap, the anchor and the nullifiers, which fail
  the action with the fee charged, like any signed tx. An unsigned tx applies in full or not at
  all (it has no account to charge).
- **Effects**: both nullifiers recorded, both commitments (dummies too) appended, value moved
  through the journaled helpers. At the end of the block (`ghost::end_block`, after governance's
  step) the tree's new root becomes the anchor and the root it replaced stays usable for
  `ghost_anchor_window` blocks.
- `State::check_invariants()`: `supply = Σ balances + escrows + stakes + … + pool`; the
  commitments are exactly the frontier's leaves; the anchor is the frontier's root; retired
  anchors are filed both ways.

**Turnstile and pause.** The turnstile (ZIP 209's rule) bounds a soundness bug to theft of
honest shielded deposits rather than unbounded supply; the pool cap bounds those
(`the_turnstile_bounds_a_counterfeit_note` spends a counterfeit note whose root was forced into the
anchor set). `GhostPause` gives a fast stop without an admin key: f + 1 validators can already
halt the chain, so it adds no power, and it can only stop the pool, never move funds. Signatures
count within one epoch. A pause belongs to the activation it was voted under, so governance
resumes the pool with `DeactivateFeature { ghost }` then `ActivateFeature { ghost }` (in one block
or two). Recovery from a suspected bug follows Zcash: freeze the pool (pause, or switch it off)
and migrate to a new versioned pool through the turnstile (not built).

**API.** `GET /api/ghost`: on or off, paused (since, by whom, signatures so far and the
threshold), pool and cap, frontier size, the current anchor and recent replaced ones, the window,
nullifier count, the instance, the circuit id, the limits, the fee grid and the canonical fees. `GET /api/ghost/address/{name or
address}`: a published address and its fingerprint. Tx JSON shows ghost bundles without their
proof (its size and hash instead) but with the commitments and ciphertexts wallets scan; an
unsigned tx's summary is "👻 a ghost transaction happened".

**Tests** (`node/src/ghost/tests.rs`, real proofs from the crate's prover, through the mempool
and blocks, with the incremental root checked against a rebuild and the invariants after every
block): shield → private transfer (found by trial decryption) → unshield, with a restart, a
rebuild from the block logs and a records round trip giving the same root; the proof bound to
fee, recipient and ciphertexts; double spends refused in one block, across blocks and in the
mempool (and replacement by tip); stale anchors; the turnstile against a counterfeit note; the cap;
the pause by f + 1 of 4 validators, across epochs, and governance's resume; disabled by default
(every tag refused with the same error, at the mempool and in blocks); size limits and the block
budget; another chain instance; the mempool's ghost bounds; session keys; the release profile.
Also: a 4-validator simulation (`sim::ghost_txs_finalize_through_consensus`: the three
transfers submitted to different validators, gossiped, proposed under the block budget,
proof-checked by every verifier and executed to the same root), consensus `verify`
(`verify_checks_ghost_proofs_and_size_limits`), the HTTP API, wire-form
and bound tests and shared Rust/TypeScript vectors in `types.rs`, and the `ghost_tx` fuzz target
with real-proof seeds (`node/fuzz`).

**Not built yet.** Clients: the CLI (`blockula ghost shield|send|unshield|scan|export-viewing-key`),
`@blockula/sdk/ghost` on a WASM build of the crate (the SDK encodes ghost txs and their sighash but
can't prove), the MCP tools (`ghost_balance`, `ghost_send`, capped by `BLOCKULA_MAX_SEND`), and the
explorer's ghost view. No delegated proving by default (the prover sees the witness). Also open:
a withdraw-only mode and the migration path, `/api/ghost` commitment paging for wallet sync, and
2-in/2-out only (no multi-action bundles).

## 9. Activation gates

From MAINNET-DESIGN §6. `ActivateFeature { ghost }` only after all of:

1. A dedicated external ZK and cryptography audit of this crate, the Plonky3 0.8 components it
   uses and the node integration, with no open high or critical findings.
2. A clean under-constraint sweep and verifier fuzzing on the audited code (section 11), plus an
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
| Proof size | 129,159–135,016 bytes (126–132 KiB) over two runs of 20 proofs (the masks are random); opened values 8.9 KB, FRI and Merkle paths ~120 KB |
| Prove | 56–64 ms median across runs (constraint check included) |
| Verify | 6.2–6.3 ms median |
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
  bit in every byte of a proof: 128,807 flips, all rejected, 0 panics (11 minutes in release).
- **Tree**: frontier against `FullTree` at every size up to 2¹⁰ + 1, and at 2³² − 1 leaves and
  at capacity (states built with `from_bytes`) against a hand fold and against authentication
  paths; the strict decoding rules, including the full tree's root slot.
- **Secret hygiene**: `{:?}` of witnesses, notes, paths, keys, decrypted notes and the trial
  decryptor contains no key, note or position material (`debug_output_redacts_secrets`).
- **Encryption** (`tests/encryption.rs`): round trip, sizes, other keys, tampering in each
  region, wrong `cm`, outgoing recovery, no-ovk noise, wrong recipient, freshness, random garbage.
- **Lifecycle** (`tests/lifecycle.rs`): through the public API with a toy node (frontier, root
  history, nullifier set, turnstile): shield, scan, private transfer, outgoing recovery, unshield,
  a replayed spend refused, and an unshield from outside the pool's history stopped by the
  turnstile.
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
- **Memory hygiene**: partial. `Debug` never prints secrets: key types, `SpendWitness`, `Note`,
  `MerklePath` (the position identifies the note), `DecryptedNote` and `TrialDecryptor` print
  `Type(..)`, and `ActionWitness` shows only its public anchor and sighash. Nothing implements
  `Display`. ML-KEM's own decapsulation key type derives a `Debug` that prints the key, so the
  crate never hands one out (`IncomingViewingKey::decapsulation_key` is crate-private). On
  drop, `SpendingKey`, the viewing keys and `TrialDecryptor`'s expanded ML-KEM key wipe their
  bytes (`zeroize`). Not wiped: copies a caller makes (`seed()`, `Clone`,
  `OutgoingViewingKey.0`), field-element secrets (`sk`, `ak`, `nk`, witness values: KoalaBear
  elements can't be reliably wiped without `unsafe` volatile writes), the prover's trace and
  Plonky3's buffers, and the shared secrets and plaintexts inside encryption and ML-KEM.
- **No WASM yet**, so no prover in TypeScript: the SDK encodes ghost txs and their sighash
  (vectors shared with the node) but can't prove. The KATs are the contract for a port.
- **Timing side channels** in proving and trial decryption haven't been analyzed.
- **Network-level privacy** isn't provided. The node a wallet submits to sees its IP. A full node
  forwards an unsigned ghost tx to a validator without the client's IP (it does for signed txs, so
  the validator's per-IP limits see the end client), but it still knows it, and so does anything
  between the wallet and it. Submitting through Tor or a relay is the user's part. The price: the
  validator counts the unsigned txs a full node forwards against that full node's IP
  (`TX_PER_IP_PER_MINUTE`, 120 a minute), so one full node relays at most that many ghost txs a
  minute whoever sends them (each client is still held to its own limit at the full node), and a
  sender of valid ghost txs can use that allowance up; wallets that need more submit to another
  node or to a validator directly.
- **Metadata.** The fee grid and the canonical fee leave a few fee values per base-fee level, not
  one; a tip (only when the ghost share is full), `valid_until` and the time a tx appears still say
  something about when and how it was built, and an unshield's amount and recipient are public.
