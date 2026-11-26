use alloc::vec::Vec;

use super::*;
use crate::c::ctype::{tolower, toupper};
use crate::c::testing::{Client, FREE, STATICS};

/// The `ctype` table genuine built for the UK, recorded.
const UK_TABLE: [u8; 256] = [
    0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x41, 0x41, 0x41, 0x41, 0x41, 0x40, 0x40,
    0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40,
    0x05, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02,
    0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02,
    0x02, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10,
    0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x02, 0x02, 0x02, 0x02, 0x02,
    0x02, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08,
    0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x02, 0x02, 0x02, 0x02, 0x40,
    0x02, 0x10, 0x08, 0x00, 0x00, 0x10, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x02, 0x02, 0x02,
    0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x10, 0x08, 0x02, 0x02, 0x08, 0x08,
    0x05, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02,
    0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02,
    0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10,
    0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x02, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x08,
    0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08,
    0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x02, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08,
];

const NAME: u32 = FREE;
const QUERY: u32 = 0;

fn started() -> Client {
    let mut client = Client::new();
    let statics = Statics::of_base(STATICS);
    start_in_c(&mut client.session.kernel.os(), statics).expect("the C locale");
    client
}

/// `setlocale(category, name)`: its answer, and the string there.
fn set(client: &mut Client, category: u32, name: Option<&[u8]>) -> (u32, Vec<u8>) {
    let given = match name {
        Some(text) => {
            client.put(NAME, &[text, b"\0"].concat());
            NAME
        }
        None => QUERY,
    };
    let answer = client.answer(setlocale, &[category, given]);
    let text = if answer == 0 {
        Vec::new()
    } else {
        client.string(answer)
    };
    (answer, text)
}

fn composite(client: &mut Client) -> Vec<u8> {
    set(client, 31, None).1
}

#[test]
fn the_library_starts_in_the_c_locale_and_names_it_as_a_composite() {
    let mut client = started();
    assert_eq!(composite(&mut client), b"=0,0,0,0,0");
    assert_eq!(
        set(&mut client, 2, None).1,
        b"=0,0,0,0,0",
        "whatever the category asked"
    );
    assert_eq!(client.bytes(STATICS + at::CTYPE, 256), ctype::C_LOCALE);
}

#[test]
fn the_native_locale_is_the_uk_its_table_genuine_s() {
    let mut client = started();
    let (answer, name) = set(&mut client, 31, Some(b""));
    assert_eq!(
        (answer, name.as_slice()),
        (STATICS + at::LOCALE_NAME, &b"UK"[..])
    );
    assert_eq!(composite(&mut client), b"=1,1,1,1,1");
    assert_eq!(client.bytes(STATICS + at::CTYPE, 256), UK_TABLE);
    assert_eq!(client.answer(tolower, &[200]), 0xE8);
    assert_eq!(client.answer(toupper, &[255]), u32::from(b'Y'));
    assert_eq!(
        client.answer(toupper, &[u32::MAX]),
        0xFF,
        "the lower table's last byte"
    );
    assert_eq!(client.answer(tolower, &[u32::MAX]), 0);
}

#[test]
fn a_territory_s_name_answers_the_caller_s_string_and_c_puts_everything_back() {
    let mut client = started();
    assert_eq!(set(&mut client, 31, Some(b"uk")).0, NAME, "any case");
    assert_eq!(composite(&mut client), b"=1,1,1,1,1");
    assert_eq!(set(&mut client, 31, Some(b"C")).0, NAME);
    assert_eq!(composite(&mut client), b"=0,0,0,0,0");
    assert_eq!(client.bytes(STATICS + at::CTYPE, 256), ctype::C_LOCALE);
    let (answer, name) = set(&mut client, 31, Some(b"ISO8859-1"));
    assert_eq!(
        (answer, name.as_slice()),
        (STATICS + at::LOCALE_NAME, &b"UK"[..])
    );
}

#[test]
fn names_no_loaded_territory_has_and_categories_outside_the_mask_are_refused() {
    let mut client = started();
    for name in [
        &b"Germany"[..],
        b"UK ",
        b"c",
        b"1",
        b"POSIX",
        b"Latin1",
        b"ISO8859-2",
    ] {
        assert_eq!(set(&mut client, 31, Some(name)).0, 0, "{name:?}");
    }
    for category in [0, 32, 63, 64] {
        assert_eq!(
            set(&mut client, category, Some(b"UK")).0,
            0,
            "category {category}"
        );
    }
    assert_eq!(composite(&mut client), b"=0,0,0,0,0", "nothing changed");
}

