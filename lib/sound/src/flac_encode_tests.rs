//! The encoder against the decoder: the breadth oracle. A round trip proves
//! the two halves agree, which a shared misreading of the format would
//! satisfy as well; conformance is the foreign streams in `flac_tests`.

extern crate std;

use std::vec;
use std::vec::Vec;

use super::{
    encode, BlockCode, Blocking, Codes, EncodeError, Frame, Options, Params, Partition, Predictor,
    RateCode, Residual, Stereo, Subframe, Writer,
};
use tairix_fuzzseed::Prng;

use crate::{DecodeError, DecodeLimits, InputError, PcmSource, SoundInput};

const LIMITS: DecodeLimits = DecodeLimits::new(8, 4096, 64);

/// `count` frames of `channels`, each sample within `bits`: a correlated
/// signal with noise on it, so every predictor has something to find.
fn signal(seed: u32, count: usize, channels: usize, bits: u8) -> Vec<i32> {
    let mut prng = Prng::new(u64::from(seed));
    let top = (1i64 << (bits - 1)) - 1;
    let mut phase = vec![0i64; channels];
    let mut out = Vec::with_capacity(count * channels);
    for _ in 0..count {
        for (channel, phase) in phase.iter_mut().enumerate() {
            let noise = i64::from(prng.next_u8() % 64) - 32;
            let step = 37 * (i64::try_from(channel).expect("eight at most") + 1);
            *phase = (*phase + step + noise).rem_euclid(4 * (top + 1));
            let triangle = if *phase <= 2 * top {
                *phase - top
            } else {
                3 * top - *phase
            };
            out.push(i32::try_from(triangle.clamp(-top - 1, top)).expect("within the width"));
        }
    }
    out
}

/// The PCM a decode writes for `interleaved` samples of `bits`.
fn pcm(interleaved: &[i32], bits: u8) -> Vec<u8> {
    let (bytes, shift) = match bits {
        0..=8 => (1, 8 - u32::from(bits)),
        9..=16 => (2, 16 - u32::from(bits)),
        17..=24 => (3, 24 - u32::from(bits)),
        _ => (4, 32 - u32::from(bits)),
    };
    let mut out = Vec::new();
    for &sample in interleaved {
        let placed = (i64::from(sample) << shift).to_le_bytes();
        out.extend_from_slice(&placed[..bytes]);
        if bytes == 1 {
            let last = out.len() - 1;
            out[last] ^= 0x80;
        }
    }
    out
}

/// Every frame `file` holds, and how its stream ended.
fn decode(file: &[u8], block: usize) -> (Vec<u8>, Result<(), DecodeError>) {
    let mut input = file;
    let mut source = PcmSource::open(&mut input, &LIMITS).expect("opens");
    let frame = source.info().frame_bytes();
    let mut out = vec![0u8; block * frame];
    let mut all = Vec::new();
    loop {
        match source.next_block(&mut input, &mut out) {
            Ok(0) => return (all, Ok(())),
            Ok(written) => all.extend_from_slice(&out[..written * frame]),
            Err(err) => return (all, Err(err)),
        }
    }
}

#[test]
fn the_chooser_round_trips_every_width_and_channel_count() {
    for bits in [4u8, 5, 8, 12, 16, 17, 20, 24, 31, 32] {
        for channels in [1u8, 2, 3, 6, 8] {
            let samples = signal(
                u32::from(bits) * 31 + u32::from(channels),
                3000,
                usize::from(channels),
                bits,
            );
            let params = Params {
                rate: 44_100,
                channels,
                bits,
            };
            let options = Options {
                block: 1152,
                ..Options::default()
            };
            let file = encode(params, &samples, options).expect("encodes").finish();
            let (decoded, end) = decode(&file, 700);
            assert_eq!(
                decoded,
                pcm(&samples, bits),
                "{bits} bits, {channels} channels"
            );
            assert_eq!(
                end,
                Ok(()),
                "{bits} bits, {channels} channels: the digest agrees"
            );
        }
    }
}

