//! compress's LZW: a stream coded in any split of calls, its state saved and
//! restored between each, is the stream coded at once; restored state and
//! tables a coder did not leave are refused, never trusted; a reader resumes
//! at any place a clear makes; and the policies and dialects differ exactly
//! where compress's implementations do.

extern crate std;

use std::vec;
use std::vec::Vec;

use tairix_fuzzseed::Prng;

use super::*;

/// Every clearing, with the dialect its streams are read in.
const CODINGS: [(Clearing, Dialect); 3] = [
    (Clearing::Worsening, Dialect::Compress),
    (Clearing::AtLast, Dialect::Squash),
    (Clearing::Periodic, Dialect::Squash),
];

/// Bytes with no structure to find: a fixed generator's.
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut noise = vec![0; len];
    Prng::new(seed).fill(&mut noise);
    noise
}

/// Text of a few words, compressible as text is.
fn prose(len: usize, seed: u64) -> Vec<u8> {
    const WORDS: [&[u8]; 5] = [b"squash ", b"group ", b"code ", b"table ", b"clear\n"];
    let mut rng = Prng::new(seed);
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        out.extend_from_slice(WORDS[rng.below(WORDS.len())]);
    }
    out.truncate(len);
    out
}

fn mixed() -> Vec<u8> {
    let mut plain = prose(40_000, 3);
    plain.extend(noise(30_000, 5));
    plain.extend(prose(20_000, 7));
    plain
}

fn encoder_space(bits: u8, len: usize) -> Vec<u8> {
    vec![0u8; encoder_work(bits, len as u64)]
}

/// `plain` coded in one call into `work`, with the places its table was
/// cleared.
fn encode_in(clearing: Clearing, bits: u8, work: &mut [u8], plain: &[u8]) -> (Vec<u8>, Vec<Place>) {
    let mut coding = Encoding::new(clearing, bits, work).unwrap();
    let mut out = vec![0u8; 2 * plain.len() + 64];
    let mut places = Vec::new();
    let progress = coding
        .encode(work, plain, false, &mut out, &mut |place| {
            places.push(place);
        })
        .unwrap();
    assert_eq!(progress.status, Status::Done);
    assert_eq!(progress.consumed, plain.len());
    out.truncate(progress.produced);
    (out, places)
}

/// `plain` coded in one call.
fn encode(clearing: Clearing, bits: u8, plain: &[u8]) -> Vec<u8> {
    encode_in(clearing, bits, &mut encoder_space(bits, plain.len()), plain).0
}

/// `stream` decoded in one call, with the places its table was cleared.
fn decode_placed(
    dialect: Dialect,
    bits: u8,
    stream: &[u8],
    most: usize,
) -> Result<(Vec<u8>, Vec<Place>), Error> {
    let mut work = vec![0u8; decoder_work(bits)];
    let mut coding = Decoding::new(dialect, bits).unwrap();
    let mut out = vec![0u8; most];
    let mut places = Vec::new();
    let progress = coding.decode(&mut work, stream, false, &mut out, &mut |place| {
        places.push(place);
    })?;
    out.truncate(progress.produced);
    Ok((out, places))
}

/// `stream` decoded in one call.
fn decode(dialect: Dialect, bits: u8, stream: &[u8], most: usize) -> Result<Vec<u8>, Error> {
    Ok(decode_placed(dialect, bits, stream, most)?.0)
}

/// A generator of call sizes.
struct Sizes(u64);

impl Sizes {
    fn next(&mut self, most: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        usize::try_from(self.0 % most as u64).unwrap()
    }
}

