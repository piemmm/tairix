//! Palettes in a session, as RISC OS 5 holds and reports them: `PaletteV`'s
//! block calls, `OS_ReadPalette`'s report of how an entry shows beside
//! ColourTrans's words as held, and `ColourTrans_ReadPalette` and
//! `WritePalette` on the screen and on sprites.

mod common;

use common::{call, error_number, select_depth};
use tairix_riscos_filing::session::Session;
use tairix_riscos_kernel::os::Fail;
use tairix_riscos_kernel::swi::chunk::BLEND_TABLE;
use tairix_riscos_kernel::swi::colour_trans as ct;
use tairix_riscos_kernel::swi::os::{CALL_A_VECTOR, READ_PALETTE, SPRITE_OP};

/// Where the tests write words, and read them back.
const WORDS: u32 = 0x9000;
const READ: u32 = 0xA000;
/// A sprite area, and a sprite name.
const AREA: u32 = 0x1_0000;
const AREA_BYTES: u32 = 0x8000;
const NAME: u32 = 0xB000;
/// `PaletteV` and its block reasons.
const PALETTE_V: u32 = 0x23;
const WRITE_BLOCK: u32 = 8;
/// A logical colour's entries, both flash states.
const NORMAL: u32 = 16;

fn mode(session: &mut Session, number: u8) {
    common::vdu(session, &[22, number]).expect("a mode");
}

fn write_words(session: &mut Session, at: u32, words: &[u32]) {
    let os = &mut session.kernel.os();
    for (i, &word) in (0u32..).zip(words) {
        os.write_word(at + 4 * i, word).expect("written");
    }
}

fn read_words(session: &mut Session, at: u32, count: u32) -> Vec<u32> {
    let os = &mut session.kernel.os();
    (0..count)
        .map(|i| os.read_word(at + 4 * i).expect("read"))
        .collect()
}

/// `OS_ReadPalette`'s two words for `logical`.
fn reported(session: &mut Session, logical: u32) -> [u32; 2] {
    let regs = call(session, READ_PALETTE, &[logical, NORMAL]).expect("read");
    [regs.r[2], regs.r[3]]
}

/// `count` entries' words as ColourTrans reads the current palette, both
/// flash states each.
fn held(session: &mut Session, count: u32) -> Vec<u32> {
    let regs = call(
        session,
        ct::READ_PALETTE,
        &[u32::MAX, u32::MAX, READ, 0x800, 2],
    )
    .expect("read");
    assert_eq!(regs.r[2], READ + 8 * count.max(16), "past what it wrote");
    read_words(session, READ, 2 * count)
}

/// Six words, each low byte its own.
fn marked() -> Vec<u32> {
    (0..6)
        .map(|i| ((0x10_0000 + i * 0x1_0101) << 8) | (i + 0x20))
        .collect()
}

#[test]
fn a_block_write_takes_an_entry_s_two_states_in_turn_or_a_single_state_s_word() {
    let mut session = common::drivers(b"");
    let words = marked();
    write_words(&mut session, WORDS, &words);
    mode(&mut session, 12);
    call(
        &mut session,
        CALL_A_VECTOR,
        &[
            0,
            3 | (NORMAL << 24),
            WORDS,
            0,
            WRITE_BLOCK,
            0,
            0,
            0,
            0,
            PALETTE_V,
        ],
    )
    .expect("written");
    assert_eq!(reported(&mut session, 0), [0x1000_0011, 0x1101_0112]);
    assert_eq!(reported(&mut session, 2), [0x1404_0411, 0x1505_0512]);
    mode(&mut session, 12);
    call(
        &mut session,
        CALL_A_VECTOR,
        &[
            0,
            3 | (17 << 24),
            WORDS,
            0,
            WRITE_BLOCK,
            0,
            0,
            0,
            0,
            PALETTE_V,
        ],
    )
    .expect("written");
    assert_eq!(
        reported(&mut session, 1),
        [0x1101_0111, 0x0000_FF12],
        "the second state kept"
    );
    mode(&mut session, 12);
    write_words(&mut session, WORDS + 0x100, &[5, 2, 9]);
    call(
        &mut session,
        CALL_A_VECTOR,
        &[
            WORDS + 0x100,
            3 | (NORMAL << 24),
            WORDS,
            0,
            WRITE_BLOCK,
            0,
            0,
            0,
            0,
            PALETTE_V,
        ],
    )
    .expect("written");
    assert_eq!(reported(&mut session, 5), [0x1000_0011, 0x1101_0112]);
    assert_eq!(reported(&mut session, 9), [0x1404_0411, 0x1505_0512]);
}

