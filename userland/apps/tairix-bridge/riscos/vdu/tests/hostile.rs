//! What hostile programs were found doing to the drivers, each now answered
//! as a value: selectors naming no screen, a module answering a service with
//! a wild list, a copy cursor left behind by a smaller window, a cursor
//! wrapped round the coordinates, characters as large as VDU 5 allows, and
//! sprites whose control blocks claim sizes no memory or sum holds.

mod common;

use common::{
    answers, call, drivers, error_number, insert_module, vdu, with_files, BY_POINTER, SELECTOR_AT,
    USER,
};
use tairix_riscos_filing::session::{Session, SLOT_BYTES};
use tairix_riscos_kernel::os::{Fail, Regs};
use tairix_riscos_kernel::swi::os::{BYTE, PLOT, READ_C, SCREEN_MODE, SPRITE_OP};
use tairix_riscos_kernel::swi::X;
use tairix_riscos_vdu::display;

/// A 32-bit guest module whose service handler claims
/// `Service_ModeExtension` with a list at the top of memory.
fn wild_module() -> Vec<u8> {
    let header = [0, 0, 0, 0x34, 0x4C, 0x4C, 0, 0, 0, 0, 0, 0, 0x48];
    let handler = [
        0xE331_0050, // TEQ R1, #&50
        0x11A0_F00E, // MOVNE PC, R14
        0xE3A0_1000, // MOV R1, #0
        0xE3E0_4003, // MVN R4, #3
        0xE1A0_F00E, // MOV PC, R14
    ];
    let flags = [1];
    let mut image: Vec<u8> = header
        .iter()
        .chain(&handler)
        .chain(&flags)
        .flat_map(|w: &u32| w.to_le_bytes())
        .collect();
    image.extend_from_slice(b"Wild\0\0\0\0");
    image
}