#[test]
fn a_stream_coded_a_call_at_a_time_through_saved_state_is_the_stream_coded_at_once() {
    let plain = mixed();
    for (clearing, dialect) in CODINGS {
        for bits in [9, 12, 16] {
            let (whole, whole_places) = encode_in(
                clearing,
                bits,
                &mut encoder_space(bits, plain.len()),
                &plain,
            );
            let (back, back_places) = decode_placed(dialect, bits, &whole, plain.len()).unwrap();
            assert_eq!(back, plain);
            assert_eq!(back_places, whole_places, "{clearing:?} {bits}");
            let mut sizes = Sizes(u64::from(bits) * 7919);
            // Encode in calls of every size, the state carried in bytes, so
            // groups are held for want of room and places fall in every call.
            let mut work = encoder_space(bits, plain.len());
            let mut saved = [0u8; ENCODING_STATE];
            Encoding::new(clearing, bits, &mut work)
                .unwrap()
                .save(&mut saved);
            let (mut taken, mut stream, mut places) = (0, Vec::new(), Vec::new());
            loop {
                let give = sizes.next(300).min(plain.len() - taken);
                let more = taken + give < plain.len();
                let mut out = vec![0u8; sizes.next(40)];
                let mut coding = Encoding::restore(&saved, bits, work.len()).unwrap();
                let (plain_at, packed_at) = (taken, stream.len());
                let progress = coding
                    .encode(
                        &mut work,
                        &plain[taken..taken + give],
                        more,
                        &mut out,
                        &mut |place| {
                            places.push(Place {
                                plain: plain_at + place.plain,
                                packed: packed_at + place.packed,
                            });
                        },
                    )
                    .unwrap();
                coding.save(&mut saved);
                taken += progress.consumed;
                stream.extend_from_slice(&out[..progress.produced]);
                if progress.status == Status::Done {
                    break;
                }
            }
            assert_eq!(stream, whole, "{clearing:?} {bits}");
            assert_eq!(places, whole_places, "{clearing:?} {bits}");
            // Decode it likewise.
            let mut work = vec![0u8; decoder_work(bits)];
            let mut saved = [0u8; DECODING_STATE];
            Decoding::new(dialect, bits).unwrap().save(&mut saved);
            let (mut taken, mut back, mut places) = (0, Vec::new(), Vec::new());
            loop {
                let give = sizes.next(50).min(stream.len() - taken);
                let more = taken + give < stream.len();
                let mut out = vec![0u8; sizes.next(700)];
                let mut coding = Decoding::restore(&saved, dialect, bits).unwrap();
                let (plain_at, packed_at) = (back.len(), taken);
                let progress = coding
                    .decode(
                        &mut work,
                        &stream[taken..taken + give],
                        more,
                        &mut out,
                        &mut |place| {
                            places.push(Place {
                                plain: plain_at + place.plain,
                                packed: packed_at + place.packed,
                            });
                        },
                    )
                    .unwrap();
                coding.save(&mut saved);
                taken += progress.consumed;
                back.extend_from_slice(&out[..progress.produced]);
                if progress.status == Status::Done {
                    break;
                }
            }
            assert_eq!(back, plain, "{dialect:?} {bits}");
            assert_eq!(places, whole_places, "{dialect:?} {bits}");
        }
    }
}

#[test]
fn a_decoder_takes_only_whole_groups_while_more_follows() {
    let stream = encode(Clearing::AtLast, 12, &prose(500, 1));
    let mut work = vec![0u8; decoder_work(12)];
    let mut coding = Decoding::new(Dialect::Squash, 12).unwrap();
    let mut out = [0u8; 1000];
    // Eight nine-bit codes are nine bytes: eight are not a group.
    let progress = coding
        .decode(&mut work, &stream[..8], true, &mut out, &mut |_| {})
        .unwrap();
    assert_eq!((progress.status, progress.consumed), (Status::Input, 0));
    let progress = coding
        .decode(&mut work, &stream[..9], true, &mut out, &mut |_| {})
        .unwrap();
    assert_eq!((progress.status, progress.consumed), (Status::Input, 9));
    assert!(progress.produced > 0);
}

