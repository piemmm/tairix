//! Each number here is one RISC OS 5.30 gave for the same fonts and strings.

use super::*;
use crate::fixture::{
    call, find, find_at, fonts, read_words, write, write_words, BLOCK, MATRIX, TEXT,
};
use crate::module::call::{
    CHAR_BBOX, CURRENT_FONT, CURRENT_RGB, ENUMERATE_CHARACTERS, FIND_CARET, FIND_CARET_J,
    FUTURE_FONT, FUTURE_RGB, LOOKUP_FONT, READ_DEFN, READ_FONT_METRICS, READ_INFO, SCAN_STRING,
    SET_FONT, SET_FONT_COLOURS, STRING_BBOX, STRING_WIDTH,
};
use tairix_riscos_filing::session::Session;

const FONT: u32 = 1 << 8;
const KERN: u32 = 1 << 9;
const BLOCKED: u32 = 1 << 5;
const MATRIXED: u32 = 1 << 6;
const LENGTH: u32 = 1 << 7;
const RTL: u32 = 1 << 10;
const CARET: u32 = 1 << 17;
const BBOX: u32 = 1 << 18;
const MATRIX_OUT: u32 = 1 << 19;
const COUNT: u32 = 1 << 20;
const NONE: i32 = -1;
const FAR: i32 = i32::MAX;

/// What a scan answers: how far into the string, the offset there and R7.
#[derive(Debug, PartialEq, Eq)]
struct Scanned {
    at: u32,
    x: i32,
    y: i32,
    r7: u32,
}

/// What a scan is asked: its flags, R3 and R4, its block and matrix, and R7.
#[derive(Clone, Copy)]
struct Ask {
    flags: u32,
    limit: (i32, i32),
    block: Option<[i32; 5]>,
    matrix: Option<[i32; 6]>,
    r7: u32,
}

impl Ask {
    const DEFAULT: Self = Self {
        flags: 0,
        limit: (FAR, FAR),
        block: None,
        matrix: None,
        r7: 0,
    };
}

/// `Font_ScanString` of `text` in `handle` as `ask` says.
fn scan(s: &mut Session, handle: u32, text: &[u8], ask: &Ask) -> Result<Scanned, u32> {
    let Ask {
        flags,
        limit,
        block,
        matrix,
        r7,
    } = *ask;
    let at = write(s, TEXT, text);
    if let Some(block) = block {
        write_words(s, BLOCK, &block);
        write_words(s, BLOCK + 20, &[0x5EED_5EED; 4]);
    }
    if let Some(matrix) = matrix {
        write_words(s, MATRIX, &matrix);
    }
    let regs = call(
        s,
        SCAN_STRING,
        &[
            handle,
            at,
            flags | FONT,
            limit.0.cast_unsigned(),
            limit.1.cast_unsigned(),
            BLOCK,
            MATRIX,
            r7,
        ],
    )?;
    Ok(Scanned {
        at: regs.r[1] - at,
        x: regs.r[3].cast_signed(),
        y: regs.r[4].cast_signed(),
        r7: regs.r[7],
    })
}

/// The offset a plain scan of `text` ends at.
fn width(s: &mut Session, handle: u32, text: &[u8], flags: u32) -> (i32, i32) {
    let scanned = scan(
        s,
        handle,
        text,
        &Ask {
            flags,
            ..Ask::DEFAULT
        },
    )
    .expect("scanned");
    (scanned.x, scanned.y)
}

fn bbox(s: &mut Session) -> Vec<i32> {
    read_words(s, BLOCK + 20, 4)
}

const ODD: [i32; 6] = [43690, 1234, -567, 70000, 0, 0];
const ROT90: [i32; 6] = [0, 65536, -65536, 0, 0, 0];
const HALF: [i32; 6] = [32768, 0, 0, 32768, 0, 0];

#[test]
fn advances_and_kerns_are_floored_thousandths_at_the_font_s_size() {
    let mut s = fonts();
    let h = find(&mut s, b"Scan", 192);
    for (text, plain, kerned) in [
        (&b"A"[..], 8400, 8400),
        (b"AB", 16200, 15000),
        (b"ABC", 20196, 18996),
        (b"A B", 19200, 19200),
        (b"BA", 16200, 16644),
        (b"AA", 16800, 16788),
        (b"ABA", 24600, 23844),
        (b"iii", 10008, 10008),
    ] {
        assert_eq!(width(&mut s, h, text, 0), (plain, 0), "{text:?}");
        assert_eq!(width(&mut s, h, text, KERN), (kerned, 0), "{text:?} kerned");
    }
    let small = find(&mut s, b"Scan", 100);
    assert_eq!(width(&mut s, small, b"ABC", 0).0, 10518);
    assert_eq!(width(&mut s, small, b"CCC", 0).0, 6243);
    assert_eq!(width(&mut s, small, b"ABCDEi", 0).0, 18504);
    assert_eq!(
        width(&mut s, small, b"AA", KERN).0,
        8743,
        "a kern of -6.25 floors to -7"
    );
    assert_eq!(width(&mut s, small, b"BA", KERN).0, 8668);
    let tall = find_at(&mut s, b"Scan", (160, 240), (90, 45)).expect("10 by 15");
    assert_eq!(
        width(&mut s, tall, b"ABCDEi", 0).0,
        29610,
        "across, the size across"
    );
    let master = find_at(&mut s, b"Scan", (-192, 192), (0, 0)).expect("the master");
    assert_eq!(master, 2, "a negative size finds the master");
    assert_eq!(
        width(&mut s, master, b"AB", 0).0,
        1350,
        "a master measures at one point"
    );
}

