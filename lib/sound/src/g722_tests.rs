//! The decoder against the public-domain reference it is ported from: the
//! samples it answers for two code streams, synthesised here, compared sample
//! for sample at the start and through FNV-1a over 8192, the wide output
//! wrapped as the reference narrows it.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "the tests synthesise codes and signals by narrowing values they keep in range"
)]

extern crate std;

use std::vec::Vec;

use super::G722;
use crate::g72x::tests::{fnv, wrapped, xorshift};

const COUNT: usize = 4096;

fn decode(codes: impl Iterator<Item = u8>) -> Vec<i16> {
    let mut decoder = G722::new();
    codes
        .flat_map(|code| {
            let (first, second) = decoder.decode_wide(code);
            [wrapped(first), wrapped(second)]
        })
        .collect()
}

#[test]
fn it_decodes_as_the_reference_does() {
    let random = decode(xorshift(COUNT, 0x2545_F491_4F6C_DD1D).into_iter());
    assert_eq!(random[..12], [-1, 0, 0, -1, 0, 0, -1, -1, 0, 1, -2, -17]);
    assert_eq!(fnv(&random), 0xe4f1_f506_bc0a_8720);
    let ramp = decode((0..COUNT).map(|i| ((i / 5) * 37) as u8));
    assert_eq!(ramp[..12], [0, 0, -1, -1, 0, 0, 0, -1, -1, 0, 0, -5]);
    assert_eq!(fnv(&ramp), 0x7064_b1ef_ed10_adb0);
}

#[test]
fn a_sample_past_the_rails_is_held_there() {
    // The random stream drives sample 251 past the positive rail.
    let mut decoder = G722::new();
    let samples: Vec<i16> = xorshift(128, 0x2545_F491_4F6C_DD1D)
        .into_iter()
        .flat_map(|code| {
            let (first, second) = decoder.decode(code);
            [first, second]
        })
        .collect();
    assert_eq!(samples[251], 32767);
}

#[test]
fn any_code_stream_decodes_without_fault() {
    let mut decoder = G722::new();
    for code in xorshift(400_000, 7) {
        decoder.decode(code);
    }
    for code in [0xFF, 0x00, 0x3F, 0xC0] {
        for _ in 0..20_000 {
            decoder.decode(code);
        }
    }
}
