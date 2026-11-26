//! Squash, held to what RISC OS 5.30's Squash module was recorded doing:
//! the streams both of its compressors wrote, every call of the traces
//! recorded across it, its edge cases, and its file's header. Inputs are
//! regenerated here; genuine streams are pinned by length and CRC-32.

extern crate std;

use std::vec;
use std::vec::Vec;

use super::*;

/// The probes' text input.
const TEXT: &[u8] = include_bytes!("../tests/fixtures/squash/text.txt");

/// The recorded traces, laid out as their header says.
const TRACES: &str = include_str!("../tests/fixtures/squash/traces.txt");

/// The probes' generator of bytes: a linear congruence's bits 16 to 23.
fn lcg(len: usize, seed: u32) -> Vec<u8> {
    let mut state = seed;
    (0..len)
        .map(|_| {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            state.to_le_bytes()[2]
        })
        .collect()
}

/// The probes' words, chosen by the same congruence.
fn words(len: usize, seed: u32) -> Vec<u8> {
    const WORDS: [&[u8]; 8] = [
        b"squash ", b"risc ", b"os ", b"module ", b"group ", b"code ", b"table ", b"clear\n",
    ];
    let mut state = seed;
    let mut out = Vec::new();
    while out.len() < len {
        state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        out.extend_from_slice(WORDS[usize::from(state.to_le_bytes()[2] & 7)]);
    }
    out.truncate(len);
    out
}

/// Words, noise, then other words: a stream whose table fills and clears in
/// both compressors.
fn mixed() -> Vec<u8> {
    let mut plain = words(60_000, 2);
    plain.extend(lcg(30_000, 1));
    plain.extend(words(60_000, 5));
    plain
}

fn status(progress: &Progress) -> u32 {
    match progress.status {
        Status::Done => 0,
        Status::Input => 1,
        Status::Output => 2,
    }
}

/// `plain` compressed as `clearing` says, in calls of `piece` bytes of input
/// with room for anything, as the module's caller would, with the places its
/// table was cleared.
fn compressed_placed(plain: &[u8], clearing: Clearing, piece: usize) -> (Vec<u8>, Vec<Place>) {
    let mut work = vec![0u8; COMPRESS_WORK];
    start_compression(&mut work, clearing).unwrap();
    let mut out = vec![0u8; 2 * plain.len() + 64];
    let (mut taken, mut made, mut places) = (0, 0, Vec::new());
    loop {
        let give = piece.min(plain.len() - taken);
        let more = taken + give < plain.len();
        let (plain_at, packed_at) = (taken, made);
        let progress = compress(
            &mut work,
            &plain[taken..taken + give],
            more,
            &mut out[made..],
            &mut |place| {
                places.push(Place {
                    plain: plain_at + place.plain,
                    packed: packed_at + place.packed,
                });
            },
        )
        .unwrap();
        taken += progress.consumed;
        made += progress.produced;
        if progress.status == Status::Done {
            out.truncate(made);
            return (out, places);
        }
    }
}

fn compressed(plain: &[u8], clearing: Clearing, piece: usize) -> Vec<u8> {
    compressed_placed(plain, clearing, piece).0
}

/// `stream` decompressed in one call.
fn decompressed(stream: &[u8], most: usize) -> Result<(Progress, Vec<u8>), Error> {
    let mut work = vec![0u8; DECOMPRESS_WORK];
    start_decompression(&mut work)?;
    let mut out = vec![0u8; most];
    let progress = decompress(&mut work, stream, false, &mut out, &mut |_| {})?;
    out.truncate(progress.produced);
    Ok((progress, out))
}

fn length_and_crc(bytes: &[u8]) -> (usize, u32) {
    (bytes.len(), tairix_crc32::checksum(bytes))
}