#[test]
fn matrices_carry_each_step_floored_a_product_at_a_time() {
    let mut s = fonts();
    let h = find(&mut s, b"Scan", 192);
    let m = |s: &mut Session, text: &[u8], flags: u32, matrix| {
        let scanned = scan(
            s,
            h,
            text,
            &Ask {
                flags: flags | MATRIXED,
                matrix: Some(matrix),
                ..Ask::DEFAULT
            },
        )
        .expect("scanned");
        (scanned.x, scanned.y)
    };
    assert_eq!(m(&mut s, b"AB", 0, ODD), (10798, 304));
    assert_eq!(m(&mut s, b"ABC", 0, ODD), (13461, 379));
    assert_eq!(m(&mut s, b"AB", 0, ROT90), (0, 16200));
    assert_eq!(
        m(&mut s, b"AB", 0, [-43690, 0, 0, 65536, 0, 0]),
        (-10800, 0)
    );
    assert_eq!(
        m(&mut s, b"AB", 0, [43690, -1234, 567, 70000, 0, 0]),
        (10798, -306)
    );
    assert_eq!(
        m(&mut s, b"AB", KERN, ODD),
        (9998, 281),
        "the kern carried apart"
    );
    assert_eq!(
        m(&mut s, b"AB", 0, [65536, 0, 0, 65536, 1000, 2000]),
        (16200, 0)
    );
    let small = find(&mut s, b"Scan", 100);
    let scanned = scan(
        &mut s,
        small,
        b"B",
        &Ask {
            flags: MATRIXED,
            matrix: Some(ODD),
            ..Ask::DEFAULT
        },
    )
    .expect("scanned");
    assert_eq!(
        (scanned.x, scanned.y),
        (2707, 76),
        "floored to millipoints, then carried"
    );
    let half = find(&mut s, b"Scan\\M32768 0 0 32768 0 0", 100);
    let scanned = scan(
        &mut s,
        half,
        b"B",
        &Ask {
            flags: MATRIXED,
            matrix: Some(ODD),
            ..Ask::DEFAULT
        },
    )
    .expect("scanned");
    assert_eq!(
        (scanned.x, scanned.y),
        (1353, 38),
        "the font's matrix, then the painting one"
    );
    assert_eq!(width(&mut s, half, b"B", 0).0, 2031);
}

#[test]
fn justification_adds_its_offsets_untransformed_between_letters_and_at_spaces() {
    let mut s = fonts();
    let h = find(&mut s, b"Scan", 192);
    let j =
        |s: &mut Session, text: &[u8], flags: u32, block: [i32; 5], matrix: Option<[i32; 6]>| {
            let flags = flags | BLOCKED | if matrix.is_some() { MATRIXED } else { 0 };
            let scanned = scan(
                s,
                h,
                text,
                &Ask {
                    flags,
                    block: Some(block),
                    matrix,
                    ..Ask::DEFAULT
                },
            )
            .expect("scanned");
            (scanned.x, scanned.y)
        };
    assert_eq!(j(&mut s, b"AB", 0, [0, 0, 100, 0, NONE], None), (16300, 0));
    assert_eq!(j(&mut s, b"A", 0, [0, 0, 100, 0, NONE], None), (8400, 0));
    assert_eq!(
        j(&mut s, b"A  B", 0, [0, 0, 100, 0, NONE], None),
        (22500, 0)
    );
    assert_eq!(j(&mut s, b"AB", 0, [0, 0, 0, 7, NONE], None), (16200, 7));
    assert_eq!(j(&mut s, b"A ", 0, [1000, 0, 0, 0, NONE], None), (12400, 0));
    assert_eq!(
        j(&mut s, b"A B", 0, [1000, 0, 0, 0, 66], None),
        (20200, 0),
        "spaces, whatever splits"
    );
    assert_eq!(j(&mut s, b"ABAB", 0, [1000, 0, 0, 0, 66], None), (32400, 0));
    assert_eq!(
        j(&mut s, b"AB", KERN, [0, 0, 100, 0, NONE], None),
        (15100, 0)
    );
    assert_eq!(
        j(&mut s, b"A B", 0, [1000, 300, 77, 5, NONE], Some(ODD)),
        (13951, 670)
    );
    assert_eq!(
        j(&mut s, b"A B", 0, [1000, 7, 0, 0, NONE], Some(ROT90)),
        (1000, 19207)
    );
    assert_eq!(
        j(&mut s, b"A B", RTL, [1000, 7, 100, 3, NONE], None),
        (-20400, -13)
    );
}

#[test]
fn a_limit_stops_before_the_step_past_it_or_at_the_last_split() {
    let mut s = fonts();
    let h = find(&mut s, b"Scan", 192);
    for (limit, at, x) in [
        (5000, 0, 0),
        (8400, 1, 8400),
        (16199, 1, 8400),
        (16200, 2, 16200),
        (20000, 2, 16200),
    ] {
        let scanned = scan(
            &mut s,
            h,
            b"ABCABC",
            &Ask {
                limit: (limit, FAR),
                ..Ask::DEFAULT
            },
        )
        .expect("scanned");
        assert_eq!((scanned.at, scanned.x), (at, x), "limit {limit}");
    }
    for (limit, at, x) in [
        (5000, 0, 0),
        (8400, 1, 8400),
        (16200, 1, 8400),
        (20000, 3, 19200),
    ] {
        let scanned = scan(
            &mut s,
            h,
            b"A B A B",
            &Ask {
                flags: BLOCKED,
                limit: (limit, FAR),
                block: Some([0, 0, 0, 0, 32]),
                ..Ask::DEFAULT
            },
        )
        .expect("scanned");
        assert_eq!((scanned.at, scanned.x), (at, x), "split at {limit}");
    }
}

