//! Vorbis in Ogg: libvorbis's own streams against libvorbis's own decodes of
//! them, and streams written here for what libvorbis never writes; the
//! mapping's granules placing and trimming the stream; chains, multiplexing,
//! seeking, and every header refusal.
//!
//! The `tests/vectors/vorbis-*` streams were encoded by libvorbis 1.3.7 (its
//! vendor string `Xiph.Org libVorbis I 20200704`) through `GStreamer` 1.24.2,
//! and each `.flac` beside them is libvorbis's own decode, its float output
//! quantised as `floor(x * 32768 + 0.5)`, clipped to 16 bits and stored
//! losslessly, its channels in the stream's own order. `vorbis-floor0.flac`
//! is libvorbis's decode of [`floor0_oracle`], which only this crate writes.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "test streams keep their values within the fields they fill"
)]

extern crate std;

use std::vec;
use std::vec::Vec;

use tairix_fuzzseed::Prng;

use crate::flac_encode::{encode, Options, Params};
use crate::ogg::test_support::{
    assert_within_one, decode, decode_link, other_stream, pages, quantised, reference, reseal,
    LIMITS,
};
use crate::ogg::PageWriter;
use crate::vorbis::writer::{
    floor0_three_channels, floor1_stereo, ogg_stream, packets, PacketSpec, Spec,
};
use crate::{DecodeError, Encoding, Link, PcmSource, SoundFormat, TagKind};

/// One of libvorbis's streams, its decode, and what the stream states.
struct Vector {
    what: &'static str,
    stream: &'static [u8],
    flac: &'static [u8],
    channels: u8,
    rate: u32,
}

const VECTORS: [Vector; 3] = [
    Vector {
        what: "stereo",
        stream: include_bytes!("../../tests/vectors/vorbis-stereo.ogg"),
        flac: include_bytes!("../../tests/vectors/vorbis-stereo.flac"),
        channels: 2,
        rate: 44_100,
    },
    Vector {
        what: "mono",
        stream: include_bytes!("../../tests/vectors/vorbis-mono.ogg"),
        flac: include_bytes!("../../tests/vectors/vorbis-mono.flac"),
        channels: 1,
        rate: 8_000,
    },
    Vector {
        what: "5.1",
        stream: include_bytes!("../../tests/vectors/vorbis-surround.ogg"),
        flac: include_bytes!("../../tests/vectors/vorbis-surround.flac"),
        channels: 6,
        rate: 48_000,
    },
];

#[test]
fn libvorbis_s_own_streams_decode_to_its_own_samples() {
    for Vector {
        what,
        stream,
        flac,
        channels,
        rate,
    } in VECTORS
    {
        let (source, samples, end) = decode(stream);
        assert_eq!(end, Ok(()), "{what}");
        let info = source.info();
        assert_eq!(
            (info.format, info.encoding),
            (SoundFormat::Ogg, Encoding::Vorbis),
            "{what}"
        );
        assert_eq!(
            (info.channels.channels(), info.rate.hz()),
            (channels, rate),
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
                .any(|tag| tag.kind == TagKind::Software
                    && tag.value.starts_with("Xiph.Org libVorbis")),
            "{what}: the vendor string as the encoder"
        );
    }
}

/// The stream `vorbis-floor0.flac` is libvorbis's decode of: floor 0 at both
/// orders, residues 0 and 1, a coefficient book listing its vectors, a book
/// of one entry, and two submaps — none of which libvorbis's encoder writes.
/// A change to the writer or its scenario changes this stream, and the
/// reference must then be made again from libvorbis.
fn floor0_oracle() -> Vec<u8> {
    let spec = floor0_three_channels();
    let modes: Vec<u8> = (0..40).map(|i| [0u8, 1, 1, 0, 1, 1, 1, 0][i % 8]).collect();
    ogg_stream(&spec, &packets(&spec, &modes, &[], 0x0f10), 0x0f0, 0, 0)
}

#[test]
fn the_constructs_libvorbis_never_writes_decode_as_libvorbis_decodes_them() {
    let (_, samples, end) = decode(&floor0_oracle());
    assert_eq!(end, Ok(()));
    let want = reference(include_bytes!("../../tests/vectors/vorbis-floor0.flac"));
    assert_within_one(&quantised(&samples), &want, "floor 0");
}

/// What the stream's packets decode to with no granule moving them.
fn plain(spec: &Spec, packets: &[PacketSpec]) -> Vec<f32> {
    decode(&ogg_stream(spec, packets, 1, 0, 0)).1
}

