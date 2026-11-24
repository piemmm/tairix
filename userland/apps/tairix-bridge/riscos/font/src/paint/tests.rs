use alloc::vec::Vec;

use tairix_riscos_filing::session::Session;
use tairix_riscos_kernel::swi::font::flag::{HANDLE, WORDS};
use tairix_riscos_kernel::swi::font::TRUE_COLOURS;

use crate::fixture::{
    call, fill, find, find_at, font_paint, fonts, mode, pixels, write, write_words, AT, BLOCK, TEXT,
};
use crate::module::call::{
    CARET, CURRENT_FONT, PAINT, SET_FONT_COLOURS, SET_FONT_MAX, SET_PALETTE,
};

/// `FontMax` to `FontMax5` as the machine the probes ran on had them
/// configured.
const PROBED_FONT_MAX: [u32; 6] = [4_194_304, 16, 36, 36, 0, 0];

/// Mode 28 with white to black anti-aliased in fifteen steps, as
/// `ColourTrans_SetFontColours` sets them, its screen white, `FontMax` as
/// the probes had it.
fn mode_28() -> Session {
    let mut s = fonts();
    mode(&mut s, 28);
    call(&mut s, SET_FONT_MAX, &PROBED_FONT_MAX).expect("set");
    call(
        &mut s,
        SET_PALETTE,
        &[0, 0, 1, 14, 0xFFFF_FF00, 0, TRUE_COLOURS],
    )
    .expect("the palette");
    fill(&mut s, 0xFF);
    s
}

/// `Font_Paint` of `text` in `handle` with `flags` from the probes' point.
fn paint(s: &mut Session, handle: u32, text: &[u8], flags: u32) -> Result<(), u32> {
    font_paint(s, handle, text, flags, AT, [0, 0])
}

/// The rows, from the screen's top, of `rows` that hold any pixel other
/// than `background` within columns `left` to `left + width`, each as the
/// first and last such column.
fn inked(
    s: &mut Session,
    left: u32,
    width: u32,
    rows: core::ops::Range<u32>,
    background: u32,
) -> Vec<(u32, u32, u32)> {
    let start = rows.start;
    pixels(s, left, width, rows)
        .iter()
        .zip(start..)
        .filter_map(|(row, y)| {
            let first = row.iter().position(|&p| p != background)?;
            let last = row.iter().rposition(|&p| p != background)?;
            Some((
                y,
                left + u32::try_from(first).ok()?,
                left + u32::try_from(last).ok()?,
            ))
        })
        .collect()
}

#[test]
fn a_glyph_s_origin_is_the_pixel_its_pen_is_in() {
    let expected: Vec<(u32, u32, u32)> = (272..280).map(|y| (y, 125, 128)).collect();
    for (x, y) in [(100_000, 160_000), (100_799, 160_000), (100_000, 160_799)] {
        let mut s = mode_28();
        let handle = find(&mut s, b"Pix", 256);
        let at = write(&mut s, TEXT, b"A");
        call(&mut s, PAINT, &[handle, at, HANDLE, x, y]).expect("painted");
        assert_eq!(inked(&mut s, 120, 20, 265..285, 0xFF), expected, "{x} {y}");
        let solid = pixels(&mut s, 125, 4, 272..280);
        assert!(
            solid.iter().flatten().all(|&p| p == 0),
            "a pixel-aligned rectangle covers wholly"
        );
    }
}

#[test]
fn above_font_max_3_a_glyph_is_filled_as_draw_fills_it_in_the_last_colour() {
    let mut s = mode_28();
    let handle = find(&mut s, b"Pix", 640);
    paint(&mut s, handle, b"A", 0).expect("painted");
    let expected: Vec<(u32, u32, u32)> = (260..280).map(|y| (y, 125, 134)).collect();
    assert_eq!(inked(&mut s, 120, 26, 255..285, 0xFF), expected);
}

