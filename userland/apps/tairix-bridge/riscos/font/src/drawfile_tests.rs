//! DrawFile's text areas painted through this Font Manager, each held to
//! the lines RISC OS 5.30's DrawFile painted for the same text: the area is
//! written as Draw objects by `Font_SwitchOutputToBuffer`, and so are those
//! lines, painted by hand where and how genuine painted them.

use alloc::vec::Vec;

use tairix_riscos_draw::fixture;
use tairix_riscos_filing::session::Session;
use tairix_riscos_kernel::os::Regs;
use tairix_riscos_kernel::swi::{chunk, colour_trans, X};

use crate::fixture::{call, find, fonts, mode, read_words, write, write_words};
use crate::module::call::{PAINT, SWITCH_OUTPUT_TO_BUFFER};

/// Where the file, the hand-painted strings and their blocks go, and the
/// buffer output is written to.
const FILE: u32 = 0x10000;
const STRINGS: u32 = 0x20000;
const COORDINATES: u32 = 0x21000;
const IDENTITY: u32 = 0x21100;
const BUFFER: u32 = 0x30000;
const ROOM: i32 = 0x2_0000;

/// The column the probes painted in, 100 points to 200 both ways, and the
/// area's colours.
const COLUMN: [i32; 4] = [64_000, 64_000, 128_000, 128_000];
const RED: u32 = 0x0000_FF00;
const WHITE: u32 = 0xFFFF_FF00;

/// `Font_Paint`'s flags for a text area's line: a coordinate block, a
/// matrix and a length, in millipoints.
const LINE: u32 = 0xE0;

/// Mode 28, as the probes ran in, and Lay's handle at `sixteenths`.
fn session() -> (Session, u32) {
    let mut s = fonts();
    mode(&mut s, 28);
    let lay = find(&mut s, b"Lay", 160);
    (s, lay)
}

/// What `draw` writes while output goes to the buffer.
fn written(s: &mut Session, draw: impl FnOnce(&mut Session)) -> Vec<i32> {
    write_words(s, BUFFER, &[0, ROOM]);
    call(s, SWITCH_OUTPUT_TO_BUFFER, &[0, BUFFER]).expect("switched");
    draw(s);
    let end = call(s, SWITCH_OUTPUT_TO_BUFFER, &[0, 0])
        .expect("switched back")
        .r[1];
    read_words(s, BUFFER, (end - BUFFER) / 4)
}

