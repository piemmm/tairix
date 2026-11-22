//! Sprite areas and the sprites in them through `OS_SpriteOp`, held to what
//! genuine RISC OS 5.30 was recorded doing: making, naming, finding, copying
//! and deleting sprites, their pixels and masks, their shape changed, their
//! palettes, verification, and the system area, dynamic area 3; plotting into
//! a sprite output is switched to, and the JPEG SWIs' refusals.

mod common;

use common::{drivers, vdu};
use tairix_riscos_filing::session::{Session, SLOT_BYTES};
use tairix_riscos_kernel::memory::layout::APPLICATION;
use tairix_riscos_kernel::os::{Fail, Regs};
use tairix_riscos_kernel::swi::os::{BYTE, CHANGE_DYNAMIC_AREA, READ_DYNAMIC_AREA, SPRITE_OP};
use tairix_riscos_kernel::swi::X;

/// Where a test's user area and the names it gives lie, in the slot.
const AREA: u32 = 0x2_0000;
const AREA_BYTES: u32 = 0x1_0000;
const NAME: u32 = 0x9000;
const OTHER_NAME: u32 = 0x9100;
/// The area values R0 adds: a user area by name and by pointer.
const USER: u32 = 256;
const POINTER: u32 = 512;

/// `OS_SpriteOp` with registers R0 to R7: what it leaves, or its error's
/// number.
fn op(session: &mut Session, low: [u32; 8]) -> Result<Regs, u32> {
    let mut regs = Regs::new([
        low[0], low[1], low[2], low[3], low[4], low[5], low[6], low[7], 0, 0, 0, 0, 0,
    ]);
    let mut os = session.kernel.os();
    match os.perform(X | SPRITE_OP, &mut regs) {
        Ok(()) => Ok(regs),
        Err(Fail::Error(block)) => Err(os.read_word(block).expect("an error block")),
        Err(other) => panic!("OS_SpriteOp {low:x?}: {other:?}"),
    }
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
        op(&mut session, [POINTER + 40, AREA, 0, 0, 0, 0, 0, 0]).err(),
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
const JPEG_INFO: u32 = 0x4_9980;
const JPEG_PLOT_SCALED: u32 = 0x4_9982;
const JPEG_PLOT_TRANSFORMED: u32 = 0x4_9984;
const JPEG_PAST_SWIS: u32 = 0x4_9987;

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

fn swi(session: &mut Session, number: u32, low: [u32; 8]) -> Result<Regs, u32> {
    let mut regs = Regs::new([
        low[0], low[1], low[2], low[3], low[4], low[5], low[6], low[7], 0, 0, 0, 0, 0,
    ]);
    let mut os = session.kernel.os();
    match os.perform(X | number, &mut regs) {
        Ok(()) => Ok(regs),
        Err(Fail::Error(block)) => Err(os.read_word(block).expect("an error block")),
        Err(other) => panic!("SWI &{number:X}: {other:?}"),
    }
}

/// The number and text of the error SWI `number` gives.
fn refusal(session: &mut Session, number: u32, low: [u32; 8]) -> (u32, Vec<u8>) {
    let mut regs = Regs::new([
        low[0], low[1], low[2], low[3], low[4], low[5], low[6], low[7], 0, 0, 0, 0, 0,
    ]);
    let mut os = session.kernel.os();
    match os.perform(X | number, &mut regs) {
        Err(Fail::Error(block)) => (
            os.read_word(block).expect("a number"),
            os.read_zstring(block + 4, 256).expect("a text"),
        ),
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
    use tairix_riscos_kernel::swi::os::READ_VDU_VARIABLES;
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
    let asked = AREA + 0x7100;
    session.kernel.os().write_word(asked, 148).expect("written");
    session
        .kernel
        .os()
        .write_word(asked + 4, u32::MAX)
        .expect("written");
    swi(
        &mut session,
        READ_VDU_VARIABLES,
        [asked, asked + 8, 0, 0, 0, 0, 0, 0],
    )
    .expect("read");
    let screen = session
        .kernel
        .os()
        .read_word(asked + 8)
        .expect("the screen");
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
    use tairix_riscos_kernel::swi::os::READ_VDU_VARIABLES;
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
    let asked = AREA + 0x7100;
    session.kernel.os().write_word(asked, 148).expect("written");
    session
        .kernel
        .os()
        .write_word(asked + 4, u32::MAX)
        .expect("written");
    swi(
        &mut session,
        READ_VDU_VARIABLES,
        [asked, asked + 8, 0, 0, 0, 0, 0, 0],
    )
    .expect("read");
    let screen = session
        .kernel
        .os()
        .read_word(asked + 8)
        .expect("the screen");
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
    use tairix_riscos_kernel::swi::os::READ_VDU_VARIABLES;
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
    let asked = AREA + 0x7100;
    session.kernel.os().write_word(asked, 148).expect("written");
    session
        .kernel
        .os()
        .write_word(asked + 4, u32::MAX)
        .expect("written");
    swi(
        &mut session,
        READ_VDU_VARIABLES,
        [asked, asked + 8, 0, 0, 0, 0, 0, 0],
    )
    .expect("read");
    let screen = session
        .kernel
        .os()
        .read_word(asked + 8)
        .expect("the screen");
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
