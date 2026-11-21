//! The UK territory's tests, through the Territory Manager's SWIs and the
//! kernel's own that call them: names, formats, ordinals, text, tables and
//! the clock's variables.

use alloc::vec::Vec;

use tairix_riscos_arm::psr;

use crate::msgtrans::tests::{session, swi};
use crate::swi::os::READ_VAR_VAL;
use crate::swi::X;
use crate::territory::CHUNK;
use crate::testkit::{Session, DATA};

const BUFFER: u32 = DATA + 0x100;
const TIME: u32 = DATA + 0x80;
const TEXT: u32 = DATA + 0x400;

/// A territory SWI.
fn territory(
    s: &mut Session,
    offset: u32,
    registers: [u32; 13],
) -> Result<crate::os::Regs, (u32, Vec<u8>)> {
    swi(s, CHUNK + offset, registers)
}

/// Writes `text` with a terminator at [`TEXT`].
fn text(s: &mut Session, text: &[u8]) -> u32 {
    let mut bytes = text.to_vec();
    bytes.push(0);
    s.write(TEXT, &bytes);
    TEXT
}

/// The five-byte time now, at [`TIME`].
fn now(s: &mut Session) -> u32 {
    let mut word = alloc::vec![3u8];
    word.extend_from_slice(&[0; 7]);
    s.write(TIME, &word);
    swi(s, 0x07, [14, TIME, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("read");
    TIME
}

/// `format` applied to now.
fn formatted(s: &mut Session, format: &[u8]) -> Result<Vec<u8>, (u32, Vec<u8>)> {
    let time = now(s);
    let format = text(s, format);
    let regs = territory(
        s,
        0x0B,
        [u32::MAX, time, BUFFER, 128, format, 0, 0, 0, 0, 0, 0, 0, 0],
    )?;
    Ok(s.read(BUFFER, (regs.r[1] - BUFFER) as usize))
}

#[test]
fn the_territory_is_the_uk_and_says_so() {
    let mut s = session();
    assert_eq!(territory(&mut s, 0, [0; 13]).expect("number").r[0], 1);
    let (exists, _) = s.user_swi(X | (CHUNK + 4), [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(exists.flag(psr::Z), "the UK is registered");
    let (absent, _) = s.user_swi(X | (CHUNK + 4), [7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(!absent.flag(psr::Z));
    territory(&mut s, 3, [1, BUFFER, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("named");
    assert_eq!(s.read(BUFFER, 3), b"UK\0");
    let unknown =
        territory(&mut s, 3, [2, BUFFER, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect_err("none");
    assert_eq!(unknown, (0x190, b"No territory has that number".to_vec()));
    territory(&mut s, 5, [101, BUFFER, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("named");
    assert_eq!(s.read(BUFFER, 7), b"Latin1\0");
    assert_eq!(
        territory(&mut s, 0x12, [u32::MAX, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
            .expect("alphabet")
            .r[0],
        101
    );
    let identifier = territory(&mut s, 0x13, [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
        .expect("identifier")
        .r[0];
    assert_eq!(s.read(identifier, 7), b"Latin1\0");
    assert_eq!(territory(&mut s, 0x15, [1; 13]).expect("direction").r[0], 0);
    let ime = territory(&mut s, 0x22, [1, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("IME");
    assert_eq!(&ime.r[..3], &[0, 5, 6], "no input method");
    assert_eq!(
        territory(&mut s, crate::territory::DAYLIGHT_RULES, [1; 13]).expect_err("time zone 1"),
        (
            0xAC2,
            b"The UK keeps no daylight saving rule for that time zone and year".to_vec()
        )
    );
    assert_eq!(
        territory(&mut s, 0x24, [1; 13]).expect_err("reserved"),
        (0x1E8, b"Unknown Territory SWI".to_vec())
    );
}

#[test]
fn every_field_formats_as_the_uk_writes_it() {
    let mut s = session();
    let text = formatted(
        &mut s,
        b"%WE %W3 %WN %DY%ST %MO %M3 %MN %CE%YR %DN %WK %24:%MI:%SE.%CS %12%AM %TZ %ZDY %%%0end",
    )
    .expect("formatted");
    assert_eq!(
        text,
        b"Saturday Sat 7 01st January Jan 01 2000 001 52 00:00:00.00 12am UTC 1 %\0end"
    );
}

#[test]
fn a_buffer_too_small_overflows_and_an_unknown_field_is_refused() {
    let mut s = session();
    let time = now(&mut s);
    let format = text(&mut s, b"%24:%MI");
    let fits = territory(
        &mut s,
        0x0B,
        [u32::MAX, time, BUFFER, 6, format, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("fits");
    assert_eq!(
        (fits.r[2], fits.r[3]),
        (0, format),
        "nothing left; R3 the format"
    );
    let short = territory(
        &mut s,
        0x0B,
        [u32::MAX, time, BUFFER, 5, format, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert_eq!(
        short.expect_err("too small"),
        (0x2C1, b"Buffer overflow".to_vec())
    );
    let empty = text(&mut s, b"");
    let none = territory(
        &mut s,
        0x0B,
        [u32::MAX, time, BUFFER, 0, empty, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert_eq!(none.expect_err("no room for the terminator").0, 0x2C1);
    let bad = text(&mut s, b"%QQ");
    let refused = territory(
        &mut s,
        0x0B,
        [u32::MAX, time, BUFFER, 64, bad, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert_eq!(
        refused.expect_err("unknown"),
        (0x2C2, b"Format has an unknown % field".to_vec())
    );
}

#[test]
fn the_standard_formats_are_the_uk_s_and_the_kernel_s_conversions_use_them() {
    let mut s = session();
    let time = now(&mut s);
    for (offset, expected) in [
        (0x0C, &b"00:00:00 01-Jan-2000"[..]),
        (0x0D, b"01-Jan-2000"),
        (0x0E, b"00:00:00"),
    ] {
        let regs = territory(
            &mut s,
            offset,
            [u32::MAX, time, BUFFER, 64, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        )
        .expect("converted");
        assert_eq!(s.read(BUFFER, (regs.r[1] - BUFFER) as usize), expected);
    }
    let regs = swi(
        &mut s,
        0xC0,
        [time, BUFFER, 64, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("OS_ConvertStandardDateAndTime");
    assert_eq!(s.read(regs.r[0], 20), b"00:00:00 01-Jan-2000");
    let format = text(&mut s, b"%DY/%MN/%CE%YR");
    let regs = swi(
        &mut s,
        0xC1,
        [time, BUFFER, 64, format, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("OS_ConvertDateAndTime");
    assert_eq!(s.read(regs.r[0], 10), b"01/01/2000");
}

#[test]
fn ordinals_convert_both_ways() {
    let mut s = session();
    let time = now(&mut s);
    territory(
        &mut s,
        0x0F,
        [u32::MAX, time, BUFFER, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("local");
    let words: Vec<u32> = (0..9).map(|i| s.peek(BUFFER + 4 * i)).collect();
    assert_eq!(words, [0, 0, 0, 0, 1, 1, 2000, 7, 1]);
    territory(&mut s, 9, [0, time, BUFFER, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("UTC");
    assert_eq!(s.peek(BUFFER + 24), 2000);
    s.poke(BUFFER, &[0, 30, 45, 12, 15, 6, 2001]);
    territory(
        &mut s,
        0x11,
        [u32::MAX, TIME + 16, BUFFER, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("a time");
    let back = TIME + 16;
    territory(
        &mut s,
        9,
        [0, back, BUFFER + 64, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("UTC");
    let round: Vec<u32> = (0..7).map(|i| s.peek(BUFFER + 64 + 4 * i)).collect();
    assert_eq!(round, [0, 30, 45, 12, 15, 6, 2001]);
    s.poke(BUFFER, &[0, 0, 0, 0, 1, 1, 1899]);
    let refused = territory(
        &mut s,
        0x11,
        [u32::MAX, back, BUFFER, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert_eq!(
        refused.expect_err("before 1900"),
        (0x195, b"Time values out of range".to_vec())
    );
}

#[test]
fn text_reads_as_ordinals() {
    let mut s = session();
    let at = text(&mut s, b"Sat,01 Jan 2000.12:34:56");
    territory(
        &mut s,
        0x10,
        [u32::MAX, 3, at, BUFFER, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("read");
    let words: Vec<u32> = (0..7).map(|i| s.peek(BUFFER + 4 * i)).collect();
    assert_eq!(words, [0, 56, 34, 12, 1, 1, 2000]);
    let at = text(&mut s, b"Sat,32 Jan 2000");
    let refused = territory(
        &mut s,
        0x10,
        [u32::MAX, 2, at, BUFFER, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert_eq!(
        refused.expect_err("no 32nd"),
        (0x194, b"That date or time cannot be read".to_vec())
    );
}

#[test]
fn tables_are_handed_out_and_properties_bounded() {
    let mut s = session();
    let upper = territory(&mut s, 0x18, [u32::MAX; 13]).expect("upper").r[0];
    assert_eq!(s.read(upper + u32::from(b'a'), 3), b"ABC");
    let lower = territory(&mut s, 0x17, [u32::MAX; 13]).expect("lower").r[0];
    assert_eq!(s.read(lower + 0xC9, 1), [0xE9]);
    let digits = territory(&mut s, 0x16, [u32::MAX, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
        .expect("digits")
        .r[0];
    assert_eq!(s.read(digits + 6, 2), [0xFF, 0x03], "0 to 9");
    let refused = territory(
        &mut s,
        0x16,
        [u32::MAX, 11, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert_eq!(
        refused.expect_err("no such property"),
        (0x193, b"No such character property".to_vec())
    );
    let digits = territory(&mut s, 0x1C, [u32::MAX; 13])
        .expect("representation")
        .r[0];
    assert_eq!(s.read(digits, 16), b"0123456789ABCDEF");
}

#[test]
fn collation_answers_in_r0_and_the_flags() {
    let mut s = session();
    s.write(BUFFER, b"apple\0");
    s.write(BUFFER + 32, b"Banana\0");
    let (less, _) = s.user_swi(
        X | (CHUNK + 0x1D),
        [u32::MAX, BUFFER, BUFFER + 32, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert!(less.flag(psr::N) && !less.flag(psr::Z) && !less.flag(psr::C));
    assert!(less.r[0].cast_signed() < 0);
    let (same, _) = s.user_swi(
        X | (CHUNK + 0x1D),
        [u32::MAX, BUFFER, BUFFER, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert!(same.flag(psr::Z) && same.r[0] == 0);
    let (more, _) = s.user_swi(
        X | (CHUNK + 0x1D),
        [u32::MAX, BUFFER + 32, BUFFER, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    assert!(more.flag(psr::C) && !more.flag(psr::Z) && !more.flag(psr::N));
    let transformed = territory(
        &mut s,
        0x21,
        [u32::MAX, BUFFER + 64, BUFFER, 64, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("t");
    assert_eq!(transformed.r[0], 18, "three passes of five and their ends");
    assert_eq!(s.read(BUFFER + 64 + 18, 1), [0]);
    let small = territory(
        &mut s,
        0x21,
        [
            u32::MAX,
            BUFFER + 128,
            BUFFER,
            18,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ],
    )
    .expect("t");
    assert_eq!(small.r[0], 18);
    assert_eq!(
        s.peek(BUFFER + 128),
        0,
        "nothing written where it does not fit"
    );
}

#[test]
fn symbols_and_the_calendar_are_the_uk_s() {
    let mut s = session();
    let point = territory(&mut s, 0x1E, [u32::MAX, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
        .expect("point")
        .r[0];
    assert_eq!(s.read(point, 2), b".\0");
    let currency = territory(&mut s, 0x1E, [u32::MAX, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
        .expect("currency")
        .r[0];
    assert_eq!(s.read(currency, 5), b"GBP \0");
    let pound = territory(&mut s, 0x1E, [u32::MAX, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
        .expect("pound")
        .r[0];
    assert_eq!(s.read(pound, 2), [0xA3, 0]);
    assert_eq!(
        territory(
            &mut s,
            0x1E,
            [u32::MAX, 11, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        )
        .expect("digits")
        .r[0],
        2
    );
    let list = territory(
        &mut s,
        0x1E,
        [u32::MAX, 18, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("list")
    .r[0];
    assert_eq!(s.read(list, 2), b";\0");
    let time = now(&mut s);
    territory(
        &mut s,
        0x1F,
        [u32::MAX, time, BUFFER, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("calendar");
    let words: Vec<u32> = (0..12).map(|i| s.peek(BUFFER + 4 * i)).collect();
    assert_eq!(words, [2, 6, 12, 31, 2, 9, 3, 2, 2, 9, 3, 3]);
}

#[test]
fn names_read_as_territory_numbers() {
    let mut s = session();
    let number = |s: &mut Session, name: &[u8]| {
        let mut bytes = name.to_vec();
        bytes.push(b'\r');
        s.write(TEXT, &bytes);
        territory(s, 0x20, [u32::MAX, TEXT, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
            .expect("read")
            .r[0]
    };
    assert_eq!(number(&mut s, b"UK"), 1);
    assert_eq!(number(&mut s, b"germany"), 7);
    assert_eq!(number(&mut s, b"UK (Dvorak)"), 70);
    assert_eq!(number(&mut s, b"Nowhere"), 0);
    assert_eq!(
        number(&mut s, b"Canada"),
        0,
        "Canada1 comes first and stops the search"
    );
}

#[test]
fn the_clock_s_variables_read_and_set_it() {
    let mut s = crate::cli::tests::session();
    let read = |s: &mut Session, name: &[u8]| {
        let mut bytes = name.to_vec();
        bytes.push(0);
        s.write(TEXT, &bytes);
        let expanded = u32::from(crate::vars::EXPANDED);
        let regs = swi(
            s,
            READ_VAR_VAL,
            [TEXT, BUFFER, 64, 0, expanded, 0, 0, 0, 0, 0, 0, 0, 0],
        )
        .expect("read");
        s.read(BUFFER, regs.r[2] as usize)
    };
    assert_eq!(read(&mut s, b"Sys$Time"), b"00:00:00");
    assert_eq!(read(&mut s, b"Sys$Date"), b"Sat,01 Jan");
    assert_eq!(read(&mut s, b"Sys$Year"), b"2000");
    crate::cli::tests::cli(&mut s, "Set Sys$Time 12:30:00").expect("set");
    assert_eq!(read(&mut s, b"Sys$Time"), b"12:30:00");
    crate::cli::tests::cli(&mut s, "Set Sys$Year 2001").expect("set");
    assert_eq!(
        read(&mut s, b"Sys$Date"),
        b"Mon,01 Jan",
        "1 January 2001 was a Monday"
    );
    let _ = s.output();
    assert_eq!(
        crate::cli::tests::written(&mut s, "Time"),
        b"Mon,01 Jan 2001.12:30:00\n\r"
    );
}

#[test]
fn os_word_reads_and_sets_the_clock_as_text() {
    let mut s = session();
    s.write(BUFFER, &[0; 32]);
    swi(&mut s, 0x07, [14, BUFFER, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("read");
    assert_eq!(s.read(BUFFER, 25), b"Sat,01 Jan 2000.00:00:00\r");
    let mut block = alloc::vec![24u8];
    block.extend_from_slice(b"Tue,04 Jul 2006.10:20:30");
    s.write(BUFFER, &block);
    swi(&mut s, 0x07, [15, BUFFER, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("set");
    s.write(BUFFER, &[0; 32]);
    swi(&mut s, 0x07, [14, BUFFER, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).expect("read");
    assert_eq!(s.read(BUFFER, 24), b"Tue,04 Jul 2006.10:20:30");
}