#[test]
fn an_encoder_holds_a_finished_group_its_output_has_no_room_for() {
    let plain = noise(40, 2);
    let mut work = encoder_space(9, plain.len());
    let mut coding = Encoding::new(Clearing::Worsening, 9, &mut work).unwrap();
    // Eight codes are made once nine bytes are taken; a group is nine bytes.
    let mut out = [0u8; 8];
    let progress = coding
        .encode(&mut work, &plain, true, &mut out, &mut |_| {})
        .unwrap();
    assert_eq!(
        (progress.status, progress.consumed, progress.produced),
        (Status::Output, 9, 0)
    );
    let mut out = [0u8; 9];
    let progress = coding
        .encode(&mut work, &plain[9..], true, &mut out, &mut |_| {})
        .unwrap();
    assert_eq!(progress.produced, 9);
    assert_eq!(&out[..], &encode(Clearing::Worsening, 9, &plain)[..9]);
}

/// The input positions at which a stream of `plain`, coded as `clearing`
/// says, clears its table.
fn clears(clearing: Clearing, bits: u8, plain: &[u8]) -> Vec<usize> {
    let (_, places) = encode_in(clearing, bits, &mut encoder_space(bits, plain.len()), plain);
    places.iter().map(|place| place.plain).collect()
}

#[test]
fn each_clearing_clears_a_full_table_when_its_implementation_does() {
    // Noise fills a nine-bit table at once and never pays.
    let plain = noise(100_000, 9);
    // At its last code: 257 to 510 assigned, one entry a code after the
    // first, so a clear follows each 255th code.
    let at_last = clears(Clearing::AtLast, 9, &plain);
    assert!(at_last.len() > 100);
    // compress's first check is at 10000 bytes, each next 10000 on, and it
    // clears only a ratio that fell.
    let worsening = clears(Clearing::Worsening, 9, &plain);
    assert!(worsening.first().is_some_and(|&first| first >= 10_000));
    assert!(worsening.windows(2).all(|pair| pair[1] - pair[0] >= 10_000));
    // Squash's restartable path clears at every check: the first once 32768
    // bytes are taken, so the stream resumes with the 32768th, each next
    // 32768 bytes on.
    let periodic = clears(Clearing::Periodic, 9, &plain);
    assert_eq!(periodic.len(), 3);
    assert!((32_767..32_767 + 16).contains(&periodic[0]));
    assert!(periodic
        .windows(2)
        .all(|pair| (32_768..32_768 + 16).contains(&(pair[1] - pair[0]))));
    // compress and the restartable path assign the last code, their table
    // full and kept short of their first check; Squash's whole call clears
    // in its place.
    for (clearing, last) in [
        (Clearing::Worsening, 512),
        (Clearing::Periodic, 512),
        (Clearing::AtLast, 511),
    ] {
        let mut work = encoder_space(9, plain.len());
        let mut coding = Encoding::new(clearing, 9, &mut work).unwrap();
        let mut out = vec![0u8; 8000];
        coding
            .encode(&mut work, &plain[..600], true, &mut out, &mut |_| {})
            .unwrap();
        if clearing == Clearing::AtLast {
            assert!(coding.free <= last, "{clearing:?}");
        } else {
            assert_eq!(coding.free, last, "{clearing:?}");
        }
        assert_eq!(coding.clearing.limit(9), last);
    }
}

#[test]
fn squash_reads_a_final_code_one_bit_short_and_compress_does_not() {
    // A first code names a byte: nine bits, the ninth zero, so the byte alone
    // holds all of it but that bit.
    let stream = encode(Clearing::Worsening, 12, b"ab");
    assert_eq!(stream.len(), 3);
    assert_eq!(decode(Dialect::Squash, 12, &stream[..1], 8).unwrap(), b"a");
    assert_eq!(decode(Dialect::Compress, 12, &stream[..1], 8).unwrap(), b"");
    // Two bits short is short for both.
    for dialect in [Dialect::Compress, Dialect::Squash] {
        assert_eq!(decode(dialect, 12, &stream[..2], 8).unwrap(), b"a");
        assert_eq!(decode(dialect, 12, &stream, 8).unwrap(), b"ab");
    }
}

