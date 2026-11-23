//! Sprite areas and the sprites in them through `OS_SpriteOp`, held to what
//! genuine RISC OS 5.30 was recorded doing: making, naming, finding, copying
//! and deleting sprites, their pixels and masks, their shape changed, their
//! palettes, verification, and the system area, dynamic area 3; plotting into
//! a sprite output is switched to, and the JPEG SWIs' refusals.

mod common;

use common::{call, drivers, error_number, insert_module, select, vdu, BY_POINTER, USER};
use tairix_riscos_filing::session::{Session, SLOT_BYTES};
use tairix_riscos_kernel::memory::layout::APPLICATION;
use tairix_riscos_kernel::os::{Fail, Regs};
use tairix_riscos_kernel::swi::chunk::SPRITE_EXTEND;
use tairix_riscos_kernel::swi::os::{
    BYTE, CHANGE_DYNAMIC_AREA, READ_DYNAMIC_AREA, READ_VDU_VARIABLES, SET_COLOUR, SPRITE_OP,
};
use tairix_riscos_kernel::swi::X;
use tairix_vdu::mode::var::{N_COLOUR, X_EIG, Y_EIG};

/// Where a test's user area and the names it gives lie, in the slot.
const AREA: u32 = 0x2_0000;
const AREA_BYTES: u32 = 0x1_0000;
const NAME: u32 = 0x9000;
const OTHER_NAME: u32 = 0x9100;

/// The screen's first byte, as `OS_ReadVduVariables` 148 gives it.
fn screen_start(session: &mut Session) -> u32 {
    let asked = AREA + 0x7100;
    session.kernel.os().write_word(asked, 148).expect("written");
    session
        .kernel
        .os()
        .write_word(asked + 4, u32::MAX)
        .expect("written");
    swi(
        session,
        READ_VDU_VARIABLES,
        [asked, asked + 8, 0, 0, 0, 0, 0, 0],
    )
    .expect("read");
    session
        .kernel
        .os()
        .read_word(asked + 8)
        .expect("the screen")
}

/// `OS_SpriteOp` with registers R0 to R7: what it leaves, or its error's
/// number.
fn op(session: &mut Session, low: [u32; 8]) -> Result<Regs, u32> {
    swi(session, SPRITE_OP, low)
}

/// Writes `text` with a terminating zero at `at`.
fn put_name(session: &mut Session, at: u32, text: &[u8]) {
    let mut os = session.kernel.os();
    os.write_bytes(at, text).expect("written");
    os.write_byte(at + u32::try_from(text.len()).expect("short"), 0)
        .expect("written");
}

/// A session with an empty user area at [`AREA`].
fn with_area() -> Session {
    let mut session = drivers(b"");
    {
        let mut os = session.kernel.os();
        os.write_word(AREA, AREA_BYTES).expect("written");
        os.write_word(AREA + 8, 16).expect("written");
    }
    op(&mut session, [USER + 9, AREA, 0, 0, 0, 0, 0, 0]).expect("initialised");
    session
}

/// Makes sprite `name`, `width` by `height` in mode `mode`, with a palette
/// where `palette`.
fn make(session: &mut Session, name: &[u8], width: u32, height: u32, mode: u32, palette: bool) {
    put_name(session, NAME, name);
    op(
        session,
        [
            USER + 15,
            AREA,
            NAME,
            u32::from(palette),
            width,
            height,
            mode,
            0,
        ],
    )
    .unwrap_or_else(|e| panic!("made {}: error &{e:X}", String::from_utf8_lossy(name)));
}

/// The address of sprite `name`.
fn address(session: &mut Session, name: &[u8]) -> u32 {
    put_name(session, NAME, name);
    op(session, [USER + 24, AREA, NAME, 0, 0, 0, 0, 0])
        .expect("found")
        .r[2]
}

/// The words of the sprite control block at `at`, its name apart.
fn header(session: &mut Session, at: u32) -> [u32; 8] {
    let mut os = session.kernel.os();
    let word = |os: &mut tairix_riscos_kernel::os::Os<'_>, offset: u32| {
        os.read_word(at + offset).expect("readable")
    };
    [0, 16, 20, 24, 28, 32, 36, 40].map(|offset| word(&mut os, offset))
}

/// The area's control block.
fn block(session: &mut Session) -> [u32; 4] {
    let mut os = session.kernel.os();
    [0, 4, 8, 12].map(|offset| os.read_word(AREA + offset).expect("readable"))
}

#[test]
fn a_new_sprite_is_laid_out_as_risc_os_5_makes_it() {
    let mut session = with_area();
    make(&mut session, b"S", 5, 3, 0, false);
    let at = address(&mut session, b"s");
    assert_eq!(at, AREA + 16, "the first sprite of the area");
    assert_eq!(header(&mut session, at), [0x38, 0, 2, 0, 4, 0x2C, 0x2C, 0]);
    assert_eq!(block(&mut session), [AREA_BYTES, 1, 16, 0x48]);
    let name = session.kernel.os().read_word(at + 4).expect("readable");
    assert_eq!(
        name,
        u32::from(b's'),
        "stored in lower case, padded with zeros"
    );
    // Mode 2 doubles its pixels: five take two words.
    make(&mut session, b"two", 5, 3, 2, true);
    let at = address(&mut session, b"two");
    assert_eq!(header(&mut session, at), [0xC4, 1, 2, 0, 7, 0xAC, 0xAC, 2]);
    // An eight-bit mode's palette is VIDC1's sixty-four entries.
    make(&mut session, b"eight", 5, 3, 28, true);
    let at = address(&mut session, b"eight");
    assert_eq!(header(&mut session, at)[5], 0x22C);
}

#[test]
fn a_width_of_nought_is_minus_one_words_and_a_huge_one_finds_no_room() {
    let mut session = with_area();
    make(&mut session, b"w0", 0, 3, 28, false);
    let at = address(&mut session, b"w0");
    assert_eq!(
        header(&mut session, at),
        [0x2C, u32::MAX, 2, 0, 0x1F, 0x2C, 0x2C, 28]
    );
    put_name(&mut session, NAME, b"big");
    assert_eq!(
        op(
            &mut session,
            [USER + 15, AREA, NAME, 0, 0x1000_0000, 1, 28, 0]
        )
        .err(),
        Some(0x82)
    );
    assert_eq!(
        op(
            &mut session,
            [USER + 15, AREA, NAME, 0, 0xFFFF_FFFD, 3, 28, 0]
        )
        .err(),
        Some(0x82),
        "a negative width, which RISC OS 5 makes a corrupt sprite of"
    );
}

#[test]
fn modes_no_sprite_may_be_in_are_refused_as_recorded() {
    let mut session = with_area();
    put_name(&mut session, NAME, b"x");
    let refusal = |session: &mut Session, mode: u32| {
        op(session, [USER + 15, AREA, NAME, 0, 4, 4, mode, 0]).err()
    };
    assert_eq!(refusal(&mut session, 3), Some(0x708), "a text mode");
    assert_eq!(refusal(&mut session, 0x80), Some(0x708), "the shadow bit");
    assert_eq!(
        refusal(&mut session, 60),
        Some(0x19),
        "a mode nothing knows"
    );
    assert_eq!(
        refusal(&mut session, 0x0016_80B5),
        Some(0x719),
        "a type 0 word"
    );
    assert_eq!(
        refusal(&mut session, 0x302C_80B7),
        Some(0x719),
        "91 dots to the inch"
    );
    assert_eq!(
        refusal(&mut session, 0x7840_8051),
        Some(0x719),
        "a paletted type with flags"
    );
    assert_eq!(
        refusal(&mut session, 0x7860_1051),
        Some(0x719),
        "a colour space not RGB"
    );
    assert_eq!(
        refusal(&mut session, 0x7801_0001),
        Some(0x719),
        "a broken fixed pattern"
    );
    put_name(&mut session, NAME, b"deep");
    assert_eq!(
        op(
            &mut session,
            [USER + 15, AREA, NAME, 1, 4, 4, 0x3016_80B5, 0]
        )
        .err(),
        Some(0x718),
        "a palette for sixteen million colours"
    );
}

