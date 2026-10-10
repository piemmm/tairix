//! WAVE files synthesised here — RIFF, RF64 and BW64, every codec the format
//! module claims, chunks in any order — read through the crate's own entry
//! points.

extern crate std;

use std::vec;
use std::vec::Vec;

use tairix_abi::driver::audio::{ChannelMap, ChannelPosition, SampleFormat};

use crate::g711::{ALAW, ULAW};
use crate::g72x::tests::xorshift;
use crate::{
    ima, msadpcm, Cue, DataLength, DecodeError, DecodeLimits, Encoding, Loop, LoopKind, PcmSource,
    SoundFormat, Tag, TagKey, TagKind,
};

const LIMITS: DecodeLimits = DecodeLimits::new(8, 1024, 64);

fn chunk(id: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.extend(u32::try_from(body.len()).expect("small").to_le_bytes());
    out.extend(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
    out
}

fn wave(kind: &[u8], chunks: &[Vec<u8>]) -> Vec<u8> {
    let body: Vec<u8> = chunks.concat();
    let mut out = kind.to_vec();
    out.extend(u32::try_from(body.len() + 4).expect("small").to_le_bytes());
    out.extend(b"WAVE");
    out.extend(body);
    out
}

fn fmt(
    tag: u16,
    channels: u16,
    rate: u32,
    align: u16,
    bits: u16,
    extension: Option<&[u8]>,
) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend(tag.to_le_bytes());
    body.extend(channels.to_le_bytes());
    body.extend(rate.to_le_bytes());
    body.extend((rate * u32::from(align)).to_le_bytes());
    body.extend(align.to_le_bytes());
    body.extend(bits.to_le_bytes());
    if let Some(extension) = extension {
        body.extend(u16::try_from(extension.len()).expect("small").to_le_bytes());
        body.extend(extension);
    }
    chunk(b"fmt ", &body)
}

/// A `WAVE_FORMAT_EXTENSIBLE` extension: `valid` bits, `mask`, subformat
/// `sub`.
fn extensible(valid: u16, mask: u32, sub: u16) -> Vec<u8> {
    let mut extension = valid.to_le_bytes().to_vec();
    extension.extend(mask.to_le_bytes());
    extension.extend(sub.to_le_bytes());
    extension.extend([
        0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
    ]);
    extension
}

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
fn pcm_of_each_width_reads_as_it_lies() {
    for (bits, width, sample) in [
        (8, 1, SampleFormat::U8),
        (12, 2, SampleFormat::S16),
        (16, 2, SampleFormat::S16),
        (24, 3, SampleFormat::S24),
        (32, 4, SampleFormat::S32),
    ] {
        let data = xorshift(width * 2 * 50, u64::from(bits));
        let align = u16::try_from(width * 2).expect("small");
        let file = wave(
            b"RIFF",
            &[fmt(1, 2, 48_000, align, bits, None), chunk(b"data", &data)],
        );
        let (source, pcm) = decode(&file, 7);
        let info = source.info();
        assert_eq!(info.format, SoundFormat::Wav);
        assert_eq!(info.sample, sample, "{bits} bits");
        assert_eq!(
            info.encoding,
            Encoding::Linear {
                bits: u8::try_from(bits).expect("small")
            }
        );
        assert_eq!(info.channels, ChannelMap::STEREO);
        assert_eq!(info.frames, Some(50));
        assert_eq!(pcm, data);
    }
}

