use super::*;
use crate::drawfile::text_area::tests::{area, object};

const PLACES: Places = Places {
    buffer: 0x9000,
    block: 0x9200,
    matrix: 0x9300,
    name: 0x9400,
    name_bytes: 256,
};

/// The column the probes laid out in: 100 points to 200 both ways.
const COLUMN: [i32; 4] = [64_000, 64_000, 128_000, 128_000];

/// A painter over one column `column`, its buffer holding `text`.
fn painted<'a>(bytes: &'a [u8], text: &[u8]) -> Painter<'a> {
    let mut painter = Painter::new(&area(bytes), &Matrix::IDENTITY, PLACES);
    painter.buffer[PREFIX..PREFIX + text.len()].copy_from_slice(text);
    painter.len = text.len();
    painter
}

fn column(column: [i32; 4]) -> alloc::vec::Vec<u8> {
    object(column, &[column], 0xFF00, 0xFFFF_FF00, b"\\! 1\n")
}

/// Where `needle` first is in `text`.
fn at(text: &[u8], needle: &[u8]) -> usize {
    text.windows(needle.len())
        .position(|w| w == needle)
        .expect("there")
}

#[test]
fn a_line_breaks_at_its_last_space_its_trailing_spaces_dropped() {
    let bytes = column(COLUMN);
    // RISC OS 5.30 split at the second `e` and painted to the space before.
    let text = b"\x1A\x01 aaaa bbbb cccc dddd eeee ffff";
    let painter = painted(&bytes, text);
    let split = at(text, b"eeee") + 1;
    let space = at(text, b" eeee");
    assert_eq!(
        painter.break_at(split),
        Break {
            end: space,
            next: space + 1,
            hyphen: None
        }
    );
    let text = b"aaaa bbbb cccc dddd   eeee";
    let painter = painted(&bytes, text);
    let e = at(text, b"eeee");
    assert_eq!(
        painter.break_at(e),
        Break {
            end: at(text, b"   "),
            next: e,
            hyphen: None
        }
    );
}

#[test]
fn a_break_at_the_first_of_several_spaces_starts_the_next_line_past_them() {
    let bytes = column(COLUMN);
    let text = b"aaaaaaaaaaaaaaaaaaa.   bb";
    let painter = painted(&bytes, text);
    let first_space = at(text, b" ");
    assert_eq!(
        painter.break_at(first_space),
        Break {
            end: first_space,
            next: at(text, b"bb"),
            hyphen: None
        }
    );
}

#[test]
fn a_soft_hyphen_breaks_a_word_and_shows() {
    let bytes = column(COLUMN);
    let text = b"aaaaaaaaaaaaaaaaaaa\x15\x00bbbb";
    let painter = painted(&bytes, text);
    let hyphen = at(text, b"\x15");
    assert_eq!(
        painter.break_at(hyphen + 2),
        Break {
            end: hyphen + 1,
            next: hyphen + 2,
            hyphen: Some(hyphen)
        }
    );
}

#[test]
fn a_colour_change_holding_space_and_comment_bytes_is_no_break() {
    // RISC OS 5.30 broke "...cccc ddd<19 FF FF FF 32 32 32 14>deeee" before
    // "ddd", reading through the code.
    let bytes = column(COLUMN);
    for channel in [b' ', 0x15] {
        let mut text = b"aaaa bbbb cccc ddd\x13\xFF\xFF\xFF".to_vec();
        text.extend([channel, channel, channel, 14]);
        text.extend_from_slice(b"deeee ffff");
        let painter = painted(&bytes, &text);
        let split = at(&text, b"eeee") + 2;
        let space = at(&text, b" ddd");
        assert_eq!(
            painter.break_at(split),
            Break {
                end: space,
                next: space + 1,
                hyphen: None
            }
        );
    }
}

