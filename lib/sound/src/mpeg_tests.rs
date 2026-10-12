//! The decoder end to end over streams written here: where a stream is
//! believed to start, the tags around it, the CRC, free format, a change of
//! mode, inputs answering "not yet", exact seeks across Layer III's
//! reservoir, and LAME's gapless trim.

#![allow(
    clippy::cast_possible_truncation,
    reason = "the stream writers narrow fields they keep within their widths"
)]

extern crate std;

use std::vec;
use std::vec::Vec;

use tairix_fuzzseed::Prng;

use super::frame::Header;
use super::info::DECODER_DELAY;
use crate::bits::BitWriter;
use crate::crc::{crc16, crc16_bits, crc16_reflected};
use crate::input::Patchy;
use crate::{DecodeError, DecodeLimits, Encoding, PcmSource, SoundFormat};

const LIMITS: DecodeLimits = DecodeLimits::new(2, 0, 0);

/// MPEG-1 Layer I at 32 kHz: 32 kbit/s mono, 64 kbit/s stereo, free format.
const LAYER1_MONO: [u8; 4] = [0xFF, 0xFF, 0x18, 0xC0];
const LAYER1_STEREO: [u8; 4] = [0xFF, 0xFF, 0x28, 0x00];
const LAYER1_FREE: [u8; 4] = [0xFF, 0xFF, 0x08, 0xC0];

/// A free-format Layer I frame's length: eighteen slots, 48 kbit/s.
const FREE_LEN: usize = 72;

/// MPEG-1 Layer III at 48 kHz and 128 kbit/s, stereo: 384-byte frames.
const LAYER3_MPEG1: [u8; 4] = [0xFF, 0xFB, 0x94, 0x00];

/// The same, mono.
const LAYER3_MPEG1_MONO: [u8; 4] = [0xFF, 0xFB, 0x94, 0xC0];

/// MPEG-2 Layer III at 16 kHz and 160 kbit/s, mono: 720-byte frames, whose
/// main data alone outruns the deepest reservoir.
const LAYER3_LSF: [u8; 4] = [0xFF, 0xF3, 0xE8, 0xC0];

/// The subbands every Layer I frame here allocates, by allocation code:
/// four-bit samples in subband 0, three-bit in subband 1.
const LAYER1_ALLOCATION: [u32; 2] = [3, 2];

fn header(bytes: [u8; 4]) -> Header {
    Header::parse(&bytes).expect("a header")
}

fn floats(bytes: &[u8]) -> impl Iterator<Item = f32> + '_ {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| f32::from_le_bytes(*bytes))
}

fn channels_of(source: &PcmSource) -> usize {
    usize::from(source.info().channels.channels())
}

/// Every sample `file` decodes to, and how the decode ended.
fn decode(file: &[u8]) -> (Vec<f32>, Result<(), DecodeError>) {
    let mut input = file;
    let mut source = match PcmSource::open(&mut input, &LIMITS) {
        Ok(source) => source,
        Err(err) => return (Vec::new(), Err(err)),
    };
    let channels = channels_of(&source);
    let mut out = vec![0u8; 1000 * channels * 4];
    let mut samples = Vec::new();
    loop {
        match source.next_block(&mut input, &mut out) {
            Ok(0) => return (samples, Ok(())),
            Ok(written) => samples.extend(floats(&out[..written * channels * 4])),
            Err(err) => return (samples, Err(err)),
        }
    }
}

fn decode_whole(file: &[u8]) -> Vec<f32> {
    let (samples, end) = decode(file);
    assert_eq!(end, Ok(()));
    samples
}

