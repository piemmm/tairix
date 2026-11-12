use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use tairix_compress::deflate::{Deflate, Flush};

use super::{Tape, TapeError, Tone, CLOCK, CSW_MAGIC, MAX_CYCLES, UEF_MAGIC};

/// Cycles of a bit at 1200 baud, rounded down.
const BIT: u64 = CLOCK / 1200;

fn uef(chunks: &[(u16, &[u8])]) -> Vec<u8> {
    let mut out = UEF_MAGIC.to_vec();
    out.extend_from_slice(&[10, 0]);
    for (id, body) in chunks {
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&u32::try_from(body.len()).unwrap().to_le_bytes());
        out.extend_from_slice(body);
    }
    out
}

/// The tone at the middle of bit `k` of a tape starting with a frame.
fn bit(tape: &Tape, start: u64, k: u64, baud: u64) -> Tone {
    tape.tone_at(start + (2 * k + 1) * CLOCK / (2 * baud))
}

fn byte_at(tape: &Tape, start: u64, baud: u64) -> u8 {
    (0..8).fold(0, |byte, k| {
        byte | (u8::from(bit(tape, start, k + 1, baud) == Tone::High) << k)
    })
}

#[test]
fn a_data_chunk_frames_each_byte_at_1200_baud() {
    let tape = Tape::decode(&uef(&[(0x0100, &[0x41, 0x2A])])).unwrap();
    assert_eq!(bit(&tape, 0, 0, 1200), Tone::Low, "start bit");
    assert_eq!(byte_at(&tape, 0, 1200), 0x41);
    assert_eq!(bit(&tape, 0, 9, 1200), Tone::High, "stop bit");
    assert_eq!(byte_at(&tape, 10 * CLOCK / 1200, 1200), 0x2A);
    // Twenty bits of 1/1200 s each.
    assert_eq!(tape.len(), 20 * CLOCK / 1200);
    assert_eq!(tape.tone_at(tape.len()), Tone::Silence);
}

#[test]
fn carriers_gaps_and_the_baud_chunk_time_the_tape() {
    let tape = Tape::decode(&uef(&[
        (0x0110, &2400u16.to_le_bytes()),
        (0x0112, &1200u16.to_le_bytes()),
        (0x0117, &300u16.to_le_bytes()),
        (0x0100, &[0x00]),
        (0x0116, &0.25f32.to_le_bytes()),
    ]))
    .unwrap();
    // A second of carrier, half a second of silence, a byte at 300 baud, a
    // quarter second of silence: 1.78⅓ s.
    assert_eq!(tape.tone_at(CLOCK - 1), Tone::High);
    assert_eq!(tape.tone_at(CLOCK), Tone::Silence);
    let byte = CLOCK + CLOCK / 2;
    assert_eq!(tape.tone_at(byte), Tone::Low);
    assert_eq!(byte_at(&tape, byte, 300), 0x00);
    assert_eq!(bit(&tape, byte, 9, 300), Tone::High);
    assert_eq!(tape.len(), 3_566_667);
}

#[test]
fn a_carrier_with_its_dummy_byte_and_a_defined_format_frame_as_declared() {
    let tape = Tape::decode(&uef(&[
        (0x0111, &[1, 0, 1, 0]),
        (0x0104, &[7, b'E', 2, 0x03]),
    ]))
    .unwrap();
    let after_carrier = CLOCK / 2400;
    assert_eq!(byte_at(&tape, after_carrier, 1200), 0xAA);
    // 7E2: seven data bits, even parity (0x03 has two ones: parity 0), two
    // stops.
    let frame = after_carrier + 10 * CLOCK / 1200 + CLOCK / 2400;
    let bits: Vec<Tone> = (0..11).map(|k| bit(&tape, frame, k, 1200)).collect();
    let (high, low) = (Tone::High, Tone::Low);
    assert_eq!(
        bits,
        [low, high, high, low, low, low, low, low, low, high, high]
    );
}

#[test]
fn security_cycles_are_whole_cycles_but_a_marked_first_is_half_of_one() {
    // Three cycles, high low high, the first only a pulse.
    let tape = Tape::decode(&uef(&[(0x0114, &[3, 0, 0, b'P', b'W', 0b1010_0000])])).unwrap();
    let half_high = CLOCK / 4800;
    assert_eq!(tape.tone_at(0), Tone::High);
    assert_eq!(tape.tone_at(half_high + 1), Tone::Low);
    assert_eq!(tape.tone_at(half_high + CLOCK / 1200 + 1), Tone::High);
    // 416⅔ + 1666⅔ + 833⅓ cycles.
    assert_eq!(tape.len(), 2917);
}