#[test]
fn a_word_longer_than_its_line_breaks_where_the_scan_stopped_and_one_that_never_fits_after_a_character(
) {
    let bytes = column(COLUMN);
    let text = b"mmmmmmmmmmmmmmmmmmmmmmmmm";
    let painter = painted(&bytes, text);
    assert_eq!(
        painter.break_at(12),
        Break {
            end: 12,
            next: 12,
            hyphen: None
        }
    );
    let text = b"\x1A\x01mm";
    let mut painter = painted(&bytes, text);
    assert_eq!(
        painter.break_at(2),
        Break {
            end: 2,
            next: 2,
            hyphen: None
        },
        "the code a line of its own"
    );
    painter.start = 2;
    assert_eq!(
        painter.break_at(2),
        Break {
            end: 3,
            next: 3,
            hyphen: None
        },
        "then a character at least"
    );
    painter.start = 0;
    assert_eq!(
        painter.break_at(0),
        Break {
            end: 3,
            next: 3,
            hyphen: None
        },
        "past the code and one character"
    );
}

#[test]
fn the_next_line_is_underlined_as_the_last_code_of_this_one_leaves_it() {
    let bytes = column(COLUMN);
    let painter = painted(&bytes, b"\x19\x0A\x14aaaa \x19\x05\x09bbbb");
    assert_eq!(painter.underline_after(0, painter.len), Some((5, 9)));
    let painter = painted(&bytes, b"\x19\x0A\x14aaaa \x19\x00\x00bbbb");
    assert_eq!(painter.underline_after(0, painter.len), None, "turned off");
    let painter = painted(&bytes, b"aaaa bbbb");
    assert_eq!(
        painter.underline_after(0, painter.len),
        None,
        "none in the line: none after it"
    );
    let painter = painted(&bytes, b"\x19\x80\x14x");
    assert_eq!(painter.underline_after(0, painter.len), Some((-128, 20)));
}

#[test]
fn a_justified_line_counts_its_spaces_outside_codes() {
    let bytes = column(COLUMN);
    let painter = painted(&bytes, b"\x1A\x01 mm \x13\x20\x20\x20\x20\x20\x20\x0Emm mm");
    assert_eq!(painter.spaces(0, painter.len), 3);
}

#[test]
fn a_column_less_its_margins_is_the_scan_s_width_in_whole_millipoints() {
    let bytes = column(COLUMN);
    let painter = painted(&bytes, b"");
    let room = painter.room();
    assert_eq!(room.width, 98_000);
    assert_eq!(
        (room.left / PER_MILLIPOINT, room.right / PER_MILLIPOINT),
        (101_000, 199_000)
    );
    // A column 64001 draw units wide is 100,001.5625 millipoints, cut.
    let bytes = column([-64_001, 64_000, 0, 128_000]);
    let painter = painted(&bytes, b"");
    assert_eq!(painter.room().width, 98_001);
}

#[test]
fn a_point_goes_through_the_render_matrix_to_millipoints_cut_toward_nought() {
    let bytes = column(COLUMN);
    let mut painter = painted(&bytes, b"");
    let point = |mp: i64| mp * PER_MILLIPOINT;
    assert_eq!(
        painter.millipoints(point(101_000), point(190_000)),
        (101_000, 190_000)
    );
    // RISC OS 5.30's line starts under a quarter turn and a 2x stretch.
    painter.render = Matrix {
        a: 0,
        b: 0x1_0000,
        c: -0x1_0000,
        d: 0,
        e: 256_000,
        f: 0,
    };
    assert_eq!(
        painter.millipoints(point(101_000), point(190_000)),
        (210_000, 101_000)
    );
    painter.render = Matrix {
        a: 0x2_0000,
        ..Matrix::IDENTITY
    };
    assert_eq!(
        painter.millipoints(point(101_000), point(190_000)),
        (202_000, 190_000)
    );
    // Column 64001: x 101,001.5625 and y 190,001.5625, through 0.7.
    painter.render = Matrix {
        a: 45875,
        d: 45875,
        ..Matrix::IDENTITY
    };
    let (x, y) = (
        64001 * PER_DRAW_UNIT + point(1000),
        128_001 * PER_DRAW_UNIT - point(10_000),
    );
    assert_eq!(painter.millipoints(x, y), (70_700, 133_000));
    painter.render = Matrix::IDENTITY;
    assert_eq!(
        painter.millipoints(-64001 * PER_DRAW_UNIT + point(1000), 0),
        (-99_001, 0)
    );
}
