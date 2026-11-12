//! The packing of fields: integers, absent values and records, each read
//! back as it was written.

use super::*;

record! {
    /// A record of each kind of field.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    struct Sample {
        byte: u8,
        word: u16,
        long: u32,
        far: Far,
        maybe: Option<u16>,
        bytes: [u8; 3],
    }
}

#[test]
fn a_record_is_read_back_as_it_was_written() {
    assert_eq!(<Sample as Field>::SIZE, 1 + 2 + 4 + 4 + 3 + 3);
    let sample = Sample {
        byte: 1,
        word: 0x0203,
        long: 0x0405_0607,
        far: Far::new(0x1234, 0x5678),
        maybe: Some(9),
        bytes: [7, 8, 9],
    };
    let mut bytes = [0u8; 17];
    let mut at = 0;
    sample.put(&mut bytes, &mut at);
    assert_eq!(at, 17);
    assert_eq!(&bytes[..3], &[1, 3, 2], "little-endian, in order");
    let mut at = 0;
    assert_eq!(Sample::take(&bytes, &mut at), sample);
    let absent = Sample {
        maybe: None,
        ..sample
    };
    let mut at = 0;
    absent.put(&mut bytes, &mut at);
    assert_eq!(
        &bytes[11..14],
        &[0, 0, 0],
        "an absent value written as its default"
    );
    let mut at = 0;
    assert_eq!(Sample::take(&bytes, &mut at).maybe, None);
}

#[test]
fn a_field_past_the_bytes_reads_as_zero() {
    let mut at = 2;
    assert_eq!(u32::take(&[1, 2, 3], &mut at), 0);
    assert_eq!(at, 6, "moved past it all the same");
}
