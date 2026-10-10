//! The decoders against Sun's reference decoder: the samples it answers for
//! two code streams per rate, synthesised here, compared sample for sample at
//! the start and through FNV-1a over 8192. The reference narrows a sample by
//! wrapping it, so its output is matched by the wide output wrapped the same
//! way; the narrowing this decoder answers saturates instead.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "the tests synthesise codes and signals by narrowing values they keep in range"
)]

extern crate std;

use std::vec::Vec;

use super::{G72x, G72xRate};

/// A 64-bit xorshift's bytes, from `seed`.
pub(crate) fn xorshift(count: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    (0..count)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 32) as u8
        })
        .collect()
}

/// FNV-1a over `samples`' little-endian bytes.
pub(crate) fn fnv(samples: &[i16]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in samples.iter().flat_map(|sample| sample.to_le_bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

const COUNT: usize = 8192;
const SEED: u64 = 0x2545_F491_4F6C_DD1D;

/// `wide` as the reference's C `short` narrows it.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the reference truncates its output to 16 bits"
)]
pub(crate) const fn wrapped(wide: i32) -> i16 {
    wide as i16
}

fn decode(rate: G72xRate, codes: impl Iterator<Item = u8>) -> Vec<i16> {
    let mut decoder = G72x::new(rate);
    codes
        .map(|code| wrapped(decoder.decode_wide(code)))
        .collect()
}

/// A rate, then the random stream's first samples and hash, then the
/// ramp's.
type Vector = (G72xRate, [i16; 12], u64, [i16; 12], u64);

#[test]
fn every_rate_decodes_as_the_reference_does() {
    let vectors: [Vector; 3] = [
        (
            G72xRate::Kbit24,
            [60, 0, 16, 36, -68, 104, -84, -72, 124, -128, -220, 476],
            0xebef_e8dc_b9ad_7f2b,
            [0, 0, 0, 0, 0, 0, 0, 16, 16, 16, 16, 16],
            0xdca7_0506_e4c5_0066,
        ),
        (
            G72xRate::Kbit32,
            [-36, 88, 8, -56, -32, 28, 56, 64, -56, 76, -52, -76],
            0xa937_fdb6_0b95_db2e,
            [0, 0, 0, 0, 0, 0, 0, 8, 8, 8, 8, 8],
            0x0b1e_35b8_4a07_d3fd,
        ),
        (
            G72xRate::Kbit40,
            [104, 48, 4, -28, 120, -160, 48, 28, 132, 52, 220, 280],
            0x89c3_6765_d08d_4a95,
            [0, 0, 0, 0, 0, 0, 0, 4, 4, 4, 4, 4],
            0x15b0_659d_bc4f_ff2a,
        ),
    ];
    for (rate, random_start, random_hash, ramp_start, ramp_hash) in vectors {
        let mask = (1u8 << rate.bits()) - 1;
        let random = decode(
            rate,
            xorshift(COUNT, SEED).into_iter().map(|byte| byte & mask),
        );
        assert_eq!(random[..12], random_start, "{rate:?} random");
        assert_eq!(fnv(&random), random_hash, "{rate:?} random");
        let codes = 1usize << rate.bits();
        let ramp = decode(rate, (0..COUNT).map(|i| (i / 7 % codes) as u8));
        assert_eq!(ramp[..12], ramp_start, "{rate:?} ramp");
        assert_eq!(fnv(&ramp), ramp_hash, "{rate:?} ramp");
    }
}

#[test]
fn a_sample_past_the_rails_is_held_there() {
    // The 24 kbit/s stream drives sample 41 past the negative rail.
    let mask = (1u8 << G72xRate::Kbit24.bits()) - 1;
    let mut decoder = G72x::new(G72xRate::Kbit24);
    let samples: Vec<i16> = xorshift(43, SEED)
        .into_iter()
        .map(|byte| decoder.decode(byte & mask))
        .collect();
    assert_eq!(samples[41..], [-32768, 32767]);
}

#[test]
fn any_code_stream_decodes_without_fault() {
    for rate in [G72xRate::Kbit24, G72xRate::Kbit32, G72xRate::Kbit40] {
        for seed in [1, 0xDEAD_BEEF, SEED] {
            let mut decoder = G72x::new(rate);
            for byte in xorshift(200_000, seed) {
                decoder.decode(byte);
            }
            // Codes stuck at either extreme drive the multipliers to their
            // limits.
            for code in [0xFF, 0x00] {
                for _ in 0..10_000 {
                    decoder.decode(code);
                }
            }
        }
    }
}