/// A first page whose granule falls short of the samples its packets return
/// trims them from the start; one past them starts the stream later and
/// trims nothing; the last page's granule trims the end.
#[test]
fn granules_trim_the_start_and_the_end() {
    let spec = floor1_stereo();
    let packets = packets(&spec, &[1, 1, 0, 1, 1, 1, 0, 0, 1, 1], &[], 21);
    let whole = plain(&spec, &packets);
    let frames = whole.len() / 2;
    let (source, late, end) = decode(&ogg_stream(&spec, &packets, 2, 5000, 0));
    assert_eq!(end, Ok(()));
    assert_eq!(late, whole, "a later start trims nothing");
    assert_eq!(source.info().frames, Some(frames as u64));
    let (source, trimmed, _) = decode(&ogg_stream(&spec, &packets, 3, -40, 0));
    assert_eq!(source.info().frames, Some(frames as u64 - 40));
    assert_eq!(trimmed, whole[80..], "forty frames trimmed from the start");
    let (source, cut, _) = decode(&ogg_stream(&spec, &packets, 4, 0, 50));
    assert_eq!(source.info().frames, Some(frames as u64 - 50));
    assert_eq!(
        cut,
        whole[..whole.len() - 100],
        "fifty trimmed from the end"
    );
}

/// A stream on one page has its end cut, never its beginning, even where its
/// granule falls short of the samples.
#[test]
fn a_stream_on_one_audio_page_is_cut_at_its_end() {
    let spec = floor1_stereo();
    let packets = packets(&spec, &[1, 1], &[], 22);
    let whole = plain(&spec, &packets);
    let (source, cut, _) = decode(&ogg_stream(&spec, &packets, 5, 0, 30));
    assert_eq!(source.info().frames, Some(whole.len() as u64 / 2 - 30));
    assert_eq!(cut, whole[..whole.len() - 60]);
}

/// A FLAC link of 3000 mono frames, and the bytes they decode to.
fn flac_link(serial: u32) -> (Vec<u8>, Vec<u8>) {
    let mut prng = Prng::new(u64::from(serial));
    let samples: Vec<i32> = (0..3000).map(|_| i32::from(prng.next_u8()) - 128).collect();
    let params = Params {
        rate: 32_000,
        channels: 1,
        bits: 16,
    };
    let file = encode(params, &samples, Options::default())
        .expect("encodes")
        .finish_ogg(serial, 4000);
    let bytes = samples
        .iter()
        .flat_map(|&sample| i16::try_from(sample).expect("16-bit").to_le_bytes())
        .collect();
    (file, bytes)
}

/// Links of different codecs, rates and channel counts are each read whole
/// in turn, every one with its own stream and positions.
#[test]
fn a_chain_of_links_of_any_codec_is_read_link_by_link() {
    let stereo = floor1_stereo();
    let first_packets = packets(&stereo, &[1, 1, 0, 0, 1, 1], &[], 31);
    let first = ogg_stream(&stereo, &first_packets, 10, 0, 0);
    let (flac, flac_bytes) = flac_link(11);
    let three = floor0_three_channels();
    let third_packets = packets(&three, &[0, 1, 1, 0, 1], &[], 32);
    let third = ogg_stream(&three, &third_packets, 12, 0, 0);
    let file = [first.clone(), flac.clone(), third.clone()].concat();
    let mut source = PcmSource::open(&mut &file[..], &LIMITS).expect("opens");
    let (samples, end) = decode_link(&mut source, &file);
    assert_eq!((samples, end), (plain(&stereo, &first_packets), Ok(())));
    assert!(source.next_link(&mut &file[..]).expect("a second link"));
    assert_eq!(source.info().encoding, Encoding::Flac);
    assert_eq!(source.info().rate.hz(), 32_000);
    let mut out = vec![0u8; 2 * 3000];
    let written = source
        .next_block(&mut &file[..], &mut out)
        .expect("decodes");
    assert_eq!(&out[..written * 2], &flac_bytes[..]);
    assert_eq!(source.next_block(&mut &file[..], &mut out), Ok(0));
    assert!(source.next_link(&mut &file[..]).expect("a third link"));
    assert_eq!(
        source.link(),
        Link {
            index: 2,
            offset: (first.len() + flac.len()) as u64
        }
    );
    assert_eq!(source.info().channels.channels(), 3);
    let (samples, end) = decode_link(&mut source, &file);
    assert_eq!((samples, end), (plain(&three, &third_packets), Ok(())));
    assert_eq!(source.next_link(&mut &file[..]), Ok(false));
    let mut reopened =
        PcmSource::open_link(&mut &file[..], &LIMITS, source.link()).expect("reopens");
    let (samples, _) = decode_link(&mut reopened, &file);
    assert_eq!(samples, plain(&three, &third_packets));
}