#[test]
fn names_match_without_regard_to_ascii_case_and_end_at_any_control_byte() {
    let mut session = with_area();
    make(&mut session, b"Abc", 4, 4, 12, false);
    for spelt in [&b"ABC"[..], b"abc\r", b"abc def", b"aBc\t"] {
        put_name(&mut session, NAME, spelt);
        assert!(op(&mut session, [USER + 40, AREA, NAME, 0, 0, 0, 0, 0]).is_ok());
    }
    make(&mut session, b"abcdefghijklmn", 2, 2, 12, false);
    put_name(&mut session, NAME, b"ABCDEFGHIJKLzz");
    assert!(
        op(&mut session, [USER + 40, AREA, NAME, 0, 0, 0, 0, 0]).is_ok(),
        "twelve bytes are a name"
    );
    put_name(&mut session, NAME, b"missing");
    assert_eq!(
        op(&mut session, [USER + 40, AREA, NAME, 0, 0, 0, 0, 0]).err(),
        Some(0x86)
    );
    assert_eq!(
        op(&mut session, [BY_POINTER + 40, AREA, 0, 0, 0, 0, 0, 0]).err(),
        Some(0x86),
        "a null pointer"
    );
}

#[test]
fn a_name_is_returned_within_its_buffer_and_a_number_past_the_count_refused() {
    let mut session = with_area();
    make(&mut session, b"first", 2, 2, 12, false);
    make(&mut session, b"second", 2, 2, 12, false);
    let regs = op(&mut session, [USER + 13, AREA, NAME, 20, 2, 0, 0, 0]).expect("named");
    assert_eq!(regs.r[3], 6);
    let mut name = [0u8; 7];
    session
        .kernel
        .os()
        .read_bytes(NAME, &mut name)
        .expect("readable");
    assert_eq!(&name, b"second\0");
    let regs = op(&mut session, [USER + 13, AREA, NAME, 4, 1, 0, 0, 0]).expect("named");
    assert_eq!(regs.r[3], 3, "cut to fit with its zero");
    let regs = op(&mut session, [USER + 13, AREA, NAME, 0, 1, 0, 0, 0]).expect("named");
    assert_eq!(regs.r[3], 0);
    assert_eq!(
        op(&mut session, [USER + 13, AREA, NAME, 20, 3, 0, 0, 0]).err(),
        Some(0x86)
    );
    assert_eq!(
        op(&mut session, [USER + 13, AREA, NAME, 20, 0, 0, 0, 0]).err(),
        Some(0x86)
    );
}

#[test]
fn renaming_or_copying_onto_a_name_already_used_does_nothing() {
    let mut session = with_area();
    make(&mut session, b"one", 4, 2, 12, false);
    make(&mut session, b"two", 4, 2, 12, false);
    put_name(&mut session, NAME, b"one");
    put_name(&mut session, OTHER_NAME, b"TWO");
    let before = block(&mut session);
    op(
        &mut session,
        [USER + 26, AREA, NAME, OTHER_NAME, 0, 0, 0, 0],
    )
    .expect("no error");
    op(
        &mut session,
        [USER + 27, AREA, NAME, OTHER_NAME, 0, 0, 0, 0],
    )
    .expect("no error");
    assert_eq!(block(&mut session), before);
    put_name(&mut session, OTHER_NAME, b"Three");
    op(
        &mut session,
        [USER + 27, AREA, NAME, OTHER_NAME, 0, 0, 0, 0],
    )
    .expect("copied");
    assert_eq!(block(&mut session)[1], 3, "the copy appended");
    let regs = op(&mut session, [USER + 13, AREA, NAME, 20, 3, 0, 0, 0]).expect("named");
    assert_eq!(regs.r[3], 5);
    op(&mut session, [USER + 25, AREA, NAME, 0, 0, 0, 0, 0]).expect("deleted");
    assert_eq!(block(&mut session)[1], 2);
    let regs = op(&mut session, [USER + 13, AREA, NAME, 20, 1, 0, 0, 0]).expect("named");
    assert_eq!(regs.r[3], 3, "the area closed up: two first");
}

#[test]
fn pixels_are_masked_to_the_depth_and_eight_bits_take_a_colour_and_a_tint() {
    let mut session = with_area();
    make(&mut session, b"p", 4, 2, 12, false);
    put_name(&mut session, NAME, b"p");
    op(&mut session, [USER + 42, AREA, NAME, 1, 0, 0x13, 0, 0]).expect("written");
    let regs = op(&mut session, [USER + 41, AREA, NAME, 1, 0, 0, 0, 0]).expect("read");
    assert_eq!((regs.r[5], regs.r[6]), (3, 0));
    assert_eq!(
        op(&mut session, [USER + 41, AREA, NAME, 4, 0, 0, 0, 0]).err(),
        Some(0x703)
    );
    make(&mut session, b"v", 4, 2, 10, false);
    put_name(&mut session, NAME, b"v");
    op(&mut session, [USER + 42, AREA, NAME, 0, 1, 5, 192, 0]).expect("written");
    let at = address(&mut session, b"v");
    let image = session.kernel.os().read_word(at + 0x2C).expect("readable");
    assert_eq!(
        image & 0xFFFF,
        0x2727,
        "the VIDC1 byte repeated across the field"
    );
    let regs = op(&mut session, [USER + 41, AREA, NAME, 0, 1, 0, 0, 0]).expect("read");
    assert_eq!((regs.r[5], regs.r[6]), (5, 0xC0));
}

#[test]
fn a_mask_is_made_opaque_after_the_image_and_removed_again() {
    let mut session = with_area();
    make(&mut session, b"m", 5, 3, 12, false);
    put_name(&mut session, NAME, b"m");
    let regs = op(&mut session, [USER + 43, AREA, NAME, 0, 0, 0, 0, 0]).expect("read");
    assert_eq!(regs.r[5], 1, "a sprite with no mask is opaque");
    op(&mut session, [USER + 44, AREA, NAME, 0, 0, 0, 0, 0]).expect("nothing to write");
    op(&mut session, [USER + 29, AREA, NAME, 0, 0, 0, 0, 0]).expect("masked");
    let at = address(&mut session, b"m");
    let words = header(&mut session, at);
    assert_eq!(
        (words[5], words[6]),
        (0x2C, 0x38),
        "the mask after the image"
    );
    assert_eq!(
        session.kernel.os().read_word(at + 0x38).expect("readable"),
        u32::MAX
    );
    op(&mut session, [USER + 44, AREA, NAME, 2, 1, 0, 0, 0]).expect("written");
    let regs = op(&mut session, [USER + 43, AREA, NAME, 2, 1, 0, 0, 0]).expect("read");
    assert_eq!(regs.r[5], 0);
    op(&mut session, [USER + 30, AREA, NAME, 0, 0, 0, 0, 0]).expect("unmasked");
    let words = header(&mut session, at);
    assert_eq!((words[0], words[5], words[6]), (0x38, 0x2C, 0x2C));
    assert_eq!(block(&mut session)[3], 16 + 0x38, "the area closed up");
}

#[test]
fn rows_and_columns_go_in_and_out_counted_from_the_bottom_left() {
    let mut session = with_area();
    make(&mut session, b"r", 4, 2, 28, false);
    put_name(&mut session, NAME, b"r");
    op(&mut session, [USER + 42, AREA, NAME, 0, 0, 7, 0, 0]).expect("bottom left");
    op(&mut session, [USER + 31, AREA, NAME, 0, 0, 0, 0, 0]).expect("a row below");
    let regs = op(&mut session, [USER + 40, AREA, NAME, 0, 0, 0, 0, 0]).expect("read");
    assert_eq!((regs.r[3], regs.r[4]), (4, 3));
    let regs = op(&mut session, [USER + 41, AREA, NAME, 0, 1, 0, 0, 0]).expect("read");
    assert_eq!(regs.r[5], 7, "the old bottom row is now the second");
    assert_eq!(
        op(&mut session, [USER + 31, AREA, NAME, 4, 0, 0, 0, 0]).err(),
        Some(0x703)
    );
    op(&mut session, [USER + 45, AREA, NAME, 0, 0, 0, 0, 0]).expect("a column at the left");
    let regs = op(&mut session, [USER + 40, AREA, NAME, 0, 0, 0, 0, 0]).expect("read");
    assert_eq!((regs.r[3], regs.r[4]), (5, 3));
    let regs = op(&mut session, [USER + 41, AREA, NAME, 1, 1, 0, 0, 0]).expect("read");
    assert_eq!(regs.r[5], 7, "moved right");
    op(
        &mut session,
        [USER + 58, AREA, NAME, 0, (-2i32).cast_unsigned(), 0, 0, 0],
    )
    .expect("two out");
    op(
        &mut session,
        [USER + 57, AREA, NAME, 1, (-2i32).cast_unsigned(), 0, 0, 0],
    )
    .expect("two out");
    let regs = op(&mut session, [USER + 40, AREA, NAME, 0, 0, 0, 0, 0]).expect("read");
    assert_eq!((regs.r[3], regs.r[4]), (3, 1));
    assert_eq!(
        op(
            &mut session,
            [USER + 57, AREA, NAME, 1, (-2i32).cast_unsigned(), 0, 0, 0]
        )
        .err(),
        Some(0x703)
    );
}

