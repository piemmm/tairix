use super::{decode, header, Assignment, FrameError, Samples, Stream};
use crate::bits::BitWriter;

const STEREO: Stream = Stream {
    rate: 44_100,
    bits: 16,
    channels: 2,
    max_block: 4096,
};

/// RFC 9639 example 1's frame header.
const HEADER: [u8; 7] = [0xff, 0xf8, 0x69, 0x18, 0x00, 0x00, 0xbf];

#[test]
fn a_header_states_its_block_numbering_and_channels() {
    let parsed = header(&HEADER, &STEREO).expect("parses");
    assert_eq!(parsed.block_size, 1);
    assert_eq!(parsed.number, 0);
    assert_eq!(parsed.assignment, Assignment::Independent(2));
    assert!(!parsed.variable);
    assert_eq!(parsed.len, 7);
}

/// A header with `body` after its sync, its CRC-8 made to agree.
fn with_crc(body: &[u8]) -> std::vec::Vec<u8> {
    let mut bytes = [&[0xffu8, 0xf8][..], body].concat();
    bytes.push(crate::crc::crc8(&bytes));
    bytes
}

extern crate std;

#[test]
fn reserved_and_forbidden_codes_are_refused() {
    let cases: [(&[u8], FrameError); 6] = [
        (&[0x09, 0x18, 0x00], FrameError::Reserved),
        (&[0x6f, 0x18, 0x00, 0x00], FrameError::Reserved),
        (&[0x69, 0xb8, 0x00, 0x00], FrameError::Reserved),
        (&[0x69, 0x16, 0x00, 0x00], FrameError::Reserved),
        (&[0x69, 0x19, 0x00, 0x00], FrameError::Reserved),
        (&[0x79, 0x18, 0x00, 0xff, 0xff], FrameError::Reserved),
    ];
    for (body, expected) in cases {
        assert_eq!(header(&with_crc(body), &STEREO), Err(expected), "{body:x?}");
    }
}

#[test]
fn a_coded_number_is_read_to_its_full_width_and_its_malformed_forms_refused() {
    let variable = [0xffu8, 0xf9, 0x69, 0x18];
    let mut seven = variable.to_vec();
    seven.extend([0xfe, 0x83, 0xbf, 0xbf, 0xbf, 0xbf, 0xbf, 0x00]);
    seven.push(crate::crc::crc8(&seven));
    let parsed = header(&seven, &STEREO).expect("a 36-bit sample number");
    assert_eq!(parsed.number, (3u64 << 30) | ((1u64 << 30) - 1));
    for number in [&[0xffu8][..], &[0x80], &[0xc2, 0x00]] {
        let mut bytes = variable.to_vec();
        bytes.extend(number);
        bytes.push(0x00);
        bytes.push(crate::crc::crc8(&bytes));
        assert_eq!(
            header(&bytes, &STEREO),
            Err(FrameError::Invalid),
            "{number:x?}"
        );
    }
    let mut fixed = [0xffu8, 0xf8, 0x69, 0x18].to_vec();
    fixed.extend([0xfe, 0x82, 0x80, 0x80, 0x80, 0x80, 0x80, 0x00]);
    fixed.push(crate::crc::crc8(&fixed));
    assert_eq!(
        header(&fixed, &STEREO),
        Err(FrameError::Invalid),
        "a frame number past 31 bits"
    );
}

#[test]
fn a_header_must_agree_with_its_stream_and_its_crc() {
    let mut damaged = HEADER;
    damaged[6] ^= 1;
    assert_eq!(header(&damaged, &STEREO), Err(FrameError::HeaderCrc));
    let mono = Stream {
        channels: 1,
        ..STEREO
    };
    assert_eq!(header(&HEADER, &mono), Err(FrameError::Mismatch));
    let short = Stream {
        max_block: 16,
        ..STEREO
    };
    let big = with_crc(&[0xc9, 0x18, 0x00]);
    assert_eq!(header(&big, &short), Err(FrameError::Invalid));
    assert_eq!(
        header(&[0xff, 0xf0, 0x69], &STEREO),
        Err(FrameError::NoSync)
    );
    assert_eq!(header(&HEADER[..5], &STEREO), Err(FrameError::Truncated));
}

#[test]
fn the_frame_bound_is_twice_a_verbatim_frame_with_its_side_channel() {
    assert_eq!(
        STEREO.verbatim_len(1, 7),
        7 + (33 + 16usize).div_ceil(8) + 2
    );
    assert_eq!(STEREO.frame_bound(1, 7), 2 * STEREO.verbatim_len(1, 7));
    let eight = Stream {
        bits: 32,
        channels: 8,
        max_block: 65_535,
        rate: 0,
    };
    assert_eq!(
        eight.max_frame_len(),
        2 * (16 + (65_535 * 256 + 64) / 8 + 2)
    );
}

const MONO8: Stream = Stream {
    rate: 44_100,
    bits: 8,
    channels: 1,
    max_block: 4096,
};

const STEREO8: Stream = Stream {
    channels: 2,
    ..MONO8
};