/// The Vorbis stream is read past another stream's pages interleaved with
/// its own, its opening page first among the link's.
#[test]
fn a_multiplexed_vorbis_stream_is_read_past_the_other_stream() {
    let spec = floor1_stereo();
    let audio = packets(&spec, &[1, 1, 0, 1, 1, 0, 0, 1], &[], 41);
    let own = pages(&ogg_stream(&spec, &audio, 7, 0, 0));
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
    let (_, samples, end) = decode(&file);
    assert_eq!((samples, end), (plain(&spec, &audio), Ok(())));
}

/// Another stream's damaged page claiming a length past the file hides the
/// pages after it: the stream has lost them, and does not end short of its
/// last page in silence.
#[test]
fn pages_hidden_behind_another_stream_s_damaged_page_are_lost() {
    let spec = floor1_stereo();
    let audio = packets(&spec, &[1, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1], &[], 43);
    let own = pages(&ogg_stream(&spec, &audio, 7, 0, 0));
    let mut other = other_stream(99, 30, 300);
    other[5][26] = 255;
    other[5][27..27 + 255].fill(255);
    let mut file = own[0].clone();
    for (at, page) in own[1..].iter().enumerate() {
        file.extend_from_slice(&other[at]);
        file.extend_from_slice(page);
    }
    for page in &other[own.len() - 1..] {
        file.extend_from_slice(page);
    }
    let (_, samples, end) = decode(&file);
    let whole = plain(&spec, &audio);
    assert_eq!(end, Err(DecodeError::OggPageLost));
    assert!(!samples.is_empty() && samples.len() < whole.len());
    assert_eq!(samples, whole[..samples.len()]);
}

/// A stream whose last page lies further back from its link's end than the
/// search reaches states no length, and still plays to its trimmed end.
#[test]
fn a_stream_ending_far_before_its_link_states_no_length() {
    let spec = floor1_stereo();
    let audio = packets(&spec, &[1, 1, 0, 1], &[], 42);
    let own = pages(&ogg_stream(&spec, &audio, 8, 0, 20));
    let other = other_stream(98, 40, 40_000);
    let mut file = own[0].clone();
    file.extend_from_slice(&other[0]);
    for page in &own[1..] {
        file.extend_from_slice(page);
    }
    for page in &other[1..] {
        file.extend_from_slice(page);
    }
    let (source, samples, end) = decode(&file);
    assert_eq!(source.info().frames, None);
    let whole = plain(&spec, &audio);
    assert_eq!((samples, end), (whole[..whole.len() - 40].to_vec(), Ok(())));
}

/// Packets laid across page boundaries, and pages on which no packet both
/// starts and ends, decode and seek exactly.
#[test]
fn seeking_lands_on_the_exact_frame_however_packets_cross_pages() {
    let spec = floor1_stereo();
    let modes: Vec<u8> = (0..400).map(|i| u8::from(i % 7 != 3)).collect();
    let audio = packets(&spec, &modes, &[], 51);
    let mut pages_writer = PageWriter::new(9);
    pages_writer.packet(&spec.identification(), 0);
    pages_writer.flush(false);
    pages_writer.packet(&Spec::comment("t", &[]), 0);
    pages_writer.packet(&spec.setup(), 0);
    pages_writer.flush(false);
    let mut position = 0u64;
    let mut previous = None;
    for (at, packet) in audio.iter().enumerate() {
        let size = spec.blocksize(spec.modes[usize::from(packet.mode)].0);
        if let Some(before) = previous {
            position += (before + size) as u64 / 4;
        }
        previous = Some(size);
        pages_writer.packet(&spec.packet(packet), position);
        if pages_writer.filled() > 150 || at + 1 == audio.len() {
            pages_writer.flush(at + 1 == audio.len());
        }
    }
    let file = pages_writer.into_bytes();
    let (_, whole, end) = decode(&file);
    assert_eq!(end, Ok(()));
    let frames = whole.len() / 2;
    let mut source = PcmSource::open(&mut &file[..], &LIMITS).expect("opens");
    let mut prng = Prng::new(52);
    let mut out = [0u8; 8 * 64];
    let mut targets: Vec<u64> = (0..60).map(|_| prng.next_u64() % frames as u64).collect();
    targets.extend([0, 1, frames as u64 - 1, 127, 128, 129]);
    for target in targets {
        source.seek(target).expect("seekable");
        let written = source
            .next_block(&mut &file[..], &mut out)
            .expect("decodes");
        assert!(written > 0, "at {target}");
        let at = target as usize * 2;
        let got: Vec<f32> = out[..written * 8]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| f32::from_le_bytes(*bytes))
            .collect();
        assert_eq!(got, whole[at..at + written * 2], "at {target}");
    }
    assert_eq!(
        source.seek(frames as u64 + 1),
        Err(DecodeError::SeekPastEnd)
    );
}