#[test]
fn explicit_bits_and_markers_are_read_as_declared() {
    let tape = Tape::decode(&uef(&[(0x0120, b"side A\0"), (0x0102, &[5, 0b0000_0110])])).unwrap();
    // Three bits, least significant first: 0, 1, 1.
    assert_eq!(tape.len(), 3 * CLOCK / 1200);
    assert_eq!(
        [0, 1, 2].map(|k| bit(&tape, 0, k, 1200)),
        [Tone::Low, Tone::High, Tone::High]
    );
    assert_eq!(
        Tape::decode(&tape.encode_uef()).unwrap().markers,
        tape.markers,
        "the marker is written back"
    );
}

/// The regression: UEF write-back dropped every marker and moved a tape
/// built on another base frequency onto 1200 Hz.
#[test]
fn markers_and_base_frequencies_survive_a_round_trip() {
    let hertz = 1250.0f32.to_bits().to_le_bytes();
    let standard = 1200.0f32.to_bits().to_le_bytes();
    let image = uef(&[
        (0x0120, b"side A\0"),
        (0x0110, &[0x40, 0x00]),
        (0x0100, b"HELLO"),
        (0x0112, &[0x80, 0x00]),
        (0x0113, &hertz),
        (0x0110, &[0x40, 0x00]),
        (0x0100, b"WORLD"),
        (0x0120, b"end\0"),
        (0x0113, &standard),
        (0x0110, &[0x10, 0x00]),
        (0x0100, b"!"),
    ]);
    let tape = Tape::decode(&image).unwrap();
    assert_eq!(tape.bases.len(), 2, "to 1250 Hz and back");
    let again = Tape::decode(&tape.encode_uef()).unwrap();
    assert_eq!(again, tape);
    assert_eq!(
        again
            .markers
            .iter()
            .map(|m| m.label.as_str())
            .collect::<Vec<_>>(),
        ["side A", "end"]
    );
}

#[test]
fn a_damaged_or_foreign_image_is_refused() {
    assert_eq!(Tape::decode(b"not a tape"), Err(TapeError::Unrecognised));
    let mut short = uef(&[(0x0100, &[1, 2, 3])]);
    short.truncate(short.len() - 1);
    assert_eq!(Tape::decode(&short), Err(TapeError::Truncated));
    assert_eq!(
        Tape::decode(&uef(&[(0x0117, &[100, 0])])),
        Err(TapeError::Malformed)
    );
    assert_eq!(
        Tape::decode(&uef(&[(0x0104, &[7, b'X', 1])])),
        Err(TapeError::Malformed)
    );
    assert_eq!(
        Tape::decode(&uef(&[(0x0113, &f32::NAN.to_le_bytes())])),
        Err(TapeError::Malformed)
    );
    let hour = f32::from(u16::try_from(MAX_CYCLES / CLOCK).unwrap()) + 1.0;
    assert_eq!(
        Tape::decode(&uef(&[(0x0116, &hour.to_le_bytes())])),
        Err(TapeError::Malformed)
    );
}

/// A CSW image of `pulses`, version `major`.
fn csw(major: u8, rate: u32, pulses: &[u8]) -> Vec<u8> {
    let mut out = CSW_MAGIC.to_vec();
    if major == 1 {
        out.extend_from_slice(&[1, 1]);
        out.extend_from_slice(&u16::try_from(rate).unwrap().to_le_bytes());
        out.extend_from_slice(&[1, 0, 0, 0, 0]);
        out.extend_from_slice(pulses);
        return out;
    }
    out.extend_from_slice(&[2, 0]);
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&[2, 0, 0]);
    out.extend_from_slice(&[0; 16]);
    out.extend_from_slice(&zlib(pulses));
    out
}

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut encoder = Box::new(tairix_compress::zlib::Encoder::new());
    let mut out = vec![0; encoder.bound(data.len())];
    let len = encoder
        .compress(data, &mut out, tairix_compress::zlib::Flush::Finish)
        .unwrap();
    out.truncate(len);
    out
}

