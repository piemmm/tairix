//! Painting from bitmap files, and the colours and geometry rules RISC OS
//! 5.30 was recorded keeping, in mode 27: sixteen colours at ninety dots an
//! inch, the colours nought to one at offset 14, so each pixel is its level.

use alloc::vec::Vec;

use tairix_riscos_filing::session::Session;
use tairix_riscos_kernel::os::Regs;
use tairix_riscos_kernel::swi::font::TRUE_COLOURS;
use tairix_riscos_kernel::swi::{colour_trans, os as kos, X};

use crate::fixture::{
    call, deep_mode, fill, find, find_at, fonts, mode, pixels, save, write, write_words, BLOCK,
    MATRIX, TEXT,
};
use crate::glyphs::Bitmap;
use crate::layout::flag::{HANDLE, MATRIX as MATRIX_FLAG};
use crate::makebitmap::{laid_out, GlyphPixels, Size};
use crate::module::call::{
    CURRENT_FONT, CURRENT_RGB, LOOKUP_FONT, PAINT, READ_COLOUR_TABLE, SET_COLOUR_TABLE,
    SET_FONT_COLOURS, SET_PALETTE,
};
use crate::outlines::Depth;

const AT: [u32; 2] = [100_000, 160_000];
/// The screen row the pen's line is in, and the column its pixel is.
const BASELINE: u32 = 279;
const PEN: u32 = 125;

/// Mode 27 cleared, every level painting itself.
fn mode_27() -> Session {
    let mut s = fonts();
    mode(&mut s, 27);
    call(&mut s, SET_FONT_COLOURS, &[0, 0, 1, 14]).expect("the colours");
    s
}

fn paint_at(
    s: &mut Session,
    handle: u32,
    text: &[u8],
    flags: u32,
    x: u32,
    r6: u32,
) -> Result<(), u32> {
    let at = write(s, TEXT, text);
    call(s, PAINT, &[handle, at, HANDLE | flags, x, AT[1], 0, r6]).map(drop)
}

fn paint(s: &mut Session, handle: u32, text: &[u8]) -> Result<(), u32> {
    paint_at(s, handle, text, 0, AT[0], 0)
}

/// The pixels around the pen: twelve rows from nine above its line, so its
/// line is the tenth, and twelve columns from three before it.
fn around(s: &mut Session) -> Vec<Vec<u32>> {
    pixels(s, PEN - 3, 12, BASELINE - 9..BASELINE + 3)
}

/// The levels of `rows`, the top first, from `corner`.
fn glyph(corner: (i32, i32), rows: &[&[u8]]) -> Bitmap {
    let height = i32::try_from(rows.len()).expect("few");
    Bitmap {
        left: corner.0,
        top: corner.1 + height - 1,
        width: rows[0].len(),
        levels: rows.concat(),
    }
}

/// A file of `depth` at 16 points and ninety dots an inch, `placements`
/// across and up, holding `glyphs` at `A`, saved as `name`.
fn crafted(
    s: &mut Session,
    name: &str,
    depth: Depth,
    placements: (bool, bool),
    glyphs: Vec<((usize, usize), Bitmap)>,
) {
    let size = Size {
        depth,
        points: (256, 256),
        resolution: (90, 90),
        placements,
    };
    let drawn = glyphs.into_iter().map(|(placement, bitmap)| {
        Ok(GlyphPixels {
            glyph: u32::from(b'A'),
            placement,
            bitmap,
        })
    });
    let bytes = laid_out(b"Pix", &size, drawn).expect("laid out");
    save(s, name, &bytes);
}

/// The 5 by 6 pattern of distinct levels the bitmap probes used, from
/// (1, -1).
fn pattern() -> Bitmap {
    let levels: Vec<Vec<u8>> = (0..6u8)
        .rev()
        .map(|r| (0..5u8).map(|c| (r * 5 + c) % 16).collect())
        .collect();
    let rows: Vec<&[u8]> = levels.iter().map(Vec::as_slice).collect();
    glyph((1, -1), &rows)
}