#[test]
fn a_reader_resumes_at_every_place_a_clear_makes() {
    let plain = mixed();
    for (clearing, dialect) in CODINGS {
        // A small dictionary clears often, whatever the clearing.
        let (stream, places) = encode_in(clearing, 12, &mut vec![0u8; encoder_work(12, 0)], &plain);
        assert!(places.len() > 10, "{clearing:?} cleared {}", places.len());
        let (back, read_places) = decode_placed(dialect, 12, &stream, plain.len()).unwrap();
        assert_eq!(back, plain);
        assert_eq!(read_places, places, "{clearing:?}");
        // A fresh decoder at each place decodes the rest.
        for place in &places {
            let rest = decode(dialect, 12, &stream[place.packed..], plain.len()).unwrap();
            assert_eq!(rest, plain[place.plain..], "{clearing:?} at {place:?}");
        }
    }
}

#[test]
fn restored_state_no_coder_left_is_refused() {
    let plain = prose(5000, 11);
    let stream = encode(Clearing::AtLast, 12, &plain);
    // A decoder part way through, its group half read.
    let mut work = vec![0u8; decoder_work(12)];
    let mut coding = Decoding::new(Dialect::Squash, 12).unwrap();
    let mut out = [0u8; 1001];
    coding
        .decode(&mut work, &stream[..400], true, &mut out, &mut |_| {})
        .unwrap();
    let mut good = [0u8; DECODING_STATE];
    coding.save(&mut good);
    assert!(Decoding::restore(&good, Dialect::Squash, 12).is_ok());
    let field = |at: usize, value: &[u8]| {
        let mut bad = good;
        bad[at..at + value.len()].copy_from_slice(value);
        Decoding::restore(&bad, Dialect::Squash, 12).err()
    };
    let refused = Some(Error::Workspace);
    // Width, read bits past the bits held, bits past a group, a free code
    // past the table or below its first, a last code not yet made, a string
    // longer than the stack.
    assert_eq!(field(0, &[8]), refused);
    assert_eq!(field(0, &[13]), refused);
    assert_eq!(field(1, &[200]), refused);
    assert_eq!(field(2, &[130]), refused);
    assert_eq!(field(4, &4097u32.to_le_bytes()), refused);
    assert_eq!(field(4, &256u32.to_le_bytes()), refused);
    assert_eq!(field(8, &4096u32.to_le_bytes()), refused);
    assert_eq!(field(12, &4097u32.to_le_bytes()), refused);
    assert_eq!(Decoding::restore(&good, Dialect::Squash, 17).err(), refused);

    let mut work = encoder_space(12, plain.len());
    let mut coding = Encoding::new(Clearing::AtLast, 12, &mut work).unwrap();
    let mut out = [0u8; 300];
    coding
        .encode(&mut work, &plain, true, &mut out, &mut |_| {})
        .unwrap();
    let mut good = [0u8; ENCODING_STATE];
    coding.save(&mut good);
    let len = work.len();
    assert!(Encoding::restore(&good, 12, len).is_ok());
    let field = |at: usize, value: &[u8]| {
        let mut bad = good;
        bad[at..at + value.len()].copy_from_slice(value);
        Encoding::restore(&bad, 12, len).err()
    };
    // A clearing no encoder has, width, a full group, three held, a flag,
    // slots the workspace does not hold, a free code past the full table,
    // the clear as a string.
    assert_eq!(field(0, &[3]), refused);
    assert_eq!(field(1, &[8]), refused);
    assert_eq!(field(2, &[8]), refused);
    assert_eq!(field(3, &[3]), refused);
    assert_eq!(field(4, &[2]), refused);
    assert_eq!(field(5, &[10]), refused);
    assert_eq!(field(6, &4095u32.to_le_bytes()), refused);
    assert_eq!(field(10, &256u32.to_le_bytes()), refused);
    assert_eq!(Encoding::restore(&good, 12, 4096).err(), refused);
    // Input and bits no stream reaches, which coding on would overflow, and a
    // check further off than its gap.
    let counters = ENCODING_STATE - 4 * 8;
    assert_eq!(field(counters, &u64::MAX.to_le_bytes()), refused);
    assert_eq!(field(counters + 8, &u64::MAX.to_le_bytes()), refused);
    assert_eq!(field(counters + 16, &u64::MAX.to_le_bytes()), refused);
}

