//! Witness to trace.
//!
//! Witnesses are first lowered to a [`RawAction`] of field elements, then laid out. The layout
//! step follows the raw action without judging it: values that break a constraint give a trace
//! that fails it, with everything else consistent. That is how the soundness tests play a
//! cheating prover.

use p3_field::{PrimeCharacteristicRing, PrimeField32};
use p3_matrix::dense::RowMajorMatrix;

use super::layout::*;
use super::{ActionWitness, CIRCUIT_ID, ValueBalance, sighash_fields};
use crate::field::{Digest, F, P, ZERO_DIGEST, u64_to_limbs};
use crate::hash::{Domain, compress, permute, sponge, sponge_inputs};
use crate::note::nullifier_input;

/// A spend at the field level.
#[derive(Clone, Debug)]
pub(crate) struct RawSpend {
    pub sk: Digest,
    pub limbs: [F; 4],
    pub rcm: Digest,
    pub position: u32,
    pub siblings: [Digest; 32],
    pub dummy: bool,
}

/// An output at the field level.
#[derive(Clone, Debug)]
pub(crate) struct RawOutput {
    pub owner: Digest,
    pub limbs: [F; 4],
    pub rcm: Digest,
}

/// An action at the field level, with the public values that aren't computed from the trace.
#[derive(Clone, Debug)]
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

/// A laid-out trace and the public values it proves.
pub(crate) struct Built {
    pub trace: RowMajorMatrix<F>,
    pub nullifiers: [Digest; 2],
    pub commitments: [Digest; 2],
    pub public: Vec<F>,
}

fn out8(state: [F; 16]) -> Digest {
    permute(state)[..8].try_into().expect("8")
}

/// A field element as a small signed integer (elements above p/2 are negative).
fn centered(x: F) -> i64 {
    let v = i64::from(x.as_canonical_u32());
    if v > i64::from(P / 2) { v - i64::from(P) } else { v }
}

fn commitment_input(owner: &Digest, limbs: &[F; 4], rcm: &Digest) -> [F; 20] {
    let mut input = [F::ZERO; 20];
    input[..8].copy_from_slice(owner);
    input[8..12].copy_from_slice(limbs);
    input[12..].copy_from_slice(rcm);
    input
}

/// Lays out the trace.
pub(crate) fn build(raw: &RawAction) -> Built {
    const EXTRA: usize = WIDTH - P2_COLS;
    let mut inputs = vec![[F::ZERO; 16]; HEIGHT];
    let mut extra = vec![[F::ZERO; EXTRA]; HEIGHT];
    let col = |c: usize| c - P2_COLS;
    let mut nullifiers = [ZERO_DIGEST; 2];
    let mut commitments = [ZERO_DIGEST; 2];

    // Value bits: the low 16 bits of each limb. A limb ≥ 2^16 has no valid decomposition.
    let set_value_bits = |row: &mut [F; EXTRA], limbs: &[F; 4]| {
        for (k, limb) in limbs.iter().enumerate() {
            let v = limb.as_canonical_u32();
            for i in 0..16 {
                row[col(VBITS) + 16 * k + i] = F::from_bool((v >> i) & 1 == 1);
            }
        }
    };

    for (i, sp) in raw.spends.iter().enumerate() {
        let b = spend_base(i);
        inputs[b + NK] = sponge_inputs(Domain::NullifierKey, &sp.sk)[0];
        inputs[b + AK] = sponge_inputs(Domain::SpendAuth, &sp.sk)[0];
        let nk = sponge(Domain::NullifierKey, &sp.sk);
        let ak = sponge(Domain::SpendAuth, &sp.sk);
        let mut own_in = [F::ZERO; 16];
        own_in[..8].copy_from_slice(&ak);
        own_in[8..].copy_from_slice(&nk);
        let own = sponge_inputs(Domain::Owner, &own_in);
        inputs[b + OWN_A] = own[0];
        inputs[b + OWN_B] = own[1];
        let owner = sponge(Domain::Owner, &own_in);
        let cm_in = sponge_inputs(Domain::NoteCommitment, &commitment_input(&owner, &sp.limbs, &sp.rcm));
        inputs[b + CM_A..b + CM_C + 1].copy_from_slice(&cm_in);
        let cm = out8(cm_in[2]);
        let mut node = cm;
        for k in 0..32 {
            let bit = (sp.position >> k) & 1 == 1;
            let sib = sp.siblings[k];
            let (left, right) = if bit { (sib, node) } else { (node, sib) };
            let row = b + MERKLE + k;
            inputs[row][..8].copy_from_slice(&left);
            inputs[row][8..].copy_from_slice(&right);
            extra[row][col(BIT)] = F::from_bool(bit);
            node = compress(&left, &right);
        }
        let nf_in = sponge_inputs(Domain::Nullifier, &nullifier_input(&nk, &cm, sp.position));
        inputs[b + NF_A..b + NF_C + 1].copy_from_slice(&nf_in);
        nullifiers[i] = out8(nf_in[2]);

        let mut pacc = [0u32; 2];
        for r in 0..SPEND_ROWS {
            let row = &mut extra[b + r];
            row[col(REG_NK)..col(REG_NK) + 8].copy_from_slice(&nk);
            row[col(REG_CM)..col(REG_CM) + 8].copy_from_slice(&cm);
            row[col(DUMMY)] = F::from_bool(sp.dummy);
            row[col(PACC_LO)] = F::from_u32(pacc[0]);
            row[col(PACC_HI)] = F::from_u32(pacc[1]);
            if (MERKLE..MERKLE + 32).contains(&r) {
                let k = r - MERKLE;
                if (sp.position >> k) & 1 == 1 {
                    pacc[k / 16] += 1 << (k % 16);
                }
            }
        }
        set_value_bits(&mut extra[b + CM_B], &sp.limbs);
    }

    for (j, out) in raw.outputs.iter().enumerate() {
        let b = output_base(j);
        let cm_in = sponge_inputs(Domain::NoteCommitment, &commitment_input(&out.owner, &out.limbs, &out.rcm));
        inputs[b..b + OUTPUT_ROWS].copy_from_slice(&cm_in);
        commitments[j] = out8(cm_in[2]);
        set_value_bits(&mut extra[b + 1], &out.limbs);
    }

    // Balance accumulator: row r holds the sum over value rows before r.
    let vsign = &periodic_columns()[sel::VSIGN];
    let mut acc = [F::ZERO; 4];
    for r in 0..HEIGHT {
        extra[r][col(ACC)..col(ACC) + 4].copy_from_slice(&acc);
        for k in 0..4 {
            acc[k] += vsign[r] * inputs[r][k];
        }
    }

    // Carries for the claimed value balance.
    let last = &mut extra[BALANCE_ROW];
    let sign = F::ONE - raw.vb_neg.double();
    let mut carry_in = 0i64;
    for k in 0..3 {
        let t = centered(last[col(ACC) + k] - sign * raw.vb[k]) + carry_in;
        let c = t.div_euclid(1 << 16);
        let bits = (c + 4).clamp(0, 7);
        for i in 0..3 {
            last[col(CBITS) + 3 * k + i] = F::from_bool((bits >> i) & 1 == 1);
        }
        carry_in = c;
    }

    let p2 = p2_trace(inputs);
    let mut values = Vec::with_capacity(HEIGHT * WIDTH);
    for r in 0..HEIGHT {
        values.extend_from_slice(&p2.values[r * P2_COLS..(r + 1) * P2_COLS]);
        values.extend_from_slice(&extra[r]);
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
