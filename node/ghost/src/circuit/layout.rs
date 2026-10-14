//! Trace layout of the Action circuit: columns, the fixed row schedule, the periodic selector
//! columns that encode it, and the public-value layout.
//!
//! Every row proves one Poseidon2 permutation (`out = P(in)`, columns `0..164`, Plonky3's
//! `Poseidon2Air` layout). The schedule says what each row's permutation is for and how its
//! input is wired; it is the same for every proof, so the verifier evaluates the selectors
//! itself (periodic columns with period = trace height) and nothing about it is committed.
//!
//! ```text
//! rows   0..42   spend 0   NK AK OWN_A OWN_B CM_A CM_B CM_C MERKLE×32 NF_A NF_B NF_C
//! rows  42..84   spend 1   (same)
//! rows  84..87   output 0  CM_A CM_B CM_C
//! rows  87..90   output 1  CM_A CM_B CM_C
//! rows  90..128  padding   input 0; row 127 also checks the value balance
//! ```

use std::sync::OnceLock;

use p3_field::PrimeCharacteristicRing;
use p3_koala_bear::{
    GenericPoseidon2LinearLayersKoalaBear, KOALABEAR_POSEIDON2_HALF_FULL_ROUNDS,
    KOALABEAR_POSEIDON2_PARTIAL_ROUNDS_16, KOALABEAR_POSEIDON2_RC_16_EXTERNAL_FINAL,
    KOALABEAR_POSEIDON2_RC_16_EXTERNAL_INITIAL, KOALABEAR_POSEIDON2_RC_16_INTERNAL,
    KOALABEAR_S_BOX_DEGREE,
};
use p3_poseidon2_air::{Poseidon2Air, RoundConstants, num_cols};

use crate::field::F;
use crate::hash::Domain;

/// log2 of the trace height.
pub const LOG_HEIGHT: usize = 7;
/// Trace height (rows).
pub const HEIGHT: usize = 1 << LOG_HEIGHT;

const HALF_FULL: usize = KOALABEAR_POSEIDON2_HALF_FULL_ROUNDS;
const PARTIAL: usize = KOALABEAR_POSEIDON2_PARTIAL_ROUNDS_16;
/// S-box registers per S-box: none, so S-box constraints are degree 3.
const SBOX_REGISTERS: usize = 0;

/// The Poseidon2 sub-AIR (width 16, KoalaBear, Plonky3's round constants).
pub type P2Air = Poseidon2Air<
    F,
    GenericPoseidon2LinearLayersKoalaBear,
    16,
    KOALABEAR_S_BOX_DEGREE,
    SBOX_REGISTERS,
    HALF_FULL,
    PARTIAL,
>;

/// The sub-AIR's round constants: the same ones [`crate::hash::permutation`] uses.
pub fn p2_constants() -> RoundConstants<F, 16, HALF_FULL, PARTIAL> {
    RoundConstants::new(
        KOALABEAR_POSEIDON2_RC_16_EXTERNAL_INITIAL,
        KOALABEAR_POSEIDON2_RC_16_INTERNAL,
        KOALABEAR_POSEIDON2_RC_16_EXTERNAL_FINAL,
    )
}

/// Generates the Poseidon2 columns for a batch of permutation inputs.
pub fn p2_trace(inputs: Vec<[F; 16]>) -> p3_matrix::dense::RowMajorMatrix<F> {
    p3_poseidon2_air::generate_trace_rows::<
        F,
        GenericPoseidon2LinearLayersKoalaBear,
        16,
        KOALABEAR_S_BOX_DEGREE,
        SBOX_REGISTERS,
        HALF_FULL,
        PARTIAL,
    >(inputs, &p2_constants(), 0)
}

// ---- Columns -------------------------------------------------------------------------------

/// Poseidon2 columns (inputs, round states).
pub const P2_COLS: usize =
    num_cols::<16, KOALABEAR_S_BOX_DEGREE, SBOX_REGISTERS, HALF_FULL, PARTIAL>();
