//! Helpers the Ogg mappings' tests share: decoding a file through the public
//! source, the 16-bit references, and taking files apart page by page.

#![allow(
    clippy::cast_possible_truncation,
    reason = "test references stay within the 16 bits they were stored in"
)]

extern crate std;

use std::vec;
use std::vec::Vec;

use crate::ogg::PageWriter;
use crate::{DecodeError, DecodeLimits, PcmSource};

pub(super) const LIMITS: DecodeLimits = DecodeLimits::new(8, 4096, 64);

/// Every sample the current link of `file` decodes to, interleaved, and how
/// the link ended.
pub(super) fn decode_link(
    source: &mut PcmSource,
    file: &[u8],
) -> (Vec<f32>, Result<(), DecodeError>) {
    let mut input = file;
    let mut out = vec![0u8; 777 * source.info().frame_bytes()];
    let mut samples = Vec::new();
    loop {
        match source.next_block(&mut input, &mut out) {
            Ok(0) => return (samples, Ok(())),
            Ok(written) => samples.extend(
                out[..written * source.info().frame_bytes()]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|bytes| f32::from_le_bytes(*bytes)),
            ),
            Err(err) => return (samples, Err(err)),
        }
    }
}

pub(super) fn decode(file: &[u8]) -> (PcmSource, Vec<f32>, Result<(), DecodeError>) {
    let mut source = PcmSource::open(&mut &file[..], &LIMITS).expect("opens");
    let (samples, end) = decode_link(&mut source, file);
    (source, samples, end)
}

/// The 16-bit samples a FLAC reference holds.
pub(super) fn reference(flac: &[u8]) -> Vec<i32> {
    let mut source = PcmSource::open(&mut &flac[..], &LIMITS).expect("a reference");
    let mut input = flac;
    let mut out = vec![0u8; 4096 * source.info().frame_bytes()];
    let mut samples = Vec::new();
    loop {
        match source.next_block(&mut input, &mut out) {
            Ok(0) => break,
            Ok(written) => samples.extend(
                out[..written * source.info().frame_bytes()]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|bytes| i32::from(i16::from_le_bytes(*bytes))),
            ),
            Err(err) => panic!("the reference fails: {err:?}"),
        }
    }
    samples
}

/// `samples` quantised as the references were.
pub(super) fn quantised(samples: &[f32]) -> Vec<i32> {
    samples
        .iter()
        .map(|&sample| {
            (f64::from(sample) * 32768.0 + 0.5)
                .floor()
                .clamp(-32768.0, 32767.0) as i32
        })
        .collect()
}

pub(super) fn assert_within_one(got: &[i32], want: &[i32], what: &str) {
    assert_eq!(got.len(), want.len(), "{what}: sample counts");
    let worst = got
        .iter()
        .zip(want)
        .map(|(a, b)| (a - b).abs())
        .max()
        .unwrap_or(0);
    assert!(worst <= 1, "{what}: off by {worst}");
}

/// The pages of `file` in order, each whole.
pub(super) fn pages(file: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < file.len() {
        let segments = usize::from(file[at + 26]);
        let body: usize = file[at + 27..at + 27 + segments]
            .iter()
            .map(|&len| usize::from(len))
            .sum();
        out.push(file[at..at + 27 + segments + body].to_vec());
        at += 27 + segments + body;
    }
    out
}

/// `page` with its CRC made good again.
pub(super) fn reseal(page: &mut [u8]) {
    page[22..26].fill(0);
    let crc = crate::crc::crc32(0, page);
    page[22..26].copy_from_slice(&crc.to_le_bytes());
}

/// A stream whose codec is not read here, `count` pages after its first.
pub(super) fn other_stream(serial: u32, count: u32, page: usize) -> Vec<Vec<u8>> {
    let mut writer = PageWriter::new(serial);
    writer.packet(b"\x80theora header", 0);
    writer.flush(false);
    for at in 1..=count {
        writer.packet(&vec![at as u8; page], u64::from(at));
        writer.flush(at == count);
    }
    pages(&writer.into_bytes())
}