/// What the pattern paints, as the probes read it back.
fn pattern_painted() -> Vec<Vec<u32>> {
    let mut rows = alloc::vec![alloc::vec![0; 12]; 12];
    for r in 0..6u32 {
        for c in 0..5u32 {
            rows[usize::try_from(10 - r).expect("small")][usize::try_from(4 + c).expect("small")] =
                (r * 5 + c) % 16;
        }
    }
    rows
}

#[test]
fn a_file_of_the_size_is_painted_in_place_of_the_outline() {
    let mut s = mode_27();
    crafted(
        &mut s,
        "HostFS:$.Lib.Pix.f320x320",
        Depth::Four,
        (false, false),
        alloc::vec![((0, 0), pattern())],
    );
    let pix = find(&mut s, b"Pix", 256);
    paint(&mut s, pix, b"A").expect("painted");
    assert_eq!(
        around(&mut s),
        pattern_painted(),
        "as RISC OS 5.30 painted the probes' file"
    );
}

#[test]
fn a_file_is_not_painted_at_another_size_nor_for_a_font_found_with_a_matrix() {
    let mut s = mode_27();
    crafted(
        &mut s,
        "HostFS:$.Lib.Pix.f320x320",
        Depth::Four,
        (false, false),
        alloc::vec![((0, 0), pattern())],
    );
    let outline = {
        let mut plain = mode_27();
        let pix = find(&mut plain, b"Pix", 256);
        paint(&mut plain, pix, b"A").expect("painted");
        around(&mut plain)
    };
    let matrixed = find(&mut s, b"Pix\\M65536 0 0 65536 0 0", 256);
    paint(&mut s, matrixed, b"A").expect("painted");
    assert_eq!(
        around(&mut s),
        outline,
        "a font with a matrix draws its outline"
    );
    mode(&mut s, 27);
    call(&mut s, SET_FONT_COLOURS, &[0, 0, 1, 14]).expect("the colours");
    let pix = find(&mut s, b"Pix", 256);
    write_words(&mut s, MATRIX, &[65536, 0, 0, 65536, 0, 0]);
    paint_at(&mut s, pix, b"A", MATRIX_FLAG, AT[0], MATRIX).expect("painted");
    assert_eq!(
        around(&mut s),
        pattern_painted(),
        "a paint through the identity's matrix takes the file"
    );
    mode(&mut s, 27);
    call(&mut s, SET_FONT_COLOURS, &[0, 0, 1, 14]).expect("the colours");
    let seventeen = find(&mut s, b"Pix", 272);
    paint(&mut s, seventeen, b"A").expect("painted");
    assert_ne!(around(&mut s), pattern_painted());
}

#[test]
fn a_one_bit_file_is_painted_where_only_one_bit_can_be() {
    let mut s = mode_27();
    let checker: Vec<Vec<u8>> = (0..5u8)
        .rev()
        .map(|r| (0..6u8).map(|c| ((r + c) % 2) * 15).collect())
        .collect();
    let rows: Vec<&[u8]> = checker.iter().map(Vec::as_slice).collect();
    crafted(
        &mut s,
        "HostFS:$.Lib.Pix.b320x320",
        Depth::One,
        (false, false),
        alloc::vec![((0, 0), glyph((0, 0), &rows))],
    );
    call(&mut s, SET_FONT_COLOURS, &[0, 0, 1, 0]).expect("one bit");
    let pix = find(&mut s, b"Pix", 256);
    paint(&mut s, pix, b"A").expect("painted");
    let painted = around(&mut s);
    for (r, row) in (0..5u32).zip(painted[5..10].iter().rev()) {
        let wanted: Vec<u32> = (0..6u32).map(|c| (r + c) % 2).collect();
        assert_eq!(
            row[3..9],
            wanted[..],
            "row {r} of the checker, as RISC OS 5.30 painted it"
        );
    }
}