/// Permutation input, 16 columns.
pub const IN: usize = 0;
/// Permutation output (the last full round's state), 16 columns.
pub const OUT: usize = P2_COLS - 16;
/// Merkle path bit: 1 if the running node is a right child at this level.
pub const BIT: usize = P2_COLS;
/// Spend register: the nullifier key `nk` (8).
pub const REG_NK: usize = BIT + 1;
/// Spend register: the note commitment `cm` (8).
pub const REG_CM: usize = REG_NK + 8;
/// Spend register: position bits 0..16 accumulated over the Merkle rows.
pub const PACC_LO: usize = REG_CM + 8;
/// Spend register: position bits 16..32.
pub const PACC_HI: usize = PACC_LO + 1;
/// Spend register: 1 for a dummy spend (value 0, no membership check).
pub const DUMMY: usize = PACC_HI + 1;
/// Running value balance, one column per 16-bit limb: Σ spent − Σ output.
pub const ACC: usize = DUMMY + 1;
/// Bits of the four value limbs on value rows (64).
pub const VBITS: usize = ACC + 4;
/// Carry bits for the balance check on the last row (3 carries × 3 bits).
pub const CBITS: usize = VBITS + 64;
/// Total columns.
pub const WIDTH: usize = CBITS + 9;

// ---- Row schedule --------------------------------------------------------------------------

/// Rows per spend.
pub const SPEND_ROWS: usize = 42;
/// Rows per output.
pub const OUTPUT_ROWS: usize = 3;
/// Spend row offsets.
pub const NK: usize = 0;
/// `ak = H(sk)`.
pub const AK: usize = 1;
/// `owner = H(ak ‖ nk)`, chunk 1.
pub const OWN_A: usize = 2;
/// `owner`, chunk 2.
pub const OWN_B: usize = 3;
/// `cm`, chunk 1 (owner), in a spend block.
pub const CM_A: usize = 4;
/// `cm`, chunk 2 (value limbs, rcm[0..4]).
pub const CM_B: usize = 5;
/// `cm`, chunk 3 (rcm[4..8], zero padding).
pub const CM_C: usize = 6;
/// First Merkle row (level 0); levels 0..32 are rows `MERKLE..MERKLE + 32`.
pub const MERKLE: usize = 7;
/// `nf`, chunk 1 (nk).
pub const NF_A: usize = MERKLE + 32;
/// `nf`, chunk 2 (cm).
pub const NF_B: usize = NF_A + 1;
/// `nf`, chunk 3 (position halves).
pub const NF_C: usize = NF_B + 1;
/// First output row.
pub const OUTPUTS: usize = 2 * SPEND_ROWS;
/// First padding row.
pub const PADDING: usize = OUTPUTS + 2 * OUTPUT_ROWS;
/// The row that checks the value balance.
pub const BALANCE_ROW: usize = HEIGHT - 1;

const _: () = assert!(NF_C + 1 == SPEND_ROWS);
const _: () = assert!(PADDING < BALANCE_ROW);

/// First row of spend `i`.
pub const fn spend_base(i: usize) -> usize {
    i * SPEND_ROWS
}

/// First row of output `j`.
pub const fn output_base(j: usize) -> usize {
    OUTPUTS + j * OUTPUT_ROWS
}

// ---- Periodic selector columns -------------------------------------------------------------