#[test]
fn splits_are_counted_and_only_minus_one_names_no_split_character() {
    let mut s = fonts();
    let h = find(&mut s, b"Scan", 192);
    let split = |s: &mut Session, text: &[u8], limit: i32, split: i32| {
        let scanned = scan(
            s,
            h,
            text,
            &Ask {
                flags: BLOCKED | COUNT,
                limit: (limit, FAR),
                block: Some([0, 0, 0, 0, split]),
                r7: 99,
                ..Ask::DEFAULT
            },
        )
        .expect("scanned");
        (scanned.at, scanned.x, scanned.r7)
    };
    assert_eq!(
        split(&mut s, b"ABC D", 17000, 32),
        (0, 0, 0),
        "no split met: the start"
    );
    assert_eq!(split(&mut s, b"A B C", 23000, 32), (3, 19200, 1));
    assert_eq!(split(&mut s, b"ABAB", 30000, 66), (3, 24600, 1));
    assert_eq!(
        split(&mut s, b"AB", 10000, -2),
        (0, 0, 0),
        "only -1 is no split character"
    );
}

#[test]
fn a_limit_is_checked_only_on_an_axis_a_step_moves_and_a_cut_sequence_is_not_taken() {
    let mut s = fonts();
    let h = find(&mut s, b"Scan", 192);
    let edge =
        |s: &mut Session, text: &[u8], flags: u32, limit: (i32, i32), matrix: Option<[i32; 6]>| {
            let flags = flags | if matrix.is_some() { MATRIXED } else { 0 };
            let scanned = scan(
                s,
                h,
                text,
                &Ask {
                    flags,
                    limit,
                    matrix,
                    ..Ask::DEFAULT
                },
            )
            .expect("scanned");
            (scanned.at, scanned.x, scanned.y)
        };
    assert_eq!(edge(&mut s, b"AB", 0, (0, FAR), None), (0, 0, 0));
    assert_eq!(
        edge(&mut s, b"AB", 0, (-10000, FAR), None),
        (2, 16200, 0),
        "below nought, a limit is passed going down"
    );
    assert_eq!(edge(&mut s, b"AB", RTL, (5000, FAR), None), (2, -16200, 0));
    assert_eq!(edge(&mut s, b"AB", RTL, (-10000, FAR), None), (1, -8400, 0));
    assert_eq!(
        edge(&mut s, b"AB", 0, (FAR, 10000), Some(ROT90)),
        (1, 0, 8400)
    );
    assert_eq!(
        edge(&mut s, b"AB", 0, (0, FAR), Some(ROT90)),
        (2, 0, 16200),
        "only an axis that moves"
    );
    assert_eq!(
        edge(&mut s, b"A\x0B\x0C\xFE\xFFB", 0, (FAR, -100), None),
        (1, 8400, 0),
        "a move is a step"
    );
    let length = scan(
        &mut s,
        h,
        b"A\x09\xE8\x03\x00B",
        &Ask {
            flags: LENGTH,
            r7: 3,
            ..Ask::DEFAULT
        },
    )
    .expect("scanned");
    assert_eq!(
        (length.at, length.x, length.r7),
        (1, 8400, 3),
        "a sequence the length cuts is not taken"
    );
}

#[test]
fn a_caret_is_the_place_nearest_the_point_its_box_what_comes_before() {
    let mut s = fonts();
    let h = find(&mut s, b"Scan", 192);
    for (x, at) in [
        (0, 0),
        (4200, 0),
        (4201, 1),
        (12000, 1),
        (16000, 2),
        (99999, 3),
        (-50, 0),
    ] {
        assert_eq!(
            scan(
                &mut s,
                h,
                b"ABC",
                &Ask {
                    flags: CARET,
                    limit: (x, 0),
                    ..Ask::DEFAULT
                }
            )
            .expect("caret")
            .at,
            at,
            "{x}"
        );
    }
    let rotated = |s: &mut Session, point| {
        scan(
            s,
            h,
            b"AB",
            &Ask {
                flags: CARET | MATRIXED,
                limit: point,
                matrix: Some(ROT90),
                ..Ask::DEFAULT
            },
        )
        .expect("caret")
        .at
    };
    assert_eq!(
        (
            rotated(&mut s, (0, 5000)),
            rotated(&mut s, (5000, 0)),
            rotated(&mut s, (-3000, 13000))
        ),
        (1, 0, 2)
    );
}

#[test]
fn a_caret_through_a_turned_matrix_is_where_its_point_lands() {
    let mut s = fonts();
    let h = find(&mut s, b"Scan", 192);
    let diagonal = [46341, 46341, -46341, 46341, 0, 0];
    let tilted = |s: &mut Session, point| {
        scan(
            s,
            h,
            b"AB",
            &Ask {
                flags: CARET | MATRIXED,
                limit: point,
                matrix: Some(diagonal),
                ..Ask::DEFAULT
            },
        )
        .expect("caret")
    };
    assert_eq!(tilted(&mut s, (3500, 0)).at, 0);
    assert_eq!(tilted(&mut s, (0, 3500)).at, 0);
    assert_eq!(tilted(&mut s, (7000, -2000)).at, 0);
    assert_eq!(
        tilted(&mut s, (4000, 4000)),
        Scanned {
            at: 1,
            x: 5939,
            y: 5939,
            r7: 0
        }
    );
}