/// A frame of `block` samples at 44.1 kHz with channel code `assignment`
/// and width code `width`, its subframes the bits `body` writes, both CRCs
/// made to agree.
fn coded(
    assignment: u64,
    width: u64,
    block: u32,
    body: impl FnOnce(&mut BitWriter),
) -> std::vec::Vec<u8> {
    let mut writer = BitWriter::default();
    writer.write(0x7FFC, 15);
    writer.write(0, 1);
    writer.write(6, 4);
    writer.write(9, 4);
    writer.write(assignment, 4);
    writer.write(width, 3);
    writer.write(0, 1);
    writer.write(0, 8);
    writer.write(u64::from(block - 1), 8);
    let crc = crate::crc::crc8(writer.bytes());
    writer.write(u64::from(crc), 8);
    body(&mut writer);
    writer.align();
    let crc = crate::crc::crc16(0, writer.bytes());
    writer.write(u64::from(crc), 16);
    writer.into_bytes()
}

/// A frame of 8-bit samples.
fn frame(assignment: u64, block: u32, body: impl FnOnce(&mut BitWriter)) -> std::vec::Vec<u8> {
    coded(assignment, 1, block, body)
}

/// A subframe header of `kind`, no wasted bits.
fn kind(writer: &mut BitWriter, kind: u64) {
    writer.write(kind << 1, 8);
}

fn decoded(bytes: &[u8], stream: &Stream) -> Result<std::vec::Vec<i64>, FrameError> {
    let mut samples = Samples::default();
    decode(bytes, stream, &mut samples)?;
    Ok((0..usize::from(stream.channels))
        .flat_map(|channel| samples.channel(channel).to_vec())
        .collect())
}

#[test]
fn a_hand_built_frame_decodes_as_written() {
    let bytes = frame(0, 4, |writer| {
        kind(writer, 1);
        for sample in [1i64, -2, 127, -128] {
            writer.write_signed(sample, 8);
        }
    });
    assert_eq!(decoded(&bytes, &MONO8), Ok(std::vec![1, -2, 127, -128]));
    let mut damaged = bytes.clone();
    let last = damaged.len() - 1;
    damaged[last] ^= 1;
    assert_eq!(decoded(&damaged, &MONO8), Err(FrameError::FrameCrc));
}

/// Left 127 and a side of -128 put the right channel at 255, past 8 bits.
#[test]
fn a_decorrelated_channel_past_the_width_is_refused() {
    let bytes = frame(8, 1, |writer| {
        kind(writer, 0);
        writer.write_signed(127, 8);
        kind(writer, 0);
        writer.write_signed(-128, 9);
    });
    assert_eq!(decoded(&bytes, &STEREO8), Err(FrameError::Invalid));
}

#[test]
fn a_prediction_past_the_width_is_refused() {
    let bytes = frame(0, 2, |writer| {
        kind(writer, 8 + 1);
        writer.write_signed(127, 8);
        writer.write(0, 2);
        writer.write(0, 4);
        writer.write(4, 4);
        writer.unary(1);
        writer.write(4, 4);
    });
    assert_eq!(decoded(&bytes, &MONO8), Err(FrameError::Invalid));
}

/// Parameter 30, quotient 3 and remainder `2^30 - 1` fold to `2^32 - 1`,
/// the most negative 32-bit residual: a sample it makes would fit a 32-bit
/// stream, but the format excludes the residual itself.
#[test]
fn a_residual_folded_past_32_bits_short_of_the_most_negative_is_refused() {
    let mono32 = Stream { bits: 32, ..MONO8 };
    let folded = |remainder: u64| {
        coded(0, 7, 2, |writer| {
            kind(writer, 8 + 1);
            writer.write_signed(0, 32);
            writer.write(1, 2);
            writer.write(0, 4);
            writer.write(30, 5);
            writer.unary(3);
            writer.write(remainder, 30);
        })
    };
    assert_eq!(
        decoded(&folded((1 << 30) - 1), &mono32),
        Err(FrameError::Invalid)
    );
    assert_eq!(
        decoded(&folded((1 << 30) - 2), &mono32),
        Ok(std::vec![0, (1 << 31) - 1]),
        "one short of the limit is the largest residual"
    );
}

/// A subframe's bits, written by a test.
type Body<'a> = &'a dyn Fn(&mut BitWriter);

#[test]
fn reserved_subframe_forms_are_refused() {
    let cases: [(Body<'_>, FrameError); 6] = [
        (&|writer| kind(writer, 2), FrameError::Reserved),
        (&|writer| kind(writer, 13), FrameError::Reserved),
        (&|writer| writer.write(0x80, 8), FrameError::Reserved),
        (
            &|writer| {
                writer.write(1 << 1 | 1, 8);
                writer.unary(7);
            },
            FrameError::Invalid,
        ),
        (
            &|writer| {
                kind(writer, 32);
                writer.write_signed(0, 8);
                writer.write(0xF, 4);
            },
            FrameError::Reserved,
        ),
        (
            &|writer| {
                kind(writer, 32);
                writer.write_signed(0, 8);
                writer.write(3, 4);
                writer.write_signed(-1, 5);
            },
            FrameError::Reserved,
        ),
    ];
    for (index, (body, expected)) in cases.into_iter().enumerate() {
        let bytes = frame(0, 4, |writer| {
            body(writer);
            writer.write(0, 64);
        });
        assert_eq!(decoded(&bytes, &MONO8), Err(expected), "case {index}");
    }
}

#[test]
fn a_partition_order_the_block_cannot_split_is_refused() {
    let bytes = frame(0, 6, |writer| {
        kind(writer, 8);
        writer.write(0, 2);
        writer.write(2, 4);
        writer.write(0, 64);
    });
    assert_eq!(decoded(&bytes, &MONO8), Err(FrameError::Invalid));
}
