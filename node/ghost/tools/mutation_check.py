#!/usr/bin/env python3
"""Mutation check for the Action AIR: deletes each constraint group in turn, reruns the circuit
tests and reports which tests caught the deletion. Every deletion should be caught, except the
documented equivalent mutant (dummy booleanity, docs/GHOST-MODE.md 5.3).

Run from node/ghost: python3 tools/mutation_check.py  (≈ 37 test runs; restores air.rs at the end)
"""
import re
import subprocess

AIR = "src/circuit/air.rs"

# (name, exact text in air.rs, replacement)
MUTATIONS = [
    ("regs zero outside spends", "            builder.assert_zero((one() - s[sel::SPEND].clone()) * l(c));\n            builder.assert_zero(s[sel::CARRY].clone() * (n(c) - l(c)));", "            builder.assert_zero(s[sel::CARRY].clone() * (n(c) - l(c)));"),
    ("regs carry", "            builder.assert_zero((one() - s[sel::SPEND].clone()) * l(c));\n            builder.assert_zero(s[sel::CARRY].clone() * (n(c) - l(c)));", "            builder.assert_zero((one() - s[sel::SPEND].clone()) * l(c));"),
    ("root check", "            builder.assert_zero(\n                s[sel::ROOT].clone() * (one() - l(DUMMY)) * (root - pis[pv::ANCHOR + j].clone()),\n            );", "            let _ = root;"),
    ("dummy zero value", "            builder.assert_zero(s[sel::SPEND_VALUE].clone() * l(DUMMY) * l(IN + k));", ""),
    ("range sum", "            builder.assert_zero(sum - s[sel::VALUE].clone() * l(IN + k));", "            let _ = sum;"),
    ("balance limb 3", "            builder.assert_zero(s[sel::BALANCE].clone() * e);", "            if k < 3 { builder.assert_zero(s[sel::BALANCE].clone() * e); }"),
    ("nf_c zeros", "        for j in 2..8 {\n            builder.assert_zero(s[sel::NF_C].clone() * l(IN + j));\n        }", ""),
    ("pacc block start", "            builder.assert_zero(s[sel::BLOCK_START].clone() * l(c));", ""),
    ("bit outside merkle", "        builder.assert_zero((one() - s[sel::MERKLE].clone()) * l(BIT));", ""),
    ("cont capacity", "            builder.assert_zero(s[sel::CONT_NEXT].clone() * (n(IN + 8 + j) - l(OUT + 8 + j)));", ""),
    ("ak wiring", "            builder.assert_zero(s[sel::AK_ROW].clone() * (n(IN + j) - l(OUT + j)));", ""),
    ("carry bits zero off balance", "        for c in &carry {\n            builder.assert_zero((one() - s[sel::BALANCE].clone()) * c.clone());\n        }", ""),
    ("start iv", "            builder.assert_zero(s[sel::START].clone() * l(IN + 8 + j) - s[sel::IV + j].clone());", ""),
    ("nk reg set", "            builder.assert_zero(s[sel::NK_ROW].clone() * (l(REG_NK + j) - l(OUT + j)));", ""),
    ("nk->ak same sk", "            builder.assert_zero(s[sel::NK_ROW].clone() * (n(IN + j) - l(IN + j)));", ""),
    ("own_b nk", "            builder.assert_zero(s[sel::OWN_B].clone() * (l(IN + j) - l(REG_NK + j)));", ""),
    ("owner->cm", "            builder.assert_zero(s[sel::OWN_B].clone() * (n(IN + j) - l(OUT + j)));", ""),
    ("cm_c zeros", "        for j in 4..8 {\n            builder.assert_zero(s[sel::CM_C].clone() * l(IN + j));\n        }", ""),
    ("merkle level0", "            builder.assert_zero(s[sel::SPEND_CM_C].clone() * (n(IN + j) - cm.clone() + swap.clone()));", ""),
    ("reg_cm set", "            builder.assert_zero(s[sel::SPEND_CM_C].clone() * (l(REG_CM + j) - cm));", "            let _ = cm;"),
    ("merkle cont", "            builder.assert_zero(s[sel::MERKLE_CONT].clone() * (n(IN + j) - node + swap));", "            let _ = (node, swap);"),
    ("bit bool", "        builder.assert_bool(local[BIT]);", ""),
    ("nf_a nk", "            builder.assert_zero(s[sel::NF_A].clone() * (l(IN + j) - l(REG_NK + j)));", ""),
    ("nf_b cm", "            builder.assert_zero(s[sel::NF_B].clone() * (l(IN + j) - l(REG_CM + j)));", ""),
    ("nf_c pacc_lo", "        builder.assert_zero(s[sel::NF_C].clone() * (l(IN) - l(PACC_LO)));", ""),
    ("pub nf", "                builder.assert_zero(\n                    s[sel::PUB_NF + i].clone() * (l(OUT + j) - pis[pv::NF + 8 * i + j].clone()),\n                );", ""),
    ("pacc carry", "            builder.assert_zero(s[sel::CARRY].clone() * (n(c) - l(c) - s[w].clone() * l(BIT)));", ""),
    ("dummy bool", "        builder.assert_bool(local[DUMMY]);", ""),
    ("vbits bool", "                builder.assert_bool(local[VBITS + 16 * k + i]);", ""),
    ("acc first row", "            builder.when_first_row().assert_zero(l(ACC + k));", ""),
    ("acc transition", "            builder\n                .when_transition()\n                .assert_zero(n(ACC + k) - l(ACC + k) - s[sel::VSIGN].clone() * l(IN + k));", ""),
    ("cbits bool", "            builder.assert_bool(local[CBITS + i]);", ""),
    ("pub cm", "                builder.assert_zero(\n                    s[sel::PUB_CM + i].clone() * (l(OUT + j) - pis[pv::CM + 8 * i + j].clone()),\n                );", ""),
    ("pad zero", "            builder.assert_zero(s[sel::PAD].clone() * l(IN + j));", ""),
    ("neg bool", "        builder.when_first_row().assert_bool(neg);", ""),
    ("circuit id", "        builder\n            .when_first_row()\n            .assert_zero(pis[pv::CIRCUIT].clone() - F::from_u32(CIRCUIT_ID));", ""),
    ("poseidon2", "            self.p2.eval(&mut sub);", "            let _ = &mut sub;"),
]


def main():
    orig = open(AIR).read()
    uncaught = []
    try:
        for name, old, new in MUTATIONS:
            if old not in orig:
                print(f"{name:28s} -> MISSING (air.rs changed; update this list)")
                uncaught.append(name)
                continue
            open(AIR, "w").write(orig.replace(old, new, 1))
            r = subprocess.run(["cargo", "test", "--lib", "circuit::tests"], capture_output=True, text=True)
            failed = re.findall(r"test (circuit::tests::\S+) \.\.\. FAILED", r.stdout)
            if not failed:
                uncaught.append(name)
            caught = ", ".join(f.split("::")[-1] for f in failed) or ("NOTHING" if "test result" in r.stdout else "BUILD ERROR")
            print(f"{name:28s} -> {caught}", flush=True)
    finally:
        open(AIR, "w").write(orig)
    print("uncaught:", uncaught)


if __name__ == "__main__":
    main()