#[test]
fn floats_read_finite_doubles_narrow_and_laws_expand() {
    let singles = [0.5f32, f32::NAN, -2.0, f32::NEG_INFINITY];
    let data: Vec<u8> = singles.iter().flat_map(|v| v.to_le_bytes()).collect();
    let (_, pcm) = decode(
        &wave(
            b"RIFF",
            &[fmt(3, 1, 8000, 4, 32, None), chunk(b"data", &data)],
        ),
        3,
    );
    let read: Vec<f32> = pcm
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    assert_eq!(read, [0.5, 0.0, -2.0, -1.0]);
    let doubles = [0.125f64, -0.5];
    let data: Vec<u8> = doubles.iter().flat_map(|v| v.to_le_bytes()).collect();
    let (source, pcm) = decode(
        &wave(
            b"RIFF",
            &[fmt(3, 1, 8000, 8, 64, None), chunk(b"data", &data)],
        ),
        1,
    );
    assert_eq!(source.info().sample, SampleFormat::F32);
    let read: Vec<f32> = pcm
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    assert_eq!(read, [0.125, -0.5]);
    let codes: Vec<u8> = (0..=255).collect();
    for (tag, table) in [(6, &ALAW), (7, &ULAW)] {
        let (_, pcm) = decode(
            &wave(
                b"RIFF",
                &[fmt(tag, 1, 8000, 1, 8, None), chunk(b"data", &codes)],
            ),
            9,
        );
        assert_eq!(pcm, s16(table));
    }
}

#[test]
fn an_extensible_format_reads_its_subformat_and_channel_mask() {
    use ChannelPosition::{FrontCentre, FrontLeft, FrontRight, LowFrequency, RearLeft, RearRight};

    // Front left, right, centre, low frequency, rear left and right.
    let data = xorshift(6 * 4 * 10, 1);
    let file = wave(
        b"RIFF",
        &[
            fmt(0xFFFE, 6, 48_000, 24, 32, Some(&extensible(24, 0x3F, 1))),
            chunk(b"data", &data),
        ],
    );
    let (source, pcm) = decode(&file, 4);
    let positions = [
        FrontLeft,
        FrontRight,
        FrontCentre,
        LowFrequency,
        RearLeft,
        RearRight,
    ];
    assert_eq!(
        source.info().channels,
        ChannelMap::new(&positions).expect("a map")
    );
    assert_eq!(
        source.info().sample,
        SampleFormat::S32,
        "24 valid bits, left-justified"
    );
    assert_eq!(pcm, data);
    let mono = wave(
        b"RIFF",
        &[
            fmt(0xFFFE, 1, 8000, 4, 32, Some(&extensible(32, 0x4, 3))),
            chunk(b"data", &[0; 8]),
        ],
    );
    assert_eq!(
        open(&mono).expect("opens").info().channels,
        ChannelMap::MONO
    );
    for (extension, refused) in [
        (extensible(16, 0x3 | 0x40, 1), DecodeError::WavChannelMask),
        (extensible(16, 0x7, 1), DecodeError::WavChannelMask),
        (extensible(16, 0x3, 2), DecodeError::WavUnknownSubformat),
        (extensible(17, 0x3, 1), DecodeError::WavBadExtensible),
        (
            extensible(16, 0x3, 1)[..20].to_vec(),
            DecodeError::WavBadExtensible,
        ),
    ] {
        let file = wave(
            b"RIFF",
            &[
                fmt(0xFFFE, 2, 8000, 4, 16, Some(&extension)),
                chunk(b"data", &[0; 8]),
            ],
        );
        assert_eq!(open(&file).err(), Some(refused));
    }
    let mut guid = extensible(16, 0x3, 1);
    guid[10] ^= 1;
    let file = wave(
        b"RIFF",
        &[
            fmt(0xFFFE, 2, 8000, 4, 16, Some(&guid)),
            chunk(b"data", &[0; 8]),
        ],
    );
    assert_eq!(open(&file).err(), Some(DecodeError::WavUnknownSubformat));
}

/// Blocks of `align` bytes from a generator, each opening with a header
/// `valid` makes legal, the last `partial` bytes long.
fn adpcm_data(align: usize, blocks: usize, partial: usize, valid: impl Fn(&mut [u8])) -> Vec<u8> {
    let mut data = Vec::new();
    for index in 0..=blocks {
        let len = if index == blocks { partial } else { align };
        let mut block = xorshift(len, 77 + index as u64);
        valid(&mut block);
        data.extend(block);
    }
    data
}

