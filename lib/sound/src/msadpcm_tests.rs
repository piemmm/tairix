//! The block decoder against an encoder written here from the format's own
//! equations: the encoder tracks the decoder's state, so the decode of what it
//! wrote must be its reconstruction exactly, and close to the tone it coded.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "the tests synthesise codes and signals by narrowing values they keep in range"
)]

extern crate std;

use std::vec;
use std::vec::Vec;

use super::{decode_block, frames_in, HEADER};
use crate::DecodeError;

/// The seven coefficient pairs every Microsoft ADPCM file carries first.
const STANDARD: [[i16; 2]; 7] = [
    [256, 0],
    [512, -256],
    [0, 0],
    [192, 64],
    [240, 0],
    [460, -208],
    [392, -232],
];

const ADAPT: [i32; 16] = [
    230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230,
];

/// One channel's encoder: the code nearest each sample, and the state the
/// decoder will hold.
struct Encoder {
    coefficients: [i32; 2],
    delta: i32,
    sample1: i32,
    sample2: i32,
}

impl Encoder {
    fn code(&mut self, sample: i16) -> (u8, i16) {
        let predicted =
            (self.sample1 * self.coefficients[0] + self.sample2 * self.coefficients[1]) >> 8;
        let reconstruct = |code: i32| {
            let signed = if code >= 8 { code - 16 } else { code };
            (predicted + signed * self.delta).clamp(-32768, 32767)
        };
        let code = (0..16)
            .min_by_key(|&code| (reconstruct(code) - i32::from(sample)).abs())
            .unwrap_or(0);
        let value = reconstruct(code);
        self.sample2 = self.sample1;
        self.sample1 = value;
        self.delta = ((ADAPT[code as usize] * self.delta) >> 8).max(16);
        (code as u8, value as i16)
    }
}

fn tone(count: usize, period: f64, scale: f64) -> Vec<i16> {
    (0..count)
        .map(|i| ((i as f64 * core::f64::consts::TAU / period).sin() * scale) as i16)
        .collect()
}

/// A block of `tones`, one a channel, each with predictor `choice`, and the
/// frames its decode must give.
fn block(tones: &[Vec<i16>], choice: u8) -> (Vec<u8>, Vec<i16>) {
    let channels = tones.len();
    let mut encoders: Vec<Encoder> = tones
        .iter()
        .map(|tone| Encoder {
            coefficients: STANDARD[usize::from(choice)].map(i32::from),
            delta: 64,
            sample1: i32::from(tone[1]),
            sample2: i32::from(tone[0]),
        })
        .collect();
    let mut bytes = vec![choice; channels];
    for field in [
        |e: &Encoder| e.delta,
        |e: &Encoder| e.sample1,
        |e: &Encoder| e.sample2,
    ] {
        for encoder in &encoders {
            bytes.extend((field(encoder) as i16).to_le_bytes());
        }
    }
    let mut expected: Vec<i16> = Vec::new();
    for seed in [0, 1] {
        expected.extend(tones.iter().map(|tone| tone[seed]));
    }
    let mut codes = Vec::new();
    for at in 2..tones[0].len() {
        for (encoder, tone) in encoders.iter_mut().zip(tones) {
            let (code, value) = encoder.code(tone[at]);
            codes.push(code);
            expected.push(value);
        }
    }
    bytes.extend(
        codes
            .chunks(2)
            .map(|pair| pair[0] << 4 | pair.get(1).copied().unwrap_or(0)),
    );
    (bytes, expected)
}

fn decode(block: &[u8], channels: usize, skip: usize, frames: usize) -> Vec<i16> {
    let mut out = vec![0u8; frames * 2 * channels];
    let written = decode_block(block, channels, &STANDARD, skip, &mut out).expect("a block");
    out.as_chunks::<2>()
        .0
        .iter()
        .take(written * channels)
        .map(|pair| i16::from_le_bytes(*pair))
        .collect()
}

#[test]
fn a_block_decodes_to_what_its_encoder_reconstructed() {
    for choice in 0..7 {
        let (bytes, expected) = block(&[tone(1012, 37.5, 12000.0)], choice);
        assert_eq!(frames_in(bytes.len(), 1), Some(1012));
        assert_eq!(decode(&bytes, 1, 0, 1012), expected, "predictor {choice}");
    }
    let (bytes, expected) = block(&[tone(506, 20.0, 9000.0), tone(506, 51.0, -20000.0)], 1);
    assert_eq!(frames_in(bytes.len(), 2), Some(506));
    assert_eq!(decode(&bytes, 2, 0, 506), expected);
}

#[test]
fn the_reconstruction_follows_the_tone() {
    let input = tone(1012, 37.5, 12000.0);
    let (bytes, _) = block(core::slice::from_ref(&input), 1);
    let decoded = decode(&bytes, 1, 0, 1012);
    let signal: f64 = input.iter().map(|&s| f64::from(s).powi(2)).sum();
    let noise: f64 = input
        .iter()
        .zip(&decoded)
        .map(|(&a, &b)| (f64::from(a) - f64::from(b)).powi(2))
        .sum();
    assert!(10.0 * (signal / noise).log10() > 20.0, "SNR");
}

#[test]
fn a_block_is_entered_where_asked() {
    let (bytes, _) = block(&[tone(506, 20.0, 9000.0), tone(506, 51.0, -20000.0)], 3);
    let whole = decode(&bytes, 2, 0, 506);
    assert_eq!(decode(&bytes, 2, 1, 10), whole[2..22], "past a seed");
    assert_eq!(decode(&bytes, 2, 300, 10), whole[600..620]);
}

#[test]
fn a_predictor_past_the_table_or_a_block_short_of_its_header_is_refused() {
    let (mut bytes, _) = block(&[tone(64, 20.0, 9000.0)], 0);
    bytes[0] = 7;
    let mut out = [0u8; 256];
    assert_eq!(
        decode_block(&bytes, 1, &STANDARD, 0, &mut out),
        Err(DecodeError::WavAdpcmBlockCorrupt)
    );
    assert_eq!(
        decode_block(&[0u8; HEADER - 1], 1, &STANDARD, 0, &mut out),
        Err(DecodeError::WavAdpcmBlockCorrupt)
    );
}