#[test]
fn both_compressors_write_the_streams_squash_wrote() {
    let noise = lcg(30_000, 1);
    let mixed = mixed();
    let noise70 = lcg(70_000, 11);
    let whole = |plain: &[u8]| compressed(plain, Clearing::AtOnce, plain.len());
    let pieces = |plain: &[u8]| compressed(plain, Clearing::Periodically, 1000);
    let cases: [(&str, Vec<u8>, (usize, u32)); 10] = [
        ("text", whole(TEXT), (834, 0x8E5F_1353)),
        ("zeros", whole(&vec![0; 100_000]), (530, 0xBA53_68AA)),
        ("noise", whole(&noise), (41_028, 0xDEE9_F949)),
        ("noise in pieces", pieces(&noise), (42_403, 0x4596_A44D)),
        ("words", whole(&words(60_000, 2)), (9_777, 0x49C7_1256)),
        ("mixed", whole(&mixed), (61_482, 0xF475_F86F)),
        ("mixed in pieces", pieces(&mixed), (74_709, 0xA6AB_0EF8)),
        ("noise70", whole(&noise70), (95_772, 0x8219_B775)),
        ("noise70 in pieces", pieces(&noise70), (98_765, 0x5E83_DD46)),
        (
            "one byte",
            whole(b"x"),
            (5, tairix_crc32::checksum(&[0x1F, 0x9D, 0x8C, b'x', 0])),
        ),
    ];
    for (name, stream, genuine) in cases {
        assert_eq!(length_and_crc(&stream), genuine, "{name}");
    }
    // The pieces a stream is given in change nothing; the clearing does.
    assert_eq!(
        compressed(&mixed, Clearing::Periodically, 7_919),
        pieces(&mixed)
    );
    for plain in [&noise, &mixed, &noise70] {
        for clearing in [Clearing::AtOnce, Clearing::Periodically] {
            let stream = compressed(plain, clearing, 4_096);
            assert_eq!(decompressed(&stream, plain.len()).unwrap().1, *plain);
        }
    }
}

/// One recorded trace: what its calls were given, and what each did.
struct Trace<'a> {
    label: &'a str,
    input: Vec<u8>,
    compressing: bool,
    piece: usize,
    room: usize,
    pieces: bool,
    calls: Vec<(u32, usize, usize)>,
}

fn traces() -> Vec<Trace<'static>> {
    let mut lines = TRACES.lines().filter(|line| !line.starts_with('#'));
    let mut traces = Vec::new();
    while let (Some(head), Some(calls)) = (lines.next(), lines.next()) {
        let fields: Vec<&str> = head.split(' ').collect();
        let input = match fields[1] {
            "text" => TEXT.to_vec(),
            "packed-text" => compressed(TEXT, Clearing::AtOnce, TEXT.len()),
            "noise" => lcg(30_000, 1),
            "packed-noise" => compressed(&lcg(30_000, 1), Clearing::AtOnce, 30_000),
            "packed-words" => compressed(&words(60_000, 2), Clearing::AtOnce, 60_000),
            other => panic!("no input {other}"),
        };
        let calls = calls
            .split(' ')
            .map(|call| {
                let mut parts = call.split('/').map(|part| part.parse::<usize>().unwrap());
                let (status, took, made) = (
                    parts.next().unwrap(),
                    parts.next().unwrap(),
                    parts.next().unwrap(),
                );
                (u32::try_from(status).unwrap(), took, made)
            })
            .collect();
        traces.push(Trace {
            label: fields[0],
            input,
            compressing: fields[2] == "compress",
            piece: fields[3].parse().unwrap(),
            room: fields[4].parse().unwrap(),
            pieces: fields[5] == "pieces",
            calls,
        });
    }
    traces
}

