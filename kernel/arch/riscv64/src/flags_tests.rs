//! Host pins that no inline block claiming `preserves_flags` writes what that
//! option promises the compiler it leaves alone on RISC-V: `fflags` and the
//! vector status. The blocks cannot run on the host, so they are read from
//! the port's sources; the needles live here, where none can match itself.

use std::path::Path;
use std::vec::Vec;

use tairix_fuzzseed::asm::{invocations, mnemonic, template_instructions};

/// The CSRs `preserves_flags` covers, by name and by number.
const COVERED_CSRS: [(&str, &str); 6] = [
    ("fflags", "0x001"),
    ("fcsr", "0x003"),
    ("vstart", "0x008"),
    ("vxsat", "0x009"),
    ("vxrm", "0x00a"),
    ("vcsr", "0x00f"),
];

/// Whether `operand` names a CSR `preserves_flags` covers.
fn names_covered_csr(operand: &str) -> bool {
    let operand = operand.trim().to_ascii_lowercase();
    COVERED_CSRS
        .iter()
        .any(|(name, number)| operand == *name || operand == *number)
}

/// Whether the floating-point operation `op` accrues exception flags: the
/// computational, compare and convert ones. Loads, stores, moves, sign
/// injection and classification leave `fflags` alone.
fn accrues_fflags(op: &str) -> bool {
    let Some(rest) = op.strip_prefix('f') else {
        return false;
    };
    let base = rest.split('.').next().unwrap_or(rest);
    matches!(
        base,
        "add"
            | "sub"
            | "mul"
            | "div"
            | "sqrt"
            | "min"
            | "max"
            | "madd"
            | "msub"
            | "nmadd"
            | "nmsub"
            | "cvt"
            | "eq"
            | "lt"
            | "le"
    )
}

/// Whether `instruction` may write `fflags` or the vector status.
fn writes_covered_state(instruction: &str) -> bool {
    let Some(op) = mnemonic(instruction) else {
        return false;
    };
    let op = op.to_ascii_lowercase();
    let operands: Vec<&str> = instruction
        .split_once(' ')
        .map_or("", |(_, operands)| operands)
        .split(',')
        .collect();
    match op.as_str() {
        "fscsr" | "fsflags" | "fsflagsi" => true,
        "csrw" | "csrs" | "csrc" | "csrwi" | "csrsi" | "csrci" => {
            operands.first().is_some_and(|csr| names_covered_csr(csr))
        }
        "csrrw" | "csrrs" | "csrrc" | "csrrwi" | "csrrsi" | "csrrci" => {
            operands.get(1).is_some_and(|csr| names_covered_csr(csr))
        }
        _ => op.starts_with("vset") || accrues_fflags(&op),
    }
}

#[test]
fn the_covered_state_is_read_from_each_instruction_that_writes_it() {
    for writes in [
        "fscsr {t}",
        "fsflags t0",
        "fsflagsi 0",
        "csrw fcsr, {t}",
        "csrrs zero, 0x001, t0",
        "CSRW VXRM, t0",
        "vsetvli t0, a0, e8, m1",
        "fadd.d f0, f1, f2",
        "fcvt.w.d a0, f0",
        "feq.d a0, f0, f1",
    ] {
        assert!(writes_covered_state(writes), "{writes}");
    }
    for leaves in [
        "fld f0, 16({a})",
        "fsd f0, 16({a})",
        "fmv.x.d a0, f0",
        "fsgnj.d f0, f1, f1",
        "fclass.d a0, f0",
        "frcsr {t}",
        "csrw sstatus, t0",
        "csrr t0, fcsr",
        "fence",
        "2:",
    ] {
        assert!(!writes_covered_state(leaves), "{leaves}");
    }
}

/// A block telling the compiler it preserves the flags writes none of them,
/// or the compiler may keep an `fflags` reading across it.
#[test]
fn no_block_claiming_to_preserve_the_flags_writes_them() {
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
    let mut claims = 0usize;
    for source in tairix_fuzzseed::sources::crate_sources(root).expect("the port's sources") {
        for block in invocations(source.text(), "asm")
            .into_iter()
            .filter(|block| block.contains("preserves_flags"))
        {
            claims += 1;
            for instruction in template_instructions(block) {
                assert!(
                    !writes_covered_state(&instruction),
                    "{}: `{instruction}` may write the flags its block claims to preserve",
                    source.relative()
                );
            }
        }
    }
    assert!(claims > 0, "the scan found no block to check");
}