/// A Layer I frame of `header`, `len` bytes, its scalefactors and samples
/// drawn from `noise` — one channel's, carried in both where `mirror` — with
/// a correct CRC where the header asks for one.
fn layer1_frame(header_bytes: [u8; 4], len: usize, noise: &mut Prng, mirror: bool) -> Vec<u8> {
    let parsed = header(header_bytes);
    let channels = usize::from(parsed.channels());
    let mut bits = BitWriter::default();
    for subband in 0..32 {
        let allocation = LAYER1_ALLOCATION.get(subband).copied().unwrap_or(0);
        for _ in 0..channels {
            bits.write(u64::from(allocation), 4);
        }
    }
    let protected_bits = usize::try_from(bits.bits()).expect("small");
    let mut draw = |bits: &mut BitWriter, levels: u32, width: u32| {
        let first = noise.next_u32() % levels;
        bits.write(u64::from(first), width);
        if channels == 2 {
            let second = if mirror {
                first
            } else {
                noise.next_u32() % levels
            };
            bits.write(u64::from(second), width);
        }
    };
    for _ in &LAYER1_ALLOCATION {
        draw(&mut bits, 63, 6);
    }
    for _ in 0..12 {
        for &allocation in &LAYER1_ALLOCATION {
            draw(&mut bits, (1u32 << (allocation + 1)) - 1, allocation + 1);
        }
    }
    let mut frame = header_bytes.to_vec();
    if parsed.protected {
        frame.extend([0, 0]);
    }
    frame.extend(bits.into_bytes());
    assert!(frame.len() <= len, "a frame's samples fit it");
    frame.resize(len, 0);
    if parsed.protected {
        let crc = crc16_bits(
            crc16_bits(0xFFFF, &frame[2..4], 16),
            &frame[6..],
            protected_bits,
        );
        frame[4..6].copy_from_slice(&crc.to_be_bytes());
    }
    frame
}

/// `count` Layer I frames of `header` at its stated bitrate.
fn layer1_stream(header_bytes: [u8; 4], count: usize, seed: u64) -> Vec<u8> {
    let parsed = header(header_bytes);
    let len = parsed.len_at(parsed.bitrate().expect("a stated bitrate"));
    let mut noise = Prng::new(seed);
    (0..count)
        .flat_map(|_| layer1_frame(header_bytes, len, &mut noise, false))
        .collect()
}

/// A Layer III stream of `frames` frames of `header`; see [`layer3_frames`].
fn layer3_stream(
    header_bytes: [u8; 4],
    frames: usize,
    quads: impl Fn(usize) -> usize,
    reach: usize,
    seed: u64,
) -> Vec<u8> {
    layer3_frames(&vec![(header_bytes, false); frames], quads, reach, seed)
}

/// One granule's count1 quadruples under table B: each value, never silent
/// so every frame sounds, and the signs of its set bits.
fn quadruples(noise: &mut Prng, count: usize) -> Vec<(u32, Vec<u64>)> {
    (0..count)
        .map(|_| {
            let value = noise.next_u32() % 15 + 1;
            let signs = (0..value.count_ones())
                .map(|_| u64::from(noise.next_u32() & 1))
                .collect();
            (value, signs)
        })
        .collect()
}

/// Write `quads`, answering the bits they took.
fn write_quadruples(bits: &mut BitWriter, quads: &[(u32, Vec<u64>)]) -> u32 {
    let start = bits.bits();
    for (value, signs) in quads {
        bits.write(u64::from(15 - value), 4);
        for &sign in signs {
            bits.write(sign, 1);
        }
    }
    u32::try_from(bits.bits() - start).expect("small")
}

/// Layer III frames, each of its own header and, where `mirror`, a stereo
/// frame carrying one channel's content in both: every granule's lines in
/// its count1 region under table B, `quads(frame)` quadruples a granule.
/// Each frame's main data is laid into the reservoir as far back as `reach`
/// bytes, so frames lean on the ones before them.
fn layer3_frames(
    frames: &[([u8; 4], bool)],
    quads: impl Fn(usize) -> usize,
    reach: usize,
    seed: u64,
) -> Vec<u8> {
    let mut noise = Prng::new(seed);
    let mut coded = Vec::new();
    for (frame, &(header_bytes, mirror)) in frames.iter().enumerate() {
        let parsed = header(header_bytes);
        let len = parsed.len_at(parsed.bitrate().expect("a stated bitrate"));
        let granules = if parsed.lsf() { 1 } else { 2 };
        let mut bits = BitWriter::default();
        let mut lengths = [[0u32; 2]; 2];
        for granule in lengths.iter_mut().take(granules) {
            let first = quadruples(&mut noise, quads(frame));
            granule[0] = write_quadruples(&mut bits, &first);
            if parsed.channels() == 2 {
                let second = if mirror {
                    first
                } else {
                    quadruples(&mut noise, quads(frame))
                };
                granule[1] = write_quadruples(&mut bits, &second);
            }
        }
        coded.push((
            parsed,
            header_bytes,
            bits.into_bytes(),
            lengths,
            len - parsed.data_start(),
        ));
    }
    let mut main = vec![0u8; coded.iter().map(|(.., area)| area).sum()];
    let mut begins = Vec::new();
    let (mut area_start, mut end) = (0usize, 0usize);
    for (frame, (_, _, data, _, area)) in coded.iter().enumerate() {
        let start = end.max(area_start.saturating_sub(reach));
        assert!(
            start + data.len() <= area_start + area,
            "frame {frame}'s main data fits"
        );
        main[start..start + data.len()].copy_from_slice(data);
        begins.push(area_start - start);
        end = start + data.len();
        area_start += area;
    }
    let mut stream = Vec::new();
    let mut area_start = 0;
    for ((parsed, header_bytes, _, lengths, area), begin) in coded.iter().zip(begins) {
        let side = side_info(*parsed, begin, lengths);
        stream.extend_from_slice(header_bytes);
        if parsed.protected {
            stream.extend(crc16(crc16(0xFFFF, &header_bytes[2..]), &side).to_be_bytes());
        }
        stream.extend(side);
        stream.extend_from_slice(&main[area_start..area_start + area]);
        area_start += area;
    }
    stream
}

