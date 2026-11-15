//! Replays the committed sample of the ARM7TDMI conformance suite on the
//! 26-bit profile, the ARMv4 profile it was drawn for.

use tairix_riscos_arm::profile::BIT26;
use tairix_riscos_arm::vectors::{check, decode, Mismatch, Vector};

const SAMPLE: &[u8] = include_bytes!("vectors/arm7tdmi.bin");

/// 128 vectors of each of the suite's 19 ARM-state files `BIT26` is held to.
const SAMPLE_VECTORS: usize = 19 * 128;

fn sample() -> Vec<Vector> {
    decode(SAMPLE).expect("the committed sample decodes")
}

#[test]
fn the_processor_matches_every_sampled_vector() {
    let vectors = sample();
    assert_eq!(vectors.len(), SAMPLE_VECTORS);
    for (index, vector) in vectors.iter().enumerate() {
        if let Err(mismatch) = check(&BIT26, vector) {
            panic!("vector {index}, {:08X}: {mismatch:?}", vector.word);
        }
    }
}

#[test]
fn the_runner_reports_a_wrong_register_and_a_wrong_transfer() {
    let vectors = sample();
    let mut wrong = vectors[0].clone();
    wrong.after.user[0] ^= 1;
    assert!(matches!(
        check(&BIT26, &wrong),
        Err(Mismatch::Registers { .. })
    ));
    let mut transferring = vectors
        .iter()
        .find(|vector| !vector.transfers.is_empty())
        .expect("a sampled vector reaches memory")
        .clone();
    transferring.transfers[0].address ^= 4;
    assert!(matches!(
        check(&BIT26, &transferring),
        Err(Mismatch::Transfer { .. })
    ));
}
