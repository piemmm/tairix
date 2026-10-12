//! Opus in Ogg: libopus's streams against libopus's own decodes of them and
//! its encoder's range-coder states; pre-skip, gain and the granules placing
//! and trimming the stream; seeking with the mapping's pre-roll; chains; and
//! every header and timeline refusal.
//!
//! The `tests/vectors/opus-*` streams were encoded by libopus 1.4 from signals
//! synthesised for them, every packet's settings forced so that between them
//! they reach each mode, bandwidth and frame size, mono and stereo, the
//! switches between modes with and without a redundant frame, redundancy for
//! loss, discontinuous transmission, constant bitrate's padding, packets of
//! many frames, 5.1, 6.1 and 7.1. Each `.flac` beside them is libopus's float
//! decode, its pre-skip and trim applied and its gain, quantised as
//! `floor(x * 32768 + 0.5)` and stored losslessly; each `.ranges` holds the
//! encoder's final range for every packet, big-endian.

#![allow(
    clippy::cast_possible_truncation,
    reason = "test streams keep their values within the fields they fill"
)]
#![allow(
    clippy::float_cmp,
    reason = "where a decode is asserted exact, exactness is the property under test"
)]

extern crate std;

use std::vec;
use std::vec::Vec;

use super::{gain_factor, parse_head, HEAD_SIGNATURE};
use crate::ogg::test_support::{
    assert_within_one, decode, decode_link, other_stream, pages, quantised, reference, reseal,
    LIMITS,
};
use crate::ogg::PageWriter;
use crate::opus::{Multistream, Packet};
use crate::{DecodeError, DecodeLimits, Encoding, Link, PcmSource, SoundFormat, TagKind};

/// One of libopus's streams and what it states.
struct Vector {
    what: &'static str,
    stream: &'static [u8],
    flac: &'static [u8],
    ranges: &'static [u8],
    channels: u8,
}

macro_rules! vector {
    ($what:literal, $name:literal, $channels:literal) => {
        Vector {
            what: $what,
            stream: include_bytes!(concat!("../../tests/vectors/", $name, ".opus")),
            flac: include_bytes!(concat!("../../tests/vectors/", $name, ".flac")),
            ranges: include_bytes!(concat!("../../tests/vectors/", $name, ".ranges")),
            channels: $channels,
        }
    };
}

const VECTORS: [Vector; 9] = [
    vector!("CELT", "opus-celt", 2),
    vector!("SILK", "opus-silk", 2),
    vector!("hybrid", "opus-hybrid", 2),
    vector!("switching", "opus-switching", 2),
    vector!("constant bitrate", "opus-cbr", 2),
    vector!("discontinuous", "opus-dtx", 2),
    vector!("5.1", "opus-surround", 6),
    vector!("7.1", "opus-octophonic", 8),
    vector!("6.1", "opus-heptaphonic", 7),
];

/// The packets of `file`'s one logical stream, its headers first.
fn packets_of(file: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut packet = Vec::new();
    for page in pages(file) {
        let segments = usize::from(page[26]);
        let mut at = 27 + segments;
        for &len in &page[27..27 + segments] {
            packet.extend_from_slice(&page[at..at + usize::from(len)]);
            at += usize::from(len);
            if len < 255 {
                out.push(core::mem::take(&mut packet));
            }
        }
    }
    out
}

#[test]
fn libopus_s_streams_decode_to_its_own_samples() {
    for Vector {
        what,
        stream,
        flac,
        channels,
        ..
    } in VECTORS
    {
        let (source, samples, end) = decode(stream);
        assert_eq!(end, Ok(()), "{what}");
        let info = source.info();
        assert_eq!(
            (info.format, info.encoding),
            (SoundFormat::Ogg, Encoding::Opus),
            "{what}"
        );
        assert_eq!(
            (info.channels.channels(), info.rate.hz()),
            (channels, 48_000),
            "{what}"
        );
        let want = reference(flac);
        assert_eq!(
            info.frames,
            Some((want.len() / usize::from(channels)) as u64),
            "{what}"
        );
        assert_within_one(&quantised(&samples), &want, what);
        assert!(
            source
                .metadata()
                .tags
                .iter()
                .any(|tag| tag.kind == TagKind::Title),
            "{what}: the comment header's tags"
        );
    }
}