#[test]
fn a_caret_is_the_nearest_place_of_all_and_its_box_what_comes_before() {
    let mut s = fonts();
    let h = find(&mut s, b"Scan", 192);
    let detour = b"A\x0B\x20\x4E\x00\x09\x30\xDF\xFFB";
    let found = scan(
        &mut s,
        h,
        detour,
        &Ask {
            flags: CARET | BBOX | BLOCKED,
            limit: (7800, 20000),
            block: Some([0, 0, 0, 0, NONE]),
            ..Ask::DEFAULT
        },
    )
    .expect("caret");
    assert_eq!(
        found,
        Scanned {
            at: 10,
            x: 7800,
            y: 20000,
            r7: 0
        },
        "the nearest of all, not the first"
    );
    assert_eq!(bbox(&mut s), [600, 0, 7812, 28412]);
    let before = scan(
        &mut s,
        h,
        b"ABC",
        &Ask {
            flags: CARET | BBOX | BLOCKED,
            limit: (9000, 0),
            block: Some([0, 0, 0, 0, NONE]),
            ..Ask::DEFAULT
        },
    )
    .expect("caret");
    assert_eq!(before.at, 1);
    assert_eq!(bbox(&mut s), [600, 0, 7812, 8412]);
}

#[test]
fn a_box_holds_each_glyph_s_box_where_it_is_placed() {
    let mut s = fonts();
    let h = find(&mut s, b"Scan", 192);
    let b = |s: &mut Session, text: &[u8], flags: u32, matrix: Option<[i32; 6]>| {
        let flags = flags | BBOX | BLOCKED | if matrix.is_some() { MATRIXED } else { 0 };
        scan(
            s,
            h,
            text,
            &Ask {
                flags,
                block: Some([0, 0, 0, 0, NONE]),
                matrix,
                ..Ask::DEFAULT
            },
        )
        .expect("scanned");
        bbox(s)
    };
    assert_eq!(b(&mut s, b"AB", 0, None), [600, 0, 15612, 8412]);
    assert_eq!(b(&mut s, b"C", 0, None), [360, -2400, 3612, 6012]);
    assert_eq!(
        b(&mut s, b" ", 0, None),
        [0, 0, 0, 0],
        "a glyph with no ink is a point"
    );
    assert_eq!(
        b(&mut s, b"AZ", 0, None),
        [600, 0, 8400, 8412],
        "a character the font lacks, too"
    );
    assert_eq!(
        b(&mut s, b"", 0, None),
        [1 << 29, 1 << 29, -(1 << 29), -(1 << 29)]
    );
    assert_eq!(b(&mut s, b"AB", KERN, None), [600, 0, 14412, 8412]);
    assert_eq!(b(&mut s, b"AB", KERN | RTL, None), [-14280, 0, -588, 8412]);
    assert_eq!(
        b(&mut s, b"E", 0, Some(ROT90)),
        [-10812, -1200, -1200, 13212]
    );
    assert_eq!(b(&mut s, b"A", 0, Some(ODD)), [326, 11, 5207, 9131]);
    assert_eq!(
        b(&mut s, b"A", 0, Some([65536, 0, 0, 65536, 1000, 2000])),
        [1600, 2000, 8812, 10412]
    );
    assert_eq!(
        b(&mut s, b"A\x19\xF0\x14B", 0, None),
        [600, 0, 15612, 8412],
        "underlining is not in it"
    );
    assert_eq!(
        b(&mut s, b"A\x0B\x18\xFC\xFFB", 0, None),
        [600, -1000, 15612, 8412]
    );
    let sheared = find(&mut s, b"Scan\\M65536 0 16384 65536 100 -50", 192);
    scan(
        &mut s,
        sheared,
        b"E",
        &Ask {
            flags: BBOX | BLOCKED | MATRIXED,
            block: Some([0, 0, 0, 0, NONE]),
            matrix: Some(ODD),
            ..Ask::DEFAULT
        },
    )
    .expect("scanned");
    assert_eq!(
        bbox(&mut s),
        [-544, 1212, 10582, 11795],
        "the two matrices combined first"
    );
    assert_eq!(
        scan(
            &mut s,
            h,
            b"AB",
            &Ask {
                flags: BBOX,
                ..Ask::DEFAULT
            }
        ),
        Err(0x213),
        "a box needs the block"
    );
}

