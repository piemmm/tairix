//! Unit tests for the DER reader and writer.
//!
//! The canonical forms are ITU-T X.690 §10's; the SPNEGO and Kerberos
//! encodings are the shapes RFC 4178 and RFC 4120 messages take on the wire.

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::time::Time64;

use crate::{Class, DerError, Reader, Tag, Writer, MAX_DEPTH};

fn written(write: impl FnOnce(&mut Writer)) -> Vec<u8> {
    let mut writer = Writer::new();
    write(&mut writer);
    writer.finish().expect("encodes")
}

fn read_one<'a, T>(
    input: &'a [u8],
    read: impl FnOnce(&mut Reader<'a>) -> Result<T, DerError>,
) -> Result<T, DerError> {
    let mut reader = Reader::new(input);
    let value = read(&mut reader)?;
    reader.finish()?;
    Ok(value)
}

#[test]
fn identifiers_of_every_class_and_number_round_trip() {
    for tag in [
        Tag::SEQUENCE,
        Tag::INTEGER,
        Tag::context(0),
        Tag::context(30),
        Tag::context(31),
        Tag::application(30),
        Tag::new(Class::Private, false, 127),
        Tag::new(Class::Context, true, 128),
        Tag::new(Class::Application, false, u32::MAX),
    ] {
        let bytes = written(|w| w.element(tag, b"x"));
        let tlv = read_one(&bytes, Reader::tlv).expect("reads");
        assert_eq!(tlv.tag, tag);
        assert_eq!(tlv.contents, b"x");
        assert_eq!(tlv.encoding, &bytes[..]);
    }
    assert_eq!(
        written(|w| w.element(Tag::context(31), &[]))[..2],
        [0xbf, 0x1f]
    );
    assert_eq!(
        written(|w| w.element(Tag::new(Class::Context, true, 128), &[]))[..3],
        [0xbf, 0x81, 0x00]
    );
}

#[test]
fn a_non_minimal_identifier_is_refused() {
    // The high form for a number the low form holds, and a padded number.
    for bytes in [[0x1fu8, 0x05, 0x00], [0x1f, 0x80, 0x7f]] {
        assert_eq!(
            Reader::new(&bytes).tlv(),
            Err(DerError::NonCanonical),
            "{bytes:02x?}"
        );
    }
    assert_eq!(Reader::new(&[0x1f, 0x81]).tlv(), Err(DerError::Truncated));
}

#[test]
fn lengths_are_definite_and_minimal() {
    let long = written(|w| w.octet_string(&[7; 300]));
    assert_eq!(long[..4], [0x04, 0x82, 0x01, 0x2c]);
    assert_eq!(
        read_one(&long, Reader::octet_string).expect("reads"),
        [7; 300]
    );
    assert_eq!(
        written(|w| w.octet_string(&[1; 128]))[..3],
        [0x04, 0x81, 0x80]
    );

    for (bytes, refusal) in [
        (vec![0x04, 0x80], DerError::Indefinite),
        (vec![0x04, 0x81, 0x7f], DerError::NonCanonical),
        (vec![0x04, 0x82, 0x00, 0x80], DerError::NonCanonical),
        (vec![0x04, 0x85, 1, 0, 0, 0, 0], DerError::TooLong),
        (vec![0x04, 0x03, 1, 2], DerError::Truncated),
        (vec![0x04, 0x82, 0x01], DerError::Truncated),
        (vec![0x04], DerError::Truncated),
    ] {
        assert_eq!(Reader::new(&bytes).tlv(), Err(refusal), "{bytes:02x?}");
    }
}

#[test]
fn integers_take_their_minimal_form_and_back() {
    for (value, encoding) in [
        (0i64, &[0x00][..]),
        (127, &[0x7f]),
        (128, &[0x00, 0x80]),
        (256, &[0x01, 0x00]),
        (-1, &[0xff]),
        (-128, &[0x80]),
        (-129, &[0xff, 0x7f]),
        (4_294_967_295, &[0x00, 0xff, 0xff, 0xff, 0xff]),
        (i64::MAX, &[0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]),
        (i64::MIN, &[0x80, 0, 0, 0, 0, 0, 0, 0]),
    ] {
        let bytes = written(|w| w.integer(value));
        assert_eq!(&bytes[2..], encoding, "{value}");
        assert_eq!(read_one(&bytes, Reader::integer), Ok(value));
    }
}

