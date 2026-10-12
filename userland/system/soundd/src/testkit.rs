//! What the crate's tests author their sounds with.

use alloc::vec;
use alloc::vec::Vec;

use tairix_sound::flac_encode::{encode, Options, Params};

/// A stereo 16-bit FLAC of `samples`.
pub(crate) fn flac(samples: &[i32]) -> Vec<u8> {
    flac_of(2, samples)
}

/// A 48 kHz 16-bit FLAC of `samples` interleaving `channels`.
pub(crate) fn flac_of(channels: u8, samples: &[i32]) -> Vec<u8> {
    let params = Params {
        rate: 48_000,
        channels,
        bits: 16,
    };
    encode(params, samples, Options::default())
        .expect("encodes")
        .finish()
}

/// `frames` stereo frames of a ramp, silent at both ends.
pub(crate) fn chime(frames: usize) -> Vec<i32> {
    let mut samples = vec![0i32; 2];
    samples.extend((1..frames - 1).flat_map(|n| {
        let n = i32::try_from(n % 400).expect("small");
        [n - 200, 200 - n]
    }));
    samples.extend([0, 0]);
    samples
}

/// A 48 kHz stereo 16-bit WAV of `samples`.
pub(crate) fn wav(samples: &[i32]) -> Vec<u8> {
    let data: Vec<u8> = samples
        .iter()
        .flat_map(|&sample| i16::try_from(sample).expect("16-bit").to_le_bytes())
        .collect();
    let len = |n: usize| u32::try_from(n).expect("small").to_le_bytes();
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&len(36 + data.len()));
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&len(16));
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&48_000u32.to_le_bytes());
    out.extend_from_slice(&(48_000u32 * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&len(data.len()));
    out.extend_from_slice(&data);
    out
}