#[test]
fn a_file_s_sub_placement_is_the_quarter_of_the_pixel_the_pen_is_in() {
    let mut s = mode_27();
    let dot = |up: i32| glyph((0, up), &[&[15]]);
    let glyphs = (0..4usize)
        .map(|across| ((across, 0), dot(i32::try_from(across).expect("small"))))
        .collect();
    crafted(
        &mut s,
        "HostFS:$.Lib.Pix.f320x320",
        Depth::Four,
        (true, false),
        glyphs,
    );
    let pix = find(&mut s, b"Pix", 256);
    for (past, quarter) in [
        (0, 0),
        (100, 0),
        (200, 1),
        (300, 1),
        (400, 2),
        (500, 2),
        (600, 3),
        (700, 3),
    ] {
        mode(&mut s, 27);
        call(&mut s, SET_FONT_COLOURS, &[0, 0, 1, 14]).expect("the colours");
        paint_at(&mut s, pix, b"A", 0, AT[0] + past, 0).expect("painted");
        let row = usize::try_from(BASELINE - quarter - (BASELINE - 9)).expect("small");
        assert_eq!(
            around(&mut s)[row][3],
            15,
            "{past} millipoints on: quarter {quarter}"
        );
    }
}

#[test]
fn a_damaged_glyph_in_a_file_paints_nothing_and_no_outline() {
    let mut s = mode_27();
    let size = Size {
        depth: Depth::Four,
        points: (256, 256),
        resolution: (90, 90),
        placements: (false, false),
    };
    let drawn = core::iter::once(Ok(GlyphPixels {
        glyph: u32::from(b'A'),
        placement: (0, 0),
        bitmap: pattern(),
    }));
    let mut bytes = laid_out(b"Pix", &size, drawn).expect("laid out");
    let table = usize::try_from(u32::from_le_bytes(
        bytes[16..20].try_into().expect("a word"),
    ))
    .expect("small");
    let chunk = usize::try_from(u32::from_le_bytes(
        bytes[table + 8..table + 12].try_into().expect("a word"),
    ))
    .expect("small");
    bytes[chunk + 3] = 0;
    save(&mut s, "HostFS:$.Lib.Pix.f320x320", &bytes);
    let pix = find(&mut s, b"Pix", 256);
    paint(&mut s, pix, b"A").expect("painted");
    assert!(
        around(&mut s).iter().flatten().all(|&p| p == 0),
        "as RISC OS 5.30 painted it: nothing"
    );
}

#[test]
fn a_font_of_one_bitmap_file_is_found_at_that_size_alone() {
    let mut s = mode_27();
    assert_eq!(
        find_at(&mut s, b"Bits", (256, 256), (0, 0)),
        Err(0x20B),
        "no data yet"
    );
    crafted(
        &mut s,
        "HostFS:$.Lib.Bits.f320x320",
        Depth::Four,
        (false, false),
        alloc::vec![((0, 0), pattern())],
    );
    let bits = find(&mut s, b"Bits", 256);
    paint(&mut s, bits, b"A").expect("painted");
    assert_eq!(around(&mut s), pattern_painted());
    assert_eq!(
        find_at(&mut s, b"Bits", (192, 192), (0, 0)),
        Err(0x20B),
        "No suitable font data, at 12 points"
    );
    let lookup = call(&mut s, LOOKUP_FONT, &[bits, 0, 0]).expect("looked up");
    assert_eq!(lookup.r[2] & 1, 0, "not an old bitmap font");
}

