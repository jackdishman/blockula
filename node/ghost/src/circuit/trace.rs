//! Witness to trace. This follows the witness without judging it: an inconsistent witness gives
//! a trace that fails the constraints, which is what the soundness tests rely on.

use p3_field::{PrimeCharacteristicRing, PrimeField32};
use p3_matrix::dense::RowMajorMatrix;

use super::layout::*;
use super::{ActionWitness, PublicInputs, ValueBalance};
use crate::field::{Digest, F, ZERO_DIGEST};
use crate::hash::{Domain, compress, permute, sponge, sponge_inputs};
use crate::note::{Note, nullifier_input};

/// What a trace proves, before the value balance is known to fit.
pub(crate) struct Built {
    pub trace: RowMajorMatrix<F>,
    pub nullifiers: [Digest; 2],
    pub commitments: [Digest; 2],
    /// Σ spent − Σ output, as an integer.
    pub balance: i128,
}

fn out8(state: [F; 16]) -> Digest {
    permute(state)[..8].try_into().expect("8")
}

/// Builds the trace (with the honest carries for `vb`) and the public values it implies.
pub(crate) fn build(w: &ActionWitness, vb: Option<ValueBalance>) -> Built {
    let mut inputs = vec![[F::ZERO; 16]; HEIGHT];
    let mut extra = vec![[F::ZERO; WIDTH - P2_COLS]; HEIGHT];
    let col = |c: usize| c - P2_COLS;
    let mut nullifiers = [ZERO_DIGEST; 2];
    let mut commitments = [ZERO_DIGEST; 2];
    let mut balance: i128 = 0;

    let set_value_bits = |row: &mut [F; WIDTH - P2_COLS], value: u64| {
        for i in 0..64 {
            row[col(VBITS) + i] = F::from_bool((value >> i) & 1 == 1);
        }
    };

    for (i, sp) in w.spends.iter().enumerate() {
        let b = spend_base(i);
        let sk = sp.sk;
        inputs[b + NK] = sponge_inputs(Domain::NullifierKey, &sk)[0];
        inputs[b + AK] = sponge_inputs(Domain::SpendAuth, &sk)[0];
        let nk = sponge(Domain::NullifierKey, &sk);
        let ak = sponge(Domain::SpendAuth, &sk);
        let mut own_in = [F::ZERO; 16];
        own_in[..8].copy_from_slice(&ak);
        own_in[8..].copy_from_slice(&nk);
        let own = sponge_inputs(Domain::Owner, &own_in);
        inputs[b + OWN_A] = own[0];
        inputs[b + OWN_B] = own[1];
        let owner = sponge(Domain::Owner, &own_in);
        // The circuit commits to the owner derived from sk, whatever the witness note says.
        let note = Note { owner, ..sp.note };
        let cm_in = sponge_inputs(Domain::NoteCommitment, &note.commitment_input());
        inputs[b + CM_A] = cm_in[0];
        inputs[b + CM_B] = cm_in[1];
        inputs[b + CM_C] = cm_in[2];
        let cm = out8(cm_in[2]);
        let pos = sp.path.position;
        let mut node = cm;
        for k in 0..32 {
            let bit = (pos >> k) & 1 == 1;
            let sib = sp.path.siblings[k];
            let (left, right) = if bit { (sib, node) } else { (node, sib) };
            let row = b + MERKLE + k;
            inputs[row][..8].copy_from_slice(&left);
            inputs[row][8..].copy_from_slice(&right);
            extra[row][col(BIT)] = F::from_bool(bit);
            node = compress(&left, &right);
        }
        let nf_in = sponge_inputs(Domain::Nullifier, &nullifier_input(&nk, &cm, pos));
        inputs[b + NF_A] = nf_in[0];
        inputs[b + NF_B] = nf_in[1];
        inputs[b + NF_C] = nf_in[2];
        nullifiers[i] = out8(nf_in[2]);

        // Registers.
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
                if (pos >> k) & 1 == 1 {
                    pacc[k / 16] += 1 << (k % 16);
                }
            }
        }
        set_value_bits(&mut extra[b + CM_B], note.value);
        balance += i128::from(note.value);
    }

    for (j, note) in w.outputs.iter().enumerate() {
        let b = output_base(j);
        let cm_in = sponge_inputs(Domain::NoteCommitment, &note.commitment_input());
        inputs[b..b + 3].copy_from_slice(&cm_in);
        commitments[j] = out8(cm_in[2]);
        set_value_bits(&mut extra[b + 1], note.value);
        balance -= i128::from(note.value);
    }

    // Balance accumulator: acc on row r includes the value rows before r.
    let vsign = &periodic_columns()[sel::VSIGN];
    let mut acc = [0i64; 4];
    for r in 0..HEIGHT {
        for k in 0..4 {
            extra[r][col(ACC) + k] = F::from_i64(acc[k]);
        }
        let sign = if vsign[r] == F::ONE {
            1
        } else if vsign[r] == F::NEG_ONE {
            -1
        } else {
            0
        };
        for k in 0..4 {
            acc[k] += sign * i64::from(inputs[r][k].as_canonical_u32());
        }
    }

    // Carries for the claimed value balance (the honest one unless a test overrides it).
    let vb = vb.or_else(|| ValueBalance::new(balance)).unwrap_or_default();
    let mut carry_in = 0i64;
    let last = &mut extra[BALANCE_ROW];
    for k in 0..3 {
        let vb_limb = ((vb.magnitude() >> (16 * k)) & 0xffff) as i64;
        let signed = if vb.is_negative() { -vb_limb } else { vb_limb };
        let t = last_acc(last, col(ACC) + k) - signed + carry_in;
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
    Built {
        trace: RowMajorMatrix::new(values, WIDTH),
        nullifiers,
        commitments,
        balance,
    }
}

fn last_acc(row: &[F; WIDTH - P2_COLS], c: usize) -> i64 {
    let v = row[c].as_canonical_u32();
    // acc is a small signed integer stored mod p.
    if v > crate::field::P / 2 { i64::from(v) - i64::from(crate::field::P) } else { i64::from(v) }
}

/// The public values a built trace proves.
pub(crate) fn public_inputs(w: &ActionWitness, built: &Built, vb: ValueBalance) -> PublicInputs {
    PublicInputs {
        anchor: w.anchor,
        nullifiers: built.nullifiers,
        commitments: built.commitments,
        value_balance: vb,
        sighash: w.sighash,
    }
}