#[test]
fn seven_channels_place_a_back_centre_the_vocabulary_lacks() {
    let params = Params {
        rate: 48_000,
        channels: 7,
        bits: 16,
    };
    let file = encode(params, &signal(7, 100, 7, 16), Options::default())
        .expect("encodes")
        .finish();
    assert_eq!(
        PcmSource::open(&mut &file[..], &LIMITS).map(|_| ()),
        Err(DecodeError::ChannelLayoutUnsupported)
    );
}

#[test]
fn silence_and_full_scale_extremes_round_trip() {
    for bits in [8u8, 16, 24, 32] {
        let top = i32::try_from((1i64 << (bits - 1)) - 1).expect("fits");
        let mut samples = vec![0i32; 2 * 600];
        for (index, sample) in samples.iter_mut().enumerate().skip(1200 / 2) {
            *sample = if index % 3 == 0 { top } else { -top - 1 };
        }
        let params = Params {
            rate: 96_000,
            channels: 2,
            bits,
        };
        let file = encode(params, &samples, Options::default())
            .expect("encodes")
            .finish();
        let (decoded, end) = decode(&file, 256);
        assert_eq!(decoded, pcm(&samples, bits), "{bits} bits");
        assert_eq!(end, Ok(()));
    }
}

fn rice(order: u8, parameter: u8) -> Residual {
    Residual {
        wide: parameter > 14,
        order,
        partitions: vec![Partition::Rice(parameter); 1 << order],
    }
}

fn subframe(predictor: Predictor, wasted: u8, residual: Residual) -> Subframe {
    Subframe {
        predictor,
        wasted,
        residual,
    }
}

/// One mono frame of `samples` coded as `sub`, decoded back.
fn round_trip(
    bits: u8,
    samples: &[i32],
    sub: Subframe,
    codes: Codes,
) -> Result<Vec<u8>, EncodeError> {
    let params = Params {
        rate: 44_100,
        channels: 1,
        bits,
    };
    let block = u32::try_from(samples.len()).expect("small").max(16);
    let mut writer = Writer::new(params, Blocking::Fixed(block))?;
    writer.frame(
        &[samples],
        &Frame {
            stereo: Stereo::Independent,
            subframes: vec![sub],
            codes,
        },
    )?;
    let (decoded, end) = decode(&writer.finish(), 64);
    assert_eq!(end, Ok(()));
    Ok(decoded)
}

#[test]
fn the_core_emits_every_subframe_kind_and_the_decoder_reads_each() {
    let samples = signal(3, 256, 1, 16);
    let expected = pcm(&samples, 16);
    let mut predictors = vec![Predictor::Verbatim];
    predictors.extend((0..=4).map(Predictor::Fixed));
    for (order, precision, shift, first) in [
        (1usize, 1u8, 0u8, -1),
        (3, 12, 9, 512),
        (12, 15, 15, 16_383),
        (32, 15, 13, 8192),
    ] {
        let mut coefficients = vec![0i32; order];
        coefficients[0] = first;
        predictors.push(Predictor::Lpc {
            coefficients,
            precision,
            shift,
        });
    }
    for predictor in predictors {
        let sub = subframe(predictor.clone(), 0, rice(0, 20));
        assert_eq!(
            round_trip(16, &samples, sub, Codes::default()),
            Ok(expected.clone()),
            "{predictor:?}"
        );
    }
    let constant = [-7i32; 40];
    let sub = subframe(Predictor::Constant, 0, rice(0, 0));
    assert_eq!(
        round_trip(16, &constant, sub, Codes::default()),
        Ok(pcm(&constant, 16))
    );
}