/// Every packet leaves the range decoder exactly where the encoder's range
/// coder ended it, which only a decode reading every symbol as written does.
#[test]
fn every_packet_ends_in_the_encoder_s_range_state() {
    for Vector {
        what,
        stream,
        ranges,
        ..
    } in VECTORS
    {
        let packets = packets_of(stream);
        let head = parse_head(&packets[0]).expect("a header");
        let mut decoder = Multistream::new(&head.layout).expect("a decoder");
        let mut pcm = vec![0.0f32; 5760 * head.layout.channels];
        let want: Vec<u32> = ranges
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| u32::from_be_bytes(*bytes))
            .collect();
        assert_eq!(packets.len() - 2, want.len(), "{what}");
        for (index, (packet, &range)) in packets[2..].iter().zip(&want).enumerate() {
            decoder.decode(packet, &mut pcm).expect("decodes");
            assert_eq!(decoder.final_range(), range, "{what}: packet {index}");
        }
    }
}

/// A seek near enough ahead decodes on, and one whose pre-roll would reach
/// before the first page starts from the beginning: both are exact. Any other
/// lands on its frame and converges onto the decode from the start — Opus
/// carries its decoder's state from packet to packet, SILK's longest — to
/// within a hundredth of full scale over the first 20 ms and half that past
/// 100 ms, where a seek landing on the wrong frame would be off by the
/// signal itself.
#[test]
fn seeks_land_on_their_frame_and_converge_onto_the_decode_from_the_start() {
    const HEARD: usize = 24_000;
    for Vector {
        what,
        stream,
        channels,
        ..
    } in VECTORS
    {
        let (_, whole, _) = decode(stream);
        let channels = usize::from(channels);
        let frames = whole.len() / channels;
        let mut block = vec![0u8; HEARD * channels * 4];
        let targets = [0, 100, 3000, 7000]
            .into_iter()
            .chain((8000..frames).step_by(997))
            .chain([frames - 10]);
        for target in targets.filter(|&target| target < frames) {
            let mut source = PcmSource::open(&mut &stream[..], &LIMITS).expect("opens");
            source.seek(target as u64).expect("seeks");
            let mut after = Vec::new();
            while after.len() < HEARD * channels {
                let written = source
                    .next_block(&mut &stream[..], &mut block)
                    .expect("reads");
                if written == 0 {
                    break;
                }
                let bytes = &block[..written * channels * 4];
                after.extend(
                    bytes
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|bytes| f32::from_le_bytes(*bytes)),
                );
            }
            assert!(
                after.len() >= (frames - target).min(HEARD) * channels,
                "{what} at {target}"
            );
            let worst = |from: usize, to: usize| {
                let (from, to) = (from.min(after.len()), to.min(after.len()));
                after[from..to]
                    .iter()
                    .zip(&whole[target * channels + from..])
                    .map(|(a, b)| (a - b).abs())
                    .fold(0.0f32, f32::max)
            };
            if target + 7680 <= 8000 {
                assert_eq!(
                    worst(0, after.len()),
                    0.0,
                    "{what}: a seek within the pre-roll of the start is exact"
                );
            } else {
                assert!(
                    worst(0, 960 * channels) < 0.01,
                    "{what} at {target}: the first 20 ms"
                );
                assert!(
                    worst(4800 * channels, after.len()) < 0.005,
                    "{what} at {target}: past 100 ms"
                );
            }
        }
        let mut source = PcmSource::open(&mut &stream[..], &LIMITS).expect("opens");
        let mut start = vec![0u8; 1000 * source.info().frame_bytes()];
        source
            .next_block(&mut &stream[..], &mut start)
            .expect("reads");
        source.seek(3000).expect("seeks ahead");
        let (ahead, _) = decode_link(&mut source, stream);
        assert_eq!(
            ahead,
            whole[3000 * channels..],
            "{what}: a seek just ahead decodes on"
        );
    }
}

/// The header's gain scales every sample by `10^(gain / 5120)`.
#[test]
fn the_output_gain_scales_the_decode() {
    let stream = VECTORS[2].stream;
    let head = parse_head(&packets_of(stream)[0]).expect("a header");
    assert_eq!(head.gain, -512, "the vector states a gain of -2 dB");
    let mut pages = pages(stream);
    pages[0][16 + 28..16 + 28 + 2].copy_from_slice(&0i16.to_le_bytes());
    reseal(&mut pages[0]);
    let unity: Vec<u8> = pages.concat();
    let (_, scaled, _) = decode(stream);
    let (_, plain, _) = decode(&unity);
    let factor = gain_factor(-512);
    assert!((factor - 10f32.powf(-2.0 / 20.0)).abs() < 1e-6);
    for (a, b) in scaled.iter().zip(&plain) {
        assert_eq!(*a, b * factor);
    }
}

