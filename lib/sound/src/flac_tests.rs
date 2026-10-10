//! The FLAC reader against streams another encoder wrote: RFC 9639's own
//! examples (Appendix D), each carrying the digest of its samples, with the
//! samples the RFC lists beside them.

extern crate std;

use std::vec;
use std::vec::Vec;

use tairix_abi::driver::audio::{ChannelMap, SampleFormat};

use crate::{DecodeError, DecodeLimits, Encoding, PcmSource, SoundFormat, TagKind};

const LIMITS: DecodeLimits = DecodeLimits::new(8, 4096, 64);

/// Example 1: one stereo sample in two verbatim subframes with wasted bits.
pub(crate) const EXAMPLE_1: [u8; 57] = [
    0x66, 0x4c, 0x61, 0x43, 0x80, 0x00, 0x00, 0x22, 0x10, 0x00, 0x10, 0x00, 0x00, 0x00, 0x0f, 0x00,
    0x00, 0x0f, 0x0a, 0xc4, 0x42, 0xf0, 0x00, 0x00, 0x00, 0x01, 0x3e, 0x84, 0xb4, 0x18, 0x07, 0xdc,
    0x69, 0x03, 0x07, 0x58, 0x6a, 0x3d, 0xad, 0x1a, 0x2e, 0x0f, 0xff, 0xf8, 0x69, 0x18, 0x00, 0x00,
    0xbf, 0x03, 0x58, 0xfd, 0x03, 0x12, 0x8b, 0xaa, 0x9a,
];

/// Example 2: a seek table, a Vorbis comment and padding, then a side-right
/// frame of fixed predictors and a verbatim one.
pub(crate) const EXAMPLE_2: [u8; 227] = [
    0x66, 0x4c, 0x61, 0x43, 0x00, 0x00, 0x00, 0x22, 0x00, 0x10, 0x00, 0x10, 0x00, 0x00, 0x17, 0x00,
    0x00, 0x44, 0x0a, 0xc4, 0x42, 0xf0, 0x00, 0x00, 0x00, 0x13, 0xd5, 0xb0, 0x56, 0x49, 0x75, 0xe9,
    0x8b, 0x8d, 0x8b, 0x93, 0x04, 0x22, 0x75, 0x7b, 0x81, 0x03, 0x03, 0x00, 0x00, 0x12, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
    0x04, 0x00, 0x00, 0x3a, 0x20, 0x00, 0x00, 0x00, 0x72, 0x65, 0x66, 0x65, 0x72, 0x65, 0x6e, 0x63,
    0x65, 0x20, 0x6c, 0x69, 0x62, 0x46, 0x4c, 0x41, 0x43, 0x20, 0x31, 0x2e, 0x33, 0x2e, 0x33, 0x20,
    0x32, 0x30, 0x31, 0x39, 0x30, 0x38, 0x30, 0x34, 0x01, 0x00, 0x00, 0x00, 0x0e, 0x00, 0x00, 0x00,
    0x54, 0x49, 0x54, 0x4c, 0x45, 0x3d, 0xd7, 0xa9, 0xd7, 0x9c, 0xd7, 0x95, 0xd7, 0x9d, 0x81, 0x00,
    0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xf8, 0x69, 0x98, 0x00, 0x0f, 0x99, 0x12,
    0x08, 0x67, 0x01, 0x62, 0x3d, 0x14, 0x42, 0x99, 0x8f, 0x5d, 0xf7, 0x0d, 0x6f, 0xe0, 0x0c, 0x17,
    0xca, 0xeb, 0x21, 0x00, 0x0e, 0xe7, 0xa7, 0x7a, 0x24, 0xa1, 0x59, 0x0c, 0x12, 0x17, 0xb6, 0x03,
    0x09, 0x7b, 0x78, 0x4f, 0xaa, 0x9a, 0x33, 0xd2, 0x85, 0xe0, 0x70, 0xad, 0x5b, 0x1b, 0x48, 0x51,
    0xb4, 0x01, 0x0d, 0x99, 0xd2, 0xcd, 0x1a, 0x68, 0xf1, 0xe6, 0xb8, 0x10, 0xff, 0xf8, 0x69, 0x18,
    0x01, 0x02, 0xa4, 0x02, 0xc3, 0x82, 0xc4, 0x0b, 0xc1, 0x4a, 0x03, 0xee, 0x48, 0xdd, 0x03, 0xb6,
    0x7c, 0x13, 0x30,
];