#[test]
fn escaped_partitions_wide_parameters_and_every_partition_order_decode() {
    let samples = signal(9, 4096, 1, 24);
    let expected = pcm(&samples, 24);
    for order in [0u8, 1, 4, 8, 11] {
        for parameter in [5u8, 9, 14, 15, 20, 30] {
            let sub = subframe(Predictor::Fixed(2), 0, rice(order, parameter));
            assert_eq!(
                round_trip(24, &samples, sub, Codes::default()),
                Ok(expected.clone()),
                "order {order}, parameter {parameter}"
            );
        }
    }
    let shorter_than_the_predictor = subframe(Predictor::Fixed(2), 0, rice(12, 9));
    assert_eq!(
        round_trip(24, &samples, shorter_than_the_predictor, Codes::default()),
        Err(EncodeError::Unrepresentable)
    );
    let mut partitions = vec![Partition::Escape(31); 4];
    partitions[1] = Partition::Rice(9);
    let mixed = Residual {
        wide: false,
        order: 2,
        partitions,
    };
    let sub = subframe(Predictor::Fixed(1), 0, mixed);
    assert_eq!(
        round_trip(24, &samples, sub, Codes::default()),
        Ok(expected)
    );
    let zeros = [5i32; 64];
    let escaped = Residual {
        wide: true,
        order: 0,
        partitions: vec![Partition::Escape(0)],
    };
    let sub = subframe(Predictor::Fixed(1), 0, escaped);
    assert_eq!(
        round_trip(8, &zeros, sub, Codes::default()),
        Ok(pcm(&zeros, 8))
    );
}

/// A frame whose residual is coded with a parameter far too small takes more
/// unary padding than its bound admits.
#[test]
fn a_frame_past_twice_its_verbatim_size_is_refused() {
    let samples = signal(5, 512, 1, 16);
    let params = Params {
        rate: 44_100,
        channels: 1,
        bits: 16,
    };
    let mut writer = Writer::new(params, Blocking::Fixed(512)).expect("supported");
    let frame = Frame {
        stereo: Stereo::Independent,
        subframes: vec![subframe(Predictor::Fixed(0), 0, rice(0, 0))],
        codes: Codes::default(),
    };
    assert_eq!(
        writer.frame(&[&samples], &frame),
        Err(EncodeError::Oversized)
    );
    writer.frame_unbounded(&[&samples], &frame).expect("writes");
    let (decoded, end) = decode(&writer.finish(), 64);
    assert!(decoded.is_empty());
    assert_eq!(end, Err(DecodeError::FlacFrameTooLarge));
}

#[test]
fn wasted_bits_to_one_short_of_the_width_decode() {
    for wasted in [1u8, 8, 15] {
        let samples: Vec<i32> = signal(wasted.into(), 128, 1, 16)
            .into_iter()
            .map(|sample| (sample >> wasted) << wasted)
            .collect();
        let sub = subframe(Predictor::Fixed(1), wasted, rice(0, 3));
        assert_eq!(
            round_trip(16, &samples, sub, Codes::default()),
            Ok(pcm(&samples, 16)),
            "{wasted} wasted"
        );
    }
    let odd = [1i32, 3, 5, 7];
    let sub = subframe(Predictor::Verbatim, 1, rice(0, 0));
    assert_eq!(
        round_trip(16, &odd, sub, Codes::default()),
        Err(EncodeError::Unrepresentable)
    );
    let sub = subframe(Predictor::Verbatim, 16, rice(0, 0));
    assert_eq!(
        round_trip(16, &[0; 16], sub, Codes::default()),
        Err(EncodeError::Unrepresentable)
    );
}