/// The samples `packet` decodes to.
fn samples_of(packet: &[u8]) -> u64 {
    Packet::parse(packet, false).map_or(0, |packet| packet.samples() as u64)
}

/// An Opus stream of `packets` laid `per_page` to a page, its granules
/// counting from `late` and its last page's cut short by `trim`.
fn stream_of(head: &[u8], packets: &[Vec<u8>], per_page: usize, late: u64, trim: u64) -> Vec<u8> {
    let mut writer = PageWriter::new(0x0b05);
    writer.packet(head, 0);
    writer.flush(false);
    writer.packet(&comment(), 0);
    writer.flush(false);
    let mut granule = late;
    let chunks: Vec<&[Vec<u8>]> = packets.chunks(per_page).collect();
    for (index, chunk) in chunks.iter().enumerate() {
        let last = index + 1 == chunks.len();
        for packet in *chunk {
            granule += samples_of(packet);
            writer.packet(packet, if last { granule - trim } else { granule });
        }
        writer.flush(last);
    }
    writer.into_bytes()
}

/// A comment header with a title.
fn comment() -> Vec<u8> {
    let mut tags = b"OpusTags".to_vec();
    tags.extend_from_slice(&4u32.to_le_bytes());
    tags.extend_from_slice(b"test");
    tags.extend_from_slice(&1u32.to_le_bytes());
    tags.extend_from_slice(&10u32.to_le_bytes());
    tags.extend_from_slice(b"TITLE=test");
    tags
}

/// An identification header.
fn head(channels: u8, pre_skip: u16, family: u8, table: &[u8]) -> Vec<u8> {
    let mut head = HEAD_SIGNATURE.to_vec();
    head.extend_from_slice(&[1, channels]);
    head.extend_from_slice(&pre_skip.to_le_bytes());
    head.extend_from_slice(&48_000u32.to_le_bytes());
    head.extend_from_slice(&0i16.to_le_bytes());
    head.push(family);
    head.extend_from_slice(table);
    head
}

/// The CELT vector's audio packets, which are 2.5 to 60 ms each.
fn celt_packets() -> Vec<Vec<u8>> {
    packets_of(VECTORS[0].stream)[2..].to_vec()
}

#[test]
fn identification_headers_out_of_form_are_refused() {
    let refuse = |header: &[u8]| parse_head(header).err();
    assert_eq!(
        refuse(&head(2, 312, 0, &[])[..18]),
        Some(DecodeError::OpusBadIdentification)
    );
    let mut wrong = head(2, 312, 0, &[]);
    wrong[0] = b'o';
    assert_eq!(refuse(&wrong), Some(DecodeError::OpusBadIdentification));
    let mut version = head(2, 312, 0, &[]);
    version[8] = 0x10;
    assert_eq!(refuse(&version), Some(DecodeError::OpusUnsupportedVersion));
    version[8] = 0x0F;
    assert_eq!(refuse(&version), None, "a later minor version still reads");
    assert_eq!(refuse(&head(0, 312, 0, &[])), Some(DecodeError::NoChannels));
    assert_eq!(
        refuse(&head(3, 312, 0, &[])),
        Some(DecodeError::OpusBadIdentification)
    );
    assert_eq!(
        refuse(&head(2, 312, 1, &[1, 1, 0])),
        Some(DecodeError::OpusBadIdentification),
        "a short table"
    );
    assert_eq!(
        refuse(&head(2, 312, 1, &[1, 1, 0, 2])),
        Some(DecodeError::OpusBadIdentification),
        "an entry past the decoded channels"
    );
    assert_eq!(
        refuse(&head(2, 312, 1, &[1, 2, 0, 1])),
        Some(DecodeError::OpusBadIdentification),
        "more coupled than streams"
    );
    assert_eq!(
        refuse(&head(2, 312, 1, &[0, 0, 0, 1])),
        Some(DecodeError::OpusBadIdentification),
        "no streams"
    );
    assert_eq!(
        refuse(&head(2, 312, 1, &[1, 1, 255, 1])),
        None,
        "a silent channel"
    );
    assert_eq!(
        refuse(&head(2, 312, 0, &[7, 7, 7])),
        None,
        "data past the header"
    );
    let packets = celt_packets();
    let open = |head: &[u8], limits: &DecodeLimits| {
        PcmSource::open(&mut &stream_of(head, &packets, 2, 0, 0)[..], limits).err()
    };
    assert_eq!(
        open(&head(2, 0, 255, &[1, 1, 0, 1]), &LIMITS),
        Some(DecodeError::ChannelLayoutUnsupported)
    );
    assert_eq!(
        open(&head(2, 0, 2, &[1, 1, 0, 1]), &LIMITS),
        Some(DecodeError::ChannelLayoutUnsupported)
    );
    let narrow = DecodeLimits::new(2, 4096, 64);
    let mut many = vec![8u8, 0];
    many.extend_from_slice(&[0, 1]);
    assert_eq!(
        open(&head(2, 0, 1, &many), &narrow),
        Some(DecodeError::ChannelsExceedLimit),
        "decoded channels past the limit"
    );
}