#[test]
fn a_csw_recording_reads_its_half_cycles_as_tones() {
    // At 44.1 kHz: two high half-cycles of 9 samples, two low of 18, and a
    // long pulse of silence written as a 32-bit length.
    let pulses = [9, 9, 18, 18, 0, 0x10, 0x27, 0, 0, 9];
    for image in [csw(1, 44_100, &pulses), csw(2, 44_100, &pulses)] {
        let tape = Tape::decode(&image).unwrap();
        let at = |samples: u64| samples * CLOCK / 44_100;
        assert_eq!(tape.tone_at(at(10)), Tone::High);
        assert_eq!(tape.tone_at(at(30)), Tone::Low);
        assert_eq!(tape.tone_at(at(100)), Tone::Silence);
        assert_eq!(tape.tone_at(at(54 + 10_000 + 1)), Tone::High);
    }
    assert_eq!(
        Tape::decode(&csw(1, 10, &pulses)),
        Err(TapeError::Malformed)
    );
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1F, 0x8B, 8, 0, 0, 0, 0, 0, 0, 3];
    let mut deflate = Box::new(Deflate::new());
    let mut body = vec![0; deflate.bound(data.len())];
    let len = deflate.deflate(data, &mut body, Flush::Finish).unwrap();
    out.extend_from_slice(&body[..len]);
    out.extend_from_slice(&tairix_crc32::checksum(data).to_le_bytes());
    out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
    out
}

#[test]
fn a_gzipped_image_is_unwrapped_once() {
    let image = uef(&[(0x0100, b"BBC")]);
    assert_eq!(Tape::decode(&gzip(&image)), Tape::decode(&image));
    assert_eq!(
        Tape::decode(&gzip(&gzip(&image))),
        Err(TapeError::Unrecognised)
    );
    let mut damaged = gzip(&image);
    let at = damaged.len() - 6;
    damaged[at] ^= 1;
    assert_eq!(Tape::decode(&damaged), Err(TapeError::Corrupt));
}

#[test]
fn a_tape_of_carrier_and_bytes_encodes_back_as_data_chunks() {
    let original = uef(&[
        (0x0112, &600u16.to_le_bytes()),
        (0x0110, &1000u16.to_le_bytes()),
        (0x0100, b"*HELLO\x00\xFF"),
        (0x0110, &50u16.to_le_bytes()),
        (0x0112, &100u16.to_le_bytes()),
        (0x0117, &300u16.to_le_bytes()),
        (0x0110, &80u16.to_le_bytes()),
        (0x0100, &[0xAA, 0x55, 0x01]),
        (0x0110, &10u16.to_le_bytes()),
    ]);
    let tape = Tape::decode(&original).unwrap();
    let encoded = tape.encode_uef();
    assert_eq!(encoded, original, "the chunks come back as they went in");
    assert_eq!(Tape::decode(&encoded).unwrap(), tape);
}

#[test]
fn a_stretch_of_bytes_at_two_rates_encodes_as_cycles_rather_than_misread() {
    let original = uef(&[
        (0x0110, &100u16.to_le_bytes()),
        (0x0100, b"*A"),
        (0x0117, &300u16.to_le_bytes()),
        (0x0100, &[0x93]),
    ]);
    let tape = Tape::decode(&original).unwrap();
    let back = Tape::decode(&tape.encode_uef()).unwrap();
    for at in (0..tape.len()).step_by(53) {
        assert_eq!(back.tone_at(at), tape.tone_at(at), "at {at}");
    }
}

#[test]
fn tone_that_is_not_framed_bytes_encodes_as_cycles() {
    let original = uef(&[(0x0114, &[6, 0, 0, b'W', b'W', 0b1100_1100])]);
    let tape = Tape::decode(&original).unwrap();
    let back = Tape::decode(&tape.encode_uef()).unwrap();
    for at in (0..tape.len()).step_by(97) {
        assert_eq!(back.tone_at(at), tape.tone_at(at), "at {at}");
    }
}

#[test]
fn recording_over_a_tape_replaces_it_from_there_on() {
    let mut tape = Tape::decode(&uef(&[(0x0110, &4800u16.to_le_bytes())])).unwrap();
    let mut recorded = Tape::default();
    recorded.push(Tone::Low, BIT).unwrap();
    recorded.push(Tone::Silence, BIT).unwrap();
    tape.record_over(CLOCK, &recorded).unwrap();
    assert_eq!(tape.tone_at(CLOCK - 1), Tone::High);
    assert_eq!(tape.tone_at(CLOCK), Tone::Low);
    assert_eq!(tape.len(), CLOCK + 2 * BIT);
    // Past the end, the gap is silence.
    let mut short = Tape::default();
    short.push(Tone::High, BIT).unwrap();
    short.record_over(3 * BIT, &recorded).unwrap();
    assert_eq!(short.tone_at(2 * BIT), Tone::Silence);
    assert_eq!(short.tone_at(3 * BIT), Tone::Low);
    short.truncate(BIT / 2);
    assert_eq!(short.len(), BIT / 2);
    assert_eq!(short.push(Tone::High, MAX_CYCLES), Err(TapeError::TooLong));
}