#[test]
fn every_header_code_decodes() {
    let samples = signal(11, 300, 1, 16);
    let expected = pcm(&samples, 16);
    for rate in [
        RateCode::Kilohertz,
        RateCode::Hertz,
        RateCode::TensOfHertz,
        RateCode::StreamInfo,
    ] {
        for block in [BlockCode::Byte, BlockCode::Word] {
            let params = Params {
                rate: 32_000,
                channels: 1,
                bits: 16,
            };
            let mut writer = Writer::new(params, Blocking::Fixed(300)).expect("supported");
            let codes = Codes {
                rate,
                width_from_streaminfo: true,
                block,
            };
            let frame = Frame {
                stereo: Stereo::Independent,
                subframes: vec![subframe(Predictor::Verbatim, 0, rice(0, 0))],
                codes,
            };
            let result = writer.frame(&[&samples], &frame);
            if block == BlockCode::Byte {
                assert_eq!(
                    result,
                    Err(EncodeError::Unrepresentable),
                    "300 samples need a word"
                );
                continue;
            }
            result.expect("writes");
            let (decoded, end) = decode(&writer.finish(), 64);
            assert_eq!((decoded, end), (expected.clone(), Ok(())), "{codes:?}");
        }
    }
}

#[test]
fn every_stereo_decorrelation_round_trips() {
    let samples = signal(13, 512, 2, 24);
    let left: Vec<i32> = samples.iter().step_by(2).copied().collect();
    let right: Vec<i32> = samples.iter().skip(1).step_by(2).copied().collect();
    for stereo in [
        Stereo::Independent,
        Stereo::LeftSide,
        Stereo::SideRight,
        Stereo::MidSide,
    ] {
        let params = Params {
            rate: 48_000,
            channels: 2,
            bits: 24,
        };
        let mut writer = Writer::new(params, Blocking::Fixed(512)).expect("supported");
        let sub = || subframe(Predictor::Fixed(2), 0, rice(3, 18));
        let frame = Frame {
            stereo,
            subframes: vec![sub(), sub()],
            codes: Codes::default(),
        };
        writer.frame(&[&left, &right], &frame).expect("writes");
        let (decoded, end) = decode(&writer.finish(), 100);
        assert_eq!(decoded, pcm(&samples, 24), "{stereo:?}");
        assert_eq!(end, Ok(()));
    }
}

/// A stream of `frames` frames of varying sizes, numbered by sample, with a
/// large first sample number so the coded number takes its widest form.
#[test]
fn a_variable_stream_round_trips() {
    let params = Params {
        rate: 22_050,
        channels: 2,
        bits: 16,
    };
    let mut writer =
        Writer::new(params, Blocking::Variable { min: 16, max: 4608 }).expect("supported");
    let samples = signal(17, 9000, 2, 16);
    let mut at = 0;
    let mut sizes = [16usize, 4608, 777, 1, 2048].iter().cycle();
    let mut written = Vec::new();
    while at < 4000 {
        let size = *sizes.next().expect("cycles");
        let chunk = &samples[2 * at..2 * (at + size)];
        let left: Vec<i32> = chunk.iter().step_by(2).copied().collect();
        let right: Vec<i32> = chunk.iter().skip(1).step_by(2).copied().collect();
        let frame = super::choose(params, &[&left, &right], Options::default());
        match writer.frame(&[&left, &right], &frame) {
            Ok(()) => {
                written.extend_from_slice(chunk);
                at += size;
            }
            Err(EncodeError::BadSamples) if size == 1 => break,
            Err(err) => panic!("{err:?}"),
        }
    }
    let (decoded, end) = decode(&writer.finish(), 500);
    assert_eq!(decoded, pcm(&written, 16));
    assert_eq!(end, Ok(()));
}