#[test]
fn a_composite_sets_each_category_asked_by_place_the_rest_nought() {
    let mut client = started();
    for (name, expected) in [
        (&b"=1,2,3,4,5"[..], &b"=1,2,3,4,5"[..]),
        (b"=1", b"=1,0,0,0,0"),
        (b"=1;1", b"=1,0,0,0,0"),
        (b"= 1", b"=0,0,0,0,0"),
        (b"=01", b"=1,0,0,0,0"),
        (b"=1,1,1,1,1,1", b"=1,1,1,1,1"),
    ] {
        set(&mut client, 31, Some(b"UK"));
        assert_eq!(set(&mut client, 31, Some(name)).0, NAME);
        assert_eq!(composite(&mut client), expected, "{name:?}");
    }
    set(&mut client, 31, Some(b"C"));
    set(&mut client, 2, Some(b"=1,2,3,4,5"));
    assert_eq!(
        composite(&mut client),
        b"=0,2,0,0,0",
        "only LC_CTYPE, by its place"
    );
    assert!(
        client
            .bytes(STATICS + at::CTYPE, 256)
            .iter()
            .all(|&b| b == 0),
        "a territory not loaded"
    );
    set(&mut client, 31, Some(b"UK"));
    set(&mut client, 2, Some(b"=1"));
    assert_eq!(composite(&mut client), b"=1,0,1,1,1");
    set(&mut client, 31, Some(b"C"));
    set(&mut client, 3, Some(b"UK"));
    assert_eq!(composite(&mut client), b"=1,1,0,0,0", "a category mask");
}

/// `localeconv()`'s strings and characters.
fn lconv(client: &mut Client) -> (Vec<Vec<u8>>, Vec<u8>) {
    let at = client.answer(localeconv, &[]);
    let strings = (0..10)
        .map(|n| {
            let p = client.word(at + 4 * n);
            client.string(p)
        })
        .collect();
    (strings, client.bytes(at + 40, 8))
}

#[test]
fn localeconv_gives_the_uk_s_symbols_by_category() {
    let mut client = started();
    let (strings, chars) = lconv(&mut client);
    assert_eq!(strings[0], b".");
    assert!(strings[1..].iter().all(Vec::is_empty));
    assert_eq!(chars, [0xFF; 8]);
    set(&mut client, 8, Some(b"UK"));
    let (strings, _) = lconv(&mut client);
    assert_eq!(
        strings[..3],
        [b".".to_vec(), b",".to_vec(), b"\x03".to_vec()]
    );
    assert!(strings[3].is_empty(), "LC_MONETARY still the C locale's");
    set(&mut client, 31, Some(b"UK"));
    let (strings, chars) = lconv(&mut client);
    assert_eq!(
        strings[3..],
        [&b"GBP "[..], b"\xA3", b".", b" ", b"\x03", b"", b"-"].map(<[u8]>::to_vec)
    );
    assert_eq!(chars, [2, 2, 1, 0, 1, 0, 1, 1]);
}

#[test]
fn collation_is_strcmp_s_in_the_c_locale_and_the_territory_s_otherwise() {
    let mut client = started();
    let (a, b) = (FREE + 0x100, FREE + 0x200);
    client.put(a, b"ab\0");
    client.put(b, b"ac\0");
    assert_eq!(
        client.answer(strcoll, &[a, b]).cast_signed(),
        -128,
        "word-aligned, as strcmp"
    );
    client.put(a, b"aB\xE9z\0");
    let out = FREE + 0x300;
    assert_eq!(client.answer(strxfrm, &[out, a, 40]), 4);
    assert_eq!(client.string(out), b"aB\xE9z");
    assert_eq!(client.answer(strxfrm, &[0, a, 0]), 4, "measured only");
    set(&mut client, 31, Some(b"UK"));
    client.put(a, b"a\0");
    client.put(b, b"B\0");
    assert!(
        client.answer(strcoll, &[a, b]).cast_signed() < 0,
        "a before B"
    );
    client.put(a, b"aB\xE9z\0");
    assert_eq!(
        client.answer(strxfrm, &[out, a, 40]),
        15,
        "the territory's form"
    );
}
