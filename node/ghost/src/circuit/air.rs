//! The Action AIR: the constraints C1–C9 of docs/GHOST-MODE.md over the layout in
//! [`super::layout`].
//!
//! Degree budget: Poseidon2 S-box constraints are degree 3 and unselected; every other
//! constraint is a selector (degree 1) times an expression of degree ≤ 2.

use std::borrow::Cow;

use p3_air::{Air, AirBuilder, BaseAir, WindowAccess};
use p3_field::PrimeCharacteristicRing;
use p3_uni_stark::SubAirBuilder;

use super::CIRCUIT_ID;
use super::layout::*;
use crate::field::F;

/// The Action AIR. Stateless apart from constant tables.
pub struct ActionAir {
    p2: P2Air,
}

impl Default for ActionAir {
    fn default() -> Self {
        Self { p2: P2Air::new(p2_constants()) }
    }
}

impl BaseAir<F> for ActionAir {
    fn width(&self) -> usize {
        WIDTH
    }

    fn num_public_values(&self) -> usize {
        pv::COUNT
    }

    fn num_periodic_columns(&self) -> usize {
        sel::COUNT
    }

    fn periodic_columns(&self) -> Cow<'_, [Vec<F>]> {
        Cow::Borrowed(periodic_columns())
    }

    fn max_constraint_degree(&self) -> Option<usize> {
        Some(3)
    }
}