#[test]
fn control_sequences_move_change_and_end_as_risc_os_s_do() {
    let mut s = fonts();
    let h = find(&mut s, b"Scan", 192);
    let other = find(&mut s, b"Scan2", 192);
    assert_eq!(width(&mut s, h, b"A\x09\xE8\x03\x00B", 0), (17200, 0));
    assert_eq!(width(&mut s, h, b"A\x0B\x0C\xFE\xFFB", 0), (16200, -500));
    assert_eq!(
        width(
            &mut s,
            h,
            &[b'A', 26, u8::try_from(other).expect("a handle"), b'B'],
            0
        ),
        (17724, 0)
    );
    assert_eq!(
        width(&mut s, h, b"A\x09\x00\x00\x00B", KERN),
        (15000, 0),
        "a move keeps the kern"
    );
    assert_eq!(
        width(
            &mut s,
            h,
            &[b'A', 26, u8::try_from(h).expect("a handle"), b'B'],
            KERN
        ),
        (16200, 0)
    );
    assert_eq!(
        scan(&mut s, h, b"A\x15hello\nB", &Ask::DEFAULT)
            .expect("scanned")
            .at,
        9
    );
    assert_eq!(
        scan(&mut s, h, b"A\rB", &Ask::DEFAULT).expect("scanned").at,
        1
    );
    assert_eq!(scan(&mut s, h, b"A\x01B", &Ask::DEFAULT), Err(0x20F));
    let matrix = |code: u8, words: &[i32]| {
        let mut text = alloc::vec![b'A', code, 0, 0];
        text.extend(words.iter().flat_map(|w| w.to_le_bytes()));
        text.push(b'B');
        text
    };
    let text = matrix(28, &[65536, 0, 0, 65536, 0, 0]);
    let scanned = scan(
        &mut s,
        h,
        &text,
        &Ask {
            flags: MATRIXED | MATRIX_OUT,
            matrix: Some(HALF),
            ..Ask::DEFAULT
        },
    )
    .expect("scanned");
    assert_eq!(
        (scanned.at, scanned.x),
        (29, 12000),
        "a matrix in the string replaces R6's"
    );
    assert_eq!(read_words(&mut s, MATRIX, 6), [65536, 0, 0, 65536, 0, 0]);
    write_words(&mut s, MATRIX, &[1, 2, 3, 4, 5, 6]);
    let at = write(&mut s, TEXT, b"AB");
    let identity = call(
        &mut s,
        SCAN_STRING,
        &[
            h,
            at,
            FONT | MATRIX_OUT,
            FAR.cast_unsigned(),
            FAR.cast_unsigned(),
            0,
            MATRIX,
        ],
    );
    assert!(identity.is_ok());
    assert_eq!(read_words(&mut s, MATRIX, 6), [65536, 0, 0, 65536, 0, 0]);
    assert_eq!(
        width(&mut s, h, &matrix(27, &[32768, 0, 0, 32768]), KERN),
        (12300, 0),
        "nor is it kerned across"
    );
}

#[test]
fn wide_strings_take_wide_units_and_a_utf8_font_reads_utf8() {
    let mut s = fonts();
    let h = find(&mut s, b"Scan", 192);
    let halfwords = |units: &[u16]| {
        units
            .iter()
            .flat_map(|u| u.to_le_bytes())
            .collect::<Vec<u8>>()
    };
    let words = |units: &[u32]| {
        units
            .iter()
            .flat_map(|u| u.to_le_bytes())
            .collect::<Vec<u8>>()
    };
    let wide = |s: &mut Session, font: u32, bytes: &[u8], flags: u32| {
        let at = write(s, TEXT, bytes);
        write(
            s,
            TEXT + u32::try_from(bytes.len()).expect("short"),
            &[0, 0, 0],
        );
        let regs = call(
            s,
            SCAN_STRING,
            &[
                font,
                at,
                FONT | flags,
                FAR.cast_unsigned(),
                FAR.cast_unsigned(),
            ],
        )
        .expect("scanned");
        (regs.r[1] - at, regs.r[3].cast_signed())
    };
    assert_eq!(
        wide(&mut s, h, &halfwords(&[65, 66, 0]), 1 << 12),
        (4, 16200)
    );
    assert_eq!(
        wide(&mut s, h, &halfwords(&[65, 0x141, 66, 0]), 1 << 12),
        (6, 16200)
    );
    assert_eq!(
        wide(
            &mut s,
            h,
            &halfwords(&[65, 9, 0xFC18, 0xFFFF, 66, 0]),
            1 << 12
        ),
        (10, 15200)
    );
    assert_eq!(
        wide(
            &mut s,
            h,
            &halfwords(&[65, 21, 0x100, 0x1F, 66, 0]),
            1 << 12
        ),
        (10, 16200)
    );
    assert_eq!(
        wide(
            &mut s,
            h,
            &words(&[65, 9, (-1000i32).cast_unsigned(), 66, 0]),
            1 << 13
        ),
        (16, 15200)
    );
    assert_eq!(
        wide(&mut s, h, &words(&[65, 66, 0]), 3 << 12),
        (8, 16200),
        "words where both are asked"
    );
    let utf8 = find(&mut s, b"Scan\\EUTF8", 192);
    assert_eq!(wide(&mut s, utf8, &[b'A', 0xC2, 0xA3, b'B'], 0), (4, 16200));
    assert_eq!(
        wide(&mut s, utf8, &[b'A', 0xC2, b'B'], 0),
        (3, 16200),
        "a bad lead byte is itself"
    );
    assert_eq!(
        wide(
            &mut s,
            utf8,
            &halfwords(&[65, 0xD800, 0xDC00, 66, 0]),
            1 << 12
        ),
        (8, 16200)
    );
    assert_eq!(
        wide(&mut s, utf8, b"AB", KERN),
        (2, 15000),
        "kerned by glyph"
    );
}

