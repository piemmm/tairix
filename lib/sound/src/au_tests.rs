//! AU files synthesised here, in every encoding the format defines, read
//! through the crate's own entry points.

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

use tairix_abi::driver::audio::{ChannelMap, SampleFormat};

use crate::g711::{ALAW, ULAW};
use crate::g722::G722;
use crate::g72x::tests::xorshift;
use crate::g72x::{G72x, G72xRate};
use crate::{
    probe, DataLength, DecodeError, DecodeLimits, Encoding, InputError, PcmSource, SoundFormat,
    SoundInput, TagKind,
};

const LIMITS: DecodeLimits = DecodeLimits::new(8, 1024, 64);

/// An AU file: its header, `annotation`, then `data`, the data size
/// `declared` where given and the size of `data` otherwise.
fn au(
    encoding: u32,
    rate: u32,
    channels: u32,
    annotation: &[u8],
    data: &[u8],
    declared: Option<u32>,
) -> Vec<u8> {
    let offset = 24 + u32::try_from(annotation.len()).expect("small");
    let size = declared.unwrap_or(u32::try_from(data.len()).expect("small"));
    let mut file = b".snd".to_vec();
    for field in [offset, size, encoding, rate, channels] {
        file.extend(field.to_be_bytes());
    }
    file.extend(annotation);
    file.extend(data);
    file
}

/// Every frame of `file`, read in blocks of `frames`.
fn decode(file: &[u8], frames: usize) -> (PcmSource, Vec<u8>) {
    let mut input = file;
    let mut source = PcmSource::open(&mut input, &LIMITS).expect("opens");
    let mut block = vec![0u8; frames * source.info().frame_bytes()];
    let mut all = Vec::new();
    loop {
        let written = source.next_block(&mut input, &mut block).expect("reads");
        if written == 0 {
            break;
        }
        all.extend_from_slice(&block[..written * source.info().frame_bytes()]);
    }
    (source, all)
}

fn open(file: &[u8]) -> Result<PcmSource, DecodeError> {
    let mut input = file;
    PcmSource::open(&mut input, &LIMITS)
}

fn s16(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

#[test]
fn each_linear_and_fixed_width_reads_as_its_samples() {
    for (codes, encoding) in [
        (2..=5, Encoding::Linear { bits: 0 }),
        (11..=14, Encoding::Fixed { bits: 0 }),
    ] {
        for (code, width) in codes.zip(1..) {
            let values: [i64; 4] = [0, 1, -1, (1 << (8 * width - 1)) - 1];
            let data: Vec<u8> = values
                .iter()
                .flat_map(|&v| v.to_be_bytes()[8 - width..].to_vec())
                .collect();
            let (source, pcm) = decode(&au(code, 8000, 1, &[], &data, None), 3);
            let info = source.info();
            let bits = u8::try_from(width * 8).expect("small");
            let named = match encoding {
                Encoding::Linear { .. } => Encoding::Linear { bits },
                _ => Encoding::Fixed { bits },
            };
            assert_eq!(info.encoding, named);
            let expected: Vec<u8> = if width == 1 {
                values.iter().map(|&v| (v as i8 as u8) ^ 0x80).collect()
            } else {
                values
                    .iter()
                    .flat_map(|&v| v.to_le_bytes()[..width].to_vec())
                    .collect()
            };
            assert_eq!(pcm, expected, "{named:?}");
            let sample = [
                SampleFormat::U8,
                SampleFormat::S16,
                SampleFormat::S24,
                SampleFormat::S32,
            ][width - 1];
            assert_eq!(info.sample, sample);
        }
    }
}

#[test]
fn floats_read_finite_and_doubles_narrow() {
    let singles = [0.5f32, -1.0, f32::NAN, f32::INFINITY];
    let data: Vec<u8> = singles.iter().flat_map(|v| v.to_be_bytes()).collect();
    let (source, pcm) = decode(&au(6, 44_100, 2, &[], &data, None), 1);
    assert_eq!(source.info().sample, SampleFormat::F32);
    let read: Vec<f32> = pcm
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    assert_eq!(read, [0.5, -1.0, 0.0, 1.0]);
    let doubles = [0.25f64, -0.75];
    let data: Vec<u8> = doubles.iter().flat_map(|v| v.to_be_bytes()).collect();
    let (source, pcm) = decode(&au(7, 44_100, 1, &[], &data, None), 5);
    assert_eq!(source.info().encoding, Encoding::Float { bits: 64 });
    let read: Vec<f32> = pcm
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    assert_eq!(read, [0.25, -0.75]);
}

#[test]
fn each_law_expands_through_its_table() {
    let codes: Vec<u8> = (0..=255).collect();
    for (encoding, table, named) in [(1, &ULAW, Encoding::MuLaw), (27, &ALAW, Encoding::ALaw)] {
        let (source, pcm) = decode(&au(encoding, 8000, 1, &[], &codes, None), 7);
        assert_eq!(source.info().encoding, named);
        assert_eq!(pcm, s16(table));
    }
}

#[test]
fn the_header_states_rate_channels_frames_and_the_annotation() {
    let data = [0u8; 12];
    let file = au(3, 22_050, 2, b"Recorded at home\0\0\0\0", &data, None);
    let (source, _) = decode(&file, 4);
    let info = source.info();
    assert_eq!(info.format, SoundFormat::Au);
    assert_eq!(info.rate.hz(), 22_050);
    assert_eq!(info.channels, ChannelMap::STEREO);
    assert_eq!(info.frames, Some(3));
    assert!(info.seekable);
    assert_eq!(info.data_length, None);
    let tags = &source.metadata().tags;
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].kind, TagKind::Comment);
    assert_eq!(tags[0].value, "Recorded at home");
    let mut input = file.as_slice();
    assert_eq!(probe(&mut input).expect("probes").frames, Some(3));
    let tight = DecodeLimits::new(8, 4, 0);
    let source = PcmSource::open(&mut input, &tight).expect("opens");
    assert!(source.metadata().tags.is_empty());
    assert!(source.metadata().omitted);
}

