//! Witness to trace.
//!
//! Witnesses are first lowered to a [`RawAction`] of field elements, then laid out row by row.
//! The layout step computes every permutation input from the cells already written (the same
//! wiring the AIR checks), so it never judges the raw values: values that break a constraint
//! give a trace that fails it, with everything downstream consistent. Tests can also apply
//! [`Cheat`]s, which change a cell before anything downstream reads it. That is how the
//! soundness tests play a cheating prover.

use p3_field::{PrimeCharacteristicRing, PrimeField32};
use p3_matrix::dense::RowMajorMatrix;

use super::layout::*;
use super::{ActionWitness, CIRCUIT_ID, ValueBalance, sighash_fields};
use crate::field::{Digest, F, P, ZERO_DIGEST, u64_to_limbs};
use crate::hash::{Domain, permute};

/// A spend at the field level. Holds the witness, so no `Debug`.
#[derive(Clone)]
pub(crate) struct RawSpend {
    pub sk: Digest,
    pub limbs: [F; 4],
    pub rcm: Digest,
    pub position: u32,
    pub siblings: [Digest; 32],
    pub dummy: bool,
}

/// An output at the field level.
#[derive(Clone)]
pub(crate) struct RawOutput {
    pub owner: Digest,
    pub limbs: [F; 4],
    pub rcm: Digest,
}

/// An action at the field level, with the public values that aren't outputs of the trace.
#[derive(Clone)]
pub(crate) struct RawAction {
    pub spends: [RawSpend; 2],
    pub outputs: [RawOutput; 2],
    pub anchor: Digest,
    pub vb_neg: F,
    pub vb: [F; 4],
    pub sighash: Digest,
    pub circuit_id: F,
}

impl RawAction {
    /// Lowers a witness, claiming value balance `vb`.
    pub fn from_witness(w: &ActionWitness, vb: ValueBalance) -> Self {
        Self {
            spends: core::array::from_fn(|i| {
                let s = &w.spends[i];
                RawSpend {
                    sk: s.sk,
                    limbs: u64_to_limbs(s.note.value),
                    rcm: s.note.rcm(),
                    position: s.path.position,
                    siblings: s.path.siblings,
                    dummy: s.dummy,
                }
            }),
            outputs: core::array::from_fn(|j| {
                let n = &w.outputs[j];
                RawOutput { owner: n.owner, limbs: n.limbs(), rcm: n.rcm() }
            }),
            anchor: w.anchor,
            vb_neg: F::from_bool(vb.is_negative()),
            vb: u64_to_limbs(vb.magnitude()),
            sighash: sighash_fields(&w.sighash),
            circuit_id: F::from_u32(CIRCUIT_ID),
        }
    }
}

/// A deliberate deviation for tests: cell `(row, col)` takes `value` before anything
/// downstream reads it. Registers and accumulators keep the new value on later rows, as a
/// cheating prover would arrange.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Cheat {
    pub row: usize,
    pub col: usize,
    pub value: F,
}

/// A laid-out trace and the public values it proves.
pub(crate) struct Built {
    pub trace: RowMajorMatrix<F>,
    pub nullifiers: [Digest; 2],
    pub commitments: [Digest; 2],
    pub public: Vec<F>,
}

/// A field element as a small signed integer (elements above p/2 are negative).
fn centered(x: F) -> i64 {
    let v = i64::from(x.as_canonical_u32());
    if v > i64::from(P / 2) { v - i64::from(P) } else { v }
}

/// Lays out the honest trace.
pub(crate) fn build(raw: &RawAction) -> Built {
    build_with(raw, &[])
}

/// The row machine.
struct Machine<'a> {
    rows: Vec<[F; WIDTH]>,
    cheats: &'a [Cheat],
}