/// Indices of the periodic columns. Each is 0 except where noted.
pub mod sel {
    /// 1 on rows that start a sponge (their capacity is the domain IV).
    pub const START: usize = 0;
    /// The IV of the sponge starting on this row (8 columns).
    pub const IV: usize = 1;
    /// 1 if the next row continues this row's sponge (capacity carries over).
    pub const CONT_NEXT: usize = 9;
    /// Spend NK rows: `reg_nk = out`, next row's rate = this row's rate (same `sk`).
    pub const NK_ROW: usize = 10;
    /// Spend AK rows: next row's rate = `out` (`ak` into `owner`).
    pub const AK_ROW: usize = 11;
    /// Spend OWN_B rows: rate = `reg_nk`; next row's rate = `out` (`owner` into `cm`).
    pub const OWN_B: usize = 12;
    /// Value rows (CM_B of every note): `in[0..4]` are 16-bit limbs.
    pub const VALUE: usize = 13;
    /// +1 on spend value rows, −1 on output value rows.
    pub const VSIGN: usize = 14;
    /// Spend value rows: a dummy's limbs must be zero.
    pub const SPEND_VALUE: usize = 15;
    /// CM_C rows: `in[4..8] = 0` (padding of the 20-element input).
    pub const CM_C: usize = 16;
    /// Spend CM_C rows: `reg_cm = out`; next row is Merkle level 0 with node `out`.
    pub const SPEND_CM_C: usize = 17;
    /// Merkle rows: the path bit may be set.
    pub const MERKLE: usize = 18;
    /// Merkle levels 0..31: next row is the next level with node `out + in[0..8]`.
    pub const MERKLE_CONT: usize = 19;
    /// Merkle level 31: `out + in[0..8]` is the root; equals the anchor unless dummy.
    pub const ROOT: usize = 20;
    /// Weight of this row's path bit in position bits 0..16.
    pub const W_LO: usize = 21;
    /// Weight of this row's path bit in position bits 16..32.
    pub const W_HI: usize = 22;
    /// NF_A rows: rate = `reg_nk`.
    pub const NF_A: usize = 23;
    /// NF_B rows: rate = `reg_cm`.
    pub const NF_B: usize = 24;
    /// NF_C rows: rate = `(pacc_lo, pacc_hi, 0, …)`.
    pub const NF_C: usize = 25;
    /// NF_C of spend `i` (2 columns): `out[0..8]` = public nullifier `i`.
    pub const PUB_NF: usize = 26;
    /// CM_C of output `j` (2 columns): `out[0..8]` = public commitment `j`.
    pub const PUB_CM: usize = 28;
    /// Padding rows: input is zero.
    pub const PAD: usize = 30;
    /// Rows of a spend block: spend registers may be nonzero.
    pub const SPEND: usize = 31;
    /// Spend rows except the block's last: registers carry to the next row.
    pub const CARRY: usize = 32;
    /// First row of a spend block: position accumulators start at zero.
    pub const BLOCK_START: usize = 33;
    /// The balance row.
    pub const BALANCE: usize = 34;
    /// Number of periodic columns.
    pub const COUNT: usize = 35;
}

/// The periodic columns, `sel::COUNT` vectors of `HEIGHT` values.
pub fn periodic_columns() -> &'static [Vec<F>] {
    static TABLE: OnceLock<Vec<Vec<F>>> = OnceLock::new();
    TABLE.get_or_init(build_periodic)
}