#[test]
fn an_old_bitmap_font_paints_its_size_and_scales_it_to_others() {
    let mut s = mode_27();
    let old = find(&mut s, b"Old", 256);
    paint(&mut s, old, b"A").expect("painted");
    let painted = around(&mut s);
    for (r, row) in (0..4usize).zip(painted[6..10].iter().rev()) {
        let wanted: Vec<u32> = (0..3u32)
            .map(|c| u32::try_from(r).expect("small") * 3 + c + 1)
            .collect();
        assert_eq!(
            row[4..7],
            wanted[..],
            "row {r}, the file's own levels, as RISC OS 5.30 painted them"
        );
    }
    assert_eq!(
        call(&mut s, LOOKUP_FONT, &[old, 0, 0])
            .expect("looked up")
            .r[2]
            & 1,
        1,
        "an old bitmap font"
    );
    mode(&mut s, 27);
    call(&mut s, SET_FONT_COLOURS, &[0, 0, 1, 14]).expect("the colours");
    let twelve = find(&mut s, b"Old", 192);
    paint(&mut s, twelve, b"A").expect("painted");
    let painted = around(&mut s);
    let scaled: Vec<&[u32]> = painted[7..10].iter().map(|row| &row[3..6]).collect();
    assert_eq!(
        scaled,
        [&[2, 10, 11][..], &[1, 6, 7], &[0, 2, 4]],
        "RISC OS 5.30's 12 points from 16"
    );
}

#[test]
fn a_paint_s_coordinates_are_the_screen_s_whatever_the_graphics_origin() {
    let mut s = mode_27();
    let pix = find(&mut s, b"Pix", 256);
    paint(&mut s, pix, b"A").expect("painted");
    let unmoved = around(&mut s);
    mode(&mut s, 27);
    call(&mut s, SET_FONT_COLOURS, &[0, 0, 1, 14]).expect("the colours");
    let mut regs = Regs::default();
    let origin = write(&mut s, TEXT + 0x100, &[29, 100, 0, 50, 0]);
    regs.r[..2].copy_from_slice(&[origin, 5]);
    s.kernel
        .os()
        .perform(X | kos::WRITE_N, &mut regs)
        .expect("the origin moved");
    paint(&mut s, pix, b"A").expect("painted");
    assert_eq!(around(&mut s), unmoved);
}

#[test]
fn a_negative_resolution_paints_as_its_magnitude() {
    let mut s = mode_27();
    let positive = find_at(&mut s, b"Pix", (256, 256), (90, 90)).expect("found");
    paint(&mut s, positive, b"A").expect("painted");
    let wanted = around(&mut s);
    mode(&mut s, 27);
    call(&mut s, SET_FONT_COLOURS, &[0, 0, 1, 14]).expect("the colours");
    let negative = find_at(&mut s, b"Pix", (256, 256), (-90, 90)).expect("found");
    paint(&mut s, negative, b"A").expect("painted");
    assert_eq!(around(&mut s), wanted);
}

#[test]
fn a_colour_outside_the_mode_s_refuses_the_paint() {
    let mut s = fonts();
    mode(&mut s, 12);
    let pix = find(&mut s, b"Pix", 256);
    for (background, foreground, offset, refused) in [
        (0, 2, 14, true),
        (16, 1, 1, true),
        (0, 15, -14i32, false),
        (15, 1, 14, false),
    ] {
        call(
            &mut s,
            SET_FONT_COLOURS,
            &[0, background, foreground, offset.cast_unsigned()],
        )
        .expect("taken");
        let painted = paint(&mut s, pix, b"A");
        assert_eq!(
            painted.is_err(),
            refused,
            "{background} {foreground} {offset}"
        );
        if refused {
            assert_eq!(painted, Err(0x207));
            assert_eq!(
                call(&mut s, READ_COLOUR_TABLE, &[0, BLOCK]).map(drop),
                Err(0x207)
            );
        }
    }
}

#[test]
fn a_foreground_past_the_pseudo_palette_s_sixteen_sets_no_entry() {
    let mut s = fonts();
    mode(&mut s, 28);
    // RISC OS 5.30 keeps the colours, each number's low byte, and refuses.
    let refused = call(
        &mut s,
        SET_PALETTE,
        &[
            0,
            0xFFFF_FF00,
            0x311,
            6,
            0xFF00_0000,
            0x0000_FF00,
            TRUE_COLOURS,
        ],
    );
    assert_eq!(refused.map(drop), Err(0x205));
    let current = call(&mut s, CURRENT_FONT, &[]).expect("current");
    assert_eq!(current.r[1..4], [0, 17, 6]);
    for foreground in [17, 1] {
        call(&mut s, SET_FONT_COLOURS, &[0, 0, foreground, 6]).expect("taken");
        assert_eq!(
            call(&mut s, READ_COLOUR_TABLE, &[0, BLOCK]).map(drop),
            Err(0x205),
            "{foreground}"
        );
    }
}