impl<AB: AirBuilder<F = F>> Air<AB> for ActionAir {
    fn eval(&self, builder: &mut AB) {
        // C1: every row is one Poseidon2 permutation, out = P(in).
        {
            let mut sub = SubAirBuilder::<AB, P2Air, AB::Var>::new(builder, 0..P2_COLS);
            self.p2.eval(&mut sub);
        }

        let main = builder.main();
        let local = main.current_slice();
        let next = main.next_slice();
        let s: Vec<AB::Expr> = builder.periodic_values().iter().map(|&v| v.into()).collect();
        let pis: Vec<AB::Expr> = builder.public_values().iter().map(|&v| v.into()).collect();
        let one = || AB::Expr::ONE;
        let l = |c: usize| -> AB::Expr { local[c].into() };
        let n = |c: usize| -> AB::Expr { next[c].into() };

        // ---- Sponge wiring -----------------------------------------------------------------
        for j in 0..8 {
            // A sponge starts with its domain IV in the capacity.
            builder.assert_zero(s[sel::START].clone() * l(IN + 8 + j) - s[sel::IV + j].clone());
            // A continued sponge carries the capacity (overwrite mode).
            builder.assert_zero(s[sel::CONT_NEXT].clone() * (n(IN + 8 + j) - l(OUT + 8 + j)));
        }

        // ---- C2: keys. NK row: nk = H(sk); AK row reuses the same sk; owner = H(ak ‖ nk). ----
        for j in 0..8 {
            builder.assert_zero(s[sel::NK_ROW].clone() * (l(REG_NK + j) - l(OUT + j)));
            builder.assert_zero(s[sel::NK_ROW].clone() * (n(IN + j) - l(IN + j)));
            builder.assert_zero(s[sel::AK_ROW].clone() * (n(IN + j) - l(OUT + j)));
            builder.assert_zero(s[sel::OWN_B].clone() * (l(IN + j) - l(REG_NK + j)));
            // C3: the owner (OWN_B output) is the first chunk of the spent note's commitment.
            builder.assert_zero(s[sel::OWN_B].clone() * (n(IN + j) - l(OUT + j)));
        }

        // ---- C3: note commitments. CM_C pads the 20-element input with zeros. ----------------
        for j in 4..8 {
            builder.assert_zero(s[sel::CM_C].clone() * l(IN + j));
        }

        // ---- C4: Merkle membership. ----------------------------------------------------------
        // The running node goes left (bit 0) or right (bit 1); the other half is the sibling.
        // Level 0's node is the commitment; level k+1's is level k's compression output.
        let bit_next = n(BIT);
        for j in 0..8 {
            let swap = bit_next.clone() * (n(IN + 8 + j) - n(IN + j));
            let cm = l(OUT + j);
            builder.assert_zero(s[sel::SPEND_CM_C].clone() * (n(IN + j) - cm.clone() + swap.clone()));
            builder.assert_zero(s[sel::SPEND_CM_C].clone() * (l(REG_CM + j) - cm));
            let node = l(OUT + j) + l(IN + j);
            builder.assert_zero(s[sel::MERKLE_CONT].clone() * (n(IN + j) - node + swap));
            // The root must be the anchor unless this is a dummy spend.
            let root = l(OUT + j) + l(IN + j);
            builder.assert_zero(
                s[sel::ROOT].clone() * (one() - l(DUMMY)) * (root - pis[pv::ANCHOR + j].clone()),
            );
        }
        builder.assert_bool(local[BIT]);
        builder.assert_zero((one() - s[sel::MERKLE].clone()) * l(BIT));

        // ---- C5: nullifier = H(nk ‖ cm ‖ pos_lo ‖ pos_hi). -------------------------------------
        for j in 0..8 {
            builder.assert_zero(s[sel::NF_A].clone() * (l(IN + j) - l(REG_NK + j)));
            builder.assert_zero(s[sel::NF_B].clone() * (l(IN + j) - l(REG_CM + j)));
        }
        builder.assert_zero(s[sel::NF_C].clone() * (l(IN) - l(PACC_LO)));
        builder.assert_zero(s[sel::NF_C].clone() * (l(IN + 1) - l(PACC_HI)));
        for j in 2..8 {
            builder.assert_zero(s[sel::NF_C].clone() * l(IN + j));
        }
        for i in 0..2 {
            for j in 0..8 {
                builder.assert_zero(
                    s[sel::PUB_NF + i].clone() * (l(OUT + j) - pis[pv::NF + 8 * i + j].clone()),
                );
            }
        }

        // ---- Spend registers: zero outside spends, constant within one. -----------------------
        // The position accumulators add each Merkle row's bit times its weight; both stay below
        // 2^16 as integers, so the position is exact.
        let regs: Vec<usize> = (0..8).flat_map(|j| [REG_NK + j, REG_CM + j]).chain([DUMMY]).collect();
        for &c in &regs {
            builder.assert_zero((one() - s[sel::SPEND].clone()) * l(c));
            builder.assert_zero(s[sel::CARRY].clone() * (n(c) - l(c)));
        }
        for (c, w) in [(PACC_LO, sel::W_LO), (PACC_HI, sel::W_HI)] {
            builder.assert_zero((one() - s[sel::SPEND].clone()) * l(c));
            builder.assert_zero(s[sel::BLOCK_START].clone() * l(c));
            builder.assert_zero(s[sel::CARRY].clone() * (n(c) - l(c) - s[w].clone() * l(BIT)));
        }
        builder.assert_bool(local[DUMMY]);

        // ---- C6: values are 16-bit limbs; a dummy spend has value 0. ---------------------------
        // On non-value rows the bit sums must be 0, so all value bits are 0 there.
        for k in 0..4 {
            let mut sum = AB::Expr::ZERO;
            for i in 0..16 {
                builder.assert_bool(local[VBITS + 16 * k + i]);
                sum += l(VBITS + 16 * k + i) * F::from_u32(1 << i);
            }
            builder.assert_zero(sum - s[sel::VALUE].clone() * l(IN + k));
            builder.assert_zero(s[sel::SPEND_VALUE].clone() * l(DUMMY) * l(IN + k));
        }

        // ---- C7: value balance with explicit carries. ------------------------------------------
        // acc[k] = Σ spent limb k − Σ output limb k, an integer in [−2^17, 2^17].
        for k in 0..4 {
            builder.when_first_row().assert_zero(l(ACC + k));
            builder
                .when_transition()
                .assert_zero(n(ACC + k) - l(ACC + k) - s[sel::VSIGN].clone() * l(IN + k));
        }
        // carry_k = bits − 4 ∈ [−4, 3]; zero bits outside the balance row.
        let carry: Vec<AB::Expr> = (0..3)
            .map(|k| {
                let b = |i: usize| l(CBITS + 3 * k + i);
                b(0) + b(1) * F::TWO + b(2) * F::from_u32(4)
            })
            .collect();
        for i in 0..9 {
            builder.assert_bool(local[CBITS + i]);
        }
        for c in &carry {
            builder.assert_zero((one() - s[sel::BALANCE].clone()) * c.clone());
        }
        // acc[k] − vb[k] + c[k−1] = c[k]·2^16 with c[−1] = c[3] = 0. Every term is far below
        // p/2 in magnitude, so this holds over the integers, and summing with weights 2^(16k)
        // gives Σ in − Σ out = vb exactly.
        let neg = pis[pv::VB_NEG].clone();
        let sign = one() - neg.clone() * F::TWO;
        let shift = F::from_u32(1 << 16);
        let c = |k: usize| carry[k].clone() - F::from_u32(4);
        for k in 0..4 {
            let mut e = l(ACC + k) - sign.clone() * pis[pv::VB + k].clone();
            if k > 0 {
                e += c(k - 1);
            }
            if k < 3 {
                e -= c(k) * shift;
            }
            builder.assert_zero(s[sel::BALANCE].clone() * e);
        }

        // ---- C8 output commitments; padding rows hash zeros. -----------------------------------
        for i in 0..2 {
            for j in 0..8 {
                builder.assert_zero(
                    s[sel::PUB_CM + i].clone() * (l(OUT + j) - pis[pv::CM + 8 * i + j].clone()),
                );
            }
        }
        for j in 0..16 {
            builder.assert_zero(s[sel::PAD].clone() * l(IN + j));
        }

        // ---- Public-value sanity. C9: the sighash is bound through the Fiat-Shamir transcript,
        // which absorbs every public value before any challenge is drawn. --------------------
        builder.when_first_row().assert_bool(neg);
        builder
            .when_first_row()
            .assert_zero(pis[pv::CIRCUIT].clone() - F::from_u32(CIRCUIT_ID));
    }
}