#[test]
fn a_rub_out_box_from_the_block_takes_its_bottom_left_and_leaves_its_top_right() {
    let mut s = mode_28();
    fill(&mut s, 0xD0);
    let handle = find(&mut s, b"Pix", 256);
    write_words(
        &mut s,
        BLOCK,
        &[0, 0, 0, 0, 99_200, 156_000, 104_800, 169_600],
    );
    let at = write(&mut s, TEXT, b"A");
    call(
        &mut s,
        PAINT,
        &[handle, at, HANDLE | 0x22, AT[0], AT[1], BLOCK],
    )
    .expect("painted");
    let rubbed = pixels(&mut s, 124, 8, 266..286);
    for (row, y) in rubbed.iter().zip(266u32..) {
        let inside = (268..=284).contains(&y);
        assert_eq!(row[0] == 0xFF, inside, "row {y}'s left");
        assert_eq!(row[7], 0xD0, "column 131 stays");
    }
}

#[test]
fn a_rub_out_box_from_the_graphics_cursor_takes_both_corners() {
    let mut s = mode_28();
    fill(&mut s, 0xD0);
    let handle = find(&mut s, b"Pix", 256);
    for (x, y) in [(248, 390), (262, 424)] {
        let mut regs = tairix_riscos_kernel::os::Regs::default();
        regs.r[..3].copy_from_slice(&[4, x, y]);
        s.kernel
            .os()
            .perform(
                tairix_riscos_kernel::swi::X | tairix_riscos_kernel::swi::os::PLOT,
                &mut regs,
            )
            .expect("moved");
    }
    let at = write(&mut s, TEXT, b"A");
    call(&mut s, PAINT, &[handle, at, HANDLE | 0x12, 250, 400]).expect("painted");
    let rows = inked(&mut s, 120, 16, 265..287, 0xD0);
    assert_eq!(rows.first().map(|r| r.0), Some(267));
    assert_eq!(rows.last().map(|r| r.0), Some(284));
    assert!(rows
        .iter()
        .all(|&(_, left, right)| (left, right) == (124, 131)));
}

#[test]
fn the_caret_is_a_bar_exclusive_ored_its_ends_a_pixel_either_side() {
    let mut s = mode_28();
    call(&mut s, CARET, &[0xFF, 40, 0x10, 250, 400]).expect("drawn");
    let mut expected = alloc::vec![(260, 124, 126)];
    expected.extend((261..279).map(|y| (y, 125, 125)));
    expected.push((279, 124, 126));
    assert_eq!(inked(&mut s, 120, 12, 255..285, 0xFF), expected);
    let ends = pixels(&mut s, 124, 3, 260..261);
    assert_eq!(
        ends[0],
        [0, 0xFF, 0],
        "the end rows leave the bar's own column"
    );
    call(&mut s, CARET, &[0xFF, 40, 0x10, 250, 400]).expect("drawn again");
    assert!(
        inked(&mut s, 120, 12, 255..285, 0xFF).is_empty(),
        "twice is nothing"
    );
}

#[test]
fn a_caret_taller_than_the_screen_is_cut_to_the_graphics_window() {
    let mut s = mode_28();
    call(&mut s, CARET, &[0xFF, 0x7FFF_FFFF, 0x10, 250, 400]).expect("drawn");
    let mut expected: Vec<(u32, u32, u32)> = (0..279).map(|y| (y, 125, 125)).collect();
    expected.push((279, 124, 126));
    assert_eq!(inked(&mut s, 120, 12, 0..285, 0xFF), expected);
}

#[test]
fn a_caret_past_the_last_pixel_a_word_reaches_draws_nothing() {
    let mut s = mode_28();
    let selector = BLOCK + 0x400;
    write_words(&mut s, selector, &[1, 640, 480, 3, -1, 4, 0, 5, 0, -1]);
    mode(&mut s, selector);
    fill(&mut s, 0xFF);
    for x in [0x8000_0000, 0x7FFF_FFFF] {
        call(&mut s, CARET, &[0xFF, 40, 0x10, x, 400]).expect("nothing to draw");
    }
    assert!(inked(&mut s, 0, 640, 0..480, 0xFF).is_empty());
}

