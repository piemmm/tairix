//! International's tests: the names and numbers it answers for, each
//! alphabet's characters and the font redefined from `lib/vdu`, and the
//! commands over `OS_Byte` 70 and 71.

use alloc::vec::Vec;

use super::reason;
use crate::cli::tests::{cli, refused, session, written};
use crate::msgtrans::tests::swi;
use crate::service::INTERNATIONAL;
use crate::testkit::{Session, DATA};

const NAME: u32 = DATA + 0x700;
const BUFFER: u32 = DATA + 0x780;

/// `Service_International` `reason` with R3 to R5: R4 and R5 if claimed.
fn ask(s: &mut Session, reason: u32, r3: u32, r4: u32, r5: u32) -> Option<(u32, u32)> {
    let regs = swi(
        s,
        0x30,
        [0, INTERNATIONAL, reason, r3, r4, r5, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("issued");
    (regs.r[1] == 0).then_some((regs.r[4], regs.r[5]))
}

/// The number `name` gives as `reason` asks.
fn number(s: &mut Session, reason: u32, name: &[u8]) -> Option<u32> {
    let mut bytes = name.to_vec();
    bytes.push(0);
    s.write(NAME, &bytes);
    ask(s, reason, NAME, 0, 0).map(|(n, _)| n)
}

/// The name `reason` gives `number`.
fn name(s: &mut Session, reason: u32, number: u32) -> Option<Vec<u8>> {
    let (_, length) = ask(s, reason, number, BUFFER, 15)?;
    Some(s.read(BUFFER, length as usize))
}

#[test]
fn names_and_numbers_answer_each_other_abbreviated_or_whole() {
    let mut s = session();
    assert_eq!(number(&mut s, reason::COUNTRY_NUMBER, b"uk"), Some(1));
    assert_eq!(
        number(&mut s, reason::COUNTRY_NUMBER, b"Can."),
        Some(17),
        "the first in order"
    );
    assert_eq!(
        number(&mut s, reason::COUNTRY_NUMBER, b"Canada"),
        Some(19),
        "whole"
    );
    assert_eq!(number(&mut s, reason::COUNTRY_NUMBER, b"Canad"), None);
    assert_eq!(
        number(&mut s, reason::ALPHABET_NUMBER, b"latin10"),
        Some(116)
    );
    assert_eq!(number(&mut s, reason::ALPHABET_NUMBER, b"L."), Some(101));
    assert_eq!(
        name(&mut s, reason::COUNTRY_NAME, 48),
        Some(b"USA".to_vec())
    );
    assert_eq!(
        name(&mut s, reason::ALPHABET_NAME, 105),
        Some(b"Cyrillic".to_vec())
    );
    assert_eq!(
        name(&mut s, reason::COUNTRY_NAME, 2),
        None,
        "Master has no name here"
    );
    let (_, cut) = ask(&mut s, reason::COUNTRY_NAME, 34, BUFFER, 5).expect("named");
    assert_eq!(
        (cut, s.read(BUFFER, 5)),
        (5, b"Nethe".to_vec()),
        "cut to the room"
    );
    assert_eq!(
        ask(&mut s, reason::COUNTRY_ALPHABET, 24, 0, 0).map(|a| a.0),
        Some(105)
    );
    assert_eq!(
        ask(&mut s, reason::COUNTRY_ALPHABET, 84, 0, 0).map(|a| a.0),
        Some(105),
        "a keyboard with no name"
    );
    assert_eq!(ask(&mut s, reason::COUNTRY_ALPHABET, 0, 0, 0), None);
    let (_, two) = ask(&mut s, reason::ISO_3166, 1, BUFFER, 8).expect("a code");
    assert_eq!((two, s.read(BUFFER, 2)), (2, b"uk".to_vec()));
    ask(&mut s, reason::ISO_3166, 23, BUFFER, 8).expect("Hong Kong's");
    assert_eq!(s.read(BUFFER, 2), b"hk");
    assert_eq!(
        ask(&mut s, reason::ISO_3166, 9, BUFFER, 8),
        None,
        "Esperanto has none"
    );
}

#[test]
fn each_alphabet_s_table_holds_its_characters() {
    let mut s = session();
    let (latin2, _) = ask(&mut s, reason::UCS_TABLE, 102, 0, 0).expect("Latin2's");
    assert_eq!(s.peek(latin2 + 4 * 0xA1), 0x0104, "Ą");
    assert_eq!(s.peek(latin2 + 4 * u32::from(b'A')), u32::from(b'A'));
    let (latin1, _) = ask(&mut s, reason::UCS_TABLE, 101, 0, 0).expect("Latin1's");
    assert_eq!(s.peek(latin1 + 4 * 0x80), 0x20AC, "RISC OS's euro");
    assert_eq!(s.peek(latin1 + 4 * 0x83), u32::MAX, "unassigned");
    let (greek, _) = ask(&mut s, reason::UCS_TABLE, 107, 0, 0).expect("Greek's");
    assert_eq!(s.peek(greek + 4 * 0xC1), 0x0391, "Α");
    assert_eq!(
        ask(&mut s, reason::UCS_TABLE, 111, 0, 0),
        None,
        "UTF-8 has no table"
    );
}

/// The characters VDU 23 defined in `output`, and their glyphs.
pub(crate) fn defined(output: &[u8]) -> Vec<(u8, [u8; 8])> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(start) = output[at..].iter().position(|&b| b == 23) {
        let start = at + start;
        let Some(sequence) = output.get(start..start + 10) else {
            break;
        };
        let mut glyph = [0; 8];
        glyph.copy_from_slice(&sequence[2..]);
        found.push((sequence[1], glyph));
        at = start + 10;
    }
    found
}

#[test]
fn an_alphabet_is_drawn_from_the_system_font() {
    let mut s = session();
    let _ = s.output();
    assert!(ask(&mut s, reason::DEFINE, 101, 0x41, 0x43).is_some());
    let glyph = |c: char| tairix_vdu::font::glyph_of(c).expect("drawn");
    assert_eq!(
        defined(&s.output()),
        [(0x41, glyph('A')), (0x42, glyph('B')), (0x43, glyph('C'))]
    );
    assert!(ask(&mut s, reason::DEFINE, 101, 0x7F, 0x7F).is_some());
    assert_eq!(
        defined(&s.output()),
        [(0x7F, glyph('\u{2588}'))],
        "delete is a full block"
    );
    assert!(
        ask(&mut s, reason::DEFINE, 111, 0x80, 0x81).is_none(),
        "UTF-8 past ASCII claims nothing"
    );
    assert_eq!(
        defined(&s.output()),
        [
            (0x80, tairix_vdu::font::number(0x80)),
            (0x81, tairix_vdu::font::number(0x81))
        ],
        "but shows each code's number"
    );
    assert!(
        ask(&mut s, reason::DEFINE, 120, 32, 255).is_none(),
        "an alphabet it does not know"
    );
    assert!(ask(&mut s, reason::DEFINE, 101, 0x100, 0x1FF).is_some());
    assert!(
        defined(&s.output()).is_empty(),
        "a range past the codes defines nothing"
    );
    assert!(ask(&mut s, reason::DEFINE_UCS, 0xE9, 0xE9, 0).is_some());
    assert_eq!(defined(&s.output()), [(0xE9, glyph('é'))]);
    assert!(
        ask(&mut s, reason::DEFINE_UCS, 0xE9, 0x1_0400, 0).is_none(),
        "past the basic plane"
    );
}

#[test]
fn the_commands_read_and_select_through_os_byte() {
    let mut s = session();
    let _ = s.output();
    assert_eq!(written(&mut s, "Country"), b"UK\n\r");
    assert_eq!(written(&mut s, "Alphabet"), b"Latin1\n\r");
    assert_eq!(written(&mut s, "Keyboard"), b"UK\n\r");
    cli(&mut s, "Country Germany").expect("selected");
    let _ = s.output();
    assert_eq!(
        written(&mut s, "Keyboard"),
        b"Germany\n\r",
        "the country's keyboard with it"
    );
    cli(&mut s, "Alphabet Latin2").expect("selected");
    let _ = s.output();
    assert_eq!(written(&mut s, "Alphabet"), b"Latin2\n\r");
    cli(&mut s, "Keyboard USA").expect("selected");
    let _ = s.output();
    assert_eq!(written(&mut s, "Keyboard"), b"USA\n\r");
    assert_eq!(
        written(&mut s, "Country"),
        b"Germany\n\r",
        "the country stays"
    );
    assert_eq!(
        refused(&mut s, "Alphabet Nowhere"),
        b"No alphabet has that name"
    );
    assert_eq!(
        refused(&mut s, "Keyboard Nowhere"),
        b"No keyboard has that name"
    );
    assert_eq!(
        refused(&mut s, "Country Nowhere"),
        b"No country has that name"
    );
}

#[test]
fn os_byte_70_and_71_answer_nought_for_what_no_module_knows() {
    let mut s = session();
    let set = swi(&mut s, 0x06, [71, 120, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("asked");
    assert_eq!(set.r[1], 0, "no such alphabet");
    let set = swi(&mut s, 0x06, [70, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("asked");
    assert_eq!(set.r[1], 0, "no such country");
    let set = swi(&mut s, 0x06, [70, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("asked");
    assert_eq!(set.r[1], 1, "the old country");
    let read = swi(&mut s, 0x06, [71, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("read");
    assert_eq!(read.r[1], 7);
    let read = swi(&mut s, 0x06, [240, 0, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("read");
    assert_eq!(read.r[1], 7, "the country is OS_Byte 240's variable");
}

#[test]
fn the_listings_and_the_configured_country() {
    let mut s = session();
    let _ = s.output();
    let alphabets =
        alloc::string::String::from_utf8_lossy(&written(&mut s, "Alphabets")).into_owned();
    assert!(alphabets.starts_with("Alphabets:\n\r"), "{alphabets}");
    for name in ["Latin1", "Cyrillic", "Latin10", "UTF8"] {
        assert!(alphabets.contains(name), "{name} in {alphabets}");
    }
    let countries =
        alloc::string::String::from_utf8_lossy(&written(&mut s, "Countries")).into_owned();
    assert!(
        countries.starts_with("Countries:\n\r") && countries.contains("SAfrica"),
        "{countries}"
    );
    cli(&mut s, "Configure Country France").expect("configured");
    assert_eq!(s.kernel.os().world.cmos.read(crate::cmos::COUNTRY), 6);
    let _ = s.output();
    assert_eq!(written(&mut s, "Status Country"), b"Country    France\n\r");
    assert!(cli(&mut s, "Configure Country Nowhere").is_err());
}

#[test]
fn every_alphabet_s_characters_are_in_the_system_font() {
    for &(alphabet, _) in &super::ALPHABETS {
        for code in 0x20..=0xFF_u8 {
            if let Some(c) = super::character(alphabet, code).filter(|&c| c != '\u{7F}') {
                assert!(
                    tairix_vdu::font::glyph_of(c).is_some(),
                    "alphabet {alphabet}, &{code:02X}: U+{:04X}",
                    u32::from(c)
                );
            }
        }
    }
}

#[test]
fn the_extras_at_80_to_9f_are_table_d_s_and_latin2_s_past_the_prm() {
    let at = |alphabet, code| super::character(alphabet, code);
    assert_eq!(
        (at(102, 0x9A), at(102, 0x9B)),
        (Some('«'), Some('»')),
        "Latin2's guillemets"
    );
    assert_eq!((at(103, 0x9A), at(104, 0x9B)), (Some('Œ'), Some('œ')));
    assert_eq!(at(102, 0x81), None, "undefined, the previous glyph kept");
    for alphabet in [109, 110, 112, 113, 114, 115, 116] {
        let upper: Vec<char> = (0xA0..=0xFF)
            .filter_map(|code| at(alphabet, code))
            .collect();
        for code in 0x80..=0x9F {
            match at(alphabet, code) {
                Some(c) => {
                    assert_eq!(Some(c), at(102, code), "{alphabet} &{code:X} is Latin2's");
                    assert!(!upper.contains(&c), "{alphabet} &{code:X} not twice");
                }
                None => assert!(
                    at(102, code).is_none_or(|c| upper.contains(&c)),
                    "{alphabet} &{code:X} left out only where its upper half has it"
                ),
            }
        }
    }
}