/// Layer III side information: every granule long, unscaled, its lines in
/// the count1 region under table B, `lengths` bits long.
fn side_info(header: Header, main_data_begin: usize, lengths: &[[u32; 2]; 2]) -> Vec<u8> {
    const GLOBAL_GAIN: u64 = 170;
    let lsf = header.lsf();
    let channels = usize::from(header.channels());
    let mut bits = BitWriter::default();
    bits.write(main_data_begin as u64, if lsf { 8 } else { 9 });
    let private = match (lsf, channels) {
        (false, 1) => 5,
        (false, _) => 3,
        (true, 1) => 1,
        (true, _) => 2,
    };
    bits.write(0, private);
    if !lsf {
        bits.write(0, 4 * channels as u32);
    }
    for granule in lengths.iter().take(if lsf { 1 } else { 2 }) {
        for &length in granule.iter().take(channels) {
            bits.write(u64::from(length), 12);
            bits.write(0, 9);
            bits.write(GLOBAL_GAIN, 8);
            bits.write(0, if lsf { 9 } else { 4 });
            // No window switching; tables and region counts zero.
            bits.write(0, 1 + 15 + 4 + 3);
            if !lsf {
                bits.write(0, 1);
            }
            bits.write(0, 1);
            bits.write(1, 1);
        }
    }
    let bytes = bits.into_bytes();
    assert_eq!(bytes.len(), header.side_info_len());
    bytes
}

/// A tag frame of `header` stating `frames`, under `tag` (`Xing` or
/// `Info`), with LAME's extension stating `gapless` where given and a CRC
/// over it that holds only where `crc_holds`.
fn tag_frame(
    header_bytes: [u8; 4],
    frames: u32,
    gapless: Option<(u16, u16)>,
    crc_holds: bool,
) -> Vec<u8> {
    let parsed = header(header_bytes);
    let len = parsed.len_at(parsed.bitrate().expect("a stated bitrate"));
    let mut frame = header_bytes.to_vec();
    frame.resize(parsed.data_start(), 0);
    frame.extend_from_slice(b"Info");
    frame.extend(1u32.to_be_bytes());
    frame.extend(frames.to_be_bytes());
    if let Some((delay, padding)) = gapless {
        let at = frame.len();
        let mut lame = [0u8; 36];
        lame[..9].copy_from_slice(b"LAME3.100");
        lame[21] = (delay >> 4) as u8;
        lame[22] = ((delay & 0xF) << 4) as u8 | (padding >> 8) as u8;
        lame[23] = padding as u8;
        frame.extend_from_slice(&lame);
        let crc = crc16_reflected(0, &frame[..at + 34]) ^ u16::from(!crc_holds);
        frame[at + 34..at + 36].copy_from_slice(&crc.to_be_bytes());
    }
    frame.resize(len, 0);
    frame
}