#[test]
fn an_underline_set_at_the_ends_of_a_word_is_painted_without_fault() {
    let mut s = mode_28();
    let pix = find(&mut s, b"Pix", 256);
    write_words(
        &mut s,
        TEXT,
        &[
            25,
            i32::MIN,
            -1,
            i32::from(b'A'),
            25,
            i32::MAX,
            -1,
            i32::from(b'A'),
            0,
        ],
    );
    call(&mut s, PAINT, &[pix, TEXT, HANDLE | WORDS, AT[0], AT[1]]).expect("painted");
    assert!(
        !inked(&mut s, 120, 20, 260..285, 0xFF).is_empty(),
        "its characters, the bars off the screen"
    );
}

#[test]
fn flags_risc_os_5_30_reserves_are_refused_and_bits_2_and_3_are_not() {
    let mut s = mode_28();
    let handle = find(&mut s, b"Pix", 256);
    for flags in [1 << 15, 1 << 16, 1 << 21, 1 << 31, 0x30, 0x21] {
        assert_eq!(paint(&mut s, handle, b"A", flags), Err(0x213), "{flags:#x}");
    }
    for flags in [0x4, 0x8, 1 << 14, 0x3000] {
        assert_eq!(paint(&mut s, handle, b"A", flags), Ok(()), "{flags:#x}");
    }
    assert_eq!(
        paint(&mut s, handle, b"A", 0x4800),
        Err(0x227),
        "no supremacy in 256 colours"
    );
}

#[test]
fn a_deeper_mode_with_no_colours_for_the_foreground_paints_nothing() {
    let mut s = mode_28();
    let handle = find(&mut s, b"Pix", 256);
    call(&mut s, SET_FONT_COLOURS, &[0, 0, 2, 6]).expect("set");
    assert_eq!(paint(&mut s, handle, b"A", 0), Err(0x205));
    assert!(inked(&mut s, 120, 20, 265..285, 0xFF).is_empty());
}

#[test]
fn a_font_found_at_another_resolution_is_drawn_a_pixel_to_a_pixel() {
    let mut s = fonts();
    mode(&mut s, 12);
    call(&mut s, SET_FONT_COLOURS, &[0, 0, 1, 14]).expect("set");
    let own = find_at(&mut s, b"Pix", (256, 256), (90, 90)).expect("found");
    let mode_s = find_at(&mut s, b"Pix", (256, 256), (0, 0)).expect("found");
    paint(&mut s, own, b"A", 0).expect("painted");
    let tall: Vec<(u32, u32, u32)> = (148..156).map(|y| (y, 125, 128)).collect();
    assert_eq!(inked(&mut s, 120, 20, 140..160, 0), tall);
    mode(&mut s, 12);
    call(&mut s, SET_FONT_COLOURS, &[0, 0, 1, 14]).expect("set");
    paint(&mut s, mode_s, b"A", 0).expect("painted");
    let short: Vec<(u32, u32, u32)> = (152..156).map(|y| (y, 125, 128)).collect();
    assert_eq!(inked(&mut s, 120, 20, 140..160, 0), short);
}

#[test]
fn a_paint_leaves_its_registers_and_the_font_and_colours_its_string_set() {
    let mut s = fonts();
    mode(&mut s, 12);
    call(&mut s, SET_FONT_COLOURS, &[0, 0, 1, 14]).expect("set");
    let pix = find(&mut s, b"Pix", 256);
    let scan = find(&mut s, b"Scan", 256);
    let at = write(
        &mut s,
        TEXT,
        &[
            b'A',
            26,
            u8::try_from(pix).expect("a byte"),
            18,
            3,
            7,
            2,
            b'A',
        ],
    );
    let regs = call(&mut s, PAINT, &[scan, at, HANDLE, AT[0], AT[1], 5, 6, 7]).expect("painted");
    assert_eq!(regs.r[..8], [scan, at, HANDLE, AT[0], AT[1], 5, 6, 7]);
    let current = call(&mut s, CURRENT_FONT, &[]).expect("read");
    assert_eq!(current.r[..4], [pix, 3, 7, 2]);
}