fn build_periodic() -> Vec<Vec<F>> {
    let mut t = vec![vec![F::ZERO; HEIGHT]; sel::COUNT];
    let mut set = |col: usize, row: usize, v: F| t[col][row] = v;
    let one = F::ONE;
    let start = |set: &mut dyn FnMut(usize, usize, F), row: usize, domain: Domain| {
        set(sel::START, row, F::ONE);
        for (j, x) in domain.iv().into_iter().enumerate() {
            set(sel::IV + j, row, x);
        }
    };
    for i in 0..2 {
        let b = spend_base(i);
        start(&mut set, b + NK, Domain::NullifierKey);
        start(&mut set, b + AK, Domain::SpendAuth);
        start(&mut set, b + OWN_A, Domain::Owner);
        start(&mut set, b + CM_A, Domain::NoteCommitment);
        start(&mut set, b + NF_A, Domain::Nullifier);
        for r in [OWN_A, CM_A, CM_B, NF_A, NF_B] {
            set(sel::CONT_NEXT, b + r, one);
        }
        set(sel::NK_ROW, b + NK, one);
        set(sel::AK_ROW, b + AK, one);
        set(sel::OWN_B, b + OWN_B, one);
        set(sel::VALUE, b + CM_B, one);
        set(sel::VSIGN, b + CM_B, one);
        set(sel::SPEND_VALUE, b + CM_B, one);
        set(sel::CM_C, b + CM_C, one);
        set(sel::SPEND_CM_C, b + CM_C, one);
        for k in 0..32 {
            let row = b + MERKLE + k;
            set(sel::MERKLE, row, one);
            if k < 31 {
                set(sel::MERKLE_CONT, row, one);
            }
            if k < 16 {
                set(sel::W_LO, row, F::from_u32(1 << k));
            } else {
                set(sel::W_HI, row, F::from_u32(1 << (k - 16)));
            }
        }
        set(sel::ROOT, b + MERKLE + 31, one);
        set(sel::NF_A, b + NF_A, one);
        set(sel::NF_B, b + NF_B, one);
        set(sel::NF_C, b + NF_C, one);
        set(sel::PUB_NF + i, b + NF_C, one);
        for r in 0..SPEND_ROWS {
            set(sel::SPEND, b + r, one);
            if r + 1 < SPEND_ROWS {
                set(sel::CARRY, b + r, one);
            }
        }
        set(sel::BLOCK_START, b, one);
    }
    for j in 0..2 {
        let b = output_base(j);
        start(&mut set, b, Domain::NoteCommitment);
        set(sel::CONT_NEXT, b, one);
        set(sel::CONT_NEXT, b + 1, one);
        set(sel::VALUE, b + 1, one);
        set(sel::VSIGN, b + 1, F::NEG_ONE);
        set(sel::CM_C, b + 2, one);
        set(sel::PUB_CM + j, b + 2, one);
    }
    for row in PADDING..HEIGHT {
        set(sel::PAD, row, one);
    }
    set(sel::BALANCE, BALANCE_ROW, one);
    t
}

// ---- Public values -------------------------------------------------------------------------

/// Layout of the public values.
pub mod pv {
    /// Merkle root the spends are proven against (8).
    pub const ANCHOR: usize = 0;
    /// Nullifiers of spends 0 and 1 (2 × 8).
    pub const NF: usize = 8;
    /// Commitments of outputs 0 and 1 (2 × 8).
    pub const CM: usize = 24;
    /// 1 if the value balance is negative (value enters the pool).
    pub const VB_NEG: usize = 40;
    /// |value balance| as four 16-bit limbs, least significant first.
    pub const VB: usize = 41;
    /// The transaction's sighash (8).
    pub const SIGHASH: usize = 45;
    /// [`super::super::CIRCUIT_ID`].
    pub const CIRCUIT: usize = 53;
    /// Number of public values.
    pub const COUNT: usize = 54;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_constants() {
        assert_eq!(P2_COLS, 164);
        assert_eq!(WIDTH, 261);
        assert_eq!(OUTPUTS, 84);
        assert_eq!(PADDING, 90);
        assert_eq!(periodic_columns().len(), sel::COUNT);
        assert!(periodic_columns().iter().all(|c| c.len() == HEIGHT));
    }

    #[test]
    fn transition_selectors_are_zero_on_the_last_row() {
        // Constraints that read the next row are gated by these; the last row wraps to row 0.
        let t = periodic_columns();
        for col in [
            sel::CONT_NEXT,
            sel::NK_ROW,
            sel::AK_ROW,
            sel::OWN_B,
            sel::SPEND_CM_C,
            sel::MERKLE_CONT,
            sel::CARRY,
        ] {
            assert_eq!(t[col][HEIGHT - 1], F::ZERO);
        }
    }
}