#[test]
fn cutting_a_tape_where_a_tone_ends_leaves_no_empty_stretch() {
    let mut tape = Tape::default();
    tape.push(Tone::High, BIT).unwrap();
    tape.push(Tone::Low, BIT).unwrap();
    tape.push(Tone::Silence, BIT).unwrap();
    tape.truncate(2 * BIT);
    assert_eq!(tape.len(), 2 * BIT);
    assert_eq!(
        tape.next_change(BIT),
        Some((2 * BIT, Tone::Silence)),
        "the low tone runs to the end"
    );
    let mut again = tape.clone();
    again.push(Tone::Low, BIT).unwrap();
    assert_eq!(again.tone_at(2 * BIT), Tone::Low);
    assert_eq!(
        again.next_change(BIT),
        Some((3 * BIT, Tone::Silence)),
        "and the pushed tone joins it"
    );
    tape.truncate(0);
    assert!(tape.is_empty());
}

#[test]
fn a_tape_survives_a_snapshot_and_a_damaged_one_is_refused() {
    use crate::snapshot::{Reader, SnapshotError, Writer};
    let mut tape = Tape::decode(&uef(&[
        (0x0110, &100u16.to_le_bytes()),
        (0x0120, b"Side A\0"),
        (0x0100, b"BEEB"),
    ]))
    .unwrap();
    tape.truncate(tape.len() - BIT / 3);
    let mut w = Writer::new();
    w.put(&tape);
    let body = w.finish().unwrap();
    let body = crate::snapshot::open(&body).unwrap();
    assert_eq!(Reader::new(&body).get::<Tape>(), Ok(tape.clone()));

    let saved = |spans: &[(u64, u64)], markers: &[(u64, &str)]| {
        let mut w = Writer::new();
        w.put(&u32::try_from(spans.len()).unwrap());
        for &(cycles, code) in spans {
            w.put(&crate::snapshot::Varint(cycles << 2 | code));
        }
        w.put(
            &markers
                .iter()
                .map(|&(at, label)| super::Marker {
                    at,
                    label: label.into(),
                })
                .collect::<Vec<_>>(),
        );
        w.put(&0u32);
        crate::snapshot::open(&w.finish().unwrap()).unwrap()
    };
    let good = saved(&[(5, 0), (7, 1)], &[(12, "end")]);
    assert!(Reader::new(&good).get::<Tape>().is_ok());
    for (body, why) in [
        (saved(&[(5, 0), (0, 1)], &[]), "an empty span"),
        (saved(&[(5, 0), (7, 0)], &[]), "two alike side by side"),
        (saved(&[(5, 3)], &[]), "no such tone"),
        (saved(&[(MAX_CYCLES, 0), (1, 1)], &[]), "longer than a tape"),
        (saved(&[(5, 0)], &[(6, "")]), "a marker past the end"),
        (
            saved(&[(5, 0)], &[(3, ""), (2, "")]),
            "markers out of order",
        ),
        (
            saved(&[(5, 0)], &[(1, "\u{100}")]),
            "a label no image could give",
        ),
    ] {
        assert_eq!(
            Reader::new(&body).get::<Tape>(),
            Err(SnapshotError::Malformed),
            "{why}"
        );
    }
}

/// The regression: markers had no bound, so an image of empty marker chunks
/// decoded to millions of them, and a raw image no bound but its own size.
#[test]
fn markers_labels_and_raw_images_are_held_to_what_a_tape_holds() {
    let markers = vec![(0x0120, &b"x\0"[..]); super::MAX_MARKERS];
    assert!(Tape::decode(&uef(&markers)).is_ok());
    let mut more = markers.clone();
    more.push((0x0120, b"x\0"));
    assert_eq!(Tape::decode(&uef(&more)), Err(TapeError::TooLong));
    let label = vec![b'L'; super::MAX_LABEL + 1];
    assert_eq!(
        Tape::decode(&uef(&[(0x0120, &label)])),
        Err(TapeError::TooLong)
    );
    let mut raw = uef(&[(0x0110, &[0x10, 0x00])]);
    raw.resize(super::MAX_IMAGE + 1, 0);
    assert_eq!(Tape::decode(&raw), Err(TapeError::TooLong));
}