impl Machine<'_> {
    /// Applies the cheats for `row` on columns in `cols`.
    fn cheat(&mut self, row: usize, cols: core::ops::Range<usize>) {
        for c in self.cheats.iter().filter(|c| c.row == row && cols.contains(&c.col)) {
            self.rows[row][c.col] = c.value;
        }
    }

    /// Sets the cells of `row` from column `start` to `value`, then applies cheats there.
    fn set(&mut self, row: usize, start: usize, value: &[F]) {
        self.rows[row][start..start + value.len()].copy_from_slice(value);
        self.cheat(row, start..start + value.len());
    }

    fn get<const N: usize>(&self, row: usize, start: usize) -> [F; N] {
        self.rows[row][start..start + N].try_into().expect("N")
    }

    /// Writes the permutation input (`rate ‖ capacity`), applies input cheats, and returns
    /// `out`.
    fn permute(&mut self, row: usize, rate: &[F; 8], capacity: &[F; 8]) -> [F; 16] {
        self.hash(row, rate, capacity);
        self.out(row)
    }

    /// [`Self::permute`] when the caller only needs the cells.
    fn hash(&mut self, row: usize, rate: &[F; 8], capacity: &[F; 8]) {
        self.rows[row][IN..IN + 8].copy_from_slice(rate);
        self.rows[row][IN + 8..IN + 16].copy_from_slice(capacity);
        self.cheat(row, IN..IN + 16);
        let out = permute(self.get(row, IN));
        self.rows[row][OUT..OUT + 16].copy_from_slice(&out);
    }

    fn out(&self, row: usize) -> [F; 16] {
        self.get(row, OUT)
    }

    fn capacity(&self, row: usize) -> [F; 8] {
        self.get(row, OUT + 8)
    }
}

fn rate(parts: &[&[F]]) -> [F; 8] {
    let mut r = [F::ZERO; 8];
    let mut i = 0;
    for p in parts {
        r[i..i + p.len()].copy_from_slice(p);
        i += p.len();
    }
    r
}

fn first8(x: &[F; 16]) -> [F; 8] {
    x[..8].try_into().expect("8")
}

/// Spend register columns: `REG_NK..=DUMMY` (nk, cm, the two accumulators, the dummy flag).
const REGS: usize = DUMMY + 1 - REG_NK;
const _: () = assert!(REG_CM == REG_NK + 8 && PACC_LO == REG_CM + 8 && DUMMY == PACC_HI + 1);

/// Register columns of a spend block. A cheat on a register cell holds from its row to the end
/// of the block, overriding the definition (a prover choosing another value), so the
/// definition and carry constraints are what must catch it.
struct Regs {
    nk: Option<Digest>,
    cm: Option<Digest>,
    dummy: F,
    /// Honest position accumulators at the current row.
    pacc: [F; 2],
    /// Overrides of the register cells, indexed by column − REG_NK.
    over: [Option<F>; REGS],
    /// Value of every register cell and whether it was overridden, per row of the block.
    rows: Vec<[(F, bool); REGS]>,
}

impl Regs {
    fn new(dummy: bool) -> Self {
        Self {
            nk: None,
            cm: None,
            dummy: F::from_bool(dummy),
            pacc: [F::ZERO; 2],
            over: [None; REGS],
            rows: Vec::new(),
        }
    }

    /// The honest value of register column `c` (definitions not yet reached read as zero).
    fn honest(&self, c: usize) -> F {
        match c {
            _ if (REG_NK..REG_NK + 8).contains(&c) => self.nk.map_or(F::ZERO, |d| d[c - REG_NK]),
            _ if (REG_CM..REG_CM + 8).contains(&c) => self.cm.map_or(F::ZERO, |d| d[c - REG_CM]),
            DUMMY => self.dummy,
            PACC_LO => self.pacc[0],
            _ => self.pacc[1],
        }
    }

    /// Picks up register cheats at `row`.
    fn enter(&mut self, m: &Machine, row: usize) {
        for ch in m.cheats.iter().filter(|c| c.row == row && (REG_NK..=DUMMY).contains(&c.col)) {
            // Accumulators shift; constant registers take the value.
            let v = match ch.col {
                PACC_LO | PACC_HI => ch.value - self.honest(ch.col),
                _ => ch.value,
            };
            self.over[ch.col - REG_NK] = Some(v);
        }
    }

    /// The register value the wiring reads.
    fn reg<const N: usize>(&self, start: usize) -> [F; N] {
        core::array::from_fn(|j| self.value(start + j))
    }

    fn value(&self, c: usize) -> F {
        match (c, self.over[c - REG_NK]) {
            (PACC_LO | PACC_HI, Some(shift)) => self.honest(c) + shift,
            (_, Some(v)) => v,
            (_, None) => self.honest(c),
        }
    }

    /// Records the row's register values and advances the accumulators by its path bit.
    fn leave(&mut self, m: &Machine, row: usize) {
        let periodic = periodic_columns();
        self.rows.push(core::array::from_fn(|i| (self.value(REG_NK + i), self.over[i].is_some())));
        let bit = m.rows[row][BIT];
        self.pacc[0] += periodic[sel::W_LO][row] * bit;
        self.pacc[1] += periodic[sel::W_HI][row] * bit;
    }