#[test]
fn headers_placed_out_of_form_are_refused() {
    let packets = celt_packets();
    let good = stream_of(&head(2, 312, 0, &[]), &packets, 2, 0, 0);
    let mut shared = PageWriter::new(7);
    shared.packet(&head(2, 312, 0, &[]), 0);
    shared.packet(&comment(), 0);
    shared.flush(false);
    for packet in &packets {
        shared.packet(packet, 0);
    }
    shared.flush(true);
    assert_eq!(
        PcmSource::open(&mut &shared.into_bytes()[..], &LIMITS).err(),
        Some(DecodeError::OpusBadIdentification),
        "the identification header shares its page"
    );
    let mut sharing = PageWriter::new(8);
    sharing.packet(&head(2, 312, 0, &[]), 0);
    sharing.flush(false);
    sharing.packet(&comment(), 0);
    sharing.packet(&packets[0], 0);
    sharing.flush(true);
    assert_eq!(
        PcmSource::open(&mut &sharing.into_bytes()[..], &LIMITS).err(),
        Some(DecodeError::OpusBadComment),
        "audio on the comment header's page"
    );
    let mut pages = pages(&good);
    let body = 27 + usize::from(pages[1][26]);
    pages[1][body] = b'X';
    reseal(&mut pages[1]);
    assert_eq!(
        PcmSource::open(&mut &pages.concat()[..], &LIMITS).err(),
        Some(DecodeError::OpusBadComment),
        "a comment header's signature"
    );
}

/// The pre-skip's samples are decoded and never played; a first page whose
/// granule exceeds its packets' samples starts the stream later and trims
/// nothing; the last page's granule trims the end, even back past its final
/// packet.
#[test]
fn pre_skip_and_granules_place_and_trim_the_stream() {
    let packets = celt_packets();
    let total: u64 = packets.iter().map(|packet| samples_of(packet)).sum();
    let (_, whole, end) = decode(&stream_of(&head(2, 0, 0, &[]), &packets, 3, 0, 0));
    assert_eq!(end, Ok(()));
    assert_eq!(whole.len() as u64, 2 * total);
    let (source, skipped, _) = decode(&stream_of(&head(2, 312, 0, &[]), &packets, 3, 0, 0));
    assert_eq!(source.info().frames, Some(total - 312));
    assert_eq!(
        skipped,
        whole[2 * 312..],
        "the pre-skip is decoded, then dropped"
    );
    let (source, late, _) = decode(&stream_of(&head(2, 312, 0, &[]), &packets, 3, 48_000, 0));
    assert_eq!(source.info().frames, Some(total - 312));
    assert_eq!(late, skipped, "a later start trims nothing");
    let (source, cut, _) = decode(&stream_of(&head(2, 312, 0, &[]), &packets, 3, 0, 5000));
    assert_eq!(source.info().frames, Some(total - 312 - 5000));
    assert_eq!(
        cut,
        skipped[..skipped.len() - 2 * 5000],
        "the end trimmed back past the last packet"
    );
    let mut one = PageWriter::new(3);
    one.packet(&head(2, 120, 0, &[]), 0);
    one.flush(false);
    one.packet(&comment(), 0);
    one.flush(false);
    for packet in &packets[..4] {
        one.packet(packet, 700);
    }
    one.flush(true);
    let (source, short, end) = decode(&one.into_bytes());
    assert_eq!(end, Ok(()));
    assert_eq!(
        source.info().frames,
        Some(700 - 120),
        "a stream on one page has its end cut"
    );
    assert_eq!(short, whole[2 * 120..2 * 700]);
}