/// A long stream decoded from many entry points lands on each exactly, with
/// a seek table and without one.
#[test]
fn seeking_anywhere_lands_on_the_exact_sample() {
    let params = Params {
        rate: 44_100,
        channels: 2,
        bits: 16,
    };
    let samples = signal(19, 300_000, 2, 16);
    for table in [false, true] {
        let mut writer = encode(
            params,
            &samples,
            Options {
                block: 1152,
                ..Options::default()
            },
        )
        .expect("encodes");
        if table {
            writer.seek_points(44_100, 3);
        }
        let file = writer.finish();
        let expected = pcm(&samples, 16);
        let mut input = &file[..];
        let mut source = PcmSource::open(&mut input, &LIMITS).expect("opens");
        let mut prng = Prng::new(77);
        let mut out = [0u8; 64];
        for _ in 0..60 {
            let frame = prng.next_u64() % 300_000;
            source.seek(frame).expect("seekable");
            let written = source.next_block(&mut input, &mut out).expect("decodes");
            assert!(written > 0);
            let at = usize::try_from(frame).expect("small") * 4;
            assert_eq!(
                &out[..written * 4],
                &expected[at..at + written * 4],
                "at {frame}, table {table}"
            );
        }
    }
}

/// An input holding a file's bytes a page at a time, which answers "not yet"
/// for a page it has not been given and is given that page once asked.
struct Patchy<'f> {
    file: &'f [u8],
    held: Vec<bool>,
}

const PAGE: usize = 512;

impl SoundInput for Patchy<'_> {
    fn len(&self) -> u64 {
        self.file.len() as u64
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, InputError> {
        let offset = usize::try_from(offset).map_err(|_| InputError::Failed)?;
        let end = (offset + buf.len()).min(self.file.len());
        if offset >= end {
            return Ok(0);
        }
        let mut missing = false;
        for page in offset / PAGE..=(end - 1) / PAGE {
            if !self.held[page] {
                self.held[page] = true;
                missing = true;
            }
        }
        if missing {
            return Err(InputError::Unavailable);
        }
        buf[..end - offset].copy_from_slice(&self.file[offset..end]);
        Ok(end - offset)
    }
}

#[test]
fn a_decode_through_an_input_answering_not_yet_writes_what_a_direct_one_does() {
    let params = Params {
        rate: 44_100,
        channels: 2,
        bits: 24,
    };
    let samples = signal(23, 20_000, 2, 24);
    let file = encode(params, &samples, Options::default())
        .expect("encodes")
        .finish();
    let mut input = Patchy {
        file: &file,
        held: vec![false; file.len().div_ceil(PAGE)],
    };
    let mut source = loop {
        match PcmSource::open(&mut input, &LIMITS) {
            Ok(source) => break source,
            Err(DecodeError::InputUnavailable) => {}
            Err(err) => panic!("{err:?}"),
        }
    };
    let mut out = vec![0u8; 3000 * 6];
    let mut all = Vec::new();
    loop {
        let position = source.position();
        match source.next_block(&mut input, &mut out) {
            Ok(0) => break,
            Ok(written) => all.extend_from_slice(&out[..written * 6]),
            Err(DecodeError::InputUnavailable) => assert_eq!(source.position(), position),
            Err(err) => panic!("{err:?}"),
        }
    }
    assert_eq!(all, pcm(&samples, 24));
}

#[test]
fn a_stream_stating_no_digest_is_not_checked_against_one() {
    let params = Params {
        rate: 8_000,
        channels: 1,
        bits: 8,
    };
    let samples = signal(29, 1000, 1, 8);
    let mut writer = encode(params, &samples, Options::default()).expect("encodes");
    writer.without_digest();
    let (decoded, end) = decode(&writer.finish(), 100);
    assert_eq!((decoded, end), (pcm(&samples, 8), Ok(())));
}