#[test]
fn every_call_of_the_recorded_traces_takes_and_makes_what_squash_s_did() {
    let traces = traces();
    assert_eq!(traces.len(), 13);
    let mut calls = 0;
    for trace in &traces {
        let mut work = vec![0u8; COMPRESS_WORK];
        let mut taken = 0;
        for (at, &genuine) in trace.calls.iter().enumerate() {
            let left = trace.input.len() - taken;
            let (give, more) = if trace.pieces && left > trace.piece {
                (trace.piece, true)
            } else {
                (left, false)
            };
            let input = &trace.input[taken..taken + give];
            let mut out = vec![0u8; trace.room];
            if at == 0 {
                if trace.compressing {
                    let clearing = Clearing::for_first_call(word(give), word(trace.room), more);
                    start_compression(&mut work, clearing).unwrap();
                } else {
                    start_decompression(&mut work).unwrap();
                }
            }
            let progress = if trace.compressing {
                compress(&mut work, input, more, &mut out, &mut |_| {})
            } else {
                decompress(&mut work, input, more, &mut out, &mut |_| {})
            }
            .unwrap();
            assert_eq!(
                (status(&progress), progress.consumed, progress.produced),
                genuine,
                "{} call {at}",
                trace.label
            );
            taken += progress.consumed;
            calls += 1;
        }
    }
    assert_eq!(calls, 1_307);
}

fn word(len: usize) -> u32 {
    u32::try_from(len).unwrap()
}

#[test]
fn the_module_s_reckonings_of_room_are_squash_s() {
    for (input, answer) in [
        (0, 12),
        (1, 13),
        (12, 30),
        (100, 162),
        (1000, 1512),
        (4096, 6156),
        (65_536, 98_316),
        (1_000_000, 1_500_012),
        (0x8000_0000, 0x4000_000C),
        (0xAAAA_AAAA, 0x8000_000B),
        (0xFFFF_FFFE, 0x8000_0009),
        (0x5555_5555, 0x8000_000B),
    ] {
        assert_eq!(max_output(input), answer, "{input:#x}");
    }
    // The first room each input was found compressed at once with.
    for (input, from) in [
        (8000, 12_003),
        (30_000, 45_003),
        (40_000, 60_003),
        (70_000, 105_003),
    ] {
        assert_eq!(
            Clearing::for_first_call(input, from, false),
            Clearing::AtOnce
        );
        assert_eq!(
            Clearing::for_first_call(input, from - 1, false),
            Clearing::Periodically
        );
        assert_eq!(
            Clearing::for_first_call(input, u32::MAX, true),
            Clearing::Periodically
        );
    }
}