#[test]
fn a_padded_empty_or_oversized_integer_is_refused() {
    for (bytes, refusal) in [
        (vec![0x02, 0x02, 0x00, 0x7f], DerError::NonCanonical),
        (vec![0x02, 0x02, 0xff, 0x80], DerError::NonCanonical),
        (vec![0x02, 0x00], DerError::NonCanonical),
        (
            vec![0x02, 0x09, 0x01, 0, 0, 0, 0, 0, 0, 0, 0],
            DerError::OutOfRange,
        ),
    ] {
        assert_eq!(
            read_one(&bytes, Reader::integer),
            Err(refusal),
            "{bytes:02x?}"
        );
    }
}

#[test]
fn booleans_are_ff_or_00_only() {
    assert_eq!(
        read_one(&written(|w| w.boolean(true)), Reader::boolean),
        Ok(true)
    );
    assert_eq!(
        read_one(&written(|w| w.boolean(false)), Reader::boolean),
        Ok(false)
    );
    assert_eq!(
        read_one(&[0x01, 0x01, 0x01], Reader::boolean),
        Err(DerError::NonCanonical)
    );
    assert_eq!(read_one(&[0x05, 0x00], Reader::null), Ok(()));
    assert_eq!(
        read_one(&[0x05, 0x01, 0x00], Reader::null),
        Err(DerError::NonCanonical)
    );
}

#[test]
fn bit_strings_carry_their_padding_and_refuse_stray_bits() {
    // A Kerberos flags field: forwardable, renewable, the rest clear.
    let flags = [0x03, 0x05, 0x00, 0x40, 0x80, 0x00, 0x10];
    let bits = read_one(&flags, Reader::bit_string).expect("reads");
    assert_eq!(bits.unused_bits(), 0);
    assert_eq!(bits.bytes(), [0x40, 0x80, 0x00, 0x10]);
    assert_eq!(bits.bit_len(), 32);
    assert_eq!(
        written(|w| w.bit_string(0, &[0x40, 0x80, 0x00, 0x10])),
        flags
    );

    let padded = read_one(&[0x03, 0x02, 0x04, 0xf0], Reader::bit_string).expect("reads");
    assert_eq!(padded.bit_len(), 4);
    for bytes in [[0x03, 0x02, 0x04, 0xf8], [0x03, 0x02, 0x08, 0x00]] {
        assert_eq!(
            read_one(&bytes, Reader::bit_string),
            Err(DerError::NonCanonical)
        );
    }
    assert_eq!(
        read_one(&[0x03, 0x01, 0x01], Reader::bit_string),
        Err(DerError::NonCanonical)
    );
    assert_eq!(
        read_one(&[0x03, 0x00], Reader::bit_string),
        Err(DerError::NonCanonical)
    );

    let mut writer = Writer::new();
    writer.bit_string(3, &[0x01]);
    assert_eq!(writer.finish(), Err(DerError::NonCanonical));
}

#[test]
fn object_identifiers_compare_as_their_canonical_arcs() {
    let spnego = [0x2b, 0x06, 0x01, 0x05, 0x05, 0x02];
    let kerberos = [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x12, 0x01, 0x02, 0x02];
    for arcs in [&spnego[..], &kerberos[..]] {
        let bytes = written(|w| w.oid(arcs));
        assert_eq!(read_one(&bytes, Reader::oid), Ok(arcs));
    }
    for bytes in [
        &[0x06, 0x02, 0x80, 0x01][..],
        &[0x06, 0x02, 0x2a, 0x86],
        &[0x06, 0x00],
    ] {
        assert_eq!(
            read_one(bytes, Reader::oid),
            Err(DerError::NonCanonical),
            "{bytes:02x?}"
        );
    }
}

#[test]
fn generalized_time_names_the_instant() {
    let leap_day = b"20240229123456Z";
    let bytes = written(|w| w.element(Tag::GENERALIZED_TIME, leap_day));
    let instant = read_one(&bytes, Reader::generalized_time).expect("reads");
    assert_eq!(instant, Time64::from_secs(1_709_210_096));
    assert_eq!(written(|w| w.generalized_time(instant)), bytes);

    let half = Time64::new(1_704_067_200, 500_000_000).expect("instant");
    let encoded = written(|w| w.generalized_time(half));
    assert_eq!(&encoded[2..], b"20240101000000.5Z");
    assert_eq!(read_one(&encoded, Reader::generalized_time), Ok(half));

    let before_the_epoch = Time64::from_secs(-86_400);
    let encoded = written(|w| w.generalized_time(before_the_epoch));
    assert_eq!(&encoded[2..], b"19691231000000Z");
    assert_eq!(
        read_one(&encoded, Reader::generalized_time),
        Ok(before_the_epoch)
    );
}