#[test]
fn a_frame_header_is_recognised_and_its_layer_stated() {
    assert_eq!(crate::sniff(&LAYER3_MPEG1), Some(SoundFormat::Mpeg));
    let silent_layer2 = {
        // MPEG-1 Layer II, 128 kbit/s at 48 kHz, nothing allocated.
        let mut frame = vec![0u8; 384];
        frame[..4].copy_from_slice(&[0xFF, 0xFD, 0x84, 0x00]);
        frame.repeat(3)
    };
    for (file, encoding, frames) in [
        (
            layer1_stream(LAYER1_MONO, 3, 1),
            Encoding::MpegLayer1,
            3 * 384,
        ),
        (silent_layer2, Encoding::MpegLayer2, 3 * 1152),
        (
            layer3_stream(LAYER3_MPEG1, 3, |_| 8, 0, 1),
            Encoding::MpegLayer3,
            3 * 1152,
        ),
    ] {
        let mut input = &file[..];
        let source = PcmSource::open(&mut input, &LIMITS).expect("opens");
        assert_eq!(source.info().encoding, encoding);
        assert_eq!(source.info().frames, None, "no tag states a length");
        let channels = channels_of(&source);
        assert_eq!(decode_whole(&file).len(), frames * channels);
    }
}

#[test]
fn a_stream_is_believed_only_where_three_frames_agree() {
    let stream = layer1_stream(LAYER1_MONO, 10, 2);
    let expected = decode_whole(&stream);
    // A header that opens the file, but no frame follows it.
    let mut file = LAYER1_MONO.to_vec();
    file.resize(60, 0x11);
    file.extend_from_slice(&stream);
    assert_eq!(decode_whole(&file), expected);
    // Fewer than three frames believed where they end the file exactly.
    let two = &stream[..2 * 48];
    assert_eq!(decode_whole(two), expected[..2 * 384]);
    // A header and nothing that agrees with it.
    let mut lone = LAYER3_MPEG1.to_vec();
    lone.resize(1000, 0);
    assert_eq!(decode(&lone).1, Err(DecodeError::MpegNoSync));
}

#[test]
fn bytes_between_frames_are_stepped_over() {
    let stream = layer1_stream(LAYER1_MONO, 10, 3);
    let mut file = stream[..5 * 48].to_vec();
    file.extend([0x5A; 13]);
    file.extend_from_slice(&stream[5 * 48..]);
    assert_eq!(decode_whole(&file), decode_whole(&stream));
}

#[test]
fn tags_around_the_stream_are_stepped_over() {
    let stream = layer3_stream(LAYER3_MPEG1, 6, |frame| 4 + frame, 300, 4);
    let mut file = b"ID3\x04\x00\x00\x00\x00\x00\x64".to_vec();
    file.extend([0xFF; 100]);
    file.extend_from_slice(&stream);
    let mut ape = [0u8; 32];
    ape[..8].copy_from_slice(b"APETAGEX");
    ape[12..16].copy_from_slice(&(32u32 + 16).to_le_bytes());
    ape[20..24].copy_from_slice(&(1u32 << 31).to_le_bytes());
    file.extend(ape);
    file.extend([0xFF; 16]);
    file.extend(ape);
    let mut id3v1 = [0xFFu8; 128];
    id3v1[..3].copy_from_slice(b"TAG");
    file.extend(id3v1);
    assert_eq!(decode_whole(&file), decode_whole(&stream));
}

#[test]
fn a_frame_failing_its_crc_is_refused() {
    let protected_layer1 = [0xFF, 0xFE, 0x18, 0xC0];
    let file = layer1_stream(protected_layer1, 6, 5);
    let whole = decode_whole(&file);
    let mut corrupt = file.clone();
    // An allocation bit of the third frame, which its CRC covers.
    corrupt[2 * 48 + 6] ^= 0x10;
    let (decoded, end) = decode(&corrupt);
    assert_eq!(end, Err(DecodeError::MpegFrameCrc));
    assert_eq!(decoded, whole[..2 * 384]);

    let protected_layer3 = [0xFF, 0xFA, 0x94, 0x00];
    let file = layer3_stream(protected_layer3, 6, |_| 20, 0, 6);
    let whole = decode_whole(&file);
    let mut corrupt = file.clone();
    // The fourth frame's side information.
    corrupt[3 * 384 + 10] ^= 0x01;
    let (decoded, end) = decode(&corrupt);
    assert_eq!(end, Err(DecodeError::MpegFrameCrc));
    assert_eq!(decoded, whole[..3 * 1152 * 2]);
}