#[test]
fn a_timeline_at_odds_with_the_samples_is_refused() {
    let packets = celt_packets();
    let good = stream_of(&head(2, 312, 0, &[]), &packets, 3, 0, 0);
    let damaged = |page: usize, granule: u64| {
        let mut pages = pages(&good);
        pages[page][6..14].copy_from_slice(&granule.to_le_bytes());
        reseal(&mut pages[page]);
        pages.concat()
    };
    let opened = |file: &[u8]| PcmSource::open(&mut &file[..], &LIMITS).err();
    let first: u64 = packets[..3].iter().map(|packet| samples_of(packet)).sum();
    assert_eq!(
        opened(&damaged(2, first - 1)),
        Some(DecodeError::OggGranuleMismatch),
        "a first page short of its samples"
    );
    let (_, _, end) = decode(&damaged(4, 10_000_000));
    assert_eq!(
        end,
        Err(DecodeError::OggGranuleMismatch),
        "a middle page off the timeline"
    );
    let pages = pages(&good);
    let last = pages.len() - 1;
    let total: u64 = packets.iter().map(|packet| samples_of(packet)).sum();
    let (_, _, end) = decode(&damaged(last, total + 1));
    assert_eq!(
        end,
        Err(DecodeError::OggGranuleMismatch),
        "a last page past its samples"
    );
    let (_, _, end) = decode(&damaged(last, 400));
    assert_eq!(
        end,
        Err(DecodeError::OggGranuleMismatch),
        "a last page cutting before the page ahead of it"
    );
    assert_eq!(
        opened(&damaged(last, 100)),
        Some(DecodeError::OggGranuleMismatch),
        "a last page ending the stream inside its pre-skip, refused before it states a length"
    );
    let mut tiny = PageWriter::new(4);
    tiny.packet(&head(2, 4000, 0, &[]), 0);
    tiny.flush(false);
    tiny.packet(&comment(), 0);
    tiny.flush(false);
    tiny.packet(&packets[0], 100);
    tiny.flush(true);
    assert_eq!(
        PcmSource::open(&mut &tiny.into_bytes()[..], &LIMITS).err(),
        Some(DecodeError::OggGranuleMismatch),
        "a stream ending before its pre-skip"
    );
}

#[test]
fn audio_packets_out_of_form_are_refused() {
    let mut packets = celt_packets();
    packets.insert(2, Vec::new());
    let file = stream_of(&head(2, 312, 0, &[]), &packets, 3, 0, 0);
    assert_eq!(
        PcmSource::open(&mut &file[..], &LIMITS).err(),
        Some(DecodeError::OpusPacketInvalid),
        "an empty packet is no Opus packet"
    );
    let mut packets = celt_packets();
    packets[5] = vec![0xFC; 61_441];
    let (_, _, end) = decode(&stream_of(&head(2, 312, 0, &[]), &packets, 3, 0, 0));
    assert_eq!(
        end,
        Err(DecodeError::OggPacketTooLarge),
        "past the mapping's largest packet"
    );
    let mut packets = celt_packets();
    packets[7] = vec![packets[7][0] | 3, 0];
    let (_, _, end) = decode(&stream_of(&head(2, 312, 0, &[]), &packets, 3, 0, 0));
    assert_eq!(
        end,
        Err(DecodeError::OpusPacketInvalid),
        "a packet of no frames"
    );
}