#[test]
fn a_time_outside_the_canonical_form_or_the_calendar_is_refused() {
    for text in [
        &b"20230229000000Z"[..],
        b"20240101240000Z",
        b"20240101000060Z",
        b"20240101000000.50Z",
        b"20240101000000.Z",
        b"20240101000000",
        b"202401010000Z",
        b"20240101000000+0100",
        b"2024010100000aZ",
        b"20240101000000.1234567891Z",
    ] {
        let bytes = written(|w| w.element(Tag::GENERALIZED_TIME, text));
        assert_eq!(
            read_one(&bytes, Reader::generalized_time),
            Err(DerError::NonCanonical),
            "{}",
            core::str::from_utf8(text).unwrap_or("?")
        );
    }
    let mut writer = Writer::new();
    writer.generalized_time(Time64::from_secs(253_402_300_800));
    assert_eq!(writer.finish(), Err(DerError::OutOfRange));
}

/// An SPNEGO `negTokenResp` with `negState` accept-completed (RFC 4178 §4.2.2).
const ACCEPT_COMPLETED: [u8; 9] = [0xa1, 0x07, 0x30, 0x05, 0xa0, 0x03, 0x0a, 0x01, 0x00];

#[test]
fn a_negotiation_response_reads_and_writes() {
    let state = read_one(&ACCEPT_COMPLETED, |r| {
        r.explicit(1, |r| {
            r.sequence(|r| {
                let state = r.explicit_optional(0, Reader::enumerated)?;
                assert_eq!(r.optional(Tag::context(1))?, None);
                Ok(state)
            })
        })
    });
    assert_eq!(state, Ok(Some(0)));
    let rebuilt =
        written(|w| w.explicit(1, |w| w.sequence(|w| w.explicit(0, |w| w.enumerated(0)))));
    assert_eq!(rebuilt, ACCEPT_COMPLETED);
}

#[test]
fn a_choice_is_read_by_peeking() {
    let reader = Reader::new(&ACCEPT_COMPLETED);
    assert_eq!(reader.peek_tag(), Ok(Some(Tag::context(1))));
    assert_eq!(Reader::new(&[]).peek_tag(), Ok(None));
}

#[test]
fn a_structure_must_be_read_whole() {
    let extra = written(|w| {
        w.sequence(|w| {
            w.integer(1);
            w.integer(2);
        });
    });
    assert_eq!(
        read_one(&extra, |r| r.sequence(Reader::integer)),
        Err(DerError::TrailingData)
    );
    let mut trailing = written(|w| w.integer(1));
    trailing.push(0);
    assert_eq!(
        read_one(&trailing, Reader::integer),
        Err(DerError::TrailingData)
    );
    // An inner element claiming more than its sequence holds.
    assert_eq!(
        read_one(&[0x30, 0x03, 0x02, 0x05, 0x01], |r| r
            .sequence(Reader::integer)),
        Err(DerError::Truncated)
    );
}

#[test]
fn nesting_is_bounded() {
    fn nest(writer: &mut Writer, depth: usize) {
        if depth == 0 {
            writer.null();
        } else {
            writer.sequence(|w| nest(w, depth - 1));
        }
    }
    fn descend(reader: &mut Reader<'_>, depth: usize) -> Result<(), DerError> {
        if depth == 0 {
            reader.null()
        } else {
            reader.sequence(|r| descend(r, depth - 1))
        }
    }
    let deepest = written(|w| nest(w, MAX_DEPTH));
    assert_eq!(read_one(&deepest, |r| descend(r, MAX_DEPTH)), Ok(()));
    let deeper = written(|w| nest(w, MAX_DEPTH + 1));
    assert_eq!(
        read_one(&deeper, |r| descend(r, MAX_DEPTH + 1)),
        Err(DerError::TooDeep)
    );
}

#[test]
fn a_raw_element_must_be_one_well_formed_element() {
    let ticket = written(|w| w.sequence(|w| w.integer(5)));
    let carried = written(|w| w.explicit(3, |w| w.raw(&ticket)));
    assert_eq!(&carried[2..], &ticket[..]);
    for bad in [
        &ticket[..ticket.len() - 1],
        &[ticket.as_slice(), &ticket].concat(),
    ] {
        let mut writer = Writer::new();
        writer.raw(bad);
        assert_eq!(writer.finish(), Err(DerError::NonCanonical));
    }
}

#[test]
fn the_wrong_tag_is_refused() {
    assert_eq!(
        read_one(&written(|w| w.octet_string(b"x")), Reader::general_string),
        Err(DerError::UnexpectedTag)
    );
    // A constructed string is BER, never DER.
    assert_eq!(
        read_one(&[0x24, 0x03, 0x04, 0x01, 0x78], Reader::octet_string),
        Err(DerError::UnexpectedTag)
    );
}