#[test]
fn flipping_left_to_right_moves_an_old_format_sprite_s_wastage_across() {
    let mut session = with_area();
    {
        let mut os = session.kernel.os();
        let at = AREA + 16;
        for (i, word) in (0u32..).zip([0x38, u32::from(b'f'), 0, 0, 0, 2, 0, 0x13, 0x2C, 0x2C, 12])
        {
            os.write_word(at + 4 * i, word).expect("written");
        }
        os.write_word(AREA + 4, 1).expect("written");
        os.write_word(AREA + 12, 16 + 0x38).expect("written");
        for row in 0..3 {
            os.write_word(at + 0x2C + 4 * row, 0x0004_3210)
                .expect("written");
        }
    }
    put_name(&mut session, NAME, b"f");
    op(&mut session, [USER + 47, AREA, NAME, 0, 0, 0, 0, 0]).expect("flipped");
    let words = header(&mut session, AREA + 16);
    assert_eq!(
        (words[3], words[4]),
        (0xC, 0x1F),
        "first and last bits swapped across"
    );
    let row = session
        .kernel
        .os()
        .read_word(AREA + 16 + 0x2C)
        .expect("readable");
    assert_eq!(row, 0x0123_4000);
}

#[test]
fn a_palette_made_where_there_was_one_is_rebuilt_from_its_first_sixteen() {
    let mut session = with_area();
    make(&mut session, b"p", 3, 2, 28, false);
    put_name(&mut session, NAME, b"p");
    op(
        &mut session,
        [USER + 37, AREA, NAME, 0x8000_0001, 0, 0, 0, 0],
    )
    .expect("made");
    let regs = op(&mut session, [USER + 37, AREA, NAME, u32::MAX, 0, 0, 0, 0]).expect("read");
    assert_eq!(regs.r[3], 256);
    let palette = regs.r[4];
    let entry = |session: &mut Session, i: u32| {
        session
            .kernel
            .os()
            .read_word(palette + 8 * i)
            .expect("readable")
    };
    assert_eq!(
        entry(&mut session, 1),
        0x1111_1110,
        "the default's second entry"
    );
    for i in 0..256 {
        let at = palette + 8 * i;
        let word = (i << 16) + (i << 8) + 0x100;
        session.kernel.os().write_word(at, word).expect("written");
    }
    op(&mut session, [USER + 37, AREA, NAME, 1, 0, 0, 0, 0]).expect("rebuilt");
    let regs = op(&mut session, [USER + 37, AREA, NAME, u32::MAX, 0, 0, 0, 0]).expect("read");
    assert_eq!(regs.r[3], 64);
    let palette = regs.r[4];
    let entry = |session: &mut Session, i: u32| {
        session
            .kernel
            .os()
            .read_word(palette + 8 * i)
            .expect("readable")
    };
    assert_eq!(entry(&mut session, 0), 0x10);
    assert_eq!(
        entry(&mut session, 63),
        0x0044_9910,
        "as RISC OS 5.30 rebuilt it"
    );
    op(&mut session, [USER + 37, AREA, NAME, 0, 0, 0, 0, 0]).expect("removed");
    let regs = op(&mut session, [USER + 37, AREA, NAME, u32::MAX, 0, 0, 0, 0]).expect("read");
    assert_eq!((regs.r[3], regs.r[4]), (0, 0));
    make(&mut session, b"deep", 3, 2, 0x3016_80B5, false);
    put_name(&mut session, NAME, b"deep");
    assert_eq!(
        op(&mut session, [USER + 37, AREA, NAME, 1, 0, 0, 0, 0]).err(),
        Some(0x716)
    );
}

#[test]
fn verification_passes_what_risc_os_5_passes_and_refuses_the_rest() {
    let mut session = with_area();
    make(&mut session, b"one", 5, 3, 12, false);
    make(&mut session, b"two", 4, 4, 12, false);
    assert!(op(&mut session, [USER + 17, AREA, 0, 0, 0, 0, 0, 0]).is_ok());
    let first = AREA + 16;
    let poke = |session: &mut Session, at: u32, value: u32| {
        session.kernel.os().write_word(at, value).expect("written");
    };
    poke(&mut session, first + 32, 0x3C);
    poke(&mut session, first + 36, 0x3C);
    assert!(
        op(&mut session, [USER + 17, AREA, 0, 0, 0, 0, 0, 0]).is_ok(),
        "an image past the sprite's end passes RISC OS 5's sum"
    );
    poke(&mut session, first + 32, 0x30);
    poke(&mut session, first + 36, 0x30);
    assert_eq!(
        op(&mut session, [USER + 17, AREA, 0, 0, 0, 0, 0, 0]).err(),
        Some(0x71E)
    );
    poke(&mut session, first + 32, 0x2C);
    poke(&mut session, first + 36, 0x2C);
    poke(&mut session, AREA + 4, 3);
    assert_eq!(
        op(&mut session, [USER + 17, AREA, 0, 0, 0, 0, 0, 0]).err(),
        Some(0x71E)
    );
}

#[test]
fn a_chain_leaving_its_area_is_refused_where_risc_os_5_would_wander() {
    let mut session = with_area();
    make(&mut session, b"one", 5, 3, 12, false);
    session
        .kernel
        .os()
        .write_word(AREA + 8, 18)
        .expect("written");
    put_name(&mut session, NAME, b"x");
    assert_eq!(
        op(&mut session, [USER + 40, AREA, NAME, 0, 0, 0, 0, 0]).err(),
        Some(0x71E)
    );
    session
        .kernel
        .os()
        .write_word(AREA + 8, 16)
        .expect("written");
    session
        .kernel
        .os()
        .write_word(AREA + 4, 0x7FFF_FFFF)
        .expect("written");
    session
        .kernel
        .os()
        .write_word(AREA + 16, 0)
        .expect("written");
    assert_eq!(
        op(&mut session, [USER + 40, AREA, NAME, 0, 0, 0, 0, 0]).err(),
        Some(0x86),
        "a sprite claiming no length is met again, so the walk ends"
    );
}

#[test]
fn unknown_reasons_and_area_values_are_refused_after_any_lookup() {
    let mut session = with_area();
    make(&mut session, b"s", 4, 4, 28, false);
    put_name(&mut session, NAME, b"s");
    put_name(&mut session, OTHER_NAME, b"nowhere");
    for reason in [0, 1, 4, 7, 18, 23, 63, 66, 255] {
        assert_eq!(
            op(&mut session, [USER + reason, AREA, NAME, 0, 0, 0, 0, 0]).err(),
            Some(0x709),
            "reason {reason}"
        );
    }
    assert_eq!(
        op(&mut session, [USER + 59, AREA, OTHER_NAME, 0, 0, 0, 0, 0]).err(),
        Some(0x86),
        "59 looks its sprite up first"
    );
    assert_eq!(
        op(&mut session, [USER + 59, AREA, NAME, 0, 0, 0, 0, 0]).err(),
        Some(0x709)
    );
    assert_eq!(
        op(&mut session, [768 + 8, AREA, 0, 0, 0, 0, 0, 0]).err(),
        Some(0x709)
    );
    assert_eq!(
        op(&mut session, [USER + 53, AREA, NAME, 0, 0, 0, 0, 0]).err(),
        Some(0x71F)
    );
}