    /// Writes the register cells. A constant register holds its final definition on every row
    /// that isn't overridden, including rows before the definition.
    fn write(&self, m: &mut Machine, base: usize) {
        for (o, vals) in self.rows.iter().enumerate() {
            for (i, &(v, overridden)) in vals.iter().enumerate() {
                let c = REG_NK + i;
                let accumulator = matches!(c, PACC_LO | PACC_HI);
                m.rows[base + o][c] = if overridden || accumulator { v } else { self.honest(c) };
            }
        }
    }
}

/// Lays out the trace with `cheats` applied.
pub(crate) fn build_with(raw: &RawAction, cheats: &[Cheat]) -> Built {
    let mut m = Machine { rows: vec![[F::ZERO; WIDTH]; HEIGHT], cheats };
    let periodic = periodic_columns();
    let mut nullifiers = [ZERO_DIGEST; 2];
    let mut commitments = [ZERO_DIGEST; 2];

    for (i, sp) in raw.spends.iter().enumerate() {
        let b = spend_base(i);
        let mut regs = Regs::new(sp.dummy);

        regs.enter(&m, b + NK);
        let nk_out = m.permute(b + NK, &sp.sk, &Domain::NullifierKey.iv());
        regs.nk = Some(first8(&nk_out));
        regs.leave(&m, b + NK);

        regs.enter(&m, b + AK);
        let sk = m.get::<8>(b + NK, IN);
        let ak_out = m.permute(b + AK, &sk, &Domain::SpendAuth.iv());
        regs.leave(&m, b + AK);

        regs.enter(&m, b + OWN_A);
        m.hash(b + OWN_A, &first8(&ak_out), &Domain::Owner.iv());
        regs.leave(&m, b + OWN_A);

        regs.enter(&m, b + OWN_B);
        let cap = m.capacity(b + OWN_A);
        let owner_out = m.permute(b + OWN_B, &regs.reg::<8>(REG_NK), &cap);
        regs.leave(&m, b + OWN_B);

        regs.enter(&m, b + CM_A);
        m.hash(b + CM_A, &first8(&owner_out), &Domain::NoteCommitment.iv());
        regs.leave(&m, b + CM_A);
        regs.enter(&m, b + CM_B);
        let cap = m.capacity(b + CM_A);
        m.hash(b + CM_B, &rate(&[&sp.limbs, &sp.rcm[..4]]), &cap);
        regs.leave(&m, b + CM_B);
        regs.enter(&m, b + CM_C);
        let cap = m.capacity(b + CM_B);
        let cm_out = m.permute(b + CM_C, &rate(&[&sp.rcm[4..]]), &cap);
        regs.cm = Some(first8(&cm_out));
        regs.leave(&m, b + CM_C);

        // Merkle rows: the running node goes left or right by the bit cell.
        let mut node = first8(&cm_out);
        for k in 0..32 {
            let r = b + MERKLE + k;
            regs.enter(&m, r);
            m.rows[r][BIT] = F::from_bool((sp.position >> k) & 1 == 1);
            m.cheat(r, BIT..BIT + 1);
            let sib = sp.siblings[k];
            let (left, right) = if m.rows[r][BIT] != F::ZERO { (sib, node) } else { (node, sib) };
            let out = m.permute(r, &left, &right);
            let input_left = m.get::<8>(r, IN);
            node = core::array::from_fn(|j| out[j] + input_left[j]);
            regs.leave(&m, r);
        }

        regs.enter(&m, b + NF_A);
        m.hash(b + NF_A, &regs.reg::<8>(REG_NK), &Domain::Nullifier.iv());
        regs.leave(&m, b + NF_A);
        regs.enter(&m, b + NF_B);
        let cap = m.capacity(b + NF_A);
        m.hash(b + NF_B, &regs.reg::<8>(REG_CM), &cap);
        regs.leave(&m, b + NF_B);
        regs.enter(&m, b + NF_C);
        let cap = m.capacity(b + NF_B);
        let nf_out = m.permute(b + NF_C, &rate(&[&regs.reg::<2>(PACC_LO)]), &cap);
        nullifiers[i] = first8(&nf_out);
        regs.leave(&m, b + NF_C);

        regs.write(&mut m, b);
    }

    for (j, out) in raw.outputs.iter().enumerate() {
        let b = output_base(j);
        m.hash(b, &out.owner, &Domain::NoteCommitment.iv());
        let cap = m.capacity(b);
        m.hash(b + 1, &rate(&[&out.limbs, &out.rcm[..4]]), &cap);
        let cap = m.capacity(b + 1);
        let cm_out = m.permute(b + 2, &rate(&[&out.rcm[4..]]), &cap);
        commitments[j] = first8(&cm_out);
    }

    for r in PADDING..HEIGHT {
        m.hash(r, &[F::ZERO; 8], &[F::ZERO; 8]);
    }

    // Value bits: the low 16 bits of each limb on value rows. A limb ≥ 2^16 has no valid
    // decomposition.
    for (r, &is_value) in periodic[sel::VALUE].iter().enumerate() {
        if is_value == F::ONE {
            for k in 0..4 {
                let v = m.rows[r][IN + k].as_canonical_u32();
                for i in 0..16 {
                    m.rows[r][VBITS + 16 * k + i] = F::from_bool((v >> i) & 1 == 1);
                }
            }
        }
        m.cheat(r, VBITS..VBITS + 64);
    }

    // Balance accumulator: row r holds the sum over value rows before r.
    let mut acc = [F::ZERO; 4];
    for (r, &sign) in periodic[sel::VSIGN].iter().enumerate() {
        m.set(r, ACC, &acc);
        let row = m.rows[r];
        acc = core::array::from_fn(|k| row[ACC + k] + sign * row[IN + k]);
    }

    // Carries for the claimed value balance: c_k = (acc_k − vb_k + c_{k−1}) / 2^16.
    let sign = F::ONE - raw.vb_neg.double();
    let mut carry_in = 0i64;
    for k in 0..3 {
        let t = centered(m.rows[BALANCE_ROW][ACC + k] - sign * raw.vb[k]) + carry_in;
        let c = t.div_euclid(1 << 16);
        let bits = (c + 4).clamp(0, 7);
        for i in 0..3 {
            m.rows[BALANCE_ROW][CBITS + 3 * k + i] = F::from_bool((bits >> i) & 1 == 1);
        }
        carry_in = c;
    }
    // Any remaining cheat on a non-Poseidon2 cell (registers outside spends and so on).
    for r in 0..HEIGHT {
        m.cheat(r, P2_COLS..WIDTH);
    }

    // The Poseidon2 columns of each row from its input; then any cheat on a round column (never
    // read by the wiring).
    let p2 = p2_trace(m.rows.iter().map(|row| row[IN..IN + 16].try_into().expect("16")).collect());
    let mut values = Vec::with_capacity(HEIGHT * WIDTH);
    for r in 0..HEIGHT {
        let mut row = m.rows[r];
        row[..P2_COLS].copy_from_slice(&p2.values[r * P2_COLS..(r + 1) * P2_COLS]);
        for c in cheats.iter().filter(|c| c.row == r && c.col < P2_COLS && !(IN..IN + 16).contains(&c.col)) {
            row[c.col] = c.value;
        }
        values.extend_from_slice(&row);
    }

    let mut public = vec![F::ZERO; pv::COUNT];
    public[pv::ANCHOR..pv::ANCHOR + 8].copy_from_slice(&raw.anchor);
    for i in 0..2 {
        public[pv::NF + 8 * i..pv::NF + 8 * i + 8].copy_from_slice(&nullifiers[i]);
        public[pv::CM + 8 * i..pv::CM + 8 * i + 8].copy_from_slice(&commitments[i]);
    }
    public[pv::VB_NEG] = raw.vb_neg;
    public[pv::VB..pv::VB + 4].copy_from_slice(&raw.vb);
    public[pv::SIGHASH..pv::SIGHASH + 8].copy_from_slice(&raw.sighash);
    public[pv::CIRCUIT] = raw.circuit_id;

    Built { trace: RowMajorMatrix::new(values, WIDTH), nullifiers, commitments, public }
}

/// `Σ spent − Σ output` of a witness, over the integers.
pub(crate) fn balance(w: &ActionWitness) -> i128 {
    w.spends.iter().map(|s| i128::from(s.note.value)).sum::<i128>()
        - w.outputs.iter().map(|n| i128::from(n.value)).sum::<i128>()
}