#[test]
fn the_writer_refuses_what_the_format_cannot_carry() {
    let params = Params {
        rate: 44_100,
        channels: 1,
        bits: 16,
    };
    for blocking in [
        Blocking::Fixed(15),
        Blocking::Variable { min: 32, max: 16 },
        Blocking::Fixed(65_536),
    ] {
        assert!(Writer::new(params, blocking).is_err(), "{blocking:?}");
    }
    let bad = [
        Params { bits: 3, ..params },
        Params {
            channels: 9,
            ..params
        },
        Params { rate: 0, ..params },
    ];
    for params in bad {
        assert_eq!(
            Writer::new(params, Blocking::Fixed(16)).err(),
            Some(EncodeError::Unsupported)
        );
    }
    let mut writer = Writer::new(params, Blocking::Fixed(16)).expect("supported");
    let frame = Frame {
        stereo: Stereo::Independent,
        subframes: vec![subframe(Predictor::Verbatim, 0, rice(0, 0))],
        codes: Codes::default(),
    };
    assert_eq!(
        writer.frame(&[&[40_000; 16]], &frame),
        Err(EncodeError::BadSamples)
    );
    assert_eq!(
        writer.frame(&[&[0; 17]], &frame),
        Err(EncodeError::BadSamples)
    );
    writer
        .frame(&[&[0; 5]], &frame)
        .expect("a short last frame");
    assert_eq!(
        writer.frame(&[&[0; 16]], &frame),
        Err(EncodeError::BadSamples)
    );
}

/// A picture block of `kind` holding `data` as an image of `mime`.
fn picture_block(kind: u32, mime: &[u8], data: &[u8]) -> Vec<u8> {
    let mut block = kind.to_be_bytes().to_vec();
    block.extend(u32::try_from(mime.len()).expect("small").to_be_bytes());
    block.extend(mime);
    block.extend(0u32.to_be_bytes());
    block.extend([0u8; 16]);
    block.extend(u32::try_from(data.len()).expect("small").to_be_bytes());
    block.extend(data);
    block
}

/// A short stream carrying `blocks`, natively or in Ogg.
fn with_blocks(blocks: &[Vec<u8>], ogg: bool) -> Vec<u8> {
    let params = Params {
        rate: 44_100,
        channels: 1,
        bits: 16,
    };
    let mut writer = encode(params, &signal(3, 1024, 1, 16), Options::default()).expect("encodes");
    for block in blocks {
        writer.block(6, block);
    }
    if ogg {
        writer.finish_ogg(5, 512)
    } else {
        writer.finish()
    }
}

/// The bytes `file` reports as its cover.
fn cover(file: &[u8]) -> Option<&[u8]> {
    let mut input = file;
    let source = PcmSource::open(&mut input, &LIMITS).expect("opens");
    let range = source.metadata().cover?;
    let start = usize::try_from(range.offset).expect("in memory");
    file.get(start..start + usize::try_from(range.len).expect("in memory"))
}

#[test]
fn the_front_cover_is_reported_where_the_file_holds_it() {
    let back = picture_block(4, b"image/png", b"the back of the box");
    let front = picture_block(3, b"image/jpeg", b"the front of the box");
    for blocks in [[back.clone(), front.clone()], [front, back]] {
        assert_eq!(
            cover(&with_blocks(&blocks, false)),
            Some(&b"the front of the box"[..])
        );
    }
}

#[test]
fn without_a_front_cover_the_first_picture_stands() {
    let blocks = [
        picture_block(0, b"image/png", b"first"),
        picture_block(8, b"image/png", b"second"),
    ];
    assert_eq!(cover(&with_blocks(&blocks, false)), Some(&b"first"[..]));
}

#[test]
fn a_link_or_an_empty_picture_is_no_cover() {
    let link = picture_block(3, b"-->", b"https://example.org/cover.jpg");
    let empty = picture_block(3, b"image/png", b"");
    for block in [link, empty] {
        assert_eq!(cover(&with_blocks(&[block], false)), None);
    }
}

/// A picture laid across an Ogg stream's pages is held nowhere whole, so it
/// is stepped over rather than offered; the stream still plays.
#[test]
fn a_picture_across_ogg_pages_is_not_a_cover() {
    let front = picture_block(3, b"image/png", &[0x5A; 2048]);
    let file = with_blocks(&[front], true);
    assert_eq!(cover(&file), None);
    assert_eq!(decode(&file, 256).1, Ok(()));
}