#[test]
fn the_edge_cases_squash_was_recorded_answering_are_answered_alike() {
    const TEN: [u8; 15] = [
        0x1F, 0x9D, 0x8C, 0, 0x4A, 0x28, 0x79, 0x43, 0x29, 0x97, 0xB7, 1, 0x28, 0x9A, 0,
    ];
    let mut work = vec![0u8; COMPRESS_WORK];
    let mut call = |fresh: bool, input: &[u8], more: bool, room: usize| {
        if fresh {
            let clearing = Clearing::for_first_call(word(input.len()), word(room), more);
            start_compression(&mut work, clearing).unwrap();
        }
        let mut out = vec![0u8; room];
        let progress = compress(&mut work, input, more, &mut out, &mut |_| {}).unwrap();
        out.truncate(progress.produced);
        (status(&progress), progress.consumed, out)
    };
    let ramp: Vec<u8> = (0u8..100).map(|at| at.wrapping_mul(37)).collect();
    // The header waits for the first byte; the last code comes at the end.
    assert_eq!(
        call(true, &ramp[..1], true, 1000),
        (1, 1, vec![0x1F, 0x9D, 0x8C])
    );
    assert_eq!(call(false, &[], false, 1000), (0, 0, vec![0, 0]));
    assert_eq!(
        call(true, &ramp[..1], false, 12),
        (0, 1, vec![0x1F, 0x9D, 0x8C, 0, 0])
    );
    assert_eq!(call(true, &ramp[..10], false, 26), (0, 10, TEN.to_vec()));
    assert_eq!(call(true, &ramp[..10], false, 1000), (0, 10, TEN.to_vec()));
    // No room for the header, or for the first group.
    assert_eq!(call(true, &ramp[..10], false, 2), (2, 0, vec![]));
    assert_eq!(call(true, &ramp[..10], false, 11).0, 2);
    let (status_11, took_11, made_11) = call(true, &ramp[..10], false, 11);
    assert_eq!((status_11, took_11, made_11.len()), (2, 9, 3));
    let (status_13, took_13, made_13) = call(true, &ramp[..10], true, 13);
    assert_eq!((status_13, took_13, made_13.len()), (1, 10, 12));
    // Nothing given: nothing written while more is to come; at the end, the
    // header alone, read back as nothing, where Squash's own call aborts.
    assert_eq!(call(true, &[], true, 1000), (1, 0, vec![]));
    assert_eq!(call(true, &[], false, 1000), (0, 0, STREAM_MAGIC.to_vec()));
    assert_eq!(call(true, &[], false, 2), (2, 0, vec![]));
    assert_eq!(decompressed(&STREAM_MAGIC, 16).unwrap().1, b"");
    let (_, took_40, made_40) = call(true, &ramp[..40], false, 1000);
    assert_eq!((took_40, made_40.len()), (40, 48));

    // Only the header's third byte is held to Squash's.
    let stream = compressed(&ramp[..40], Clearing::AtOnce, 40);
    for third in [
        0x80u8, 0x89, 0x8A, 0x8B, 0x8D, 0x8F, 0x90, 0xAC, 0xCC, 0x9C, 0x0C, 0x4C,
    ] {
        let mut bad = stream.clone();
        bad[2] = third;
        assert_eq!(
            decompressed(&bad, 1000).err(),
            Some(Error::Input),
            "{third:x}"
        );
    }
    let mut magic = stream.clone();
    magic[0] = 0x1E;
    magic[1] = 0x9E;
    assert_eq!(decompressed(&magic, 1000).unwrap().1, &ramp[..40]);
    // Short of the header: at the end, refused; with more to come, waited for.
    assert_eq!(decompressed(&stream[..2], 1000).err(), Some(Error::Input));
    assert_eq!(decompressed(&[], 1000).err(), Some(Error::Input));
    let mut work = vec![0u8; DECOMPRESS_WORK];
    start_decompression(&mut work).unwrap();
    let mut out = [0u8; 100];
    let progress = decompress(&mut work, &stream[..3], true, &mut out, &mut |_| {}).unwrap();
    assert_eq!((progress.status, progress.consumed), (Status::Input, 3));
    start_decompression(&mut work).unwrap();
    let progress = decompress(&mut work, &[], true, &mut out, &mut |_| {}).unwrap();
    assert_eq!((progress.status, progress.consumed), (Status::Input, 0));
    // A first code alone, one bit of it missing; a second code; a stream cut
    // short of its last byte.
    let (progress, made) = decompressed(&[0x1F, 0x9D, 0x8C, 0x41], 1000).unwrap();
    assert_eq!((progress.consumed, made), (4, vec![0x41]));
    let (progress, made) = decompressed(&[0x1F, 0x9D, 0x8C, 0x41, 0], 1000).unwrap();
    assert_eq!((progress.consumed, made), (5, vec![0x41]));
    let (progress, made) = decompressed(&[0x1F, 0x9D, 0x8C, 1, 2, 0], 1000).unwrap();
    assert_eq!((progress.consumed, made), (6, vec![1, 1]));
    let packed = compressed(TEXT, Clearing::AtOnce, TEXT.len());
    let (progress, made) = decompressed(&packed[..packed.len() - 1], 10_000).unwrap();
    assert_eq!(
        (progress.status, progress.consumed, made.len()),
        (Status::Done, 833, 2_993)
    );
    assert_eq!(made, TEXT[..2_993]);
}