#[test]
fn a_free_format_stream_is_measured_from_its_next_frame() {
    let stated = layer1_stream(LAYER1_MONO, 8, 7);
    let mut noise = Prng::new(7);
    let mut free = Vec::new();
    for frame in 0..8 {
        let padded = frame % 2 == 0;
        let mut bytes = LAYER1_FREE;
        bytes[2] |= u8::from(padded) << 1;
        free.extend(layer1_frame(
            bytes,
            FREE_LEN + 4 * usize::from(padded),
            &mut noise,
            false,
        ));
    }
    assert_eq!(decode_whole(&free), decode_whole(&stated));
}

/// A mono frame in a stereo stream drives both channels' filterbanks with
/// its one channel: the stream decodes exactly as one whose frame carries
/// that channel in both, before it, in it and after it.
#[test]
fn a_mono_frame_in_a_stereo_stream_drives_both_channels() {
    let stereo_len = header(LAYER1_STEREO).len_at(64_000);
    let layer1 = |middle: [u8; 4], len: usize, mirror: bool| {
        let mut noise = Prng::new(8);
        let mut file = Vec::new();
        for _ in 0..3 {
            file.extend(layer1_frame(LAYER1_STEREO, stereo_len, &mut noise, false));
        }
        file.extend(layer1_frame(middle, len, &mut noise, mirror));
        file.extend(layer1_frame(LAYER1_STEREO, stereo_len, &mut noise, false));
        file
    };
    let mono = decode_whole(&layer1(LAYER1_MONO, 48, false));
    assert_eq!(mono.len(), 5 * 384 * 2);
    assert_eq!(mono, decode_whole(&layer1(LAYER1_STEREO, stereo_len, true)));

    // Layer III's IMDCT follows it too.
    let layer3 = |middle: ([u8; 4], bool)| {
        let mut frames = vec![(LAYER3_MPEG1, false); 6];
        frames[3] = middle;
        layer3_frames(&frames, |_| 24, 300, 21)
    };
    let mono = decode_whole(&layer3((LAYER3_MPEG1_MONO, false)));
    assert_eq!(mono.len(), 6 * 1152 * 2);
    assert_eq!(mono, decode_whole(&layer3((LAYER3_MPEG1, true))));
}

/// A stereo frame in a mono stream is refused, not folded into one.
#[test]
fn a_stereo_frame_in_a_mono_stream_is_refused() {
    let stereo_len = header(LAYER1_STEREO).len_at(64_000);
    let mut noise = Prng::new(9);
    let mut file = layer1_stream(LAYER1_MONO, 3, 9);
    file.extend(layer1_frame(LAYER1_STEREO, stereo_len, &mut noise, false));
    file.extend(layer1_stream(LAYER1_MONO, 2, 10));
    let (decoded, end) = decode(&file);
    assert_eq!(end, Err(DecodeError::MpegFrameMismatch));
    assert_eq!(decoded.len(), 3 * 384);
}

#[test]
fn the_encoders_silence_is_trimmed_where_its_tag_vouches_for_it() {
    let audio = layer3_stream(LAYER3_MPEG1, 10, |_| 30, 400, 11);
    let raw = decode_whole(&audio);
    let samples = 10 * 1152u64;
    for (delay, padding) in [(576u16, 1000u16), (576, 100)] {
        let mut file = tag_frame(LAYER3_MPEG1, 10, Some((delay, padding)), true);
        file.extend_from_slice(&audio);
        let skip = u64::from(delay) + DECODER_DELAY;
        let total = (samples - u64::from(delay) - u64::from(padding)).min(samples - skip);
        let mut input = &file[..];
        let source = PcmSource::open(&mut input, &LIMITS).expect("opens");
        assert_eq!(source.info().frames, Some(total));
        let (from, to) = (
            usize::try_from(skip).expect("small"),
            usize::try_from(skip + total).expect("small"),
        );
        assert_eq!(
            decode_whole(&file),
            raw[from * 2..to * 2],
            "padding {padding}"
        );
    }
    // A LAME extension its own CRC does not vouch for trims nothing.
    let mut file = tag_frame(LAYER3_MPEG1, 10, Some((576, 1000)), false);
    file.extend_from_slice(&audio);
    let mut input = &file[..];
    let source = PcmSource::open(&mut input, &LIMITS).expect("opens");
    assert_eq!(source.info().frames, Some(samples));
    assert_eq!(decode_whole(&file), raw);
}