#[test]
fn the_old_calls_measure_as_they_always_did() {
    let mut s = fonts();
    let h = find(&mut s, b"Scan", 192);
    call(&mut s, SET_FONT, &[h]).expect("set");
    let sw = |s: &mut Session, text: &[u8], x: i32, y: i32, split: i32, length: u32| {
        let at = write(s, TEXT, text);
        let r = call(
            s,
            STRING_WIDTH,
            &[
                0,
                at,
                x.cast_unsigned(),
                y.cast_unsigned(),
                split.cast_unsigned(),
                length,
            ],
        )
        .expect("width");
        (r.r[2].cast_signed(), r.r[3].cast_signed(), r.r[4], r.r[5])
    };
    assert_eq!(
        sw(&mut s, b"A B", FAR, FAR, NONE, u32::MAX),
        (19200, 0, 3, 3)
    );
    assert_eq!(
        sw(&mut s, b"AB", FAR, FAR, NONE, u32::MAX).0,
        16200,
        "never kerned"
    );
    assert_eq!(
        sw(&mut s, b"ABCABC", 10000, FAR, NONE, u32::MAX),
        (8400, 0, 1, 1)
    );
    assert_eq!(
        sw(&mut s, b"A B A B", 10000, FAR, 32, u32::MAX),
        (8400, 0, 0, 1)
    );
    assert_eq!(
        sw(&mut s, b"A B C", 20000, FAR, 32, u32::MAX),
        (19200, 0, 1, 3)
    );
    assert_eq!(
        sw(&mut s, b"A B", FAR, -1, NONE, u32::MAX).3,
        3,
        "an axis that does not move is not checked"
    );
    assert_eq!(
        sw(&mut s, b"A\x0B\xE8\x03\x00B", FAR, -1, NONE, u32::MAX),
        (8400, 0, 1, 1)
    );
    assert_eq!(
        sw(&mut s, b"A\x0B\x18\xFC\xFFB", FAR, -500, NONE, u32::MAX).3,
        6,
        "past it upwards only"
    );
    assert_eq!(sw(&mut s, b"ZA", -1, FAR, NONE, u32::MAX), (0, 0, 1, 1));
    assert_eq!(sw(&mut s, b"A B", FAR, FAR, NONE, 1), (8400, 0, 1, 1));
}

#[test]
fn the_old_caret_calls_measure_as_they_always_did() {
    let mut s = fonts();
    let h = find(&mut s, b"Scan", 192);
    call(&mut s, SET_FONT, &[h]).expect("set");
    let caret = |s: &mut Session, call_number: u32, text: &[u8], x: i32, y: i32, extra: i32| {
        let at = write(s, TEXT, text);
        let r = call(
            s,
            call_number,
            &[
                0,
                at,
                x.cast_unsigned(),
                y.cast_unsigned(),
                extra.cast_unsigned(),
                5000,
            ],
        )
        .expect("caret");
        (r.r[2].cast_signed(), r.r[3].cast_signed(), r.r[4], r.r[5])
    };
    assert_eq!(
        caret(&mut s, FIND_CARET, b"ABC", 12301, 0, 0),
        (16200, 0, 2, 2)
    );
    assert_eq!(caret(&mut s, FIND_CARET, b"ABC", 4200, 0, 0), (0, 0, 0, 0));
    assert_eq!(
        caret(
            &mut s,
            FIND_CARET,
            b"A\x0B\x20\x4E\x00\x09\x30\xDF\xFFB",
            7800,
            20000,
            0
        ),
        (8400, 0, 1, 1)
    );
    assert_eq!(
        caret(&mut s, FIND_CARET_J, b"A B", 12000, 0, 1000),
        (12400, 0, 2, 2)
    );
    assert_eq!(
        caret(&mut s, FIND_CARET_J, b"A B", 30000, 0, 1000),
        (20200, 0, 3, 3),
        "its y offset is not used"
    );
    assert_eq!(
        caret(&mut s, FIND_CARET, b"AB", i32::MIN, i32::MIN, 0),
        (0, 0, 0, 0),
        "a point two words' reach away from the pen is measured to, not overflowed past"
    );
}

#[test]
fn the_old_string_box_measures_as_it_always_did() {
    let mut s = fonts();
    let h = find(&mut s, b"Scan", 192);
    call(&mut s, SET_FONT, &[h]).expect("set");
    let string_bbox = |s: &mut Session, text: &[u8]| {
        let at = write(s, TEXT, text);
        let r = call(s, STRING_BBOX, &[0, at]).expect("box");
        r.r[1..5]
            .iter()
            .map(|v| v.cast_signed())
            .collect::<Vec<i32>>()
    };
    assert_eq!(string_bbox(&mut s, b"AB"), [600, 0, 15612, 8412]);
    assert_eq!(string_bbox(&mut s, b""), [0, 0, 0, 0]);
    assert_eq!(string_bbox(&mut s, b"  "), [0, 0, 3000, 0]);
}