/// Each block of `data` decoded alone by `decode_block`.
fn blocks_alone(
    data: &[u8],
    align: usize,
    decode_block: impl Fn(&[u8], &mut [u8]) -> usize,
) -> Vec<u8> {
    let mut all = Vec::new();
    for block in data.chunks(align) {
        let mut out = vec![0u8; 2 * 1100];
        let written = decode_block(block, &mut out);
        all.extend_from_slice(&out[..written * 2]);
    }
    all
}

#[test]
fn ima_adpcm_reads_block_after_block_and_enters_any_frame() {
    let align = 256;
    let data = adpcm_data(align, 3, 100, |block| block[2] %= 89);
    let extension = 505u16.to_le_bytes();
    let file = wave(
        b"RIFF",
        &[
            fmt(0x11, 1, 22_050, 256, 4, Some(&extension)),
            chunk(b"data", &data),
        ],
    );
    let expected = blocks_alone(&data, align, |block, out| {
        ima::decode_block(block, 1, 0, out).expect("a block")
    });
    let (source, pcm) = decode(&file, 333);
    assert_eq!(source.info().encoding, Encoding::ImaAdpcm);
    assert_eq!(source.info().frames, Some(3 * 505 + 193));
    assert_eq!(pcm, expected);
    let mut input = file.as_slice();
    let mut source = PcmSource::open(&mut input, &LIMITS).expect("opens");
    source.seek(600).expect("seeks");
    let mut block = [0u8; 2 * 20];
    assert_eq!(source.next_block(&mut input, &mut block), Ok(20));
    assert_eq!(block.to_vec(), expected[1200..1240]);
    let fact = chunk(b"fact", &1000u32.to_le_bytes());
    let file = wave(
        b"RIFF",
        &[
            fmt(0x11, 1, 22_050, 256, 4, Some(&extension)),
            fact,
            chunk(b"data", &data),
        ],
    );
    assert_eq!(
        open(&file).expect("opens").info().frames,
        Some(1000),
        "the fact chunk trims the padding"
    );
    let wrong = 504u16.to_le_bytes();
    let file = wave(
        b"RIFF",
        &[
            fmt(0x11, 1, 22_050, 256, 4, Some(&wrong)),
            chunk(b"data", &data),
        ],
    );
    assert_eq!(open(&file).err(), Some(DecodeError::WavBadAdpcmFormat));
}

/// The seven standard coefficient pairs, as an MS ADPCM extension states
/// them, with `per_block` frames a block.
fn ms_extension(per_block: u16) -> Vec<u8> {
    let pairs: [[i16; 2]; 7] = [
        [256, 0],
        [512, -256],
        [0, 0],
        [192, 64],
        [240, 0],
        [460, -208],
        [392, -232],
    ];
    let mut extension = per_block.to_le_bytes().to_vec();
    extension.extend(7u16.to_le_bytes());
    for pair in pairs {
        extension.extend(pair[0].to_le_bytes());
        extension.extend(pair[1].to_le_bytes());
    }
    extension
}

#[test]
fn ms_adpcm_reads_block_after_block_and_enters_any_frame() {
    let align = 256;
    let data = adpcm_data(align, 2, 57, |block| block[0] %= 7);
    let extension = ms_extension(500);
    let file = wave(
        b"RIFF",
        &[
            fmt(0x2, 1, 22_050, 256, 4, Some(&extension)),
            chunk(b"data", &data),
        ],
    );
    let pairs: Vec<[i16; 2]> = ms_extension(500)[4..]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| {
            [
                i16::from_le_bytes([p[0], p[1]]),
                i16::from_le_bytes([p[2], p[3]]),
            ]
        })
        .collect();
    let expected = blocks_alone(&data, align, |block, out| {
        msadpcm::decode_block(block, 1, &pairs, 0, out).expect("a block")
    });
    let (source, pcm) = decode(&file, 77);
    assert_eq!(source.info().encoding, Encoding::MsAdpcm);
    assert_eq!(source.info().frames, Some(2 * 500 + 2 + 50 * 2));
    assert_eq!(pcm, expected);
    let mut input = file.as_slice();
    let mut source = PcmSource::open(&mut input, &LIMITS).expect("opens");
    source.seek(499).expect("seeks");
    let mut block = [0u8; 2 * 4];
    assert_eq!(
        source.next_block(&mut input, &mut block),
        Ok(4),
        "across a block boundary"
    );
    assert_eq!(block.to_vec(), expected[998..1006]);
    let file = wave(
        b"RIFF",
        &[
            fmt(0x2, 1, 22_050, 256, 4, Some(&ms_extension(499))),
            chunk(b"data", &data),
        ],
    );
    assert_eq!(open(&file).err(), Some(DecodeError::WavBadAdpcmFormat));
}