/// `text` in a text area of `columns` within `bounds`, rendered by DrawFile.
fn rendered(s: &mut Session, bounds: [i32; 4], columns: &[[i32; 4]], text: &[u8]) -> Vec<i32> {
    let area = fixture::text_area(bounds, columns, RED, WHITE, text);
    let file = fixture::file(bounds, &[&area]);
    write(s, FILE, &file);
    let size = u32::try_from(file.len()).expect("small");
    written(s, |s| {
        let mut regs = Regs::new([0, FILE, size, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        s.kernel
            .os()
            .perform(X | chunk::DRAW_FILE, &mut regs)
            .expect("rendered");
    })
}

/// A line as genuine RISC OS painted it: its bytes, its place in
/// millipoints and what it added to each space.
struct Line {
    text: Vec<u8>,
    at: (i32, i32),
    spacing: i32,
}

fn line(text: impl Into<Vec<u8>>, x: i32, y: i32) -> Line {
    Line {
        text: text.into(),
        at: (x, y),
        spacing: 0,
    }
}

/// `lines` painted by hand, the area's colours set as DrawFile sets them.
fn painted(s: &mut Session, lines: &[Line]) -> Vec<i32> {
    written(s, |s| {
        let mut regs = Regs::new([0, WHITE, RED, 14, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        s.kernel
            .os()
            .perform(X | colour_trans::SET_FONT_COLOURS, &mut regs)
            .expect("the colours");
        write_words(s, IDENTITY, &[0x1_0000, 0, 0, 0x1_0000, 0, 0]);
        for line in lines {
            write(s, STRINGS, &line.text);
            write_words(s, COORDINATES, &[line.spacing, 0, 0, 0, 0, 0, 0, 0]);
            let length = u32::try_from(line.text.len()).expect("short");
            let [x, y] = [line.at.0, line.at.1].map(i32::cast_unsigned);
            call(
                s,
                PAINT,
                &[0, STRINGS, LINE, x, y, COORDINATES, IDENTITY, length],
            )
            .expect("painted");
        }
    })
}

/// `bytes` after the font change to `handle`.
fn after_font(handle: u32, bytes: &[u8]) -> Vec<u8> {
    let mut text = alloc::vec![26, u8::try_from(handle).expect("a byte")];
    text.extend_from_slice(bytes);
    text
}

/// `text` in the probes' column rendered as the lines `lines` makes of
/// Lay's handle.
fn holds(text: &[u8], lines: impl FnOnce(u32) -> Vec<Line>) {
    let (mut s, lay) = session();
    let ours = rendered(&mut s, COLUMN, &[COLUMN], text);
    let genuine = painted(&mut s, &lines(lay));
    assert!(!genuine.is_empty(), "the lines painted something");
    assert_eq!(ours, genuine);
}

#[test]
fn a_paragraph_breaks_at_the_last_space_that_fits() {
    holds(
        b"\\! 1\n\\F 1 Lay 10\n\\1 aaaa bbbb cccc dddd eeee ffff gggg hhhh\n",
        |lay| {
            alloc::vec![
                line(after_font(lay, b" aaaa bbbb cccc dddd"), 101_000, 190_000),
                line(b"eeee ffff gggg hhhh", 101_000, 180_000),
            ]
        },
    );
}

#[test]
fn right_centred_and_justified_lines_go_where_risc_os_put_them() {
    let text = |align: &str| {
        alloc::format!("\\! 1\n\\F 1 Lay 10\n\\A{align}\\1 aaaa bbbb cccc dddd eeee ffff\n")
    };
    let first = |lay| after_font(lay, b" aaaa bbbb cccc dddd");
    holds(text("R").as_bytes(), |lay| {
        alloc::vec![
            line(first(lay), 109_000, 190_000),
            line(b"eeee ffff", 156_500, 180_000)
        ]
    });
    holds(text("C").as_bytes(), |lay| {
        alloc::vec![
            line(first(lay), 105_000, 190_000),
            line(b"eeee ffff", 128_750, 180_000)
        ]
    });
    holds(text("D").as_bytes(), |lay| {
        alloc::vec![
            Line {
                spacing: 2000,
                ..line(first(lay), 101_000, 190_000)
            },
            line(b"eeee ffff", 101_000, 180_000),
        ]
    });
}

#[test]
fn colours_underlines_and_moves_travel_as_codes_in_the_line() {
    holds(
        b"\\! 1\n\\F 1 Lay 10\n\\1 ab \\C 255 0 0/cd \\B 0 0 255/ef\n",
        |lay| {
            let text = b" ab \x13\xFF\xFF\xFF\xFF\x00\x00\x0Ecd \x13\x00\x00\xFF\xFF\x00\x00\x0Eef";
            alloc::vec![line(after_font(lay, text), 101_000, 190_000)]
        },
    );
    holds(
        b"\\! 1\n\\F 1 Lay 10\n\\1 ab \\U 10 20/cd\\U./ ef \\V 3/gh\\V-3/ij\n",
        |lay| {
            let text = b" ab \x19\x0A\x14cd\x19\x00\x00 ef \x0B\xB8\x0B\x00gh\x0B\x48\xF4\xFFij";
            alloc::vec![line(after_font(lay, text), 101_000, 190_000)]
        },
    );
}

#[test]
fn an_underline_is_carried_to_the_next_line_alone() {
    let text = b"\\! 1\n\\F 1 Lay 10\n\\1 \\U 10 20/aaaa bbbb cccc dddd eeee ffff gggg hhhh iiii jjjj kkkk llll\n";
    holds(text, |lay| {
        alloc::vec![
            line(
                after_font(lay, b" \x19\x0A\x14aaaa bbbb cccc dddd"),
                101_000,
                190_000
            ),
            line(b"\x19\x0A\x14eeee ffff gggg hhhh", 101_000, 180_000),
            line(b"iiii jjjj kkkk llll", 101_000, 170_000),
        ]
    });
}

#[test]
fn margins_paragraphs_and_newlines_space_the_lines_as_risc_os_did() {
    holds(
        b"\\! 1\n\\F 1 Lay 10\n\\M 10 20\n\\1 aaaa bbbb cccc dddd eeee ffff gggg\n",
        |lay| {
            alloc::vec![
                line(after_font(lay, b" aaaa bbbb cccc"), 110_000, 190_000),
                line(b"dddd eeee ffff", 110_000, 180_000),
                line(b"gggg", 110_000, 170_000),
            ]
        },
    );
    holds(
        b"\\! 1\n\\F 1 Lay 10\n\\1 aa\nbb \ncc\n dd\tee\\\\ff\\\ngg\n",
        |lay| {
            alloc::vec![
                line(after_font(lay, b" aa bb  cc"), 101_000, 190_000),
                line(b" dd ee\\ff", 101_000, 170_000),
                line(b"gg", 101_000, 160_000),
            ]
        },
    );
    holds(
        b"\\! 1\n\\F 1 Lay 10\n\\L 15\n\\P 20\n\\1 aa\n\nbb\n\n\ncc\n",
        |lay| {
            alloc::vec![
                line(after_font(lay, b" aa"), 101_000, 185_000),
                line(b"bb", 101_000, 150_000)
            ]
        },
    );
    holds(b"\\! 1/\\F 1 Lay 10/\n\n\\1 aa\n", |lay| {
        alloc::vec![
            line(after_font(lay, b""), 101_000, 190_000),
            line(b" aa", 101_000, 180_000)
        ]
    });
}

#[test]
fn a_word_breaks_at_a_soft_hyphen_or_else_where_the_line_ends() {
    holds(
        b"\\! 1\n\\F 1 Lay 10\n\\1 aaaaaaaaaa\\-bbbbbbbbbb cccccccccccccccccccccccccccc\n",
        |lay| {
            alloc::vec![
                line(after_font(lay, b" aaaaaaaaaa-"), 101_000, 190_000),
                line(b"bbbbbbbbbb", 101_000, 180_000),
                line(b"ccccccccccccccccccc", 101_000, 170_000),
                line(b"ccccccccc", 101_000, 160_000),
            ]
        },
    );
}

#[test]
fn each_defined_font_is_changed_to_by_its_code() {
    let (mut s, lay) = session();
    let tall = crate::fixture::find_at(&mut s, b"Lay", (192, 320), (0, 0)).expect("found");
    let ours = rendered(
        &mut s,
        COLUMN,
        &[COLUMN],
        b"\\! 1\n\\F 1 Lay 10\n\\F 2 Lay 20 12\n\\1 aa \\2 bb\\1 cc\n",
    );
    let mut text = after_font(lay, b" aa ");
    text.extend_from_slice(&after_font(tall, b" bb"));
    text.extend_from_slice(&after_font(lay, b" cc"));
    let genuine = painted(&mut s, &[line(text, 101_000, 190_000)]);
    assert_eq!(ours, genuine);
}

#[test]
fn a_full_column_flows_into_the_next() {
    let (mut s, lay) = session();
    let bounds = [64_000, 64_000, 128_000, 80_000];
    let columns = [
        [64_000, 64_000, 96_000, 80_000],
        [96_000, 64_000, 128_000, 80_000],
    ];
    let text = b"\\! 1\n\\D 2\n\\F 1 Lay 10\n\\1 aaaa bbbb cccc dddd eeee ffff gggg hhhh iiii jjjj kkkk llll mmmm\n";
    let ours = rendered(&mut s, bounds, &columns, text);
    let genuine = painted(
        &mut s,
        &[
            line(after_font(lay, b" aaaa bbbb"), 101_000, 115_000),
            line(b"cccc dddd", 101_000, 105_000),
            line(b"eeee ffff", 151_000, 115_000),
            line(b"gggg hhhh", 151_000, 105_000),
        ],
    );
    assert_eq!(ours, genuine);
}

#[test]
fn a_line_longer_than_the_buffer_goes_out_in_pieces_along_it() {
    let (mut s, _) = session();
    let tiny = crate::fixture::find_at(&mut s, b"Lay", (16, 16), (0, 0)).expect("found");
    let wide = [64_000, 64_000, 64_000 + 409_600, 128_000];
    let mut text = b"\\! 1/\\F 1 Lay 1/\\1/".to_vec();
    text.extend([b'a'; 400]);
    let ours = rendered(&mut s, wide, &[wide], &text);
    let genuine = painted(
        &mut s,
        &[
            line(after_font(tiny, &[b'a'; 253]), 101_000, 190_000),
            line([b'a'; 147], 227_500, 190_000),
        ],
    );
    assert_eq!(ours, genuine);
}

/// Where a render's matrix goes.
const RENDER_MATRIX: u32 = 0x21200;
/// RISC OS 5.30's DrawFile paints a text in OS units, by a matrix, in a
/// handle.
const TEXT: u32 = 0x150;
/// The probes' text: "AB" in Lay at 16 points from (160, 200) OS units.
const AB: fixture::Text<'static> = fixture::Text {
    bounds: [40960, 51200, 51200, 61440],
    foreground: RED,
    background: WHITE,
    font: 1,
    sizes: (10240, 10240),
    at: (40960, 51200),
    string: b"AB",
};

/// A file of Lay's font table and `text`, transformed by `own`, rendered
/// through `render`: what it wrote, or the render's error.
fn text_rendered(
    s: &mut Session,
    text: &fixture::Text<'_>,
    own: Option<([i32; 6], u32)>,
    render: Option<[i32; 6]>,
) -> Result<Vec<i32>, u32> {
    let table = fixture::font_table(&[(1, b"Lay")]);
    let object = fixture::text(text, own);
    let file = fixture::file(text.bounds, &[&table, &object]);
    write(s, FILE, &file);
    let size = u32::try_from(file.len()).expect("small");
    let matrix = render.map_or(0, |words| {
        write_words(s, RENDER_MATRIX, &words);
        RENDER_MATRIX
    });
    let mut refused = None;
    let words = written(s, |s| {
        let mut regs = Regs::new([0, FILE, size, matrix, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let mut os = s.kernel.os();
        if let Err(tairix_riscos_kernel::os::Fail::Error(block)) =
            os.perform(X | chunk::DRAW_FILE, &mut regs)
        {
            refused = Some(os.read_word(block).expect("its number"));
        }
    });
    refused.map_or(Ok(words), Err)
}

/// `string` painted as genuine's DrawFile painted a text: in `handle` with
/// `flags`, at `at` in OS units, through `matrix`.
fn text_painted(
    s: &mut Session,
    handle: u32,
    string: &[u8],
    flags: u32,
    at: (i32, i32),
    matrix: [i32; 6],
) -> Vec<i32> {
    written(s, |s| {
        let mut regs = Regs::new([handle, WHITE, RED, 14, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        s.kernel
            .os()
            .perform(X | colour_trans::SET_FONT_COLOURS, &mut regs)
            .expect("the colours");
        write(s, STRINGS, string);
        write_words(s, IDENTITY, &matrix);
        let [x, y] = [at.0, at.1].map(i32::cast_unsigned);
        call(s, PAINT, &[handle, STRINGS, flags, x, y, 0, IDENTITY]).expect("painted");
    })
}

#[test]
fn a_text_is_painted_at_its_base_through_the_render_s_matrix_in_os_units() {
    let (mut s, _) = session();
    let lay16 = find(&mut s, b"Lay", 256);
    let ours = text_rendered(&mut s, &AB, None, None).expect("rendered");
    let genuine = text_painted(
        &mut s,
        lay16,
        b"AB",
        TEXT,
        (160, 200),
        [0x1_0000, 0, 0, 0x1_0000, 0, 0],
    );
    assert!(!genuine.is_empty());
    assert_eq!(ours, genuine);
    // Through 0.7 the base is (28671.9, 35839.8) draw units, cut: (111, 139).
    let seven = [45875, 0, 0, 45875, 0, 0];
    let ours = text_rendered(&mut s, &AB, None, Some(seven)).expect("rendered");
    assert_eq!(
        ours,
        text_painted(&mut s, lay16, b"AB", TEXT, (111, 139), seven)
    );
}

#[test]
fn a_transformed_text_s_matrix_follows_the_render_s_its_move_cut_to_os_units() {
    let (mut s, _) = session();
    let lay16 = find(&mut s, b"Lay", 256);
    let own = [19663, 0, 0, 0x1_0000, 2194, 0];
    let ours = text_rendered(
        &mut s,
        &AB,
        Some((own, 0)),
        Some([45875, 0, 0, 45875, 0, 0]),
    )
    .expect("rendered");
    let genuine = text_painted(
        &mut s,
        lay16,
        b"AB",
        TEXT,
        (111, 139),
        [13764, 0, 0, 45875, 5, 0],
    );
    assert_eq!(ours, genuine);
    // Its kerning and right-to-left flags are Font_Paint's bits 9 and 10.
    let ours = text_rendered(&mut s, &AB, Some((own, 0b11)), None).expect("rendered");
    let genuine = text_painted(
        &mut s,
        lay16,
        b"AB",
        TEXT | 0x600,
        (160, 200),
        [19663, 0, 0, 0x1_0000, 8, 0],
    );
    assert_eq!(ours, genuine);
}

#[test]
fn a_text_in_a_font_not_found_gives_the_render_its_error_and_paints_nothing() {
    let (mut s, _) = session();
    let table = fixture::font_table(&[(1, b"Nonesuch")]);
    let object = fixture::text(&AB, None);
    let file = fixture::file(AB.bounds, &[&table, &object]);
    write(&mut s, FILE, &file);
    let size = u32::try_from(file.len()).expect("small");
    let words = written(&mut s, |s| {
        let mut regs = Regs::new([0, FILE, size, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let mut os = s.kernel.os();
        let Err(tairix_riscos_kernel::os::Fail::Error(block)) =
            os.perform(X | chunk::DRAW_FILE, &mut regs)
        else {
            panic!("refused");
        };
        assert_eq!(
            os.read_word(block).expect("its number"),
            0x20B,
            "the Font Manager's own"
        );
    });
    assert!(
        words.is_empty(),
        "the system font stands in, through no font"
    );
}