#[test]
fn character_boxes_in_os_units_are_rounded_pixels_two_further_out() {
    let mut s = fonts();
    let eigen = |s: &mut Session, variable| {
        let mut regs = Regs::default();
        regs.r[..2].copy_from_slice(&[u32::MAX, variable]);
        s.kernel
            .os()
            .perform(
                tairix_riscos_kernel::swi::X | tairix_riscos_kernel::swi::os::READ_MODE_VARIABLE,
                &mut regs,
            )
            .expect("read");
        regs.r[2]
    };
    assert_eq!(
        (eigen(&mut s, 4), eigen(&mut s, 5)),
        (1, 1),
        "the probes' mode"
    );
    let h = find(&mut s, b"Scan", 192);
    let cbb = |s: &mut Session, font: u32, code: u32, flags: u32| {
        let r = call(s, CHAR_BBOX, &[font, code, flags]).expect("box");
        r.r[1..5]
            .iter()
            .map(|v| v.cast_signed())
            .collect::<Vec<i32>>()
    };
    assert_eq!(cbb(&mut s, h, 65, 0), [600, 0, 7812, 8412]);
    assert_eq!(cbb(&mut s, h, 69, 0), [-1200, 1200, 13212, 10812]);
    for (code, expected) in [
        (32, [-4, -4, 4, 4]),
        (65, [-2, -4, 24, 26]),
        (66, [-2, -4, 22, 26]),
        (67, [-4, -10, 14, 20]),
        (68, [-4, -4, 4, 4]),
        (69, [-6, -2, 38, 32]),
        (105, [-2, -4, 10, 20]),
        (90, [0, 0, 0, 0]),
    ] {
        assert_eq!(cbb(&mut s, h, code, 1 << 4), expected, "{code}");
    }
    let tall = find_at(&mut s, b"Scan", (160, 240), (90, 45)).expect("10 by 15");
    assert_eq!(cbb(&mut s, tall, 65, 1 << 4), [-2, -4, 20, 18]);
    assert_eq!(cbb(&mut s, tall, 69, 0), [-1000, 1500, 11010, 13515]);
    let sheared = find(&mut s, b"Scan\\M65536 0 16384 65536 100 -50", 192);
    assert_eq!(cbb(&mut s, sheared, 69, 0), [-800, 1150, 16015, 10762]);
    assert_eq!(cbb(&mut s, sheared, 65, 1 << 4), [-2, -4, 30, 24]);
    let info = |s: &mut Session, font: u32| {
        let r = call(s, READ_INFO, &[font]).expect("info");
        r.r[1..5]
            .iter()
            .map(|v| v.cast_signed())
            .collect::<Vec<i32>>()
    };
    assert_eq!(info(&mut s, h), [-2, -6, 34, 28]);
    let small = find(&mut s, b"Scan", 100);
    assert_eq!(info(&mut s, small), [-2, -4, 18, 14]);
    assert_eq!(info(&mut s, sheared), [-4, -6, 40, 26]);
    assert_eq!(call(&mut s, READ_INFO, &[0]).map(|_| ()), Err(0x20A));
}

#[test]
fn font_metrics_are_read_as_risc_os_5_30_lays_them_out() {
    let mut s = fonts();
    let h = find(&mut s, b"Scan", 192);
    let sizes = call(&mut s, READ_FONT_METRICS, &[h, 0, 0, 0, 0, 0, 0, 0]).expect("sizes");
    assert_eq!(sizes.r[..6], [0xC, 4096, 1024, 0, 0, 1064]);
    let (boxes, widths, kerns, new) = (0x20000, 0x21000, 0x22000, 0x23000);
    let read = call(
        &mut s,
        READ_FONT_METRICS,
        &[h, boxes, widths, 0, 0, kerns, new, 0],
    )
    .expect("read");
    assert_eq!(read.r[..6], sizes.r[..6]);
    assert_eq!(read_words(&mut s, boxes + 16 * 65, 4), [600, 0, 7812, 8412]);
    assert_eq!(read_words(&mut s, boxes + 16 * 90, 4), [0, 0, 0, 0]);
    assert_eq!(read_words(&mut s, widths + 4 * 65, 1), [8400]);
    let table = read_words(&mut s, kerns, 266);
    assert_eq!(
        (table[0], table[85], table[86], table[87], table[101], table[102], table[256]),
        (1040, 1040, 1048, 1056, 1056, 1064, 1064)
    );
    assert_eq!(table[257..266], [12, 0, 0, 65, -12, 66, 444, 65, -1200]);
    let short = read_words(&mut s, new, 263);
    assert_eq!(short[257].cast_unsigned(), 0x8000_000C);
    assert_eq!(
        short[260..263]
            .iter()
            .map(|w| w.cast_unsigned())
            .collect::<Vec<u32>>(),
        [0xFFFF_4141, 0x0025_4142, 0xFF9C_4241],
        "a pair a word, in glyphs and thousandths"
    );
    let small = find(&mut s, b"Scan", 100);
    call(
        &mut s,
        READ_FONT_METRICS,
        &[small, 0, widths, 0, 0, kerns, 0, 0],
    )
    .expect("read");
    assert_eq!(read_words(&mut s, widths + 4 * 66, 1), [4062]);
    assert_eq!(
        read_words(&mut s, kerns + 1040, 6),
        [65, -7, 66, 231, 65, -625]
    );
    let sheared = find(&mut s, b"Scan\\M65536 0 0 65536 0 0", 192);
    assert_eq!(
        call(&mut s, READ_FONT_METRICS, &[sheared, 0, 0, 0, 0, 0, 0, 0]).map(|_| ()),
        Err(0x21C)
    );
    let other = find(&mut s, b"Scan2", 192);
    let none = call(&mut s, READ_FONT_METRICS, &[other, 0, 0, 0, 0, 0, 0, 0]).expect("sizes");
    assert_eq!(none.r[..7], [4, 4096, 1024, 0, 0, 0, 0]);
}

#[test]
fn a_walk_leaves_its_font_and_colours_for_future_font() {
    let mut s = fonts();
    let h = find(&mut s, b"Scan", 192);
    let other = find(&mut s, b"Scan2", 192);
    call(
        &mut s,
        SET_FONT_COLOURS,
        &[h, 0, 7, (-3i32).cast_unsigned()],
    )
    .expect("colours");
    let current = call(&mut s, CURRENT_FONT, &[]).expect("current");
    assert_eq!(current.r[..4], [h, 0, 7, 253], "the offset as a byte");
    assert_eq!(call(&mut s, CURRENT_RGB, &[]).map(|_| ()), Err(0x21D));
    let text = [
        b'A',
        26,
        u8::try_from(other).expect("a handle"),
        18,
        4,
        5,
        6,
        b'B',
    ];
    scan(&mut s, h, &text, &Ask::DEFAULT).expect("scanned");
    let future = call(&mut s, FUTURE_FONT, &[]).expect("future");
    assert_eq!(future.r[..4], [other, 4, 5, 6]);
    scan(
        &mut s,
        h,
        b"A\x13\x11\x22\x33\x44\x55\x66\x05B",
        &Ask::DEFAULT,
    )
    .expect("scanned");
    let rgb = call(&mut s, FUTURE_RGB, &[]).expect("future RGB");
    assert_eq!(rgb.r[..4], [h, 0x3322_1100, 0x6655_4400, 5]);
}