#[test]
fn an_unknown_size_reads_to_the_end_and_a_disagreeing_one_yields_to_the_file() {
    let data = s16(&[1, 2, 3, 4]);
    let swapped: Vec<u8> = data.chunks(2).flat_map(|p| [p[1], p[0]]).collect();
    let (source, _) = decode(&au(3, 8000, 1, &[], &swapped, Some(u32::MAX)), 2);
    assert_eq!(source.info().frames, Some(4));
    assert_eq!(source.info().data_length, None);
    let (source, pcm) = decode(&au(3, 8000, 1, &[], &swapped, Some(100)), 2);
    assert_eq!(source.info().frames, Some(4), "the file wins");
    assert_eq!(
        source.info().data_length,
        Some(DataLength {
            declared: 100,
            held: 8
        })
    );
    assert_eq!(pcm, data);
    let (source, pcm) = decode(&au(3, 8000, 1, &[], &swapped, Some(4)), 2);
    assert_eq!(source.info().frames, Some(2), "what follows is not sound");
    assert_eq!(pcm, data[..4]);
}

#[test]
fn every_encoding_that_is_not_sampled_or_never_specified_is_refused_by_name() {
    for (encoding, refused) in [
        (8, DecodeError::AuFragmentedData),
        (9, DecodeError::AuNestedSound),
        (10, DecodeError::AuDspProgram),
        (16, DecodeError::AuDisplayData),
        (17, DecodeError::AuUnspecifiedEncoding),
        (18, DecodeError::AuUnspecifiedEncoding),
        (19, DecodeError::AuUnspecifiedEncoding),
        (20, DecodeError::AuUnspecifiedEncoding),
        (21, DecodeError::AuDspCommands),
        (22, DecodeError::AuDspCommands),
        (0, DecodeError::AuUnknownEncoding(0)),
        (15, DecodeError::AuUnknownEncoding(15)),
        (28, DecodeError::AuUnknownEncoding(28)),
    ] {
        assert_eq!(
            open(&au(encoding, 8000, 1, &[], &[0; 8], None)).err(),
            Some(refused)
        );
    }
}

/// `codes` of `bits` bits packed least significant first.
fn pack(codes: &[u8], bits: u32) -> Vec<u8> {
    let (mut out, mut buffer, mut held) = (Vec::new(), 0u32, 0);
    for &code in codes {
        buffer |= u32::from(code) << held;
        held += bits;
        while held >= 8 {
            out.push(buffer as u8);
            buffer >>= 8;
            held -= 8;
        }
    }
    if held > 0 {
        out.push(buffer as u8);
    }
    out
}

#[test]
fn each_g72x_rate_decodes_its_packed_codes_in_any_block_size() {
    for (encoding, rate) in [
        (23, G72xRate::Kbit32),
        (25, G72xRate::Kbit24),
        (26, G72xRate::Kbit40),
    ] {
        let bits = rate.bits();
        let codes: Vec<u8> = xorshift(1000, 3)
            .into_iter()
            .map(|b| b & ((1 << bits) - 1))
            .collect();
        let file = au(encoding, 8000, 1, &[], &pack(&codes, bits), None);
        let mut direct = G72x::new(rate);
        let whole = codes.len() * 8 / usize::try_from(bits).expect("small") / 8 * 8;
        let expected: Vec<i16> = codes.iter().map(|&code| direct.decode(code)).collect();
        for block in [1, 7, 64, 1000] {
            let (source, pcm) = decode(&file, block);
            assert!(!source.info().seekable);
            let frames = usize::try_from(source.info().frames.expect("stated")).expect("small");
            assert!(frames >= 1000 && frames <= whole + 8);
            assert_eq!(pcm[..2000], s16(&expected), "{rate:?} in blocks of {block}");
        }
        let mut source = open(&file).expect("opens");
        assert_eq!(source.seek(10), Err(DecodeError::SeekUnsupported));
        assert_eq!(
            open(&au(encoding, 8000, 2, &[], &[0; 8], None)).err(),
            Some(DecodeError::AuAdpcmChannels)
        );
    }
}

