//! Replays the committed samples, each on the profile it was drawn for: the
//! ARM7TDMI conformance suite's ARM state on the 26-bit profile, and its
//! Thumb and ARM's `BX` on the 32-bit one; and FPEmulator's answers to FPA
//! instructions on the 32-bit profile.

use std::collections::BTreeSet;

use tairix_riscos_arm::a32;
use tairix_riscos_arm::fpa::{Exception, Format};
use tairix_riscos_arm::instruction::Op;
use tairix_riscos_arm::profile::{Profile, BIT26, BIT32};
use tairix_riscos_arm::psr;
use tairix_riscos_arm::vectors::{check, decode, Mismatch, Vector};

/// Each sample, its profile, and how many vectors it holds: 128 of each of
/// the files the profile is held to, and FPEmulator's 2200.
const SAMPLES: [(&[u8], &Profile, usize); 3] = [
    (
        include_bytes!("vectors/arm7tdmi-26bit.bin"),
        &BIT26,
        19 * 128,
    ),
    (
        include_bytes!("vectors/arm7tdmi-32bit.bin"),
        &BIT32,
        23 * 128,
    ),
    (include_bytes!("fpa/fpemulator.bin"), &BIT32, 2200),
];

fn sample(bytes: &[u8]) -> Vec<Vector> {
    decode(bytes).expect("the committed sample decodes")
}

#[test]
fn the_processor_matches_every_sampled_vector() {
    for (bytes, profile, count) in SAMPLES {
        let vectors = sample(bytes);
        assert_eq!(vectors.len(), count, "{}", profile.name);
        for (index, vector) in vectors.iter().enumerate() {
            if let Err(mismatch) = check(profile, vector) {
                panic!(
                    "{} vector {index}, {:08X}: {mismatch:?}",
                    profile.name, vector.word
                );
            }
        }
    }
}

#[test]
fn the_32_bit_sample_holds_thumb_state() {
    let vectors = sample(SAMPLES[1].0);
    let thumb = vectors
        .iter()
        .filter(|vector| vector.before.cpsr & psr::T != 0)
        .count();
    assert_eq!(
        thumb,
        22 * 128,
        "every Thumb file's sample, and BX's in ARM state"
    );
}

#[test]
fn the_fpa_sample_holds_every_operation_format_and_trap() {
    let vectors = sample(SAMPLES[2].0);
    let mut kinds = BTreeSet::new();
    let mut traps = BTreeSet::new();
    for vector in &vectors {
        let case = vector.fpa.expect("every vector is an FPA instruction's");
        if let Some(exception) = case.trap {
            traps.insert(format!("{exception:?}"));
        }
        let Some(instruction) = a32::decode(vector.word, BIT32.isa) else {
            continue;
        };
        kinds.insert(match instruction.op {
            Op::FpaDyadic { op, .. } => op.name().to_string(),
            Op::FpaMonadic { op, .. } => op.name().to_string(),
            Op::FpaRegister { op, .. } => op.name().to_string(),
            Op::FpaCompare { op, .. } => op.name().to_string(),
            Op::FpaTransfer { op, format, .. } => format!("{}{format:?}", op.name()),
            Op::FpaMultiple { op, .. } => op.name().to_string(),
            _ => continue,
        });
    }
    let dyadic = 13;
    let monadic = 16;
    let transfers = 4 + 4 + 2 * Format::ALL.len() + 2;
    assert_eq!(kinds.len(), dyadic + monadic + transfers, "{kinds:?}");
    assert_eq!(traps.len(), Exception::ALL.len(), "{traps:?}");
}

#[test]
fn the_runner_reports_a_wrong_register_and_a_wrong_transfer() {
    for (bytes, profile, _) in SAMPLES {
        let vectors = sample(bytes);
        let mut wrong = vectors[0].clone();
        wrong.after.user[0] ^= 1;
        assert!(matches!(
            check(profile, &wrong),
            Err(Mismatch::Registers { .. })
        ));
        let mut transferring = vectors
            .iter()
            .find(|vector| !vector.transfers.is_empty())
            .expect("a sampled vector reaches memory")
            .clone();
        transferring.transfers[0].address ^= 4;
        assert!(matches!(
            check(profile, &transferring),
            Err(Mismatch::Transfer { .. })
        ));
    }
}