#[test]
fn colour_numbers_are_kept_to_their_low_byte() {
    let mut s = fonts();
    mode(&mut s, 28);
    call(&mut s, SET_FONT_COLOURS, &[0, 0x1234, 7, 14]).expect("taken");
    assert_eq!(
        call(&mut s, CURRENT_FONT, &[]).expect("current").r[1..4],
        [0x34, 7, 14]
    );
    mode(&mut s, 12);
    call(
        &mut s,
        SET_PALETTE,
        &[0, 0xFFFF_FF02, 3, 2, 0xFFFF_FF00, 0xFF00, TRUE_COLOURS],
    )
    .expect("set");
    assert_eq!(
        call(&mut s, CURRENT_FONT, &[]).expect("current").r[1..4],
        [2, 3, 2]
    );
}

/// `ColourTrans_SetFontColours` of `background` to `foreground` at most
/// `offset` for `font`.
fn colourtrans(s: &mut Session, font: u32, background: u32, foreground: u32, offset: i32) {
    let mut regs = Regs::new([
        font,
        background,
        foreground,
        offset.cast_unsigned(),
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ]);
    s.kernel
        .os()
        .perform(X | colour_trans::SET_FONT_COLOURS, &mut regs)
        .expect("the colours");
}

/// The colour table read.
fn table(s: &mut Session) -> [u8; 16] {
    call(s, READ_COLOUR_TABLE, &[0, BLOCK]).expect("read");
    let mut bytes = [0; 16];
    s.kernel.os().read_bytes(BLOCK, &mut bytes).expect("read");
    bytes
}

#[test]
fn colourtrans_records_its_words_as_the_font_s_rgb_colours_until_colours_are_set_by_number() {
    let mut s = fonts();
    mode(&mut s, 28);
    let pix = find(&mut s, b"Pix", 256);
    colourtrans(&mut s, pix, 0xFFFF_FF00, 0x0000_FF00, 6);
    let rgb = call(&mut s, CURRENT_RGB, &[]).expect("defined");
    assert_eq!(rgb.r[..4], [pix, 0xFFFF_FF00, 0x0000_FF00, 6]);
    call(
        &mut s,
        SET_PALETTE,
        &[0, 0, 1, 14, 0xFFFF_FF00, 0, TRUE_COLOURS],
    )
    .expect("set");
    assert_eq!(
        call(&mut s, CURRENT_RGB, &[]).expect("kept").r[1..4],
        [0xFFFF_FF00, 0x0000_FF00, 6],
        "as RISC OS 5.30 kept them"
    );
    call(&mut s, SET_FONT_COLOURS, &[0, 0, 1, 14]).expect("set");
    assert_eq!(call(&mut s, CURRENT_RGB, &[]).map(drop), Err(0x21D));
}

#[test]
fn colourtrans_sets_a_deep_mode_s_colours_as_font_set_palette_does_its_words_as_given() {
    let mut s = fonts();
    mode(&mut s, 28);
    let pix = find(&mut s, b"Pix", 256);
    colourtrans(&mut s, pix, 0xFFFF_FF00, 0x3070_B000, 14);
    assert_eq!(
        table(&mut s),
        [
            0xFF, 0xFE, 0xFE, 0xFD, 0xFD, 0xFC, 0xFC, 0xF4, 0x5B, 0xD5, 0x5D, 0x5D, 0x5C, 0x5C,
            0x33, 0x33
        ],
        "RISC OS 5.30's table"
    );
    deep_mode(&mut s, 5);
    let pix = find(&mut s, b"Pix", 256);
    colourtrans(&mut s, pix, 0xFFFF_FF00, 0x3070_B000, 14);
    assert_eq!(
        table(&mut s),
        [
            0xFF, 0xF9, 0xF4, 0xEF, 0xE9, 0xE4, 0xDF, 0xDA, 0xD4, 0xCF, 0xCA, 0xC5, 0xBF, 0xBA,
            0xB5, 0xB0
        ],
        "and its 16M one, each entry's low byte"
    );
}