#[test]
fn colourtrans_reads_the_words_as_held_where_os_read_palette_reports_how_each_shows() {
    let mut session = common::drivers(b"");
    mode(&mut session, 12);
    common::vdu(
        &mut session,
        &[
            19, 4, 144, 1, 2, 3, 19, 3, 17, 255, 0, 0, 19, 3, 18, 255, 0, 0,
        ],
    )
    .expect("set");
    common::vdu(&mut session, &[19, 7, 145, 0x40, 0x40, 0x40]).expect("set");
    assert_eq!(
        reported(&mut session, 3),
        [0x0000_FF10; 2],
        "both states alike"
    );
    assert_eq!(reported(&mut session, 4), [0x0302_0190; 2], "of supremacy");
    assert_eq!(reported(&mut session, 7), [0x4040_4091, 0xFFFF_FF12]);
    let words = held(&mut session, 16);
    assert_eq!(
        words[6..10],
        [0x0000_FF00, 0x0000_FF00, 0x0302_01FF, 0x0302_01FF]
    );
    assert_eq!(words[14..18], [0x4040_40FF, 0xFFFF_FF00, 0, 0xFFFF_FF00]);
}

#[test]
fn a_palette_read_is_sized_refused_where_too_small_and_absent_in_a_deeper_mode() {
    let mut session = common::drivers(b"");
    mode(&mut session, 12);
    let size = |session: &mut Session, r0: u32, r1: u32, flags: u32| {
        call(session, ct::READ_PALETTE, &[r0, r1, 0, 0, flags])
            .expect("sized")
            .r[3]
    };
    assert_eq!(size(&mut session, u32::MAX, u32::MAX, 0), 0x40);
    assert_eq!(size(&mut session, u32::MAX, u32::MAX, 2), 0x80);
    assert_eq!(size(&mut session, 15, 0, 0), 0x400);
    let small = call(
        &mut session,
        ct::READ_PALETTE,
        &[u32::MAX, u32::MAX, READ, 20, 0],
    )
    .expect_err("small");
    assert_eq!(error_number(&mut session, small), 0xA06);
    let read = call(&mut session, ct::READ_PALETTE, &[0, 0, READ, 0x10, 0]).expect("read");
    assert_eq!([read.r[2], read.r[3]], [READ + 8, 8]);
    assert_eq!(
        read_words(&mut session, READ, 2),
        [0, 0xFFFF_FF00],
        "mode 0's default"
    );
    call(
        &mut session,
        ct::READ_PALETTE,
        &[u32::MAX, 0, READ, 0x80, 2],
    )
    .expect("read");
    assert_eq!(
        read_words(&mut session, READ + 0x40, 4),
        [0, 0, 0x0000_FF00, 0x0000_FF00],
        "the default steady"
    );
    write_words(&mut session, WORDS, &marked());
    call(&mut session, ct::READ_PALETTE, &[0, WORDS, READ, 8, 0]).expect("read");
    assert_eq!(
        read_words(&mut session, READ, 2),
        marked()[..2],
        "a block of words as given"
    );
    select_depth(&mut session, 4).expect("a deeper mode");
    let deep = call(
        &mut session,
        ct::READ_PALETTE,
        &[u32::MAX, u32::MAX, 0, 0, 0],
    )
    .expect_err("none");
    assert_eq!(error_number(&mut session, deep), 0xA07);
}

