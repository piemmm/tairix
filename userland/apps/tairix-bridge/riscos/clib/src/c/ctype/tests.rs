use tairix_riscos_kernel::module::Code;

use super::*;
use crate::c::statics::at;
use crate::c::testing::{Client, STATICS};

/// Genuine's C-locale answers, by character: alnum to xdigit, then tolower
/// and toupper.
const RECORDED: [(i32, [u32; 13]); 9] = [
    (-1, [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, u32::MAX, u32::MAX]),
    (9, [0, 0, 0x40, 0, 0, 0, 0, 0, 1, 0, 0, 9, 9]),
    (32, [0, 0, 0, 0, 0, 0, 4, 0, 1, 0, 0, 32, 32]),
    (33, [0, 0, 0, 0, 2, 0, 2, 2, 0, 0, 0, 33, 33]),
    (48, [0x20, 0, 0, 0x20, 0x20, 0, 0x20, 0, 0, 0, 0x20, 48, 48]),
    (
        65,
        [0x10, 0x10, 0, 0, 0x10, 0, 0x10, 0, 0, 0x10, 0x80, 97, 65],
    ),
    (103, [8, 8, 0, 0, 8, 8, 8, 0, 0, 0, 0, 103, 71]),
    (127, [0, 0, 0x40, 0, 0, 0, 0, 0, 0, 0, 0, 127, 127]),
    (200, [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 200, 200]),
];

const ENTRIES: [Code; 13] = [
    isalnum, isalpha, iscntrl, isdigit, isgraph, islower, isprint, ispunct, isspace, isupper,
    isxdigit, tolower, toupper,
];

fn c_locale_client() -> Client {
    let mut client = Client::new();
    client.put(STATICS + at::CTYPE, &C_LOCALE);
    client
}

#[test]
fn the_c_locale_answers_as_genuine_did_the_masked_bits_and_the_shifted_letters() {
    let mut client = c_locale_client();
    for (c, answers) in RECORDED {
        for (n, (&entry, &expected)) in ENTRIES.iter().zip(&answers).enumerate() {
            assert_eq!(
                client.answer(entry, &[c.cast_unsigned()]),
                expected,
                "entry {n} of {c}"
            );
        }
    }
}

#[test]
fn a_character_past_the_table_reads_the_statics_beyond_it_as_the_macros_do() {
    let mut client = c_locale_client();
    client.put(STATICS + at::CTYPE + 256, &[0xFF]);
    assert_eq!(client.answer(isalpha, &[256]), 0x18);
    assert_eq!(
        client.answer(toupper, &[256]),
        224,
        "a lower-case bit moves it down"
    );
    assert_eq!(client.answer(tolower, &[256]), 288);
}

#[test]
fn a_territory_s_case_tables_are_read_at_the_character_unchecked() {
    let mut client = c_locale_client();
    let lower = crate::c::testing::FREE;
    let mut tables = [0; 513];
    for (c, byte) in (0u8..=255).zip(tables[1..257].iter_mut()) {
        *byte = c.to_ascii_lowercase();
    }
    for (c, byte) in (0u8..=255).zip(tables[257..].iter_mut()) {
        *byte = c.to_ascii_uppercase();
    }
    client.put(lower - 1, &tables);
    client.put(STATICS + at::LOWER_TABLE, &(lower).to_le_bytes());
    client.put(STATICS + at::UPPER_TABLE, &(lower + 256).to_le_bytes());
    assert_eq!(client.answer(tolower, &[u32::from(b'Q')]), u32::from(b'q'));
    assert_eq!(client.answer(toupper, &[u32::from(b'q')]), u32::from(b'Q'));
    assert_eq!(
        client.answer(tolower, &[u32::MAX]),
        0,
        "the byte before the lower table"
    );
    assert_eq!(
        client.answer(toupper, &[u32::MAX]),
        255,
        "the lower table's last"
    );
}