#[test]
fn a_colour_change_in_the_string_paints_what_follows_in_its_colours() {
    let mut s = fonts();
    mode(&mut s, 12);
    call(&mut s, SET_FONT_COLOURS, &[0, 0, 1, 14]).expect("set");
    let pix = find(&mut s, b"Pix", 256);
    paint(&mut s, pix, &[b'A', 18, 0, 7, 0, b'A'], 0).expect("painted");
    let rows = pixels(&mut s, 125, 10, 155..156);
    assert_eq!(rows[0][..4], [15; 4]);
    assert_eq!(rows[0][6..10], [7; 4]);
}

/// The columns of row `y` holding any pixel but white, each run's first.
fn starts(s: &mut Session, y: u32) -> Vec<u32> {
    let row = &pixels(s, 100, 100, y..y + 1)[0];
    (0u32..)
        .zip(row.iter().zip(core::iter::once(&0xFF).chain(row.iter())))
        .filter(|&(_, (&p, &before))| p != 0xFF && before == 0xFF)
        .map(|(x, _)| 100 + x)
        .collect()
}

/// The graphics cursor moved to OS units `(x, y)`.
fn move_to(s: &mut Session, x: u32, y: u32) {
    let mut regs = tairix_riscos_kernel::os::Regs::default();
    regs.r[..3].copy_from_slice(&[4, x, y]);
    s.kernel
        .os()
        .perform(
            tairix_riscos_kernel::swi::X | tairix_riscos_kernel::swi::os::PLOT,
            &mut regs,
        )
        .expect("moved");
}

#[test]
fn justified_to_the_graphics_cursor_the_spaces_take_what_the_string_falls_short_by_either_way() {
    for (text, cursor, expected) in [
        (&b"A A A"[..], 330, &[125u32, 142, 159][..]),
        (b"A A A", 200, &[125, 134, 144]),
        (b"AAA", 330, &[125, 131, 137]),
        (b"A  A", 330, &[125, 159]),
    ] {
        let mut s = mode_28();
        let handle = find(&mut s, b"Pix", 256);
        move_to(&mut s, cursor, 400);
        paint(&mut s, handle, text, 1).expect("painted");
        assert_eq!(
            starts(&mut s, 279),
            expected,
            "{:?} to {cursor}",
            core::str::from_utf8(text)
        );
    }
}

#[test]
fn justification_is_across_alone_wherever_the_cursor_is_up() {
    let mut s = mode_28();
    let handle = find(&mut s, b"Pix", 256);
    move_to(&mut s, 330, 440);
    paint(&mut s, handle, b"A A A", 1).expect("painted");
    assert_eq!(
        starts(&mut s, 279),
        [125, 142, 159],
        "as RISC OS 5.30 painted it, the line unmoved"
    );
    assert_eq!(starts(&mut s, 272), [125, 142, 159], "its top row too");
    assert_eq!(
        starts(&mut s, 271),
        Vec::<u32>::new(),
        "and nothing above it"
    );
}

#[test]
fn font_max_2_and_3_hold_a_turned_or_sheared_font_to_its_second_row() {
    let partial = |s: &mut Session| {
        pixels(s, 0, 640, 0..480)
            .iter()
            .flatten()
            .filter(|&&p| p != 0 && p != 0xFF)
            .count()
    };
    for (identifier, points, aliased) in [
        (&b"Pix\\M0 65536 -65536 0 0 0"[..], (480, 640), false),
        (b"Pix\\M0 65536 -65536 0 0 0", (640, 480), true),
        (b"Pix\\M65536 0 65536 65536 0 0", (480, 480), true),
    ] {
        let mut s = mode_28();
        let handle = find_at(&mut s, identifier, points, (0, 0)).expect("found");
        let at = write(&mut s, TEXT, b"F");
        call(&mut s, PAINT, &[handle, at, HANDLE, 200_000, 200_000]).expect("painted");
        assert_eq!(
            partial(&mut s) > 0,
            aliased,
            "{:?} {points:?}, as RISC OS 5.30 painted it",
            core::str::from_utf8(identifier)
        );
    }
}