#[test]
fn a_workspace_holding_no_stream_of_its_kind_is_refused() {
    let mut work = vec![0u8; COMPRESS_WORK];
    let mut out = [0u8; 64];
    let ignore = &mut |_: Place| {};
    assert_eq!(
        compress(&mut work, b"abc", false, &mut out, ignore).err(),
        Some(Error::Workspace)
    );
    assert_eq!(
        decompress(&mut work, b"abc", false, &mut out, ignore).err(),
        Some(Error::Workspace)
    );
    start_compression(&mut work, Clearing::AtOnce).unwrap();
    assert_eq!(
        decompress(&mut work, &STREAM_MAGIC, false, &mut out, ignore).err(),
        Some(Error::Workspace)
    );
    let mut short = vec![0u8; DECOMPRESS_WORK - 1];
    assert_eq!(
        start_decompression(&mut short).err(),
        Some(Error::Workspace)
    );
    assert_eq!(
        start_compression(&mut vec![0u8; COMPRESS_WORK - 1], Clearing::AtOnce).err(),
        Some(Error::Workspace)
    );
    // A mark a stream left, its flag made one no call writes.
    start_compression(&mut work, Clearing::AtOnce).unwrap();
    work[4] = 2;
    assert_eq!(
        compress(&mut work, b"abc", false, &mut out, ignore).err(),
        Some(Error::Workspace)
    );
}

#[test]
fn a_stream_resumes_at_every_place_its_table_was_cleared() {
    let plain = mixed();
    for (clearing, piece) in [
        (Clearing::AtOnce, plain.len()),
        (Clearing::Periodically, 1000),
    ] {
        let (stream, places) = compressed_placed(&plain, clearing, piece);
        assert!(places.len() >= 3, "{clearing:?}");
        // Read back in calls short of room, every place told as it is met.
        let mut work = vec![0u8; DECOMPRESS_WORK];
        start_decompression(&mut work).unwrap();
        let (mut taken, mut back, mut read_places) = (0, Vec::new(), Vec::new());
        loop {
            let mut out = [0u8; 4093];
            let (plain_at, packed_at) = (back.len(), taken);
            let progress = decompress(&mut work, &stream[taken..], false, &mut out, &mut |place| {
                read_places.push(Place {
                    plain: plain_at + place.plain,
                    packed: packed_at + place.packed,
                });
            })
            .unwrap();
            taken += progress.consumed;
            back.extend_from_slice(&out[..progress.produced]);
            if progress.status == Status::Done {
                break;
            }
        }
        assert_eq!(back, plain);
        assert_eq!(read_places, places, "{clearing:?}");
        for place in places {
            start_decompression_at_place(&mut work).unwrap();
            let mut out = vec![0u8; plain.len()];
            let progress = decompress(
                &mut work,
                &stream[place.packed..],
                false,
                &mut out,
                &mut |_| {},
            )
            .unwrap();
            assert_eq!(
                out[..progress.produced],
                plain[place.plain..],
                "{clearing:?}"
            );
        }
    }
}

#[test]
fn a_squash_file_s_header_reads_and_writes_as_the_prm_lays_it_out() {
    let header = FileHeader {
        length: 0x0102_0304,
        load: 0xFFFF_FD3A,
        exec: 0x1122_3344,
    };
    let bytes = header.to_bytes();
    assert_eq!(&bytes[..4], b"SQSH");
    assert_eq!(bytes[4..8], [4, 3, 2, 1]);
    assert_eq!(bytes[16..], [0; 4]);
    assert_eq!(FileHeader::parse(&bytes), Some(header));
    let mut reserved = bytes;
    reserved[16] = 9;
    assert_eq!(FileHeader::parse(&reserved), Some(header));
    assert_eq!(FileHeader::parse(&bytes[..19]), None);
    let mut other = bytes;
    other[0] = b's';
    assert_eq!(FileHeader::parse(&other), None);
}