#[test]
fn a_selector_naming_no_screen_is_refused() {
    let mut session = drivers(b"");
    for (index, value) in [
        (12, u32::MAX),
        (11, u32::MAX),
        (1, u32::MAX),
        (10, 32),
        (10, 7),
        (4, 32),
        (3, 256),
        (6, 0x100_0000),
    ] {
        let os = &mut session.kernel.os();
        let selector = [1, 640, 480, 3, u32::MAX, index, value, u32::MAX];
        os.write_words(SELECTOR_AT, &selector).expect("written");
        let mut regs = Regs::new([0, SELECTOR_AT, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let refused = os.perform(X | SCREEN_MODE, &mut regs);
        assert!(
            matches!(refused, Err(Fail::Error(_))),
            "variable {index} as {value:#x}: {refused:?}"
        );
        assert!(answers(&mut session));
    }
}

#[test]
fn a_teletext_page_past_what_one_holds_is_refused() {
    let mut session = drivers(b"");
    let os = &mut session.kernel.os();
    os.write_bytes(SELECTOR_AT, b"X640 Y500 T16 TX999999999\0")
        .expect("written");
    let mut regs = Regs::new([15, SELECTOR_AT, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(matches!(
        os.perform(X | SCREEN_MODE, &mut regs),
        Err(Fail::Error(_))
    ));
    os.write_bytes(SELECTOR_AT, b"X640 Y500 T16\0")
        .expect("written");
    let mut regs = Regs::new([15, SELECTOR_AT, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(
        os.perform(X | SCREEN_MODE, &mut regs).is_ok(),
        "a page of the cells that fit"
    );
}

#[test]
fn a_mode_extension_answered_with_a_wild_list_is_a_value() {
    let mut session = drivers(b"");
    insert_module(&mut session, &wild_module()).expect("inserted");
    let _ = vdu(&mut session, &[22, 60]);
    assert!(answers(&mut session));
}

#[test]
fn a_copy_cursor_outside_a_smaller_window_is_let_go() {
    let mut session = drivers(b"");
    vdu(&mut session, &[22, 12, 31, 0, 31]).expect("the cursor on the last row");
    for key in [0x8F, b'A'] {
        let mut regs = Regs::new([138, 0, u32::from(key), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        session
            .kernel
            .os()
            .perform(X | BYTE, &mut regs)
            .expect("inserted");
    }
    let mut regs = Regs::new([0; 13]);
    session
        .kernel
        .os()
        .perform(X | READ_C, &mut regs)
        .expect("read");
    assert_eq!(
        regs.r[0],
        u32::from(b'A'),
        "the cursor key taken by copy editing"
    );
    vdu(
        &mut session,
        &[28, 0, 5, 10, 0, 23, 16, 4, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("written");
    let mut regs = Regs::new([135, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(session.kernel.os().perform(X | BYTE, &mut regs).is_ok());
    assert!(
        display::cursor(&mut session.kernel.os()).is_some(),
        "the text cursor drawn where it stands"
    );
}

#[test]
fn shapes_about_a_cursor_wrapped_round_the_coordinates_are_values() {
    let mut session = drivers(b"");
    vdu(&mut session, &[22, 28]).expect("mode 28");
    let plot = |session: &mut Session, code: u32, x: i32, y: i32| {
        let mut regs = Regs::new([
            code,
            x.cast_unsigned(),
            y.cast_unsigned(),
            0,
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
        session.kernel.os().perform(X | PLOT, &mut regs)
    };
    for _ in 0..70_000 {
        plot(&mut session, 0, 32_767, -32_767).expect("moved");
    }
    for code in [
        5, 85, 101, 117, 133, 149, 157, 165, 173, 181, 189, 197, 205, 189,
    ] {
        assert!(
            plot(&mut session, code, 30_000, -30_000).is_ok(),
            "PLOT {code}"
        );
        assert!(
            plot(&mut session, code, -32_768, 32_767).is_ok(),
            "PLOT {code} back"
        );
    }
    assert!(answers(&mut session));
}

#[test]
fn a_character_as_large_as_vdu_5_allows_is_drawn_on_the_screen_alone() {
    let mut session = drivers(b"");
    vdu(
        &mut session,
        &[
            22, 28, 23, 255, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        ],
    )
    .expect("a block");
    vdu(
        &mut session,
        &[
            23, 17, 7, 2, 0, 0x80, 0, 0x80, 0, 0, 5, 25, 4, 0, 0, 0xBF, 3,
        ],
    )
    .expect("set up");
    vdu(&mut session, b"\xFF").expect("printed");
    let mut line = Vec::new();
    display::line(&mut session.kernel.os(), 0, &mut line).expect("read");
    assert!(
        line.iter().all(|&colour| colour != 0),
        "the block glyph's top row fills the screen's top line"
    );
}

/// Where a hostile sprite's area lies, in the slot.
const AREA: u32 = 0x2_0000;
/// "Unrecognised sprite data", and "No room to grow".
const UNRECOGNISED: u32 = 0x71E;
const NO_ROOM_TO_GROW: u32 = 0x706;

/// An area at [`AREA`] of `size` bytes holding one sprite at its first
/// offset, its control block's words after its name `block`: the bytes to the
/// next, its words and rows each less one, its first and last bits, its
/// image's and mask's offsets and its mode word.
fn hostile_sprite(session: &mut Session, size: u32, block: [u32; 8]) {
    let mut os = session.kernel.os();
    let free = 16u32.saturating_add(block[0]);
    os.write_words(AREA, &[size, 1, 16, free]).expect("written");
    os.write_word(AREA + 16, block[0]).expect("written");
    os.write_bytes(AREA + 20, b"hostile\0\0\0\0\0")
        .expect("written");
    os.write_words(AREA + 32, &block[1..]).expect("written");
}

/// `OS_SpriteOp` `reason` on the hostile sprite, R3 and R4 as given: its
/// error's number, the drivers still answering after it.
fn refused(session: &mut Session, reason: u32, r3: u32, r4: u32) -> u32 {
    let called = call(
        session,
        SPRITE_OP,
        &[BY_POINTER + reason, AREA, AREA + 16, r3, r4],
    );
    let number = match called {
        Ok(regs) => panic!("reason {reason} answered {:x?}", regs.r),
        Err(fail) => error_number(session, fail),
    };
    assert!(answers(session), "the drivers still answer after {reason}");
    number
}

#[test]
fn a_sprite_whose_image_passes_any_sum_is_unrecognised_not_a_crash() {
    let mut session = drivers(b"");
    hostile_sprite(
        &mut session,
        0x1_0000,
        [48, 0xFFFF_FFFE, 0x7FFF_FFFF, 0, 31, 44, 44, 12],
    );
    assert_eq!(
        refused(&mut session, 33, 0, 0),
        UNRECOGNISED,
        "flipped upside down"
    );
    let wide_mask = 0x8816_80B5;
    hostile_sprite(
        &mut session,
        0x1_0000,
        [52, 0x07FF_FFFF, 0, 0, 30, 44, 48, wide_mask],
    );
    assert_eq!(
        refused(&mut session, 33, 0, 0),
        UNRECOGNISED,
        "a row of a mask past what a word counts"
    );
}

#[test]
fn a_sprite_s_bits_past_a_word_are_unrecognised_before_any_pixel_is_walked() {
    let mut session = drivers(b"");
    let new_format = 0x2816_80B5;
    hostile_sprite(
        &mut session,
        0x1_0000,
        [4044, 0, 999, 0, 0xFFFF_FFFE, 44, 44, new_format],
    );
    assert_eq!(
        refused(&mut session, 47, 0, 0),
        UNRECOGNISED,
        "flipped left to right"
    );
    let deep = 0x3016_80B5;
    hostile_sprite(
        &mut session,
        0x1_0000,
        [172, 0, 31, 0, 0xFFFF_FFFE, 44, 44, deep],
    );
    assert_eq!(
        refused(&mut session, 38, 0xC000_0000, 0),
        UNRECOGNISED,
        "its alpha made"
    );
}

#[test]
fn a_sprite_whose_words_cannot_hold_its_width_is_unrecognised_before_any_work() {
    let mut session = drivers(b"");
    let no_words = [48, u32::MAX, 0xFFFF_FFFE, 0, 0, 44, 44, 1];
    hostile_sprite(&mut session, 0x1_0000, no_words);
    assert_eq!(
        refused(&mut session, 47, 0, 0),
        UNRECOGNISED,
        "flipped left to right"
    );
    let deep = 0x3016_80B5;
    hostile_sprite(
        &mut session,
        0x1_0000,
        [48, u32::MAX, 0xFFFF_FFFE, 0, 0, 44, 44, deep],
    );
    assert_eq!(
        refused(&mut session, 38, 0x8000_0000, 0),
        UNRECOGNISED,
        "its alpha made"
    );
}

#[test]
fn an_old_format_sprite_whose_bits_run_backwards_keeps_its_words() {
    let mut session = drivers(b"");
    hostile_sprite(&mut session, 0x1_0000, [48, 0, 0, 8, 0, 44, 44, 12]);
    let before = common::rma_free(&mut session);
    assert_eq!(
        refused(&mut session, 54, 0, 0),
        UNRECOGNISED,
        "its wastage removed"
    );
    let mut block = [0u8; 32];
    session
        .kernel
        .os()
        .read_bytes(AREA + 16, &mut block)
        .expect("read");
    assert_eq!(&block[16..20], &[0, 0, 0, 0], "its words as they were");
    assert_eq!(common::rma_free(&mut session), before);
}

#[test]
fn a_sprite_no_memory_backs_is_unrecognised_whatever_its_block_claims() {
    let mut session = drivers(b"");
    hostile_sprite(
        &mut session,
        0x2000_0000,
        [0x1000_0040, 0x03FF_FFFF, 0, 0, 31, 44, 44, 18],
    );
    assert_eq!(
        refused(&mut session, 60, 0, 0),
        UNRECOGNISED,
        "output switched to it"
    );
    assert_eq!(
        refused(&mut session, 47, 0, 0),
        UNRECOGNISED,
        "flipped left to right"
    );
    assert_eq!(refused(&mut session, 34, 0, 0), UNRECOGNISED, "plotted");
}

#[test]
fn rows_inserted_past_any_sum_into_a_sprite_of_none_are_no_room() {
    let mut session = drivers(b"");
    hostile_sprite(
        &mut session,
        0x1_0000,
        [44, 0xFFFF_FFFE, u32::MAX, 0, 31, 44, 44, 12],
    );
    assert_eq!(refused(&mut session, 57, 0, 0x7FFF_FFFF), NO_ROOM_TO_GROW);
}

#[test]
fn a_pixel_of_a_sprite_whose_rows_wrap_is_read_where_the_sum_wraps() {
    let mut session = drivers(b"");
    hostile_sprite(
        &mut session,
        0x1_0000,
        [48, 0xFFFF_FFFE, 0x4000_0001, 0, 31, 44, 44, 12],
    );
    let read = call(
        &mut session,
        SPRITE_OP,
        &[BY_POINTER + 41, AREA, AREA + 16, 0, 0],
    )
    .expect("read where the sum wraps");
    assert_eq!(
        (read.r[5], read.r[6]),
        (12, 0),
        "the low nibble of the sprite's own mode word"
    );
}

#[test]
fn a_copy_of_a_sprite_running_off_memory_is_refused_and_counts_nothing() {
    let mut session = drivers(b"");
    hostile_sprite(&mut session, 0x1_0000, [44, 0, 0, 0, 31, 44, 44, 12]);
    let end = tairix_riscos_kernel::memory::layout::APPLICATION + SLOT_BYTES;
    let lying = end - 44;
    {
        let mut os = session.kernel.os();
        os.write_word(lying, 0x1000).expect("written");
        os.write_bytes(NAME_AT, b"copy\0").expect("written");
    }
    let copied = call(
        &mut session,
        SPRITE_OP,
        &[BY_POINTER + 27, AREA, lying, NAME_AT],
    );
    let number = copied.map_or_else(|fail| error_number(&mut session, fail), |_| 0);
    assert_eq!(number, UNRECOGNISED);
    let count = session.kernel.os().read_word(AREA + 4).expect("read");
    assert_eq!(count, 1, "nothing appended");
}

/// Where a name a test gives lies, in the slot.
const NAME_AT: u32 = 0x9200;
/// A second area, in the slot.
const OTHER_AREA: u32 = 0x4_0000;

/// `OS_SpriteOp` `reason` on the user area at `area`, R2 to R6 as given.
fn user_op(session: &mut Session, reason: u32, area: u32, rest: &[u32]) -> Result<Regs, u32> {
    let mut r = vec![USER + reason, area];
    r.extend_from_slice(rest);
    call(session, SPRITE_OP, &r).map_err(|fail| error_number(session, fail))
}

/// An empty user area of 64 KiB at `area`.
fn empty_area(session: &mut Session, area: u32) {
    {
        let mut os = session.kernel.os();
        os.write_word(area, 0x1_0000).expect("written");
        os.write_word(area + 8, 16).expect("written");
    }
    user_op(session, 9, area, &[]).expect("initialised");
}

/// Sprite `name`, eight by eight in mode 12, made in the area at `area`.
fn made(session: &mut Session, area: u32, name: &[u8]) {
    session
        .kernel
        .os()
        .write_bytes(NAME_AT, name)
        .expect("written");
    user_op(session, 15, area, &[NAME_AT, 0, 8, 8, 12]).expect("made");
}

#[test]
fn a_merge_into_an_area_counting_past_its_sprites_keeps_the_file_s() {
    let mut session = with_files(&[]);
    empty_area(&mut session, AREA);
    made(&mut session, AREA, b"a\0");
    made(&mut session, AREA, b"b\0");
    let file = 0x9300;
    session
        .kernel
        .os()
        .write_bytes(file, b"HostFS:$.Spr\0")
        .expect("written");
    user_op(&mut session, 12, AREA, &[file]).expect("saved");
    empty_area(&mut session, OTHER_AREA);
    session
        .kernel
        .os()
        .write_bytes(NAME_AT, b"a\0")
        .expect("written");
    user_op(&mut session, 15, OTHER_AREA, &[NAME_AT, 0, 4, 4, 12]).expect("its own a");
    session
        .kernel
        .os()
        .write_word(OTHER_AREA + 4, 3)
        .expect("a lying count");
    user_op(&mut session, 11, OTHER_AREA, &[file]).expect("merged");
    for name in [&b"a\0"[..], b"b\0"] {
        session
            .kernel
            .os()
            .write_bytes(NAME_AT, name)
            .expect("written");
        let info = user_op(&mut session, 40, OTHER_AREA, &[NAME_AT]).expect("found");
        assert_eq!(
            (info.r[3], info.r[4]),
            (8, 8),
            "{} kept from the file",
            String::from_utf8_lossy(name)
        );
    }
}

/// A file of sprites `a` and `b` saved from [`AREA`], and where its name is.
fn saved_file(session: &mut Session) -> u32 {
    empty_area(session, AREA);
    made(session, AREA, b"a\0");
    made(session, AREA, b"b\0");
    let file = 0x9300;
    session
        .kernel
        .os()
        .write_bytes(file, b"HostFS:$.Spr\0")
        .expect("written");
    user_op(session, 12, AREA, &[file]).expect("saved");
    file
}

#[test]
fn a_merge_past_an_own_sprite_misplaced_in_its_chain_merges_whole() {
    let mut session = with_files(&[]);
    let file = saved_file(&mut session);
    empty_area(&mut session, OTHER_AREA);
    made(&mut session, OTHER_AREA, b"a\0");
    let own = OTHER_AREA + 16;
    let next = session.kernel.os().read_word(own).expect("read");
    session
        .kernel
        .os()
        .write_word(own, next - 2)
        .expect("a sprite ending part way into a word");
    user_op(&mut session, 11, OTHER_AREA, &[file]).expect("merged");
    let count = session.kernel.os().read_word(OTHER_AREA + 4).expect("read");
    assert_eq!(count, 3, "its own and the file's");
}

#[test]
fn a_merge_into_an_area_whose_free_space_lies_before_its_sprites_is_refused() {
    let mut session = with_files(&[]);
    let file = saved_file(&mut session);
    empty_area(&mut session, OTHER_AREA);
    session
        .kernel
        .os()
        .write_word(OTHER_AREA + 12, 8)
        .expect("free space before the first sprite");
    let mut before = [0u8; 16];
    session
        .kernel
        .os()
        .read_bytes(OTHER_AREA, &mut before)
        .expect("read");
    assert_eq!(
        user_op(&mut session, 11, OTHER_AREA, &[file]).map(drop),
        Err(UNRECOGNISED)
    );
    let mut after = [0u8; 16];
    session
        .kernel
        .os()
        .read_bytes(OTHER_AREA, &mut after)
        .expect("read");
    assert_eq!(after, before, "the area's control block as it was");
}

#[test]
fn a_palette_too_short_for_the_one_written_is_refused_and_the_image_kept() {
    let mut session = drivers(b"");
    let image = 44 + 4 * 8;
    hostile_sprite(
        &mut session,
        0x1_0000,
        [image + 4, 0, 0, 0, 31, image, image, 12],
    );
    let table = 0x9400;
    {
        let mut os = session.kernel.os();
        os.write_word(AREA + 16 + image, 0x1234_5678)
            .expect("its pixels");
        for colour in 0..16u32 {
            os.write_word(table + 4 * colour, colour << 8)
                .expect("the table");
        }
    }
    let written = call(
        &mut session,
        tairix_riscos_kernel::swi::colour_trans::WRITE_PALETTE,
        &[AREA, AREA + 16, table, 0, 1],
    );
    let number = written.map_or_else(|fail| error_number(&mut session, fail), |_| 0);
    assert_eq!(number, 0xA08, "the palette's shortness reported");
    let kept = session
        .kernel
        .os()
        .read_word(AREA + 16 + image)
        .expect("read");
    assert_eq!(kept, 0x1234_5678, "the image as it was");
}

#[test]
fn a_conversion_memory_cannot_hold_changes_nothing_of_the_sprite() {
    let mut session = drivers(b"");
    let end = tairix_riscos_kernel::memory::layout::APPLICATION + SLOT_BYTES;
    let area = end - 0x100;
    {
        let mut os = session.kernel.os();
        os.write_word(area, 0x100).expect("written");
        os.write_word(area + 8, 16).expect("written");
        os.write_bytes(NAME_AT, b"deep\0").expect("written");
    }
    user_op(&mut session, 9, area, &[]).expect("initialised");
    user_op(&mut session, 15, area, &[NAME_AT, 0, 7, 7, 0x3016_80B5]).expect("made");
    user_op(&mut session, 38, area, &[NAME_AT, 0xC000_0000]).expect("an alpha channel");
    let sprite = area + 16;
    let image = session
        .kernel
        .os()
        .read_word(sprite + 32)
        .expect("its image");
    let held = |session: &mut Session| {
        let mut bytes = vec![0u8; 44 + 7 * 7 * 4];
        session
            .kernel
            .os()
            .read_bytes(sprite, &mut bytes)
            .expect("read");
        bytes
    };
    assert_eq!(image, 44, "no palette, no mask");
    let before = held(&mut session);
    session
        .kernel
        .os()
        .write_word(area, 0x10_0000)
        .expect("an area claiming more than memory holds");
    assert_eq!(
        user_op(&mut session, 38, area, &[NAME_AT, 0x8000_0000]).map(drop),
        Err(UNRECOGNISED),
        "an alpha mask past memory's end"
    );
    assert_eq!(held(&mut session), before, "its alpha and mode word kept");
}