#[test]
fn a_rub_out_box_s_corners_are_the_nearest_pixels_halves_up() {
    let mut s = mode_28();
    fill(&mut s, 0xD0);
    let handle = find(&mut s, b"Pix", 256);
    write_words(
        &mut s,
        BLOCK,
        &[0, 0, 0, 0, 99_600, 156_400, 105_200, 170_000],
    );
    let at = write(&mut s, TEXT, b"A");
    call(
        &mut s,
        PAINT,
        &[handle, at, HANDLE | 0x22, AT[0], AT[1], BLOCK],
    )
    .expect("painted");
    let rows = inked(&mut s, 120, 16, 265..287, 0xD0);
    assert_eq!(
        (rows.first().map(|r| r.0), rows.last().map(|r| r.0)),
        (Some(267), Some(283))
    );
    assert!(rows
        .iter()
        .all(|&(_, left, right)| (left, right) == (125, 131)));
}

#[test]
fn an_underline_runs_from_where_it_is_set_through_moves_to_the_string_s_end() {
    let mut s = fonts();
    mode(&mut s, 12);
    call(&mut s, SET_FONT_COLOURS, &[0, 0, 1, 14]).expect("set");
    let pix = find(&mut s, b"Pix", 256);
    paint(
        &mut s,
        pix,
        &[b'A', 25, 0xF0, 40, b'A', 9, 0xA0, 0x0F, 0, b'A'],
        0,
    )
    .expect("painted");
    let bar = inked(&mut s, 120, 40, 156..158, 0);
    assert_eq!(
        bar,
        [(156, 131, 147), (157, 131, 147)],
        "under the second A, the move and the third"
    );
    let full = pixels(&mut s, 131, 17, 157..158);
    assert!(full[0].iter().all(|&p| p == 15));
}

#[test]
fn below_font_max_4_and_5_a_glyph_is_placed_at_a_quarter_pixel() {
    let mut s = mode_28();
    call(&mut s, SET_FONT_MAX, &[4_194_304, 16, 36, 36, 100, 100]).expect("set");
    let handle = find(&mut s, b"Pix", 256);
    let at = write(&mut s, TEXT, b"A");
    call(&mut s, PAINT, &[handle, at, HANDLE, AT[0] + 200, AT[1]]).expect("painted");
    let row = &pixels(&mut s, 125, 5, 275..276)[0];
    assert!(
        row[0] != 0 && row[0] != 0xFF,
        "a quarter in from the left: part covered"
    );
    assert_eq!(row[1..4], [0; 3]);
    assert!(row[4] != 0 && row[4] != 0xFF, "a quarter past the right");
}

#[test]
fn font_max_2_and_3_are_held_to_the_size_up_the_matrix_carries() {
    let partial = |s: &mut Session| {
        pixels(s, 120, 60, 240..282)
            .iter()
            .flatten()
            .filter(|&&p| p != 0 && p != 0xFF)
            .count()
    };
    for (identifier, points, aliased) in [
        (&b"Pix"[..], (576, 576), true),
        (b"Pix", (592, 592), false),
        (b"Pix\\M131072 0 0 131072 0 0", (320, 320), false),
        (b"Pix", (640, 320), true),
        (b"Pix", (320, 640), false),
    ] {
        let mut s = mode_28();
        let handle = find_at(&mut s, identifier, points, (0, 0)).expect("found");
        paint(&mut s, handle, b"F", 0).expect("painted");
        assert_eq!(
            partial(&mut s) > 0,
            aliased,
            "{:?} {points:?}",
            core::str::from_utf8(identifier)
        );
    }
}