#[test]
fn colourtrans_s_own_table_paints_a_direct_colour_mode() {
    let mut s = fonts();
    deep_mode(&mut s, 5);
    let pix = find(&mut s, b"Pix", 256);
    let table: Vec<i32> = (0..16).map(|level| 0x0001_0101 * level * 16).collect();
    write_words(&mut s, BLOCK, &table);
    call(
        &mut s,
        SET_COLOUR_TABLE,
        &[0, BLOCK, 0x1111_1100, 0x2222_2200, 14],
    )
    .expect("set");
    let rgb = call(&mut s, CURRENT_RGB, &[]).expect("defined");
    assert_eq!(rgb.r[1..4], [0x1111_1100, 0x2222_2200, 14]);
    paint(&mut s, pix, b"A").expect("painted");
    let row = &pixels(&mut s, PEN, 1, BASELINE - 4..BASELINE - 3)[0];
    assert_eq!(
        row[0] & 0x00FF_FFFF,
        0x00F0_F0F0,
        "level 15 the table's own entry"
    );
}

#[test]
fn a_change_of_colours_by_number_in_a_string_after_rgb_paints_in_the_numbers() {
    let mut s = mode_27();
    let pix = find(&mut s, b"Pix", 256);
    paint(
        &mut s,
        pix,
        b"\x13\xFF\xFF\xFF\x00\x00\x00\x0E\x12\x00\x05\x00A",
    )
    .expect("painted");
    let current = call(&mut s, CURRENT_FONT, &[]).expect("current");
    assert_eq!(
        current.r[1..4],
        [0, 5, 0],
        "the 18 sequence's colours stand"
    );
    assert_eq!(
        call(&mut s, CURRENT_RGB, &[]).map(drop),
        Err(0x21D),
        "and the RGB colours are undefined"
    );
    let painted = around(&mut s);
    assert!(
        painted.iter().flatten().all(|&p| p == 0 || p == 5),
        "painted in 5 alone"
    );
}

#[test]
fn a_blending_paint_mixes_each_level_with_the_screen_as_risc_os_5_30_does() {
    let mut s = fonts();
    mode(&mut s, 28);
    call(
        &mut s,
        SET_PALETTE,
        &[0, 0, 1, 14, 0xFFFF_FF00, 0, TRUE_COLOURS],
    )
    .expect("set");
    crafted(
        &mut s,
        "HostFS:$.Lib.Pix.f320x320",
        Depth::Four,
        (false, false),
        alloc::vec![((0, 0), glyph((0, 0), &[&[3, 9, 15]]))],
    );
    fill(&mut s, 0x15);
    let pix = find(&mut s, b"Pix", 256);
    paint_at(&mut s, pix, b"A", 1 << 11, AT[0], 0).expect("painted");
    let row = &pixels(&mut s, PEN, 3, BASELINE..BASELINE + 1)[0];
    assert_eq!(
        row[..],
        [0x11, 0x04, 0x00],
        "colour &15 blended toward black at levels 3, 9 and 15"
    );
}

#[test]
fn a_font_asked_for_in_another_encoding_keeps_its_bitmaps_in_that_encoding_s_directory() {
    let mut s = mode_27();
    let latin2 = find(&mut s, b"Pix\\ELatin2", 256);
    call(
        &mut s,
        crate::module::call::MAKE_BITMAP,
        &[0, latin2, 256, 256, 90, 90, 1],
    )
    .expect("made");
    let mut os = s.kernel.os();
    assert!(crate::scan::is_file(&mut os, b"HostFS:$.Lib.Pix.Latin2.f320x320").expect("read"));
    assert!(
        !crate::scan::is_file(&mut os, b"HostFS:$.Lib.Pix.f320x320").expect("read"),
        "nothing beside the font's own"
    );
}