/// A comment header out of form is refused by name, as is a stream ending
/// within its headers or an audio packet longer than its block can use.
#[test]
fn headers_and_packets_out_of_form_are_refused() {
    let spec = floor1_stereo();
    let audio = packets(&spec, &[1, 1, 1], &[], 61);
    let with_comment = |comment: &[u8]| {
        let mut writer = PageWriter::new(3);
        writer.packet(&spec.identification(), 0);
        writer.flush(false);
        writer.packet(comment, 0);
        writer.packet(&spec.setup(), 0);
        writer.flush(false);
        for packet in &audio {
            writer.packet(&spec.packet(packet), 0);
        }
        writer.flush(true);
        writer.into_bytes()
    };
    let good = Spec::comment("v", &["TITLE=t"]);
    assert!(PcmSource::open(&mut &with_comment(&good)[..], &LIMITS).is_ok());
    let mut unframed = good.clone();
    *unframed.last_mut().expect("the framing byte") = 0;
    let mut misnamed = good.clone();
    misnamed[1] = b'V';
    let mut overlong = good.clone();
    overlong[7] = 0xFF;
    for (what, comment) in [
        ("unframed", unframed),
        ("misnamed", misnamed),
        ("overlong", overlong),
    ] {
        assert_eq!(
            PcmSource::open(&mut &with_comment(&comment)[..], &LIMITS).err(),
            Some(DecodeError::VorbisBadComment),
            "{what}"
        );
    }
    let mut headers_only = PageWriter::new(4);
    headers_only.packet(&spec.identification(), 0);
    headers_only.flush(false);
    headers_only.packet(&good, 0);
    headers_only.flush(true);
    assert_eq!(
        PcmSource::open(&mut &headers_only.into_bytes()[..], &LIMITS).err(),
        Some(DecodeError::OggNoAudio)
    );
    // The most a long block's packet can use, and a short block's.
    let long_limit = 128 * 2 * 36 + 2 * 1152 + 8;
    let short_limit = 32 * 2 * 36 + 2 * 1152 + 8;
    let short = packets(&spec, &[1, 1, 0, 0, 1], &[], 62);
    let padded = |at: usize, len: usize| {
        let mut writer = PageWriter::new(5);
        writer.packet(&spec.identification(), 0);
        writer.flush(false);
        writer.packet(&good, 0);
        writer.packet(&spec.setup(), 0);
        writer.flush(false);
        let mut position = 0;
        let mut previous = None;
        for (index, packet) in short.iter().enumerate() {
            let size = spec.blocksize(spec.modes[usize::from(packet.mode)].0) as u64;
            if let Some(before) = previous {
                position += (before + size) / 4;
            }
            previous = Some(size);
            let mut bytes = spec.packet(packet);
            if index == at {
                bytes.resize(len, 0);
            }
            writer.packet(&bytes, position);
            if index != 0 {
                writer.flush(index + 1 == short.len());
            }
        }
        writer.into_bytes()
    };
    assert_eq!(
        PcmSource::open(&mut &padded(1, long_limit + 1)[..], &LIMITS).err(),
        Some(DecodeError::OggPacketTooLarge),
        "a long block's packet past its use, among the first"
    );
    let (_, _, end) = decode(&padded(2, short_limit + 1));
    assert_eq!(
        end,
        Err(DecodeError::OggPacketTooLarge),
        "a short block's past its own"
    );
    let (_, _, end) = decode(&padded(2, short_limit));
    assert_eq!(end, Ok(()), "a short block's at its own");
}