#[test]
fn a_palette_written_is_held_as_given_without_making_the_match_stale() {
    let mut session = common::drivers(b"");
    mode(&mut session, 12);
    let red = 0x0000_FF00;
    assert_eq!(
        call(&mut session, ct::RETURN_COLOUR_NUMBER, &[red])
            .expect("matched")
            .r[0],
        1
    );
    let words: Vec<u32> = (0..16u32)
        .map(|i| ((((i * 0x3B5) ^ (i << 9)) & 0xFF_FFFF) << 8) | ((i * 0x29 + 0x5A) & 0xFF))
        .collect();
    write_words(&mut session, WORDS, &words);
    let regs = call(
        &mut session,
        ct::WRITE_PALETTE,
        &[u32::MAX, u32::MAX, WORDS, 0, 0],
    )
    .expect("written");
    assert_eq!(regs.r[..5], [u32::MAX, u32::MAX, WORDS, 0, 0]);
    assert_eq!(
        call(&mut session, ct::RETURN_COLOUR_NUMBER, &[red])
            .expect("matched")
            .r[0],
        1,
        "as last read"
    );
    call(&mut session, ct::INVALIDATE_CACHE, &[]).expect("invalidated");
    assert_eq!(
        call(&mut session, ct::RETURN_COLOUR_NUMBER, &[red])
            .expect("matched")
            .r[0],
        7
    );
    let held = held(&mut session, 16);
    assert_eq!(
        held[..6],
        [0x5A, 0x5A, 0x1_B583, 0x1_B583, 0x3_6AAC, 0x3_6AAC]
    );
}

/// A sprite area holding `s4`, mode 12 with a palette, `n4` without one,
/// and `s8`, mode 15 with the palette a mode 12 screen gives it.
fn sprites(session: &mut Session) {
    mode(session, 12);
    write_words(session, AREA, &[AREA_BYTES, 0, 16]);
    call(session, SPRITE_OP, &[0x109, AREA]).expect("an area");
    for (name, palette, mode) in [(&b"s4\0"[..], 1, 12), (b"n4\0", 0, 12), (b"s8\0", 1, 15)] {
        session.kernel.os().write_bytes(NAME, name).expect("named");
        call(
            session,
            SPRITE_OP,
            &[0x10F, AREA, NAME, palette, 8, 8, mode],
        )
        .expect("made");
    }
}

fn named(session: &mut Session, name: &[u8]) -> u32 {
    session.kernel.os().write_bytes(NAME, name).expect("named");
    NAME
}

#[test]
fn a_sprite_s_palette_is_read_as_held_and_made_for_one_written_without() {
    let mut session = common::drivers(b"");
    sprites(&mut session);
    let name = named(&mut session, b"n4\0");
    call(&mut session, ct::READ_PALETTE, &[AREA, name, READ, 0x40, 0]).expect("read");
    assert_eq!(
        read_words(&mut session, READ, 3),
        [0, 0x0000_FF00, 0x00FF_0000],
        "its mode's default"
    );
    let name = named(&mut session, b"s8\0");
    let size = call(&mut session, ct::READ_PALETTE, &[AREA, name, 0, 0, 0])
        .expect("sized")
        .r[3];
    assert_eq!(size, 0x400, "a short eight-bit palette shown whole");
    call(
        &mut session,
        ct::READ_PALETTE,
        &[AREA, name, READ, 0x400, 0],
    )
    .expect("read");
    assert_eq!(read_words(&mut session, READ, 2), [0, 0x7700]);
    assert_eq!(read_words(&mut session, READ + 0x40, 2), [0x8800, 0xFF00]);
    write_words(&mut session, WORDS, &marked());
    let name = named(&mut session, b"n4\0");
    call(&mut session, ct::WRITE_PALETTE, &[AREA, name, WORDS, 0, 0]).expect("written");
    call(&mut session, ct::READ_PALETTE, &[AREA, name, READ, 0x80, 2]).expect("read");
    let words = marked();
    assert_eq!(
        read_words(&mut session, READ, 4),
        [words[0], words[0], words[1], words[1]]
    );
}

#[test]
fn the_screen_s_palette_is_not_written_while_output_goes_to_a_sprite() {
    let mut session = common::drivers(b"");
    sprites(&mut session);
    let name = named(&mut session, b"s4\0");
    call(&mut session, SPRITE_OP, &[0x13C, AREA, name, 0]).expect("switched");
    let refused: Fail = call(
        &mut session,
        ct::WRITE_PALETTE,
        &[u32::MAX, u32::MAX, WORDS, 0, 0],
    )
    .expect_err("refused");
    assert_eq!(error_number(&mut session, refused), 0xA03);
}

