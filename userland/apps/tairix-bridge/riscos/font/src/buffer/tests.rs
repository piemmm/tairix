use alloc::vec::Vec;

use tairix_riscos_filing::session::Session;
use tairix_riscos_kernel::os::Regs;
use tairix_riscos_kernel::swi::{colour_trans, X};

use super::output_flag;
use crate::fixture::{
    call, find, find_at, fonts, mode, read_words, write, write_words, BLOCK, MATRIX, TEXT,
};
use crate::layout::flag::{HANDLE, MATRIX as MATRIX_FLAG, OS_UNITS, WORDS};
use crate::module::call::{CURRENT_RGB, PAINT, SET_FONT_COLOURS, SWITCH_OUTPUT_TO_BUFFER};

/// Where the probes painted, and the buffer they gave.
const AT: [u32; 2] = [100_000, 160_000];
const BUFFER: u32 = 0xB000;
const ROOM: u32 = 8000;
const WHITE: u32 = 0xFFFF_FF00;
const RED: u32 = 0x0000_FF00;
const RUB_OUT: u32 = 1 << 1;
const BLOCK_FLAG: u32 = 1 << 5;
/// What a paint leaves alone shows as.
const PATTERN: u32 = 0x5555_5555;
/// `Pix`'s `A`: its advance at 16 points, in millipoints, and its
/// object's bytes.
const A_ADVANCE: u32 = 4800;
const A_BYTES: u32 = 96;

/// Mode 28, `identifier` found at 16 points, its colours white to red as
/// `ColourTrans_SetFontColours` sets them, output switched to an empty
/// buffer of `ROOM` bytes with `flags`.
fn switched(identifier: &[u8], flags: u32) -> (Session, u32) {
    switched_in(28, identifier, flags)
}

/// As [`switched`], in mode `number`.
fn switched_in(number: u32, identifier: &[u8], flags: u32) -> (Session, u32) {
    let mut s = fonts();
    mode(&mut s, number);
    let handle = find(&mut s, identifier, 256);
    coloured(&mut s, handle);
    write_words(&mut s, BUFFER, &[0, ROOM.cast_signed()]);
    call(&mut s, SWITCH_OUTPUT_TO_BUFFER, &[flags, BUFFER]).expect("switched");
    (s, handle)
}