#[test]
fn g722_decodes_two_samples_a_code_in_any_block_size() {
    let codes = xorshift(301, 9);
    let file = au(24, 16_000, 1, &[], &codes, None);
    let mut direct = G722::new();
    let expected: Vec<i16> = codes
        .iter()
        .flat_map(|&code| {
            let (first, second) = direct.decode(code);
            [first, second]
        })
        .collect();
    for block in [1, 3, 64, 602] {
        let (source, pcm) = decode(&file, block);
        assert_eq!(source.info().frames, Some(602));
        assert_eq!(pcm, s16(&expected), "in blocks of {block}");
    }
    assert_eq!(
        open(&au(24, 8000, 1, &[], &codes, None)).err(),
        Some(DecodeError::AuG722Rate)
    );
}

#[test]
fn a_header_out_of_bounds_is_refused() {
    let good = au(3, 8000, 1, &[], &[0; 4], None);
    let mut bad_magic = good.clone();
    bad_magic[0] = b'x';
    assert_eq!(
        PcmSource::open_as(SoundFormat::Au, &mut bad_magic.as_slice(), &LIMITS).err(),
        Some(DecodeError::AuBadMagic)
    );
    assert_eq!(
        PcmSource::open_as(SoundFormat::Au, &mut &good[..20], &LIMITS).err(),
        Some(DecodeError::AuHeaderTruncated)
    );
    let mut early = good.clone();
    early[7] = 23;
    assert_eq!(open(&early).err(), Some(DecodeError::AuDataOffsetBad));
    let mut late = good.clone();
    late[4..8].copy_from_slice(&1000u32.to_be_bytes());
    assert_eq!(open(&late).err(), Some(DecodeError::AuDataOffsetBad));
    for (rate, channels, refused) in [
        (3_000, 1, DecodeError::RateOutOfRange),
        (8_000, 0, DecodeError::NoChannels),
        (8_000, 9, DecodeError::ChannelsExceedLimit),
        (8_000, 5, DecodeError::ChannelLayoutUnsupported),
    ] {
        assert_eq!(
            open(&au(3, rate, channels, &[], &[0; 40], None)).err(),
            Some(refused)
        );
    }
}

#[test]
fn a_sampled_stream_is_entered_where_asked() {
    let samples: Vec<i16> = (0..100).collect();
    let data: Vec<u8> = samples.iter().flat_map(|s| s.to_be_bytes()).collect();
    let file = au(3, 8000, 1, &[], &data, None);
    let mut input = file.as_slice();
    let mut source = PcmSource::open(&mut input, &LIMITS).expect("opens");
    source.seek(40).expect("seeks");
    let mut block = [0u8; 8];
    assert_eq!(source.next_block(&mut input, &mut block), Ok(4));
    assert_eq!(block.to_vec(), s16(&[40, 41, 42, 43]));
    assert_eq!(source.position(), 44);
    assert_eq!(source.seek(101), Err(DecodeError::SeekPastEnd));
    assert_eq!(
        source.next_block(&mut input, &mut [0u8; 1]),
        Err(DecodeError::BufferTooSmall)
    );
}

/// An input holding only the bytes supplied so far.
struct Partial<'a> {
    file: &'a [u8],
    held: usize,
}

impl SoundInput for Partial<'_> {
    fn len(&self) -> u64 {
        u64::try_from(self.file.len()).expect("small")
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, InputError> {
        let start = usize::try_from(offset).expect("small");
        let end = (start + buf.len()).min(self.file.len());
        if end > self.held {
            return Err(InputError::Unavailable);
        }
        let mut file = self.file;
        file.read_at(offset, buf)
    }
}

#[test]
fn bytes_not_yet_at_hand_change_nothing_and_are_asked_for_again() {
    let codes = xorshift(64, 5);
    let file = au(24, 16_000, 1, &[], &codes, None);
    let mut input = Partial {
        file: &file,
        held: 10,
    };
    assert_eq!(
        PcmSource::open(&mut input, &LIMITS).err(),
        Some(DecodeError::InputUnavailable)
    );
    // The header and the seventeen codes the first block needs.
    input.held = 24 + 17;
    let mut source = PcmSource::open(&mut input, &LIMITS).expect("opens");
    let mut block = [0u8; 2 * 33];
    assert_eq!(source.next_block(&mut input, &mut block), Ok(33));
    let first = block;
    assert_eq!(
        source.next_block(&mut input, &mut block),
        Err(DecodeError::InputUnavailable)
    );
    assert_eq!(source.position(), 33, "nothing moved");
    input.held = file.len();
    assert_eq!(source.next_block(&mut input, &mut block), Ok(33));
    let (_, whole) = decode(&file, 128);
    assert_eq!(first.to_vec(), whole[..66]);
    assert_eq!(block.to_vec(), whole[66..132], "the carried sample kept");
}