#[test]
fn a_stream_ending_before_the_frames_its_tag_states_is_refused() {
    let audio = layer3_stream(LAYER3_MPEG1, 7, |_| 12, 400, 12);
    let mut file = tag_frame(LAYER3_MPEG1, 10, None, true);
    file.extend_from_slice(&audio);
    let (decoded, end) = decode(&file);
    assert_eq!(end, Err(DecodeError::MpegTruncated));
    assert_eq!(decoded, decode_whole(&audio));
}

/// Seek `file` to each of `targets` in turn and check the block there is the
/// one decoding from the start reaches.
fn assert_seeks_exact(file: &[u8], targets: &[u64]) {
    let whole = decode_whole(file);
    let mut input = file;
    let mut source = PcmSource::open(&mut input, &LIMITS).expect("opens");
    let channels = channels_of(&source);
    let mut out = vec![0u8; 700 * channels * 4];
    for &target in targets {
        source.seek(target).expect("seeks");
        let written = source.next_block(&mut input, &mut out).expect("reads");
        assert!(written > 0, "at {target}");
        let at = usize::try_from(target).expect("small") * channels;
        let got: Vec<f32> = floats(&out[..written * channels * 4]).collect();
        assert_eq!(got, whole[at..at + written * channels], "at {target}");
    }
}

#[test]
fn a_seek_lands_where_decoding_from_the_start_would() {
    let layer1 = layer1_stream(LAYER1_STEREO, 150, 13);
    assert_seeks_exact(
        &layer1,
        &[0, 383, 384 * 100 + 7, 384 * 3, 384 * 40, 384 * 149 + 300, 1],
    );
    // Frames that alternately save main data and spend more than they hold.
    let reservoir = layer3_stream(
        LAYER3_MPEG1,
        120,
        |frame| if frame % 3 == 2 { 144 } else { 10 },
        511,
        14,
    );
    assert_seeks_exact(
        &reservoir,
        &[
            1152 * 60 + 5,
            1152 * 2 + 1,
            1152 * 100,
            1152 * 33 + 1000,
            7,
            1152 * 119 + 1100,
        ],
    );
}

/// A frame at the lower rates holds one granule, so the slots the target's
/// filterbank remembers reach back into the frame two before it, whose own
/// main data must be whole too.
#[test]
fn a_seek_at_the_lower_rates_restores_the_frame_two_back() {
    let stream = layer3_stream(LAYER3_LSF, 80, |_| 144, 200, 15);
    assert_seeks_exact(
        &stream,
        &[576 * 50, 576 * 10 + 3, 576 * 3, 576 * 70 + 100, 576 * 4],
    );
}

#[test]
fn bytes_not_yet_at_hand_change_nothing() {
    let file = layer3_stream(
        LAYER3_MPEG1,
        40,
        |frame| if frame % 3 == 2 { 144 } else { 10 },
        511,
        16,
    );
    let whole = decode_whole(&file);
    let mut input = Patchy::new(&file);
    let mut source = loop {
        match PcmSource::open(&mut input, &LIMITS) {
            Ok(source) => break source,
            Err(DecodeError::InputUnavailable) => {}
            Err(err) => panic!("{err:?}"),
        }
    };
    let mut out = vec![0u8; 500 * 2 * 4];
    let mut read = |source: &mut PcmSource, input: &mut Patchy<'_>| loop {
        let position = source.position();
        match source.next_block(input, &mut out) {
            Ok(written) => break floats(&out[..written * 2 * 4]).collect::<Vec<f32>>(),
            Err(DecodeError::InputUnavailable) => assert_eq!(source.position(), position),
            Err(err) => panic!("{err:?}"),
        }
    };
    let mut decoded = Vec::new();
    loop {
        let block = read(&mut source, &mut input);
        if block.is_empty() {
            break;
        }
        decoded.extend(block);
    }
    assert_eq!(decoded, whole);
    let mut input = Patchy::new(&file);
    let mut source = PcmSource::open(&mut &file[..], &LIMITS).expect("opens");
    for target in [1152 * 30 + 9, 1152 * 4] {
        source.seek(target).expect("seeks");
        let block = read(&mut source, &mut input);
        let at = usize::try_from(target).expect("small") * 2;
        assert_eq!(block, whole[at..at + block.len()]);
    }
}