/// A page whose granule disagrees with the samples before it, a last page
/// cutting short more than its final packet, and a packet that is not audio
/// are each refused, as each would set positions apart from the timeline.
#[test]
fn a_timeline_at_odds_with_the_samples_is_refused() {
    let spec = floor1_stereo();
    let audio = packets(&spec, &[1, 1, 0, 1, 1, 1], &[], 81);
    let file = ogg_stream(&spec, &audio, 13, 0, 0);
    let mut laid = pages(&file);
    let granule_page = 4;
    let granule = u64::from_le_bytes(laid[granule_page][6..14].try_into().expect("eight bytes"));
    laid[granule_page][6..14].copy_from_slice(&(granule + 1).to_le_bytes());
    reseal(&mut laid[granule_page]);
    let (_, _, end) = decode(&laid.concat());
    assert_eq!(
        end,
        Err(DecodeError::OggGranuleMismatch),
        "a granule a frame ahead"
    );
    let (_, _, end) = decode(&ogg_stream(&spec, &audio, 14, 0, 500));
    assert_eq!(
        end,
        Err(DecodeError::OggGranuleMismatch),
        "an end cut before the final packet"
    );
    let mut stray = audio.clone();
    stray[3].mode = 1;
    let mut written = pages(&ogg_stream(&spec, &stray, 15, 0, 0));
    let body = 27 + usize::from(written[5][26]);
    written[5][body] |= 1;
    reseal(&mut written[5]);
    let (_, _, end) = decode(&written.concat());
    assert_eq!(
        end,
        Err(DecodeError::VorbisBadPacket),
        "a packet whose first bit says header"
    );
}

/// Links sharing a serial, which the format forbids, are refused where the
/// first is seen to end: never read on into the next as though one stream,
/// nor sought into it.
#[test]
fn links_sharing_a_serial_are_refused_where_the_stream_is_seen_to_end() {
    let spec = floor1_stereo();
    let first_packets = packets(&spec, &[1, 1, 0, 0, 1, 1], &[], 51);
    let second_packets = packets(&spec, &[0, 1, 1, 0, 1, 1, 1, 0, 1, 1], &[], 52);
    let file = [
        ogg_stream(&spec, &first_packets, 9, 0, 0),
        ogg_stream(&spec, &second_packets, 9, 0, 0),
    ]
    .concat();
    let (_, samples, end) = decode(&file);
    let alone = plain(&spec, &first_packets);
    assert_eq!(end, Err(DecodeError::OggBadLink));
    assert!(!samples.is_empty() && samples.len() < alone.len());
    assert_eq!(samples, alone[..samples.len()]);
    let mut source = PcmSource::open(&mut &file[..], &LIMITS).expect("opens");
    let frames = source.info().frames.expect("a length");
    source.seek(frames - 10).expect("seeks");
    let mut out = vec![0u8; 64 * source.info().frame_bytes()];
    assert_eq!(
        source.next_block(&mut &file[..], &mut out),
        Err(DecodeError::OggBadLink)
    );
}

/// A stream whose ending page places its end before its first sample is
/// refused before it states a length.
#[test]
fn an_ending_page_before_the_stream_starts_is_refused_on_opening() {
    let spec = floor1_stereo();
    let audio = packets(&spec, &[1, 1, 0, 1, 1, 0, 1], &[], 44);
    let late = ogg_stream(&spec, &audio, 6, 5000, 0);
    assert!(PcmSource::open(&mut &late[..], &LIMITS).is_ok());
    let mut pages = pages(&late);
    let last = pages.len() - 1;
    pages[last][6..14].copy_from_slice(&100u64.to_le_bytes());
    reseal(&mut pages[last]);
    assert_eq!(
        PcmSource::open(&mut &pages.concat()[..], &LIMITS).err(),
        Some(DecodeError::OggGranuleMismatch)
    );
}

/// A file cut part way through a page plays to the cut and says so.
#[test]
fn a_file_cut_part_way_through_a_page_plays_to_the_cut() {
    let spec = floor1_stereo();
    let audio = packets(&spec, &[1, 1, 0, 1, 1, 1, 1, 0, 1], &[], 71);
    let whole = ogg_stream(&spec, &audio, 6, 0, 0);
    let cut = &whole[..whole.len() - 20];
    let (_, samples, end) = decode(cut);
    assert_eq!(end, Err(DecodeError::OggPageTruncated));
    let all = plain(&spec, &audio);
    assert!(!samples.is_empty() && samples.len() < all.len());
    assert_eq!(samples, all[..samples.len()]);
}