/// `handle`'s colours white to red, as `ColourTrans_SetFontColours` sets
/// them.
fn coloured(s: &mut Session, handle: u32) {
    let mut regs = Regs::new([handle, WHITE, RED, 14, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    s.kernel
        .os()
        .perform(X | colour_trans::SET_FONT_COLOURS, &mut regs)
        .expect("the colours");
}

/// `text` painted in `handle` with `flags`, R5 and R6 as given.
fn paint(
    s: &mut Session,
    handle: u32,
    text: &[u8],
    flags: u32,
    r5: u32,
    r6: u32,
) -> Result<(), u32> {
    paint_at(s, handle, text, flags, AT.map(u32::cast_signed), [r5, r6])
}

/// `text` painted in `handle` at `at` with `flags`, R5 and R6 as given.
fn paint_at(
    s: &mut Session,
    handle: u32,
    text: &[u8],
    flags: u32,
    at: [i32; 2],
    [r5, r6]: [u32; 2],
) -> Result<(), u32> {
    let text = write(s, TEXT, text);
    let [x, y] = at.map(i32::cast_unsigned);
    call(s, PAINT, &[handle, text, HANDLE | flags, x, y, r5, r6]).map(drop)
}

/// The buffer patterned, its terminator and room set, so what a paint
/// leaves alone shows.
fn patterned(s: &mut Session) {
    write_words(s, BUFFER, &[PATTERN.cast_signed(); 64]);
    write_words(s, BUFFER, &[0, ROOM.cast_signed()]);
}

/// The output pointer, switched back.
fn pointer(s: &mut Session) -> u32 {
    call(s, SWITCH_OUTPUT_TO_BUFFER, &[0, 0])
        .expect("switched back")
        .r[1]
}

/// Output switched back: the words written, and the two after them.
fn written(s: &mut Session) -> (Vec<u32>, [u32; 2]) {
    let regs = call(s, SWITCH_OUTPUT_TO_BUFFER, &[0, 0]).expect("switched back");
    let length = regs.r[1] - BUFFER;
    let mut os = s.kernel.os();
    let words = (0..length / 4)
        .map(|i| os.read_word(BUFFER + 4 * i).expect("read"))
        .collect();
    let after = [0, 1].map(|i| os.read_word(regs.r[1] + 4 * i).expect("read"));
    (words, after)
}

/// A path object as RISC OS 5.30 wrote it: its size, box, fill, style and
/// path, its outline none and of no width.
fn object(size: u32, bbox: [u32; 4], fill: u32, style: u32, path: &[u32]) -> Vec<u32> {
    let mut words = alloc::vec![2, size];
    words.extend_from_slice(&bbox);
    words.extend_from_slice(&[fill, 0xFFFF_FFFF, 0, style]);
    words.extend_from_slice(path);
    words
}

/// The rectangle a glyph 2048 by 4096 Draw units from `x`, `y` makes.
fn rectangle(x: u32, y: u32, width: u32, height: u32, fill: u32, style: u32) -> Vec<u32> {
    let (x1, y1) = (x + width, y + height);
    object(
        0x60,
        [x, y, x1, y1],
        fill,
        style,
        &[2, x, y, 8, x1, y, 8, x1, y1, 8, x, y1, 5, 0],
    )
}

#[test]
fn a_glyph_is_its_outline_filled_in_the_foreground_in_draw_s_units() {
    let (mut s, pix) = switched(b"Pix", 0);
    paint(&mut s, pix, b"A", 0, 0, 0).expect("painted");
    let (words, after) = written(&mut s);
    assert_eq!(
        words,
        rectangle(0xFA00, 0x19000, 0x800, 0x1000, RED, 0x40),
        "RISC OS 5.30's object"
    );
    let room = AT[0] + A_ADVANCE - (A_BYTES - 40);
    assert_eq!(
        after,
        [0, room],
        "the terminator, and the room the pen after it left the copy"
    );
}

#[test]
fn a_glyph_is_put_at_the_quarter_of_a_pixel_its_pen_is_in() {
    let (mut s, pix) = switched(b"Pix", 0);
    paint_at(&mut s, pix, b"A", 0, [100_199, 160_150], [0, 0]).expect("painted");
    assert_eq!(
        written(&mut s).0,
        rectangle(0xFA00, 0x19000, 0x800, 0x1000, RED, 0x40)
    );
}

#[test]
fn a_start_in_os_units_is_floored_to_its_pixel() {
    let (mut s, pix) = switched(b"Pix", 0);
    paint_at(&mut s, pix, b"A", OS_UNITS, [251, 401], [0, 0]).expect("painted");
    assert_eq!(
        written(&mut s).0,
        rectangle(0xFA00, 0x19000, 0x800, 0x1000, RED, 0x40)
    );
}

#[test]
fn in_bigger_pixels_an_outline_is_drawn_in_512ths_of_them() {
    let (mut s, pix) = switched_in(1, b"Pix", 0);
    paint(&mut s, pix, b"A", 0, 0, 0).expect("painted");
    assert_eq!(
        written(&mut s).0,
        rectangle(0xFA00, 0x19000, 2046, 4094, RED, 0x40),
        "1023 and 2047 of them, two units each"
    );
}

#[test]
fn a_font_at_its_own_resolution_is_drawn_a_pixel_to_a_pixel_of_the_screen() {
    let (mut s, _) = switched(b"Pix", 0);
    let fine = find_at(&mut s, b"Pix", (256, 256), (180, 90)).expect("found");
    coloured(&mut s, fine);
    paint(&mut s, fine, b"A", 0, 0, 0).expect("painted");
    assert_eq!(
        written(&mut s).0,
        rectangle(0xFA00, 0x19000, 4095, 4096, RED, 0x40),
        "twice as wide, 20.48 rounded down"
    );
}

#[test]
fn a_matrix_s_translation_moves_by_the_nearest_512th_of_a_pixel() {
    let (mut s, pix) = switched(b"Pix", 0);
    write_words(&mut s, MATRIX, &[65536, 0, 0, 65536, 12, -12]);
    paint(&mut s, pix, b"A", MATRIX_FLAG, 0, MATRIX).expect("painted");
    assert_eq!(
        written(&mut s).0,
        rectangle(0xFA08, 0x18FF8, 0x800, 0x1000, RED, 0x40),
        "7.68 and -7.68 units"
    );
}

#[test]
fn an_outline_given_too_little_room_fails_part_copied_where_its_pen_ends() {
    let (mut s, pix) = switched(b"Pix", 0);
    patterned(&mut s);
    let start = -4770;
    let room = start + A_ADVANCE.cast_signed();
    assert_eq!(
        paint_at(&mut s, pix, b"A", 0, [start, 160_000], [0, 0]),
        Err(0x1E4),
        "Buffer overflow"
    );
    let words = read_words(&mut s, BUFFER, 20)
        .into_iter()
        .map(i32::cast_unsigned)
        .collect::<Vec<_>>();
    let mut wanted = alloc::vec![
        2,
        ROOM,
        PATTERN,
        PATTERN,
        PATTERN,
        PATTERN,
        RED,
        0xFFFF_FFFF,
        0,
        0x40
    ];
    wanted.extend([
        2,
        0,
        0,
        8,
        0x800,
        0,
        8,
        0,
        (room - 28).cast_unsigned(),
        PATTERN,
    ]);
    assert_eq!(
        words, wanted,
        "seven words copied as the glyph holds them, then the end and the room left"
    );
    assert_eq!(pointer(&mut s), BUFFER + 40, "past the header");
}

#[test]
fn an_outline_copied_whole_without_room_for_the_terminator_fails() {
    let (mut s, pix) = switched(b"Pix", 0);
    patterned(&mut s);
    let start = -4800 + 52;
    assert_eq!(
        paint_at(&mut s, pix, b"A", 0, [start, 160_000], [0, 0]),
        Err(0x1E4)
    );
    let words = read_words(&mut s, BUFFER, 26)
        .into_iter()
        .map(i32::cast_unsigned)
        .collect::<Vec<_>>();
    let (x0, x1) = (
        (-3072i32).cast_unsigned(),
        (-3072i32 + 2048).cast_unsigned(),
    );
    let (y0, y1) = (0x19000, 0x1A000);
    assert_eq!(&words[..2], &[2, ROOM]);
    assert_eq!(
        &words[10..],
        &[2, x0, y0, 8, x1, y0, 8, x1, y1, 8, x0, y1, 5, 0, 0, PATTERN],
        "placed, then the end"
    );
    assert_eq!(
        pointer(&mut s),
        BUFFER + A_BYTES,
        "past the object, whose size and box were never written"
    );
}

#[test]
fn a_character_with_no_outline_leaves_only_its_copy_s_words() {
    let (mut s, pix) = switched(b"Pix", 0);
    patterned(&mut s);
    paint(&mut s, pix, b" ", 0, 0, 0).expect("painted");
    let room = AT[0] + A_ADVANCE;
    let words = read_words(&mut s, BUFFER, 13)
        .into_iter()
        .map(i32::cast_unsigned)
        .collect::<Vec<_>>();
    let mut wanted = alloc::vec![
        0,
        room + 40,
        PATTERN,
        PATTERN,
        PATTERN,
        PATTERN,
        RED,
        0xFFFF_FFFF,
        0,
        0x40
    ];
    wanted.extend([0, room, PATTERN]);
    assert_eq!(words, wanted, "its header and end left past the terminator");
    assert_eq!(pointer(&mut s), BUFFER);
}

#[test]
fn an_underline_has_a_bar_only_once_a_character_runs_under_it() {
    let (mut s, pix) = switched(b"Pix", 0);
    paint(&mut s, pix, b"A\x19\xF0\x14\x09\xD2\x04\x00", 0, 0, 0).expect("painted");
    assert_eq!(
        written(&mut s).0,
        rectangle(0xFA00, 0x19000, 0x800, 0x1000, RED, 0x40),
        "a move gives it none"
    );
}

#[test]
fn an_underline_set_at_the_ends_of_a_word_is_drawn_without_fault() {
    let (mut s, pix) = switched(b"Pix", 0);
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
    let (words, _) = written(&mut s);
    let mut tags = Vec::new();
    let mut at = 0;
    while let Some(&tag) = words.get(at) {
        tags.push(tag);
        at += usize::try_from(words[at + 1] / 4).expect("a size");
    }
    assert_eq!(tags, [2; 4], "a bar ahead of each A");
}

#[test]
fn counting_takes_no_room_from_the_pen() {
    let (mut s, pix) = switched(b"Pix", output_flag::COUNT);
    paint_at(&mut s, pix, b"A", 0, [-100_000, 160_000], [0, 0]).expect("counted");
    assert_eq!(pointer(&mut s), BUFFER + A_BYTES);
}

#[test]
fn a_face_s_outline_is_drawn_at_its_size() {
    let (mut s, _) = switched(b"Pix", 0);
    s.serve_fonts(alloc::boxed::Box::new(crate::build::square_faces));
    let face = find(&mut s, b"Homerton.Medium", 256);
    coloured(&mut s, face);
    paint(&mut s, face, b"I", 0, 0, 0).expect("painted");
    assert_eq!(
        written(&mut s).0,
        rectangle(0xFA00, 0x19000, 5120, 7168, RED, 0),
        "half the em by seven tenths"
    );
}

#[test]
fn an_underline_is_one_bar_from_where_it_is_set_to_the_end_ahead_of_what_it_runs_under() {
    let (mut s, cv) = switched(b"Cv", 0);
    paint(&mut s, cv, b"C\x19\xF0\x14CCC", 0, 0, 0).expect("painted");
    let (words, _) = written(&mut s);
    let mut wanted = rectangle(0xFA00, 0x19000, 0x800, 0x1000, RED, 0x40);
    wanted.extend(rectangle(0x10600, 0x18A60, 0x2400, 0x320, RED, 0));
    for x in [0x10600, 0x11200, 0x11E00] {
        wanted.extend(rectangle(x, 0x19000, 0x800, 0x1000, RED, 0x40));
    }
    assert_eq!(words, wanted, "RISC OS 5.30's five objects");
}

#[test]
fn a_matrix_s_multipliers_times_the_size_are_rounded_and_each_point_floored() {
    let (mut s, pix) = switched(b"Pix", 0);
    write_words(&mut s, MATRIX, &[32768, 0, 0, 65536, 1000, 2000]);
    paint(&mut s, pix, b"A", MATRIX_FLAG, 0, MATRIX).expect("painted");
    let (words, _) = written(&mut s);
    let (x0, y0, x1, y1) = (0xFC80, 0x19500, 0x1007F, 0x1A500);
    let path = [2, x0, y0, 8, x1, y0, 8, x1, y1, 8, x0, y1, 5, 0];
    assert_eq!(
        words,
        object(0x60, [x0, y0, x1, y1], RED, 0x40, &path),
        "65663, not 65664"
    );
}

#[test]
fn a_curve_is_its_three_points_and_its_box_their_hull() {
    let (mut s, cv) = switched(b"Cv", 0);
    paint(&mut s, cv, b"A", 0, 0, 0).expect("painted");
    let (words, _) = written(&mut s);
    let path = [
        2, 0xFA00, 0x19000, 6, 0x10600, 0x19000, 0x10600, 0x19C00, 0xFA00, 0x19C00, 5, 0,
    ];
    assert_eq!(
        words,
        object(0x58, [0xFA00, 0x19000, 0x10600, 0x19C00], RED, 0x40, &path)
    );
    let sheared = find(&mut s, b"Cv\\M65536 0 32768 65536 0 0", 256);
    call(&mut s, SWITCH_OUTPUT_TO_BUFFER, &[0, BUFFER]).expect("switched");
    paint(&mut s, sheared, b"A", 0, 0, 0).expect("painted");
    let (words, _) = written(&mut s);
    let path = [
        2, 0xFA00, 0x19000, 6, 0x10600, 0x19000, 0x10BFF, 0x19C00, 0xFFFF, 0x19C00, 5, 0,
    ];
    assert_eq!(
        words,
        object(0x58, [0xFA00, 0x19000, 0x10BFF, 0x19C00], RED, 0x40, &path),
        "a half shear"
    );
}

#[test]
fn stroke_paths_are_not_written() {
    let (mut s, cv) = switched(b"Cv", 0);
    paint(&mut s, cv, b"B", 0, 0, 0).expect("painted");
    assert_eq!(written(&mut s).0, Vec::<u32>::new());
}

#[test]
fn a_rub_out_box_comes_first_in_the_background_at_the_block_s_own_corners() {
    let (mut s, pix) = switched(b"Pix", 0);
    write_words(
        &mut s,
        BLOCK,
        &[0, 0, 0, 0, 99_200, 156_000, 104_800, 169_600],
    );
    paint(&mut s, pix, b"A", RUB_OUT | BLOCK_FLAG, BLOCK, 0).expect("painted");
    let (words, _) = written(&mut s);
    let mut wanted = rectangle(0xF800, 0x18600, 0xE00, 0x2200, WHITE, 0);
    wanted.extend(rectangle(0xFA00, 0x19000, 0x800, 0x1000, RED, 0x40));
    assert_eq!(words, wanted);
}

#[test]
fn a_colour_change_fills_what_follows_in_its_colour() {
    let (mut s, pix) = switched(b"Pix", 0);
    paint(&mut s, pix, b"A\x13\xFF\xFF\xFF\x00\x00\xFF\x0EA", 0, 0, 0).expect("painted");
    let (words, _) = written(&mut s);
    assert_eq!(words[6], RED);
    assert_eq!(words[24 + 6], 0xFF00_0000, "blue");
}

#[test]
fn counting_moves_the_pointer_by_what_would_be_written_and_writes_nothing() {
    let (mut s, pix) = switched(b"Pix", 0);
    call(&mut s, SWITCH_OUTPUT_TO_BUFFER, &[0, 0]).expect("switched back");
    write_words(&mut s, BUFFER, &[0x5555_5555]);
    call(&mut s, SWITCH_OUTPUT_TO_BUFFER, &[1, 8]).expect("counting");
    paint(&mut s, pix, b"AF", 0, 0, 0).expect("painted");
    let enquired = call(&mut s, SWITCH_OUTPUT_TO_BUFFER, &[0, u32::MAX]).expect("asked");
    assert_eq!(
        (enquired.r[0], enquired.r[1]),
        (1, 8 + 180),
        "A's 96 bytes and F's 84"
    );
    let off = call(&mut s, SWITCH_OUTPUT_TO_BUFFER, &[0, 0]).expect("switched back");
    assert_eq!((off.r[0], off.r[1]), (1, 188));
    assert_eq!(s.kernel.os().read_word(BUFFER).expect("read"), 0x5555_5555);
}

#[test]
fn the_call_refuses_what_risc_os_5_30_reserves() {
    let mut s = fonts();
    for (flags, at) in [
        (1 << 3, BUFFER),
        (1 << 5, BUFFER),
        (u32::MAX, u32::MAX),
        (1, 0),
    ] {
        assert_eq!(
            call(&mut s, SWITCH_OUTPUT_TO_BUFFER, &[flags, at]).map(drop),
            Err(0x213),
            "{flags:#x} {at:#x}"
        );
    }
    let regs = call(&mut s, SWITCH_OUTPUT_TO_BUFFER, &[0, u32::MAX]).expect("asked");
    assert_eq!((regs.r[0], regs.r[1]), (0, 0), "nothing switched");
    let on = call(&mut s, SWITCH_OUTPUT_TO_BUFFER, &[0b11, BUFFER]).expect("switched");
    assert_eq!((on.r[0], on.r[1]), (0, 0));
    let asked = call(&mut s, SWITCH_OUTPUT_TO_BUFFER, &[0, u32::MAX]).expect("asked");
    assert_eq!((asked.r[0], asked.r[1]), (0b11, BUFFER));
}

#[test]
fn colours_set_by_number_and_a_font_of_bitmaps_alone_are_refused() {
    let (mut s, pix) = switched(b"Pix", 0);
    call(&mut s, SET_FONT_COLOURS, &[pix, 0, 7, 14]).expect("logical");
    assert_eq!(call(&mut s, CURRENT_RGB, &[]).map(drop), Err(0x21D));
    assert_eq!(
        paint(&mut s, pix, b"A", 0, 0, 0),
        Err(0x21D),
        "a Draw object needs an RGB colour"
    );
    let size = crate::makebitmap::Size {
        depth: crate::outlines::Depth::Four,
        points: (256, 256),
        resolution: (90, 90),
        placements: (false, false),
    };
    let font = crate::resolve::font(&mut s.kernel.os(), pix).expect("resolved");
    let bitmaps = crate::makebitmap::file(&font, &size).expect("laid out");
    crate::fixture::save(&mut s, "HostFS:$.Lib.Bits.f320x320", &bitmaps);
    let bits = find(&mut s, b"Bits", 256);
    let mut regs = Regs::new([bits, WHITE, RED, 14, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    s.kernel
        .os()
        .perform(X | colour_trans::SET_FONT_COLOURS, &mut regs)
        .expect("the colours");
    assert_eq!(
        paint(&mut s, bits, b"A", 0, 0, 0),
        Err(0x20B),
        "no outlines to write"
    );
}