/// Opus links chained with others are each read whole in turn, a seek
/// within a later link landing as it does in that link alone.
#[test]
fn a_chain_of_opus_links_is_read_link_by_link() {
    let first = VECTORS[0].stream;
    let mut second = pages(VECTORS[2].stream);
    for page in &mut second {
        page[14..18].copy_from_slice(&0x5ec0_u32.to_le_bytes());
        reseal(page);
    }
    let second = second.concat();
    let file = [first, &second[..]].concat();
    let mut source = PcmSource::open(&mut &file[..], &LIMITS).expect("opens");
    let (one, end) = decode_link(&mut source, &file);
    assert_eq!((one, end), (decode(first).1, Ok(())));
    assert!(source.next_link(&mut &file[..]).expect("a second link"));
    assert_eq!(
        source.link(),
        Link {
            index: 1,
            offset: first.len() as u64
        }
    );
    assert_eq!(source.info().encoding, Encoding::Opus);
    let alone = decode(&second).1;
    let (two, end) = decode_link(&mut source, &file);
    assert_eq!((&two, end), (&alone, Ok(())));
    assert_eq!(source.next_link(&mut &file[..]), Ok(false));
    let mut reopened =
        PcmSource::open_link(&mut &file[..], &LIMITS, source.link()).expect("reopens");
    reopened.seek(20_000).expect("seeks");
    let (after, _) = decode_link(&mut reopened, &file);
    let mut lone = PcmSource::open(&mut &second[..], &LIMITS).expect("opens");
    lone.seek(20_000).expect("seeks");
    assert_eq!(after, decode_link(&mut lone, &second).0);
}

/// The Opus stream is read past another stream's pages interleaved with its
/// own.
#[test]
fn a_multiplexed_opus_stream_is_read_past_the_other_stream() {
    let own = pages(VECTORS[3].stream);
    let other = other_stream(99, 30, 300);
    let mut file = own[0].clone();
    file.extend_from_slice(&other[0]);
    let mut others = other[1..].iter();
    for page in &own[1..] {
        file.extend_from_slice(page);
        if let Some(page) = others.next() {
            file.extend_from_slice(page);
        }
    }
    for page in others {
        file.extend_from_slice(page);
    }
    let (source, samples, end) = decode(&file);
    let (alone, whole, _) = decode(VECTORS[3].stream);
    assert_eq!(end, Ok(()));
    assert_eq!(source.info().frames, alone.info().frames);
    assert_eq!(samples, whole);
}

/// A file cut part way through a page plays to the cut and says so.
#[test]
fn a_file_cut_part_way_through_a_page_plays_to_the_cut() {
    let whole = VECTORS[1].stream;
    let (_, all, _) = decode(whole);
    let (_, samples, end) = decode(&whole[..whole.len() - 20]);
    assert_eq!(end, Err(DecodeError::OggPageTruncated));
    assert!(!samples.is_empty() && samples.len() < all.len());
    assert_eq!(samples, all[..samples.len()]);
}

/// Links sharing a serial, which the format forbids, are refused where the
/// first is seen to end: never read on into the next as though one stream,
/// nor sought into it.
#[test]
fn links_sharing_a_serial_are_refused_where_the_stream_is_seen_to_end() {
    let (first, second) = (VECTORS[0].stream, VECTORS[2].stream);
    assert_eq!(
        pages(first)[0][14..18],
        pages(second)[0][14..18],
        "libopus's streams share a serial"
    );
    let file = [first, second].concat();
    let (_, samples, end) = decode(&file);
    let alone = decode(first).1;
    assert_eq!(end, Err(DecodeError::OggBadLink));
    assert!(!samples.is_empty() && samples.len() < alone.len());
    assert_eq!(samples, alone[..samples.len()]);
    let mut source = PcmSource::open(&mut &file[..], &LIMITS).expect("opens");
    let frames = source.info().frames.expect("a length");
    source.seek(frames - 100).expect("seeks");
    let mut out = vec![0u8; 64 * source.info().frame_bytes()];
    assert_eq!(
        source.next_block(&mut &file[..], &mut out),
        Err(DecodeError::OggBadLink)
    );
}

/// Another stream's damaged page claiming a length past the file hides the
/// pages after it: the stream has lost them, and does not end short of its
/// last page in silence.
#[test]
fn pages_hidden_behind_another_stream_s_damaged_page_are_lost() {
    let own = pages(VECTORS[3].stream);
    let mut other = other_stream(99, 30, 300);
    other[4][26] = 255;
    other[4][27..27 + 255].fill(255);
    let mut file = own[0].clone();
    for (at, page) in own[1..].iter().enumerate() {
        file.extend_from_slice(&other[at]);
        file.extend_from_slice(page);
    }
    for page in &other[own.len() - 1..] {
        file.extend_from_slice(page);
    }
    let (_, samples, end) = decode(&file);
    let whole = decode(VECTORS[3].stream).1;
    assert_eq!(end, Err(DecodeError::OggPageLost));
    assert!(!samples.is_empty() && samples.len() < whole.len());
    assert_eq!(samples, whole[..samples.len()]);
}