/// Example 2's samples, interleaved and little-endian, as the RFC lists them
/// for its digest.
const EXAMPLE_2_PCM: [u8; 76] = [
    0x84, 0x28, 0xb6, 0x17, 0x79, 0x46, 0x31, 0x29, 0x5e, 0x3a, 0x27, 0x22, 0xd4, 0x45, 0xd1, 0x28,
    0x0b, 0x3d, 0xb7, 0x23, 0xeb, 0x45, 0xdf, 0x28, 0x72, 0x3f, 0x1e, 0x25, 0x9d, 0x46, 0x49, 0x29,
    0xb8, 0x41, 0x70, 0x26, 0x57, 0x47, 0xb8, 0x29, 0x8f, 0x43, 0x81, 0x27, 0xae, 0xc7, 0x14, 0xdf,
    0x9f, 0xc4, 0x41, 0xdd, 0x54, 0xc7, 0xe4, 0xde, 0xa5, 0xc4, 0x40, 0xdd, 0x1e, 0xc6, 0x33, 0xde,
    0x82, 0xc3, 0x90, 0xdc, 0x0b, 0xc4, 0x02, 0xdd, 0x4a, 0xc1, 0x3e, 0xdb,
];

/// Example 3: 8-bit mono, one third-order linear-predictor subframe with
/// four residual partitions, one of them escaped.
pub(crate) const EXAMPLE_3: [u8; 73] = [
    0x66, 0x4c, 0x61, 0x43, 0x80, 0x00, 0x00, 0x22, 0x10, 0x00, 0x10, 0x00, 0x00, 0x00, 0x1f, 0x00,
    0x00, 0x1f, 0x07, 0xd0, 0x00, 0x70, 0x00, 0x00, 0x00, 0x18, 0xf8, 0xf9, 0xe3, 0x96, 0xf5, 0xcb,
    0xcf, 0xc6, 0xdc, 0x80, 0x7f, 0x99, 0x77, 0x90, 0x6b, 0x32, 0xff, 0xf8, 0x68, 0x02, 0x00, 0x17,
    0xe9, 0x44, 0x00, 0x4f, 0x6f, 0x31, 0x3d, 0x10, 0x47, 0xd2, 0x27, 0xcb, 0x6d, 0x09, 0x08, 0x31,
    0x45, 0x2b, 0xdc, 0x28, 0x22, 0x22, 0x80, 0x57, 0xa3,
];

/// Example 3's samples, as the RFC tables them.
const EXAMPLE_3_SAMPLES: [i8; 24] = [
    0, 79, 111, 78, 8, -61, -90, -68, -13, 42, 67, 53, 13, -27, -46, -38, -12, 14, 24, 19, 6, -4,
    -5, 0,
];

/// Every frame `file` holds, a block of `block` frames at a time, and how the
/// stream ended.
pub(crate) fn decode_all(
    file: &[u8],
    block: usize,
) -> (PcmSource, Vec<u8>, Result<(), DecodeError>) {
    let mut input = file;
    let mut source = PcmSource::open(&mut input, &LIMITS).expect("opens");
    let mut out = vec![0u8; block * source.info().frame_bytes()];
    let mut pcm = Vec::new();
    let end = loop {
        match source.next_block(&mut input, &mut out) {
            Ok(0) => break Ok(()),
            Ok(written) => pcm.extend_from_slice(&out[..written * source.info().frame_bytes()]),
            Err(err) => break Err(err),
        }
    };
    (source, pcm, end)
}

#[test]
fn example_1_decodes_to_the_samples_its_digest_states() {
    let (source, pcm, end) = decode_all(&EXAMPLE_1, 16);
    let info = source.info();
    assert_eq!(
        (
            info.format,
            info.encoding,
            info.rate.hz(),
            info.channels,
            info.sample
        ),
        (
            SoundFormat::Flac,
            Encoding::Flac,
            44_100,
            ChannelMap::STEREO,
            SampleFormat::S16
        )
    );
    assert_eq!(info.frames, Some(1));
    assert_eq!(pcm, [0xf4, 0x63, 0xb0, 0x28]);
    assert_eq!(end, Ok(()));
}

#[test]
fn example_2_decodes_to_the_samples_the_rfc_lists() {
    for block in [1, 3, 16, 64] {
        let (source, pcm, end) = decode_all(&EXAMPLE_2, block);
        assert_eq!(pcm, EXAMPLE_2_PCM, "blocks of {block}");
        assert_eq!(end, Ok(()));
        let tags: Vec<(TagKind, &str)> = source
            .metadata()
            .tags
            .iter()
            .map(|tag| (tag.kind, tag.value.as_str()))
            .collect();
        assert_eq!(
            tags,
            [
                (TagKind::Software, "reference libFLAC 1.3.3 20190804"),
                (TagKind::Title, "שלום"),
            ]
        );
    }
}