#[test]
fn the_system_area_is_dynamic_area_3_made_empty_as_it_first_grows() {
    let mut session = drivers(b"");
    put_name(&mut session, NAME, b"x");
    assert_eq!(op(&mut session, [8, 0, 0, 0, 0, 0, 0, 0]).err(), Some(0x80));
    assert_eq!(
        op(&mut session, [15, 0, NAME, 0, 4, 4, 28, 0]).err(),
        Some(0x80)
    );
    let mut grow = Regs::new([3, 0x8000, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    session
        .kernel
        .os()
        .perform(X | CHANGE_DYNAMIC_AREA, &mut grow)
        .expect("grown");
    let regs = op(&mut session, [8, 0, 0, 0, 0, 0, 0, 0]).expect("read");
    assert_eq!(
        (regs.r[2], regs.r[3], regs.r[4], regs.r[5]),
        (0x8000, 0, 16, 16)
    );
    let mut byte = Regs::new([163, 242, 65, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    session
        .kernel
        .os()
        .perform(X | BYTE, &mut byte)
        .expect("read");
    assert_eq!((byte.r[1], byte.r[2]), (0xC8, 0x8000));
    put_name(&mut session, NAME, b"7");
    op(&mut session, [15, 0, NAME, 0, 12, 9, 28, 0]).expect("made");
    vdu(&mut session, &[23, 27, 0, 7, 0, 0, 0, 0, 0, 0]).expect("selected");
    let mut byte = Regs::new([163, 242, 66, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    session
        .kernel
        .os()
        .perform(X | BYTE, &mut byte)
        .expect("read");
    assert_eq!((byte.r[1], byte.r[2]), (12, 9));
    op(&mut session, [9, 0, 0, 0, 0, 0, 0, 0]).expect("initialised");
    let mut byte = Regs::new([163, 242, 66, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    session
        .kernel
        .os()
        .perform(X | BYTE, &mut byte)
        .expect("read");
    assert_eq!((byte.r[1], byte.r[2]), (0, 0), "the selection forgotten");
    let mut shrink = Regs::new([
        3,
        (-0x7000i32).cast_unsigned(),
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
        0,
    ]);
    let _ = session
        .kernel
        .os()
        .perform(X | CHANGE_DYNAMIC_AREA, &mut shrink);
    let mut read = Regs::new([3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    session
        .kernel
        .os()
        .perform(X | READ_DYNAMIC_AREA, &mut read)
        .expect("read");
    let size = read.r[1] & 0x7FFF_FFFF;
    let regs = op(&mut session, [8, 0, 0, 0, 0, 0, 0, 0]).expect("read");
    assert_eq!(regs.r[2], size, "the size word follows the area");
}

/// SpriteExtend's `JPEG_Info`, `JPEG_PlotScaled` and
/// `JPEG_PlotTransformed`, and the first offset past its SWIs.
const JPEG_INFO: u32 = SPRITE_EXTEND;
const JPEG_PLOT_SCALED: u32 = SPRITE_EXTEND + 2;
const JPEG_PLOT_TRANSFORMED: u32 = SPRITE_EXTEND + 4;
const JPEG_PAST_SWIS: u32 = SPRITE_EXTEND + 7;

/// A JPEG of 8 by 8 pixels at [`AREA`] plus `at`, its frame header saying it
/// is `width` by `height`: its length.
fn jpeg_at(session: &mut Session, at: u32, (width, height): (u16, u16)) -> u32 {
    let picture = tairix_image::Picture::rgba(8, 8, vec![0x80; 8 * 8 * 4]).expect("a picture");
    let options = tairix_image::JpegOptions::new(80, [0, 0, 0]).expect("options");
    let mut bytes = tairix_image::encode_jpeg(&picture, options).expect("encoded");
    let sof = bytes
        .windows(2)
        .position(|w| w == [0xFF, 0xC0])
        .expect("a frame header");
    bytes[sof + 5..sof + 7].copy_from_slice(&height.to_be_bytes());
    bytes[sof + 7..sof + 9].copy_from_slice(&width.to_be_bytes());
    session
        .kernel
        .os()
        .write_bytes(AREA + at, &bytes)
        .expect("written");
    u32::try_from(bytes.len()).expect("short")
}

/// SWI `number` with R0 to R7 `low`: what it leaves, or its error's number.
fn swi(session: &mut Session, number: u32, low: [u32; 8]) -> Result<Regs, u32> {
    call(session, number, &low).map_err(|fail| error_number(session, fail))
}

/// The number and text of the error SWI `number` gives.
fn refusal(session: &mut Session, number: u32, low: [u32; 8]) -> (u32, Vec<u8>) {
    match call(session, number, &low) {
        Err(Fail::Error(block)) => {
            let os = &mut session.kernel.os();
            (
                os.read_word(block).expect("a number"),
                os.read_zstring(block + 4, 256).expect("a text"),
            )
        }
        other => panic!("SWI &{number:X} gave {other:?}"),
    }
}

/// The first pixel of 8-bit sprite `name`.
fn first_pixel(session: &mut Session, name: &[u8]) -> u8 {
    let at = address(session, name);
    let os = &mut session.kernel.os();
    let image = at + os.read_word(at + 32).expect("read");
    let mut pixel = [0];
    os.read_bytes(image, &mut pixel).expect("read");
    pixel[0]
}

#[test]
fn a_blend_into_a_sprite_with_no_palette_takes_its_mode_s_default_colours() {
    let mut session = with_area();
    vdu(&mut session, &[22, 12]).expect("mode 12");
    make(&mut session, b"into", 4, 1, 28, false);
    make(&mut session, b"over", 1, 1, 28, false);
    put_name(&mut session, NAME, b"into");
    op(&mut session, [USER + 42, AREA, NAME, 0, 0, 63, 192, 0]).expect("white");
    put_name(&mut session, OTHER_NAME, b"over");
    let switched =
        op(&mut session, [USER + 60, AREA, NAME, 0, 0, 0, 0, 0]).expect("output to the sprite");
    op(
        &mut session,
        [USER + 52, AREA, OTHER_NAME, 0, 0, 0x8000, 0, 0],
    )
    .expect("half black over white");
    op(
        &mut session,
        [
            switched.r[0],
            switched.r[1],
            switched.r[2],
            switched.r[3],
            0,
            0,
            0,
            0,
        ],
    )
    .expect("back");
    let into_sprite = first_pixel(&mut session, b"into");
    vdu(&mut session, &[22, 28]).expect("mode 28, the default 256 colours");
    let screen = screen_start(&mut session);
    let bottom_left = screen + 479 * 640;
    session
        .kernel
        .os()
        .write_bytes(bottom_left, &[0xFF])
        .expect("white");
    op(
        &mut session,
        [USER + 52, AREA, OTHER_NAME, 0, 0, 0x8000, 0, 0],
    )
    .expect("half black over white");
    let mut on_screen = [0];
    session
        .kernel
        .os()
        .read_bytes(bottom_left, &mut on_screen)
        .expect("read");
    assert_eq!(
        into_sprite, on_screen[0],
        "the sprite's own mode's colours, not the screen's sixteen"
    );
}

#[test]
fn a_sprite_deeper_than_the_destination_is_drawn_through_its_table() {
    let mut session = with_area();
    vdu(&mut session, &[22, 12]).expect("mode 12");
    make(&mut session, b"deep", 2, 1, 28, false);
    put_name(&mut session, NAME, b"deep");
    op(&mut session, [USER + 42, AREA, NAME, 0, 0, 0, 0, 0]).expect("pixel 0");
    op(&mut session, [USER + 42, AREA, NAME, 1, 0, 1, 0, 0]).expect("pixel 1");
    let table = AREA + 0x8000;
    let colours: Vec<u8> = (0..=255u8).map(|n| 15 - (n & 15)).collect();
    session
        .kernel
        .os()
        .write_bytes(table, &colours)
        .expect("written");
    op(&mut session, [USER + 52, AREA, NAME, 0, 0, 0, 0, table]).expect("drawn through the table");
    let screen = screen_start(&mut session);
    let mut bottom_left = [0];
    session
        .kernel
        .os()
        .read_bytes(screen + 255 * 320, &mut bottom_left)
        .expect("read");
    assert_eq!(
        bottom_left[0], 0xBF,
        "colours 0 and 1, pixels 0 and 4, drawn as 15 and 11"
    );
}

#[test]
fn a_jpeg_on_a_palettised_screen_takes_each_colour_from_colourtrans_s_32k_table() {
    use tairix_riscos_kernel::swi::colour_trans::{SELECT_TABLE, WRITE_PALETTE};
    let mut session = with_area();
    common::select_full_palette(&mut session).expect("256 colours");
    let words = AREA + 0x4000;
    for i in 0..256u32 {
        let colour = (((i * 0x3B5) ^ (i << 9)) & 0xFF_FFFF) << 8;
        session
            .kernel
            .os()
            .write_word(words + 4 * i, colour)
            .expect("written");
    }
    swi(
        &mut session,
        WRITE_PALETTE,
        [u32::MAX, u32::MAX, words, 0, 0, 0, 0, 0],
    )
    .expect("written");
    let side = 16usize;
    let gradient: Vec<u8> = (0..side * side)
        .flat_map(|at| {
            let (across, down) = (at % side, at / side);
            let [red, ..] = (across * 16).to_le_bytes();
            let [green, ..] = (down * 16).to_le_bytes();
            let [blue, ..] = (255 - across * 8 - down * 8).to_le_bytes();
            [red, green, blue, 0xFF]
        })
        .collect();
    let picture = tairix_image::Picture::rgba(16, 16, gradient).expect("a picture");
    let options = tairix_image::JpegOptions::new(90, [0, 0, 0]).expect("options");
    let bytes = tairix_image::encode_jpeg(&picture, options).expect("encoded");
    let at = AREA + 0x8000;
    session
        .kernel
        .os()
        .write_bytes(at, &bytes)
        .expect("written");
    let length = u32::try_from(bytes.len()).expect("short");
    swi(
        &mut session,
        JPEG_PLOT_SCALED,
        [at, 0, 0, 0, length, 0, 0, 0],
    )
    .expect("plotted");
    let guard = AREA + 0x7000;
    let deep_mode = 0x3016_80B5;
    swi(
        &mut session,
        SELECT_TABLE,
        [deep_mode, u32::MAX, u32::MAX, u32::MAX, guard, 0, 0, 0],
    )
    .expect("a 32K table");
    let table = session
        .kernel
        .os()
        .read_word(guard + 4)
        .expect("its address");
    let limits = tairix_image::DecodeLimits::new(64, 64, 1 << 12, 1 << 20);
    let decoded = tairix_image::jpeg_decode_stored(&bytes, &limits).expect("decoded");
    let screen = screen_start(&mut session);
    for (at, rgba) in decoded.pixels().as_chunks::<4>().0.iter().enumerate() {
        let (x, y) = (
            u32::try_from(at % side).expect("small"),
            u32::try_from(at / side).expect("small"),
        );
        let cell =
            u32::from(rgba[0] >> 3) | u32::from(rgba[1] >> 3) << 5 | u32::from(rgba[2] >> 3) << 10;
        let mut wanted = [0];
        session
            .kernel
            .os()
            .read_bytes(table + cell, &mut wanted)
            .expect("read");
        let mut drawn = [0];
        let row = 479 - 15 + y;
        session
            .kernel
            .os()
            .read_bytes(screen + row * 640 + x, &mut drawn)
            .expect("read");
        assert_eq!(drawn, wanted, "pixel {x}, {y}");
    }
}

#[test]
fn the_jpeg_swis_refuse_in_their_own_words_and_past_their_last_swi() {
    let mut session = with_area();
    let at = AREA + 0x8000;
    session
        .kernel
        .os()
        .write_bytes(at, b"no JPEG here")
        .expect("written");
    assert_eq!(
        refusal(&mut session, JPEG_INFO, [0, at, 12, 0, 0, 0, 0, 0]),
        (0x71A, b"The JPEG cannot be read".to_vec())
    );
    let length = jpeg_at(&mut session, 0x8000, (8, 8));
    let quarter_turn = [0, 0x1_0000, 0xFFFF_0000, 0, 0, 0];
    for (word, value) in (0..).zip(quarter_turn) {
        session
            .kernel
            .os()
            .write_word(AREA + 0x9000 + 4 * word, value)
            .expect("written");
    }
    let (number, text) = refusal(
        &mut session,
        JPEG_PLOT_TRANSFORMED,
        [at, 0, AREA + 0x9000, length, 0, 0, 0, 0],
    );
    assert_eq!(number, 0x71B);
    assert!(
        text.starts_with(b"A JPEG is plotted only scaled"),
        "{text:?}"
    );
    let (number, text) = refusal(&mut session, JPEG_PAST_SWIS, [0; 8]);
    assert_eq!(number, 0x110, "not taken as PDriverIntercept");
    assert!(
        text.windows(12).any(|w| w == b"SpriteExtend"),
        "naming the module: {text:?}"
    );
}

#[test]
fn a_jpeg_of_no_rows_is_refused_however_it_is_plotted() {
    let mut session = with_area();
    let at = AREA + 0x8000;
    let shape = AREA + 0x9000;
    let none_high = [
        0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x00, 0x00, 0x01, 0x01, 0x01, 0x11, 0x00,
    ];
    {
        let os = &mut session.kernel.os();
        os.write_bytes(at, &none_high).expect("written");
        for (i, word) in (0u32..).zip([0, 256, 256, 256, 256, 0, 0, 0]) {
            os.write_word(shape + 4 * i, word)
                .expect("a scaling parallelogram");
        }
    }
    let length = u32::try_from(none_high.len()).expect("short");
    let bad = (0x71A, b"The JPEG cannot be read".to_vec());
    assert_eq!(
        refusal(
            &mut session,
            JPEG_PLOT_TRANSFORMED,
            [at, 1, shape, length, 0, 0, 0, 0]
        ),
        bad,
        "through a parallelogram"
    );
    assert_eq!(
        refusal(
            &mut session,
            JPEG_PLOT_SCALED,
            [at, 0, 0, 0, length, 0, 0, 0]
        ),
        bad,
        "scaled"
    );
    assert!(common::answers(&mut session));
}

#[test]
fn a_jpeg_s_header_is_read_to_the_end_of_memory_and_a_length_past_it_aborts() {
    let mut session = with_area();
    let length = jpeg_at(&mut session, 0x8000, (8, 8));
    let end = APPLICATION + SLOT_BYTES;
    let at = end - length;
    jpeg_at(&mut session, at - AREA, (8, 8));
    let info = swi(&mut session, JPEG_INFO, [1, at, 0, 0, 0, 0, 0, 0]).expect("read");
    assert_eq!((info.r[2], info.r[3]), (8, 8), "nothing past the end read");
    let mut head = [0; 12];
    session.kernel.os().read_bytes(at, &mut head).expect("read");
    session
        .kernel
        .os()
        .write_bytes(end - 12, &head)
        .expect("written");
    let mut regs = Regs::new([1, end - 12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let cut = session.kernel.os().perform(X | JPEG_INFO, &mut regs);
    assert!(
        matches!(cut, Err(Fail::Unwind(_))),
        "a header the end of memory cuts off aborts there: {cut:?}"
    );
    let mut regs = Regs::new([at, 0, 0, 0, u32::MAX, 0, 0, 0, 0, 0, 0, 0, 0]);
    let plotted = session.kernel.os().perform(X | JPEG_PLOT_SCALED, &mut regs);
    assert!(
        matches!(plotted, Err(Fail::Unwind(_))),
        "the abort at the end, before any copy: {plotted:?}"
    );
}

#[test]
fn a_jpeg_too_large_to_decode_is_short_of_memory_and_its_header_still_read() {
    let mut session = with_area();
    let length = jpeg_at(&mut session, 0x8000, (20_000, 30_000));
    let info = swi(
        &mut session,
        JPEG_INFO,
        [1, AREA + 0x8000, length, 0, 0, 0, 0, 0],
    )
    .expect("read");
    assert_eq!((info.r[2], info.r[3]), (20_000, 30_000));
    assert_eq!(
        swi(
            &mut session,
            JPEG_PLOT_SCALED,
            [AREA + 0x8000, 0, 0, 0, length, 0, 0, 0]
        )
        .err(),
        Some(0x85)
    );
    assert_eq!(
        swi(
            &mut session,
            JPEG_INFO,
            [4, AREA + 0x8000, length, 0, 0, 0, 0, 0]
        )
        .err(),
        Some(0x712),
        "a reserved flag"
    );
}

#[test]
fn a_transformed_plot_into_a_sprite_draws_in_the_sprite_s_pixels() {
    let mut session = with_area();
    make(&mut session, b"e", 4, 2, 28, false);
    make(&mut session, b"d", 8, 8, 28, false);
    for (x, y, colour) in [(0, 0, 5), (3, 0, 9), (0, 1, 17), (3, 1, 33)] {
        put_name(&mut session, NAME, b"e");
        op(&mut session, [USER + 42, AREA, NAME, x, y, colour, 0, 0]).expect("written");
    }
    put_name(&mut session, NAME, b"d");
    op(&mut session, [USER + 60, AREA, NAME, 0, 0, 0, 0, 0]).expect("switched");
    put_name(&mut session, OTHER_NAME, b"e");
    op(&mut session, [USER + 56, AREA, OTHER_NAME, 0, 0, 0, 0, 0]).expect("plotted with no matrix");
    op(&mut session, [60, 0, 0, 0, 0, 0, 0, 0]).expect("back to the screen");
    let pixel = |session: &mut Session, name: &[u8], x: u32, y: u32| {
        put_name(session, NAME, name);
        let regs = op(session, [USER + 41, AREA, NAME, x, y, 0, 0, 0]).expect("read");
        (regs.r[5], regs.r[6])
    };
    for (x, y) in [(0, 0), (3, 0), (0, 1), (3, 1), (1, 1)] {
        assert_eq!(
            pixel(&mut session, b"d", x, y),
            pixel(&mut session, b"e", x, y),
            "pixel {x},{y}"
        );
    }
}

#[test]
fn the_kernel_s_plots_refuse_an_alpha_mask_they_would_use() {
    let mut session = with_area();
    make(&mut session, b"w", 4, 4, 0xB016_80B5, false);
    put_name(&mut session, NAME, b"w");
    op(&mut session, [USER + 29, AREA, NAME, 0, 0, 0, 0, 0]).expect("masked");
    assert_eq!(
        op(&mut session, [USER + 34, AREA, NAME, 0, 0, 8, 0, 0]).err(),
        Some(0x708)
    );
    assert_eq!(
        op(&mut session, [USER + 49, AREA, NAME, 0, 0, 0, 0, 0]).err(),
        Some(0x708)
    );
    assert!(
        op(&mut session, [USER + 34, AREA, NAME, 0, 0, 0, 0, 0]).is_ok(),
        "the colours alone"
    );
    assert!(
        op(&mut session, [USER + 52, AREA, NAME, 0, 0, 8, 0, 0]).is_ok(),
        "SpriteExtend's blend"
    );
}

#[test]
fn a_plot_into_a_mode_doubling_its_pixels_fills_each_pixel_s_whole_field() {
    let mut session = with_area();
    vdu(&mut session, &[22, 4]).expect("mode 4");
    make(&mut session, b"dot", 8, 1, 4, false);
    put_name(&mut session, NAME, b"dot");
    op(&mut session, [USER + 42, AREA, NAME, 0, 0, 1, 0, 0]).expect("pixel 0 set");
    op(&mut session, [USER + 52, AREA, NAME, 0, 0, 0, 0, 0]).expect("drawn scaled");
    let screen = screen_start(&mut session);
    let mut first = [0];
    session
        .kernel
        .os()
        .read_bytes(screen + 255 * 80, &mut first)
        .expect("read");
    assert_eq!(first[0] & 0b11, 0b11, "the pixel's two bits both set");
}

/// Screen rows `bottom..=top` up from the bottom of a mode 28 screen, the
/// top one first.
fn mode_28_rows(session: &mut Session, bottom: u32, top: u32) -> Vec<u8> {
    let screen = screen_start(session);
    let mut bytes = vec![0; ((top - bottom + 1) * 640) as usize];
    session
        .kernel
        .os()
        .read_bytes(screen + (479 - top) * 640, &mut bytes)
        .expect("read");
    bytes
}

/// The graphics window over every column of pixel rows `bottom..=top`.
fn rows_window(bottom: u32, top: u32) -> Vec<u8> {
    let mut sequence = vec![24, 0, 0];
    for unit in [bottom * 2, 1279, top * 2 + 1] {
        sequence.extend_from_slice(&u16::try_from(unit).expect("on screen").to_le_bytes());
    }
    sequence
}

#[test]
fn a_plot_held_to_a_few_rows_draws_them_as_it_draws_them_on_the_whole_screen() {
    let mut session = with_area();
    vdu(&mut session, &[22, 28]).expect("mode 28");
    make(&mut session, b"rows", 8, 23, 28, false);
    put_name(&mut session, NAME, b"rows");
    op(&mut session, [USER + 29, AREA, NAME, 0, 0, 0, 0, 0]).expect("masked");
    for y in 0..23 {
        for x in 0..8 {
            op(
                &mut session,
                [USER + 42, AREA, NAME, x, y, y * 8 + x + 1, 0, 0],
            )
            .expect("set");
        }
        op(&mut session, [USER + 44, AREA, NAME, y % 8, y, 0, 0, 0]).expect("clear");
    }
    let scale = AREA + 0x7200;
    for (i, factor) in [3, 3, 2, 2].into_iter().enumerate() {
        let at = scale + 4 * u32::try_from(i).expect("small");
        session.kernel.os().write_word(at, factor).expect("written");
    }
    let plots = [
        ("blit", [USER + 34, AREA, NAME, 100, 100, 8, 0, 0], 50..=72),
        (
            "scaled",
            [USER + 52, AREA, NAME, 100, 100, 8, scale, 0],
            50..=83,
        ),
        (
            "tiled",
            [USER + 65, AREA, NAME, 100, 100, 8, scale, 0],
            40..=95,
        ),
    ];
    let windows = (45..=90).map(|y| (y, y)).chain([
        (49, 51),
        (55, 60),
        (70, 90),
        (80, 86),
        (83, 84),
        (50, 84),
        (61, 95),
        (40, 95),
    ]);
    for (name, plot, drawn) in plots {
        vdu(&mut session, &[26, 16]).expect("cleared");
        op(&mut session, plot).expect("drawn");
        let whole = mode_28_rows(&mut session, 40, 95);
        for (y, row) in (40..=95).rev().zip(whole.chunks(640)) {
            assert_eq!(
                row.iter().any(|&pixel| pixel != 0),
                drawn.contains(&y),
                "{name} draws on row {y}"
            );
        }
        for (bottom, top) in windows.clone() {
            vdu(&mut session, &[26, 16]).expect("cleared");
            vdu(&mut session, &rows_window(bottom, top)).expect("window");
            op(&mut session, plot).expect("drawn");
            let start = ((95 - top) * 640) as usize;
            assert_eq!(
                mode_28_rows(&mut session, bottom, top),
                whole[start..start + ((top - bottom + 1) * 640) as usize],
                "{name} held to rows {bottom} to {top}"
            );
        }
    }
}

#[test]
fn a_masked_colour_plot_meets_the_colour_s_words_on_lines_off_a_word() {
    let mut session = with_area();
    // Lines of 1281 bytes: each starts a byte on from the one above.
    select(&mut session, &[1, 320, 256, 5, u32::MAX, 6, 1281, u32::MAX]).expect("selected");
    call(
        &mut session,
        SET_COLOUR,
        &[
            tairix_riscos_kernel::swi::os::set_colour::BACKGROUND,
            0x0012_3456,
        ],
    )
    .expect("background");
    make(&mut session, b"masked", 4, 4, 0x3016_80B5, false);
    put_name(&mut session, NAME, b"masked");
    op(&mut session, [USER + 29, AREA, NAME, 0, 0, 0, 0, 0]).expect("mask made");
    make(&mut session, b"plain", 4, 4, 0x3016_80B5, false);
    let screen = screen_start(&mut session);
    let mut drawn = Vec::new();
    for name in [b"masked".as_slice(), b"plain"] {
        vdu(&mut session, &[12]).expect("cleared");
        put_name(&mut session, NAME, name);
        op(&mut session, [USER + 49, AREA, NAME, 0, 0, 0, 0, 0]).expect("plotted");
        let mut rows = vec![0; 1281 * 4];
        session
            .kernel
            .os()
            .read_bytes(screen + 1281 * 252, &mut rows)
            .expect("read");
        drawn.push(rows);
    }
    assert!(drawn[1].chunks(1281).all(|row| row[..16] != [0; 16]));
    assert_eq!(drawn[0], drawn[1], "a solid mask covers what no mask does");
}

#[test]
fn a_jpeg_through_a_matrix_takes_its_pixels_as_the_standard_screen_s() {
    let mut session = with_area();
    common::select_depth(&mut session, 5).expect("32 bits a pixel");
    let picture = tairix_image::Picture::rgba(16, 16, vec![0xFF; 16 * 16 * 4]).expect("a picture");
    let options = tairix_image::JpegOptions::new(90, [0, 0, 0]).expect("options");
    let bytes = tairix_image::encode_jpeg(&picture, options).expect("encoded");
    let at = AREA + 0x8000;
    session
        .kernel
        .os()
        .write_bytes(at, &bytes)
        .expect("written");
    let length = u32::try_from(bytes.len()).expect("short");
    let matrix = AREA + 0x7000;
    let screen = screen_start(&mut session);
    for (scale, side) in [(0x1_0000, 16), (0x2_0000, 32)] {
        vdu(&mut session, &[12]).expect("cleared");
        for (i, word) in (0u32..).zip([scale, 0, 0, scale, 100 << 8, 100 << 8]) {
            session
                .kernel
                .os()
                .write_word(matrix + 4 * i, word)
                .expect("written");
        }
        swi(
            &mut session,
            JPEG_PLOT_TRANSFORMED,
            [at, 0, matrix, length, 0, 0, 0, 0],
        )
        .expect("plotted");
        let mut pixels = vec![0; 640 * 480 * 4];
        session
            .kernel
            .os()
            .read_bytes(screen, &mut pixels)
            .expect("read");
        let drawn = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| **pixel != [0; 4])
            .count();
        assert_eq!(
            drawn,
            side * side,
            "a picture pixel two OS units at {scale:#x}"
        );
    }
}

/// A 32-bit guest module whose service handler answers
/// `Service_ModeExtension` for mode 99 as mode 12, which no mode the drivers
/// fall back to is.
fn mode_99_module() -> Vec<u8> {
    let header = [0, 0, 0, 0x34, 0x60, 0x60, 0, 0, 0, 0, 0, 0, 0x50];
    let handler = [
        0xE331_0050, // TEQ R1, #&50
        0x11A0_F00E, // MOVNE PC, R14
        0xE332_0063, // TEQ R2, #99
        0x11A0_F00E, // MOVNE PC, R14
        0xE28F_4008, // ADR R4, workspace list
        0xE3A0_1000, // MOV R1, #0
        0xE1A0_F00E, // MOV PC, R14
    ];
    let flags = [1];
    let workspace_list = [0, 12, u32::MAX];
    let mut image: Vec<u8> = header
        .iter()
        .chain(&handler)
        .chain(&flags)
        .chain(&workspace_list)
        .flat_map(|w: &u32| w.to_le_bytes())
        .collect();
    image.extend_from_slice(b"Mode99\0\0");
    image
}

#[test]
fn a_sprite_in_a_mode_a_module_describes_is_read_through_that_module() {
    use tairix_riscos_kernel::swi::colour_trans::READ_PALETTE;
    let mut session = with_area();
    insert_module(&mut session, &mode_99_module()).expect("inserted");
    let buffer = 0xB000;
    let mut palettes = Vec::new();
    for (name, mode) in [(b"m12".as_slice(), 12), (b"m99", 99)] {
        make(&mut session, name, 4, 2, mode, true);
        put_name(&mut session, NAME, name);
        op(&mut session, [USER + 42, AREA, NAME, 1, 0, 0x2A, 0x40, 0]).expect("written");
        let regs = op(&mut session, [USER + 41, AREA, NAME, 1, 0, 0, 0, 0]).expect("read");
        assert_eq!((regs.r[5], regs.r[6]), (0xA, 0), "mode {mode}'s four bits");
        let regs = op(&mut session, [USER + 40, AREA, NAME, 0, 0, 0, 0, 0]).expect("read");
        assert_eq!(regs.r[6], mode, "the number kept");
        swi(
            &mut session,
            READ_PALETTE,
            [AREA, NAME, buffer, 0x400, 0, 0, 0, 0],
        )
        .expect("read");
        let mut words = vec![0; 0x400];
        session
            .kernel
            .os()
            .read_bytes(buffer, &mut words)
            .expect("read");
        palettes.push(words);
    }
    assert_eq!(palettes[0], palettes[1], "every entry its pixels name");
}

/// Where a test's hook routine is written, in the slot.
const ROUTINE: u32 = 0xB800;

#[test]
fn a_scaled_plot_reads_the_rows_shown_once_guest_code_has_moved_the_window() {
    use tairix_riscos_kernel::swi::os::WRITE_C;
    let mut session = with_area();
    vdu(&mut session, &[22, 28]).expect("mode 28");
    make(&mut session, b"tall", 8, 64, 28, false);
    put_name(&mut session, NAME, b"tall");
    for y in 0..64 {
        for x in 0..8 {
            op(&mut session, [USER + 42, AREA, NAME, x, y, 7, 0, 0]).expect("set");
        }
    }
    let (hook, slot) = common::palette_v_hook();
    let base = insert_module(&mut session, &hook).expect("inserted");
    common::write_words(
        &mut session,
        ROUTINE,
        &common::once_routine(base + slot, WRITE_C, [26, 0, 0, 0, 0, 0, 0, 0]),
    );
    for (name, reason) in [("scaled", 52), ("tiled", 65)] {
        let translucent = [USER + reason, AREA, NAME, 0, 160, 0x8000, 0, 0];
        vdu(&mut session, &[26, 16]).expect("cleared");
        op(&mut session, translucent).expect("drawn");
        let whole = mode_28_rows(&mut session, 60, 160);
        vdu(&mut session, &[26, 16]).expect("cleared");
        vdu(&mut session, &rows_window(100, 110)).expect("a few rows");
        common::write_words(&mut session, base + slot, &[ROUTINE]);
        op(&mut session, translucent).expect("drawn");
        assert_eq!(
            session.kernel.os().read_word(base + slot).expect("read"),
            0,
            "{name}: the window moved during the plot"
        );
        assert_eq!(
            mode_28_rows(&mut session, 60, 160),
            whole,
            "{name}: every row the moved window shows"
        );
    }
}

/// The bytes of sprite `name`'s image, as many as `length`.
fn image_bytes(session: &mut Session, name: &[u8], length: usize) -> Vec<u8> {
    let at = address(session, name);
    let image = session.kernel.os().read_word(at + 32).expect("its image");
    let mut bytes = vec![0; length];
    session
        .kernel
        .os()
        .read_bytes(at + image, &mut bytes)
        .expect("read");
    bytes
}

#[test]
fn a_plot_draws_where_guest_code_moved_output_and_nowhere_in_another_form() {
    let mut session = with_area();
    vdu(&mut session, &[22, 28]).expect("mode 28");
    make(&mut session, b"tall", 8, 64, 28, false);
    put_name(&mut session, NAME, b"tall");
    for y in 0..64 {
        for x in 0..8 {
            op(&mut session, [USER + 42, AREA, NAME, x, y, 7, 0, 0]).expect("set");
        }
    }
    let (hook, slot) = common::palette_v_hook();
    let base = insert_module(&mut session, &hook).expect("inserted");
    for (target, mode, drawn) in [(&b"same"[..], 28, true), (b"other", 12, false)] {
        make(&mut session, target, 64, 64, mode, false);
        put_name(&mut session, OTHER_NAME, target);
        common::write_words(
            &mut session,
            ROUTINE,
            &common::once_routine(
                base + slot,
                SPRITE_OP,
                [USER + 60, AREA, OTHER_NAME, 0, 0, 0, 0, 0],
            ),
        );
        common::write_words(&mut session, base + slot, &[ROUTINE]);
        vdu(&mut session, &[16]).expect("cleared");
        put_name(&mut session, NAME, b"tall");
        op(&mut session, [USER + 52, AREA, NAME, 0, 0, 0x8000, 0, 0]).expect("drawn");
        op(&mut session, [USER + 60, AREA, 0, 0, 0, 0, 0, 0]).expect("back to the screen");
        let mode = String::from_utf8_lossy(target).into_owned();
        assert_eq!(
            session.kernel.os().read_word(base + slot).expect("read"),
            0,
            "{mode}: output moved during the plot"
        );
        assert!(
            mode_28_rows(&mut session, 0, 63)
                .iter()
                .all(|&pixel| pixel == 0),
            "{mode}: nothing on the screen output moved from"
        );
        assert_eq!(
            image_bytes(&mut session, target, 64 * 4)
                .iter()
                .any(|&byte| byte != 0),
            drawn,
            "{mode}: drawn into a sprite of the screen's form alone"
        );
    }
}

#[test]
fn a_colour_mapping_is_given_each_colour_once_whatever_its_spare_bits() {
    const COUNTER: u32 = 0xBC00;
    const DESCRIPTOR: u32 = 0xBC10;
    let mut session = with_area();
    vdu(&mut session, &[22, 28]).expect("mode 28");
    make(&mut session, b"deep", 4, 1, 0x3016_80B5, false);
    let at = address(&mut session, b"deep");
    let image = session.kernel.os().read_word(at + 32).expect("its image");
    let counting = [
        0xE92D_4006, // STMFD R13!, {R1, R2, R14}
        0xE59F_100C, // LDR R1, counter
        0xE591_2000, // LDR R2, [R1]
        0xE282_2001, // ADD R2, R2, #1
        0xE581_2000, // STR R2, [R1]
        0xE8BD_8006, // LDMFD R13!, {R1, R2, PC}
        COUNTER,
    ];
    common::write_words(&mut session, ROUTINE, &counting);
    common::write_words(&mut session, DESCRIPTOR, &[0, ROUTINE]);
    common::write_words(&mut session, COUNTER, &[0]);
    common::write_words(
        &mut session,
        at + image,
        &[0x0011_2233, 0x1111_2233, 0x2211_2233, 0x3311_2233],
    );
    put_name(&mut session, NAME, b"deep");
    op(
        &mut session,
        [USER + 52, AREA, NAME, 0, 0, 0x80, 0, DESCRIPTOR],
    )
    .expect("drawn");
    assert_eq!(
        session.kernel.os().read_word(COUNTER).expect("read"),
        1,
        "one colour, its spare byte four ways"
    );
}

#[test]
fn a_column_inserted_into_a_sprite_of_no_width_keeps_its_image_where_it_starts() {
    let mut session = with_area();
    make(&mut session, b"bare", 0, 3, 28, false);
    put_name(&mut session, NAME, b"bare");
    op(&mut session, [USER + 45, AREA, NAME, 0, 0, 0, 0, 0]).expect("a column");
    let at = address(&mut session, b"bare");
    assert_eq!(header(&mut session, at), [0x38, 0, 2, 0, 7, 0x2C, 0x2C, 28]);
    op(&mut session, [USER + 17, AREA, 0, 0, 0, 0, 0, 0]).expect("the area verified");
}

/// A session whose area holds sprite `a` in mode 28, `b` in mode 99 and a
/// larger `c` after them, a hook on `Service_ModeExtension` inserted ahead of
/// the module describing mode 99, and the address of the hook's slot.
fn hooked_mode_99() -> (Session, u32) {
    let mut session = with_area();
    let (hook, slot) = common::service_hook(tairix_riscos_kernel::service::MODE_EXTENSION);
    let base = insert_module(&mut session, &hook).expect("inserted");
    insert_module(&mut session, &mode_99_module()).expect("inserted");
    make(&mut session, b"a", 8, 8, 28, false);
    make(&mut session, b"b", 8, 8, 99, true);
    make(&mut session, b"c", 64, 64, 28, false);
    (session, base + slot)
}

#[test]
fn a_sprite_a_module_moves_while_its_mode_is_asked_is_not_changed_where_it_was() {
    let (mut session, slot) = hooked_mode_99();
    put_name(&mut session, OTHER_NAME, b"a");
    common::write_words(
        &mut session,
        ROUTINE,
        &common::once_routine(
            slot,
            SPRITE_OP,
            [USER + 25, AREA, OTHER_NAME, 0, 0, 0, 0, 0],
        ),
    );
    common::write_words(&mut session, slot, &[ROUTINE]);
    put_name(&mut session, NAME, b"b");
    assert_eq!(
        op(&mut session, [USER + 57, AREA, NAME, 0, 2, 0, 0, 0]).err(),
        Some(0x71E),
        "rows opened where b no longer is"
    );
    assert_eq!(
        session.kernel.os().read_word(slot).expect("read"),
        0,
        "a deleted"
    );
    op(&mut session, [USER + 17, AREA, 0, 0, 0, 0, 0, 0]).expect("the area verified");
    let info = op(&mut session, [USER + 40, AREA, NAME, 0, 0, 0, 0, 0]).expect("b found");
    assert_eq!((info.r[3], info.r[4]), (8, 8), "b as it was");
}

#[test]
fn a_palette_a_module_removes_while_its_mode_is_asked_is_not_written_over_the_image() {
    use tairix_riscos_kernel::swi::colour_trans::WRITE_PALETTE;
    let (mut session, slot) = hooked_mode_99();
    put_name(&mut session, OTHER_NAME, b"b");
    common::write_words(
        &mut session,
        ROUTINE,
        &common::once_routine(
            slot,
            SPRITE_OP,
            [USER + 37, AREA, OTHER_NAME, 0, 0, 0, 0, 0],
        ),
    );
    let at = address(&mut session, b"b");
    let image = |session: &mut Session| {
        let mut bytes = vec![0u8; 0x200];
        session
            .kernel
            .os()
            .read_bytes(at, &mut bytes)
            .expect("read");
        bytes
    };
    common::write_words(&mut session, slot, &[ROUTINE]);
    let words = AREA + 0x9000;
    common::write_words(&mut session, words, &[0x1122_3300; 16]);
    put_name(&mut session, NAME, b"b");
    let refused = swi(
        &mut session,
        WRITE_PALETTE,
        [AREA, NAME, words, 0, 0, 0, 0, 0],
    );
    assert_eq!(
        session.kernel.os().read_word(slot).expect("read"),
        0,
        "the palette removed"
    );
    let after = image(&mut session);
    assert_eq!(
        refused.err(),
        Some(0xA08),
        "a palette short of the mode's colours"
    );
    assert!(
        !after
            .windows(4)
            .any(|word| word == 0x1122_3300u32.to_le_bytes()),
        "no colour written over the image or past it"
    );
}

#[test]
fn a_sprite_in_a_mode_a_module_describes_shows_as_that_mode() {
    use tairix_riscos_kernel::swi::os::READ_MODE_VARIABLE;
    let mut session = with_area();
    insert_module(&mut session, &mode_99_module()).expect("inserted");
    make(&mut session, b"m99", 8, 8, 99, true);
    put_name(&mut session, NAME, b"m99");
    op(&mut session, [USER + 60, AREA, NAME, 0, 0, 0, 0, 0]).expect("output switched to it");
    let mut read = |variable: u32| {
        swi(
            &mut session,
            READ_MODE_VARIABLE,
            [u32::MAX, variable, 0, 0, 0, 0, 0, 0],
        )
        .expect("read")
        .r[2]
    };
    let (x_eig, y_eig, n_colour) = (read(X_EIG), read(Y_EIG), read(N_COLOUR));
    op(&mut session, [USER + 60, AREA, 0, 0, 0, 0, 0, 0]).expect("back to the screen");
    assert_eq!(
        (x_eig, y_eig, n_colour),
        (1, 2, 15),
        "mode 12's, as the module says"
    );
}