#[test]
fn select_table_reads_r5_for_a_sprite_alone_and_generate_table_always() {
    // Mode 12's table sizes as RISC OS 5 gave them, R5 asking each for the
    // source's palette words.
    let mut session = common::drivers(b"");
    sprites(&mut session);
    write_words(&mut session, WORDS, &[1, 640, 480, 2, u32::MAX, u32::MAX]);
    let name = named(&mut session, b"n4\0");
    for (swi, r0, r1, size) in [
        (ct::SELECT_TABLE, u32::MAX, u32::MAX, 0x10),
        (ct::GENERATE_TABLE, u32::MAX, u32::MAX, 0x40),
        (ct::SELECT_TABLE, 12, u32::MAX, 0x10),
        (ct::SELECT_TABLE, WORDS, u32::MAX, 0x10),
        (ct::SELECT_TABLE, AREA, name, 0x40),
    ] {
        let sized =
            call(&mut session, swi, &[r0, r1, u32::MAX, u32::MAX, 0, 1 << 24]).expect("sized");
        assert_eq!(sized.r[4], size, "SWI &{swi:X}, R0 &{r0:X}");
    }
}

#[test]
fn a_sprite_without_a_palette_takes_its_mode_s_default_whatever_r5_asks() {
    // Mode 12 with colour 1 made white: n4's table as RISC OS 5 built it,
    // by name or pointer, R5's bit 1 asking for the current palette or not.
    let mut session = common::drivers(b"");
    sprites(&mut session);
    common::vdu(&mut session, &[19, 1, 16, 0xFF, 0xFF, 0xFF]).expect("colour 1 white");
    call(&mut session, ct::INVALIDATE_CACHE, &[]).expect("invalidated");
    let name = named(&mut session, b"n4\0");
    let pointer = call(&mut session, SPRITE_OP, &[0x118, AREA, name])
        .expect("selected")
        .r[2];
    for (swi, sprite, flags) in [
        (ct::SELECT_TABLE, name, 0),
        (ct::SELECT_TABLE, name, 2),
        (ct::GENERATE_TABLE, name, 2),
        (ct::GENERATE_TABLE, pointer, 3),
    ] {
        call(
            &mut session,
            swi,
            &[AREA, sprite, u32::MAX, u32::MAX, READ, flags],
        )
        .expect("built");
        let mut table = [0; 16];
        session
            .kernel
            .os()
            .read_bytes(READ, &mut table)
            .expect("read");
        assert_eq!(
            table,
            [0, 9, 2, 3, 4, 5, 6, 1, 0, 9, 2, 3, 4, 5, 6, 1],
            "SWI &{swi:X}, R5 {flags}"
        );
    }
}

#[test]
fn a_sprite_palette_past_its_pixels_colours_is_read_as_its_mode_s() {
    // A four-bit sprite holding twenty entries, which RISC OS 5 read as
    // sixteen.
    let mut session = common::drivers(b"");
    mode(&mut session, 12);
    let name = named(&mut session, b"lone\0");
    let sprite = common::lone_sprite(&mut session, AREA, name, 12, true).expect("made");
    common::grow_palette(&mut session, AREA, sprite, 20, 0x4040_4000).expect("grown");
    let read = call(&mut session, ct::READ_PALETTE, &[AREA, name, 0, 0, 0]).expect("sized");
    assert_eq!(read.r[3], 0x40, "ReadPalette's size");
    let table = call(
        &mut session,
        ct::SELECT_TABLE,
        &[AREA, name, u32::MAX, u32::MAX, 0, 0],
    )
    .expect("sized");
    assert_eq!(table.r[4], 0x10, "SelectTable's size");
    let blend = call(
        &mut session,
        BLEND_TABLE,
        &[0, AREA, name, u32::MAX, u32::MAX, 128, 0],
    )
    .expect("sized");
    assert_eq!(blend.r[..3], [0x10, 0x10, 0x100], "BlendTable's colours");
}

#[test]
fn an_eight_bit_mode_s_flags_say_whether_it_writes_its_palette_in_groups() {
    let mut session = common::drivers(b"");
    common::select(
        &mut session,
        &[1, 640, 480, 3, u32::MAX, 0, 0x80, 3, 63, u32::MAX],
    )
    .expect("flagged full, counting 63 colours");
    let default_17 = reported(&mut session, 17);
    common::vdu(&mut session, &[19, 1, 16, 0x10, 0x20, 0x30]).expect("entry 1 set");
    assert_eq!(reported(&mut session, 1)[0] & !0xFF, 0x3020_1000);
    assert_eq!(
        reported(&mut session, 17),
        default_17,
        "an entry of its own, no group"
    );
}
