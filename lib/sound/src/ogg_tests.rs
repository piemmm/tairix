extern crate std;

use std::vec;
use std::vec::Vec;

use tairix_fuzzseed::Prng;

use super::PageWriter;
use crate::flac_encode::{encode, Options, Params};
use crate::{DecodeError, DecodeLimits, Encoding, PcmSource, SoundFormat, TagKind};

const LIMITS: DecodeLimits = DecodeLimits::new(8, 4096, 64);

const STEREO: Params = Params {
    rate: 44_100,
    channels: 2,
    bits: 16,
};

fn samples(seed: u64, frames: usize) -> Vec<i32> {
    let mut prng = Prng::new(seed);
    let mut value = [0i32; 2];
    let mut out = Vec::with_capacity(frames * 2);
    for _ in 0..frames {
        for slot in &mut value {
            *slot = (*slot + i32::from(prng.next_u8()) - 128).clamp(-30_000, 30_000);
            out.push(*slot);
        }
    }
    out
}

fn pcm(interleaved: &[i32]) -> Vec<u8> {
    interleaved
        .iter()
        .flat_map(|&sample| i16::try_from(sample).expect("16-bit").to_le_bytes())
        .collect()
}

fn ogg(samples: &[i32], serial: u32, comments: bool) -> Vec<u8> {
    let mut writer = encode(
        STEREO,
        samples,
        Options {
            block: 1152,
            ..Options::default()
        },
    )
    .expect("encodes");
    if comments {
        writer.comments("tairix", &["TITLE=Over Ogg", "ARTIST=Someone"]);
        writer.seek_points(10_000, 1);
        writer.padding(10);
    }
    writer.finish_ogg(serial, 4000)
}

fn decode(file: &[u8], block: usize) -> (PcmSource, Vec<u8>, Result<(), DecodeError>) {
    let mut input = file;
    let mut source = PcmSource::open(&mut input, &LIMITS).expect("opens");
    let mut out = vec![0u8; block * 4];
    let mut all = Vec::new();
    let end = loop {
        match source.next_block(&mut input, &mut out) {
            Ok(0) => break Ok(()),
            Ok(written) => all.extend_from_slice(&out[..written * 4]),
            Err(err) => break Err(err),
        }
    };
    (source, all, end)
}

/// The pages `file` holds, each whole.
fn pages(file: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < file.len() {
        let segments = usize::from(file[at + 26]);
        let body: usize = file[at + 27..at + 27 + segments]
            .iter()
            .map(|&len| usize::from(len))
            .sum();
        let len = 27 + segments + body;
        out.push(file[at..at + len].to_vec());
        at += len;
    }
    out
}

#[test]
fn flac_in_ogg_round_trips_with_its_metadata() {
    let samples = samples(1, 30_000);
    for comments in [false, true] {
        let file = ogg(&samples, 0x1234, comments);
        let (source, decoded, end) = decode(&file, 1000);
        assert_eq!(
            (source.info().format, source.info().encoding),
            (SoundFormat::Ogg, Encoding::Flac)
        );
        assert_eq!(source.info().frames, Some(30_000));
        assert_eq!(decoded, pcm(&samples));
        assert_eq!(end, Ok(()), "the digest agrees");
        let titles: Vec<&str> = source
            .metadata()
            .tags
            .iter()
            .filter(|tag| tag.kind == TagKind::Title)
            .map(|tag| tag.value.as_str())
            .collect();
        assert_eq!(titles, if comments { vec!["Over Ogg"] } else { vec![] });
    }
}

/// Another logical stream's pages interleaved with the FLAC stream's are
/// stepped over, its opening page among the FLAC one's at the start.
#[test]
fn a_multiplexed_stream_is_read_past_the_other_streams_pages() {
    let samples = samples(2, 20_000);
    let flac = pages(&ogg(&samples, 7, true));
    let mut other = PageWriter::new(99);
    for at in 0..40u8 {
        other.packet(&[at; 300], u64::from(at));
        other.flush(at == 39);
    }
    let other = pages(&other.into_bytes());
    let mut file = Vec::new();
    file.extend_from_slice(&other[0]);
    let mut others = other[1..].iter();
    for page in &flac {
        file.extend_from_slice(page);
        if let Some(other) = others.next() {
            file.extend_from_slice(other);
        }
    }
    for page in others {
        file.extend_from_slice(page);
    }
    let (_, decoded, end) = decode(&file, 512);
    assert_eq!(decoded, pcm(&samples));
    assert_eq!(end, Ok(()));
}

/// A second stream chained on is refused where the first ends, once every
/// one of the first's samples has been written.
#[test]
fn a_chained_stream_is_refused_where_the_first_ends() {
    let first = samples(3, 5000);
    let mut file = ogg(&first, 1, false);
    file.extend_from_slice(&ogg(&samples(4, 3000), 2, false));
    let (_, decoded, end) = decode(&file, 700);
    assert_eq!(decoded, pcm(&first));
    assert_eq!(end, Err(DecodeError::OggChained));
}

#[test]
fn seeking_bisects_the_pages_onto_the_exact_sample() {
    let samples = samples(5, 400_000);
    let file = ogg(&samples, 77, false);
    let expected = pcm(&samples);
    let mut input = &file[..];
    let mut source = PcmSource::open(&mut input, &LIMITS).expect("opens");
    let mut prng = Prng::new(9);
    let mut out = [0u8; 64];
    for _ in 0..50 {
        let frame = prng.next_u64() % 400_000;
        source.seek(frame).expect("seekable");
        let written = source.next_block(&mut input, &mut out).expect("decodes");
        assert!(written > 0);
        let at = usize::try_from(frame).expect("small") * 4;
        assert_eq!(
            &out[..written * 4],
            &expected[at..at + written * 4],
            "at {frame}"
        );
    }
}

#[test]
fn a_damaged_or_missing_page_is_refused() {
    let samples = samples(6, 20_000);
    let file = ogg(&samples, 3, false);
    let mut damaged = pages(&file);
    let page = damaged.len() - 2;
    let byte = damaged[page].len() / 2;
    damaged[page][byte] ^= 0x40;
    let (_, _, end) = decode(&damaged.concat(), 512);
    assert_eq!(end, Err(DecodeError::OggPageCrc));
    let mut lost = pages(&file);
    lost.remove(page);
    let (_, _, end) = decode(&lost.concat(), 512);
    assert_eq!(end, Err(DecodeError::OggPageLost));
}

#[test]
fn an_ogg_file_holding_no_flac_stream_is_refused_by_name() {
    let mut other = PageWriter::new(5);
    other.packet(b"\x01vorbis", 0);
    other.flush(true);
    assert_eq!(
        PcmSource::open(&mut &other.into_bytes()[..], &LIMITS).map(|_| ()),
        Err(DecodeError::OggNoFlacStream)
    );
}