#[test]
fn reserved_flags_are_refused() {
    let mut s = fonts();
    let h = find(&mut s, b"Scan", 192);
    for bit in [0, 1, 2, 3, 4, 11, 14, 15, 16, 21, 31] {
        assert_eq!(
            scan(
                &mut s,
                h,
                b"AB",
                &Ask {
                    flags: 1 << bit,
                    ..Ask::DEFAULT
                }
            ),
            Err(0x213),
            "bit {bit}"
        );
    }
    let counted = scan(
        &mut s,
        h,
        b"A B",
        &Ask {
            flags: COUNT,
            r7: 55,
            ..Ask::DEFAULT
        },
    )
    .expect("scanned");
    assert_eq!(counted.r7, 3, "a count is given without bit 7");
    assert_eq!(scan(&mut s, 77, b"AB", &Ask::DEFAULT), Err(0x208));
    assert_eq!(scan(&mut s, 256, b"AB", &Ask::DEFAULT), Err(0x20A));
}

#[test]
fn a_font_s_data_files_and_base_encoding_are_found_as_the_ucs_font_manager_finds_them() {
    let mut s = fonts();
    let w = |s: &mut Session, identifier: &[u8], text: &[u8]| {
        let found = find(s, identifier, 16);
        width(s, found, text, 0).0
    };
    assert_eq!(
        w(&mut s, b"SymEnc", b"!\"#"),
        2033 + 2034 + 2035,
        "a symbol font's codes are its glyphs"
    );
    assert_eq!(
        w(&mut s, b"SymEnc\\ELatin1", b"ABC"),
        2034 + 2033 + 2035,
        "its own encoding is its base"
    );
    assert_eq!(
        w(&mut s, b"SymNoEnc\\ELatin1", b"!A"),
        1033,
        "/Default is its base"
    );
    assert_eq!(
        w(&mut s, b"Base1F", b"ABC"),
        4035 + 4034 + 4033,
        "IntMetric1 reads through /Base1"
    );
    assert_eq!(w(&mut s, b"Base32F", b"ABC"), 0, "no /Base32 maps nothing");
    assert_eq!(w(&mut s, b"Base32F\\EGlyph", b"!"), 3033);
    assert_eq!(
        w(&mut s, b"TwoSets", b"ABC"),
        5065 + 5066 + 5067,
        "the first metrics file"
    );
    let defn = |s: &mut Session, identifier: &[u8]| {
        let found = find(s, identifier, 16);
        call(
            s,
            READ_DEFN,
            &[found, BLOCK, 0, u32::from_le_bytes(*b"FULL")],
        )
        .expect("defn");
        s.kernel
            .os()
            .read_until(BLOCK, 64, |b| b == 0)
            .expect("read")
            .0
    };
    assert_eq!(defn(&mut s, b"SymEnc"), b"\\FSymEnc");
    assert_eq!(defn(&mut s, b"Scan\\EGlyph"), b"\\FScan");
    let enumerate = |s: &mut Session, identifier: &[u8], code: u32| {
        let found = find(s, identifier, 16);
        call(s, ENUMERATE_CHARACTERS, &[found, code])
            .map(|r| (r.r[1].cast_signed(), r.r[2].cast_signed()))
    };
    assert_eq!(enumerate(&mut s, b"SymEnc\\ELatin1", 0), Ok((0x20, -1)));
    assert_eq!(enumerate(&mut s, b"SymEnc\\ELatin1", 0x41), Ok((0x42, 34)));
    assert_eq!(
        enumerate(&mut s, b"SymEnc\\ELatin1", 0xA0),
        Ok((0xA1, 32)),
        "the last maps on past itself"
    );
    assert_eq!(enumerate(&mut s, b"SymEnc\\ELatin1", 0xA1), Ok((-1, -1)));
    assert_eq!(enumerate(&mut s, b"SymEnc", 3), Ok((4, 3)));
    assert_eq!(
        enumerate(&mut s, b"SymEnc", 63),
        Ok((-1, 63)),
        "two chunks of glyphs"
    );
    assert_eq!(enumerate(&mut s, b"Base32F", 0), Err(0x21E));
    let malformed =
        |s: &mut Session, identifier: &[u8]| find_at(s, identifier, (192, 192), (0, 0)).map(|_| ());
    assert_eq!(malformed(&mut s, b"BadMet"), Err(0x226));
    assert_eq!(malformed(&mut s, b"BadOut"), Err(0x226));
    assert_eq!(malformed(&mut s, b"RedirNone"), Err(0x20B));
    assert_eq!(malformed(&mut s, b"NoOut"), Err(0x20B));
    let scan_font = find(&mut s, b"Scan", 192);
    let lookup = call(&mut s, LOOKUP_FONT, &[scan_font, 0, 0]).expect("characteristics");
    assert_eq!(lookup.r[2], 0);
}