#[test]
fn example_3_decodes_its_linear_predictor_and_escaped_partition() {
    let (source, pcm, end) = decode_all(&EXAMPLE_3, 7);
    let info = source.info();
    assert_eq!(
        (info.rate.hz(), info.channels, info.sample),
        (32_000, ChannelMap::MONO, SampleFormat::U8)
    );
    let expected: Vec<u8> = EXAMPLE_3_SAMPLES
        .iter()
        .map(|&sample| sample.to_le_bytes()[0] ^ 0x80)
        .collect();
    assert_eq!(pcm, expected);
    assert_eq!(end, Ok(()));
}

/// A digest that disagrees ends the stream in a refusal once every sample
/// has been written: the samples cannot be vouched for.
#[test]
fn a_stream_whose_samples_disagree_with_its_digest_is_refused_at_its_end() {
    let mut file = EXAMPLE_2;
    file[0x1a] ^= 1;
    let (_, pcm, end) = decode_all(&file, 16);
    assert_eq!(pcm, EXAMPLE_2_PCM);
    assert_eq!(end, Err(DecodeError::FlacDigestMismatch));
}

#[test]
fn seeking_lands_on_the_frame_holding_the_sample_from_either_side() {
    let mut input = &EXAMPLE_2[..];
    let mut source = PcmSource::open(&mut input, &LIMITS).expect("opens");
    let mut out = [0u8; 8];
    for frame in [18u64, 0, 15, 16, 3] {
        source.seek(frame).expect("seekable");
        let written = source.next_block(&mut input, &mut out).expect("decodes");
        let at = usize::try_from(frame).expect("small") * 4;
        assert_eq!(
            &out[..written * 4],
            &EXAMPLE_2_PCM[at..at + written * 4],
            "at {frame}"
        );
    }
    assert_eq!(source.seek(20), Err(DecodeError::SeekPastEnd));
}

#[test]
fn a_damaged_frame_is_refused_by_its_checks() {
    let mut first = EXAMPLE_2;
    first[0x8d] ^= 0x01;
    assert_eq!(
        PcmSource::open(&mut &first[..], &LIMITS).map(|_| ()),
        Err(DecodeError::FlacHeaderCrc)
    );
    let mut second = EXAMPLE_2;
    second[0xd1] ^= 0x01;
    let (_, pcm, end) = decode_all(&second, 16);
    assert_eq!(pcm, EXAMPLE_2_PCM[..16 * 4]);
    assert_eq!(end, Err(DecodeError::FlacHeaderCrc));
    let mut body = EXAMPLE_2;
    body[0xa0] ^= 0x10;
    let (_, pcm, end) = decode_all(&body, 16);
    assert!(pcm.is_empty());
    assert!(end.is_err(), "{end:?}");
}

#[test]
fn a_stream_cut_short_is_refused_once_its_samples_run_out() {
    let (_, pcm, end) = decode_all(&EXAMPLE_2[..0xd8], 16);
    assert_eq!(pcm, EXAMPLE_2_PCM[..16 * 4]);
    assert_eq!(end, Err(DecodeError::FlacTruncated));
    let (_, pcm, end) = decode_all(&EXAMPLE_2[..0xcc], 16);
    assert_eq!(pcm, EXAMPLE_2_PCM[..16 * 4]);
    assert_eq!(end, Err(DecodeError::FlacTruncated));
}

#[test]
fn an_id3v2_tag_ahead_of_the_stream_is_stepped_over() {
    let mut file = b"ID3\x04\x00\x00\x00\x00\x00\x05".to_vec();
    file.extend([0u8; 5]);
    file.extend(EXAMPLE_1);
    let (_, pcm, end) = decode_all(&file, 16);
    assert_eq!(pcm, [0xf4, 0x63, 0xb0, 0x28]);
    assert_eq!(end, Ok(()));
}

#[test]
fn metadata_the_format_forbids_is_refused() {
    let open = |file: &[u8]| PcmSource::open(&mut &file[..], &LIMITS).map(|_| ());
    let mut small_block = EXAMPLE_1;
    small_block[8..12].copy_from_slice(&[0, 15, 0, 15]);
    assert_eq!(open(&small_block), Err(DecodeError::FlacBadStreamInfo));
    let mut no_info = EXAMPLE_1;
    no_info[4] = 0x81;
    assert_eq!(open(&no_info), Err(DecodeError::FlacMissingStreamInfo));
    let mut forbidden = EXAMPLE_2;
    forbidden[0x7e] = 0xFF;
    assert_eq!(open(&forbidden), Err(DecodeError::FlacForbiddenBlock));
    let mut table = EXAMPLE_2;
    table[0x2d] = 0x11;
    assert_eq!(open(&table), Err(DecodeError::FlacBadSeekTable));
    assert_eq!(
        open(&EXAMPLE_2[..0x50]),
        Err(DecodeError::FlacMetadataTruncated)
    );
}