#[test]
fn tables_a_coder_did_not_write_are_refused_never_followed() {
    let plain = prose(20_000, 13);
    let stream = encode(Clearing::AtLast, 12, &plain);
    // A decoder whose table says each code extends a later one.
    let mut work = vec![0u8; decoder_work(12)];
    let mut coding = Decoding::new(Dialect::Squash, 12).unwrap();
    let mut out = vec![0u8; plain.len()];
    // Resumed where the decoder stopped, which takes whole groups alone, so
    // the codes after are the stream's own and the table is all that lies.
    let first = coding
        .decode(
            &mut work,
            &stream[..stream.len() / 2],
            true,
            &mut out,
            &mut |_| {},
        )
        .unwrap();
    for entry in work[..ENTRY * (4096 - FIRST as usize)].chunks_mut(ENTRY) {
        entry[..2].copy_from_slice(&4095u16.to_le_bytes());
    }
    let mut refused = false;
    let mut at = first.consumed;
    while !refused && at < stream.len() {
        match coding.decode(&mut work, &stream[at..], false, &mut out, &mut |_| {}) {
            Err(err) => {
                assert_eq!(err, Error::Workspace);
                refused = true;
            }
            Ok(progress) if progress.status == Status::Done => break,
            Ok(progress) => at += progress.consumed,
        }
    }
    assert!(refused);
    // An encoder whose slots name codes it has not made, or every slot full.
    for fill in [0xFFu8, 0x01] {
        let mut work = encoder_space(12, plain.len());
        let mut coding = Encoding::new(Clearing::AtLast, 12, &mut work).unwrap();
        let mut out = vec![0u8; 2 * plain.len()];
        coding
            .encode(&mut work, &plain[..100], true, &mut out, &mut |_| {})
            .unwrap();
        let (table, slots) = coding.dictionary(&mut work).unwrap();
        slots.fill(fill);
        table.fill(0);
        assert_eq!(
            coding
                .encode(&mut work, &plain[100..], false, &mut out, &mut |_| {})
                .err(),
            Some(Error::Workspace)
        );
    }
}

#[test]
fn a_code_past_the_next_the_table_makes_is_corrupt() {
    // `a`, then code 300 with only 257 to come.
    let mut stream = vec![b'a', 0, 0, 0, 0, 0, 0, 0, 0];
    let code = 300u32 << 9 | u32::from(b'a');
    stream[..3].copy_from_slice(&code.to_le_bytes()[..3]);
    assert_eq!(
        decode(Dialect::Squash, 12, &stream, 16),
        Err(Error::Corrupt)
    );
    // A first code that names no byte.
    assert_eq!(
        decode(Dialect::Compress, 12, &[0xFF, 0x01], 16),
        Err(Error::Corrupt)
    );
}

#[test]
fn widths_out_of_range_make_no_coder_and_too_little_workspace_none_either() {
    for bits in [0, 8, 17, 255] {
        assert!(Decoding::new(Dialect::Compress, bits).is_none());
        assert!(Encoding::new(Clearing::Worsening, bits, &mut vec![0u8; 1 << 20]).is_none());
    }
    assert!(Encoding::new(Clearing::AtLast, 12, &mut [0u8; 100]).is_none());
    let mut coding = Decoding::new(Dialect::Squash, 12).unwrap();
    assert_eq!(
        coding
            .decode(&mut [0u8; 100], &[1, 2], false, &mut [0; 4], &mut |_| {})
            .err(),
        Some(Error::Workspace)
    );
    // A dictionary sized to its input is the largest's, coding alike.
    let plain = prose(3000, 17);
    for (clearing, _) in CODINGS {
        let mut sized = vec![0u8; encoder_work(16, plain.len() as u64)];
        let mut largest = vec![0u8; encoder_work(16, u64::MAX)];
        assert!(sized.len() < largest.len());
        let (a, _) = encode_in(clearing, 16, &mut sized, &plain);
        let (b, _) = encode_in(clearing, 16, &mut largest, &plain);
        assert_eq!(a, b);
    }
}