#[test]
fn rf64_and_bw64_take_their_sizes_from_ds64() {
    let data = xorshift(400, 2);
    let mut ds64 = Vec::new();
    ds64.extend(0u64.to_le_bytes());
    ds64.extend(400u64.to_le_bytes());
    ds64.extend(200u64.to_le_bytes());
    ds64.extend(0u32.to_le_bytes());
    let mut sized = chunk(b"data", &data);
    sized[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    for kind in [b"RF64".as_slice(), b"BW64"] {
        let file = wave(
            kind,
            &[
                chunk(b"ds64", &ds64),
                fmt(1, 1, 8000, 2, 16, None),
                sized.clone(),
            ],
        );
        let (source, pcm) = decode(&file, 64);
        assert_eq!(source.info().frames, Some(200));
        assert_eq!(pcm, data);
        let file = wave(kind, &[fmt(1, 1, 8000, 2, 16, None), sized.clone()]);
        assert_eq!(open(&file).err(), Some(DecodeError::WavMissingDs64));
    }
}

#[test]
fn chunks_in_any_order_padding_and_metadata_are_read() {
    let data = xorshift(20, 4);
    let mut info = b"INFO".to_vec();
    info.extend(chunk(b"INAM", b"A tune\0"));
    info.extend(chunk(b"IART", b"Someone"));
    info.extend(chunk(b"IXYZ", b"x"));
    let mut cue = 2u32.to_le_bytes().to_vec();
    for (id, frame) in [(1u32, 3u32), (2, 7)] {
        cue.extend(id.to_le_bytes());
        cue.extend([0; 16]);
        cue.extend(frame.to_le_bytes());
    }
    let mut smpl = vec![0u8; 36];
    smpl[12..16].copy_from_slice(&60u32.to_le_bytes());
    smpl[28..32].copy_from_slice(&1u32.to_le_bytes());
    let mut entry = [0u8; 24];
    entry[4..8].copy_from_slice(&1u32.to_le_bytes());
    entry[8..12].copy_from_slice(&2u32.to_le_bytes());
    entry[12..16].copy_from_slice(&9u32.to_le_bytes());
    smpl.extend(entry);
    let file = wave(
        b"RIFF",
        &[
            chunk(b"LIST", &info),
            chunk(b"junk", b"odd"),
            chunk(b"data", &data),
            chunk(b"cue ", &cue),
            fmt(1, 1, 8000, 2, 16, None),
            chunk(b"smpl", &smpl),
        ],
    );
    let (source, pcm) = decode(&file, 3);
    assert_eq!(pcm, data);
    let metadata = source.metadata();
    let tags: Vec<(TagKind, &str)> = metadata
        .tags
        .iter()
        .map(|t| (t.kind, t.value.as_str()))
        .collect();
    assert_eq!(
        tags,
        [
            (TagKind::Title, "A tune"),
            (TagKind::Artist, "Someone"),
            (TagKind::Other(TagKey::new(b"IXYZ").expect("a key")), "x")
        ]
    );
    assert_eq!(
        metadata.cues,
        [Cue { id: 1, frame: 3 }, Cue { id: 2, frame: 7 }]
    );
    assert_eq!(
        metadata.loops,
        [Loop {
            start: 2,
            end: 9,
            kind: LoopKind::Alternating,
            count: 0
        }]
    );
    assert_eq!(metadata.unity_note, Some(60));
}

#[test]
fn many_short_tags_are_held_to_the_budget_by_what_each_costs_to_keep() {
    let mut info = b"INFO".to_vec();
    for _ in 0..500 {
        info.extend(chunk(b"ICMT", b"x"));
    }
    let file = wave(
        b"RIFF",
        &[
            fmt(1, 1, 8000, 2, 16, None),
            chunk(b"LIST", &info),
            chunk(b"data", &s16(&[0; 4])),
        ],
    );
    let source = open(&file).expect("opens");
    let metadata = source.metadata();
    let kept: usize = metadata
        .tags
        .iter()
        .map(|tag| size_of::<Tag>() + tag.value.len())
        .sum();
    let budget = usize::try_from(LIMITS.max_metadata_bytes()).expect("small");
    assert!(
        kept <= budget,
        "{} tags occupy {kept} bytes",
        metadata.tags.len()
    );
    assert!(metadata.omitted);
    assert!(metadata.within(&LIMITS));
}

#[test]
fn a_data_chunk_longer_than_the_file_yields_to_it() {
    let data = xorshift(10, 6);
    let mut file = wave(
        b"RIFF",
        &[fmt(1, 1, 8000, 2, 16, None), chunk(b"data", &data)],
    );
    let at = file.len() - 10 - 4;
    file[at..at + 4].copy_from_slice(&1000u32.to_le_bytes());
    let (source, pcm) = decode(&file, 2);
    assert_eq!(source.info().frames, Some(5));
    assert_eq!(
        source.info().data_length,
        Some(DataLength {
            declared: 1000,
            held: 10
        })
    );
    assert_eq!(pcm, data);
}

#[test]
fn every_malformed_or_foreign_file_is_refused_by_name() {
    let data = chunk(b"data", &[0; 8]);
    let pcm = fmt(1, 1, 8000, 2, 16, None);
    let many: Vec<Vec<u8>> = (0..4097).map(|_| chunk(b"junk", &[])).collect();
    for (file, refused) in [
        (
            wave(b"RIFF", &[fmt(0x55, 1, 8000, 1, 0, None), data.clone()]),
            DecodeError::WavMpegAudio,
        ),
        (
            wave(b"RIFF", &[fmt(0x50, 1, 8000, 1, 0, None), data.clone()]),
            DecodeError::WavMpegAudio,
        ),
        (
            wave(b"RIFF", &[fmt(0x31, 1, 8000, 65, 0, None), data.clone()]),
            DecodeError::WavGsm610,
        ),
        (
            wave(b"RIFF", &[fmt(0x64, 1, 8000, 1, 4, None), data.clone()]),
            DecodeError::WavUnknownFormatTag(0x64),
        ),
        (
            wave(b"RIFF", core::slice::from_ref(&data)),
            DecodeError::WavMissingFormat,
        ),
        (
            wave(b"RIFF", core::slice::from_ref(&pcm)),
            DecodeError::WavMissingData,
        ),
        (
            wave(b"RIFF", &[pcm.clone(), pcm.clone(), data.clone()]),
            DecodeError::WavDuplicateFormat,
        ),
        (
            wave(b"RIFF", &[pcm.clone(), data.clone(), data.clone()]),
            DecodeError::WavDuplicateData,
        ),
        (
            wave(b"RIFF", &[fmt(1, 1, 8000, 3, 16, None), data.clone()]),
            DecodeError::WavBadBlockAlign,
        ),
        (
            wave(b"RIFF", &[fmt(1, 1, 8000, 5, 33, None), data.clone()]),
            DecodeError::WavBadBitDepth,
        ),
        (
            wave(b"RIFF", &[fmt(3, 1, 8000, 2, 16, None), data.clone()]),
            DecodeError::WavBadBitDepth,
        ),
        (
            wave(b"RIFF", &[chunk(b"fmt ", &[1, 0, 1, 0]), data.clone()]),
            DecodeError::WavFormatTruncated,
        ),
        (wave(b"RIFF", &many), DecodeError::WavTooManyChunks),
        (b"RIFF\x04\0\0\0AVI ".to_vec(), DecodeError::UnknownFormat),
    ] {
        assert_eq!(open(&file).err(), Some(refused));
    }
}
