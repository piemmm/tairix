//! `InverseTable` and `BlendTable` in a session, as RISC OS 5's answer: a 256-
//! colour palette's tables kept until it changes, and blend tables sized,
//! built, locked and refused.

mod common;

use common::{call, error_number, select_depth};
use tairix_riscos_filing::session::Session;
use tairix_riscos_kernel::swi::chunk::{BLEND_TABLE, INVERSE_TABLE};

/// Where the tests write words and tables.
const WORDS: u32 = 0x9000;
const TABLE: u32 = 0x1_0000;

fn mode(session: &mut Session, number: u8) {
    common::vdu(session, &[22, number]).expect("a mode");
}

fn word(session: &mut Session, at: u32) -> u32 {
    session.kernel.os().read_word(at).expect("read")
}

fn byte(session: &mut Session, at: u32) -> u8 {
    let mut out = [0];
    session.kernel.os().read_bytes(at, &mut out).expect("read");
    out[0]
}

#[test]
fn an_inverse_table_is_kept_until_the_palette_changes() {
    let mut session = common::drivers(b"");
    mode(&mut session, 12);
    let refused = call(&mut session, INVERSE_TABLE, &[]).expect_err("not 256 colours");
    assert_eq!(error_number(&mut session, refused), 0x19);
    mode(&mut session, 15);
    let first = call(&mut session, INVERSE_TABLE, &[]).expect("made");
    assert_eq!(
        word(&mut session, first.r[0] + 4),
        0x842,
        "colour 1's top five bits"
    );
    assert_eq!(byte(&mut session, first.r[1]), 0, "black's cell");
    let again = call(&mut session, INVERSE_TABLE, &[]).expect("kept");
    assert_eq!(again.r[..2], first.r[..2]);
    let palette: Vec<u32> = (0..256u32)
        .map(|i| (((i * 0x3B5) ^ (i << 9)) & 0xFF_FFFF) << 8)
        .collect();
    common::select_full_palette(&mut session).expect("a full 256-colour palette");
    let os = &mut session.kernel.os();
    for (i, &colour) in (0u32..).zip(&palette) {
        os.write_word(WORDS + 4 * i, colour).expect("written");
    }
    call(
        &mut session,
        tairix_riscos_kernel::swi::colour_trans::WRITE_PALETTE,
        &[u32::MAX, u32::MAX, WORDS, 0, 0],
    )
    .expect("written");
    let custom = call(&mut session, INVERSE_TABLE, &[]).expect("made again");
    assert_eq!(word(&mut session, custom.r[0] + 4), 0x16);
    assert_eq!(
        byte(&mut session, custom.r[1] + 2047),
        0xB5,
        "a cell as RISC OS 5 built it"
    );
    session
        .kernel
        .os()
        .write_word(TABLE, 0x5555_5555)
        .expect("written");
    let current = call(
        &mut session,
        INVERSE_TABLE + 1,
        &[WORDS, TABLE, u32::MAX, u32::MAX],
    )
    .expect("current");
    assert_eq!(
        current.r[..2],
        custom.r[..2],
        "the current destination's own tables"
    );
    assert_eq!(
        word(&mut session, TABLE),
        0x5555_5555,
        "the caller's block left alone"
    );
    select_depth(&mut session, 4).expect("a deeper mode");
    let deep = call(&mut session, INVERSE_TABLE, &[]).expect_err("not 256 colours");
    assert_eq!(error_number(&mut session, deep), 0x19);
}

#[test]
fn a_blend_table_is_sized_built_locked_and_refused_as_risc_os_5_s() {
    let mut session = common::drivers(b"");
    mode(&mut session, 15);
    let size = call(&mut session, BLEND_TABLE, &[0, 15, 0, 12, 0, 128, 0]).expect("sized");
    assert_eq!(size.r[..3], [256, 16, 4096]);
    assert_eq!(size.r[6], 0);
    call(&mut session, BLEND_TABLE, &[0, 15, 0, 12, 0, 128, TABLE]).expect("built");
    let sampled: Vec<u8> = (0..8)
        .map(|i| byte(&mut session, TABLE + 259 * i))
        .collect();
    assert_eq!(
        sampled,
        [8, 9, 10, 11, 12, 13, 14, 15],
        "half way to each destination colour"
    );
    let locked = call(&mut session, BLEND_TABLE, &[4, 12, 0, 12, 0, 0, 0]).expect("locked");
    assert_ne!(locked.r[6], 0);
    assert_eq!(
        byte(&mut session, locked.r[6] + 16 + 3),
        11,
        "an opaque source its colour's last entry"
    );
    call(&mut session, BLEND_TABLE + 1, &[locked.r[6]]).expect("unlocked");
    let transparent =
        call(&mut session, BLEND_TABLE, &[0, 15, 0, 15, 0, 256, 0]).expect("wholly transparent");
    assert_eq!(transparent.r[2], 0x1_0000);
    let past = call(&mut session, BLEND_TABLE, &[0, 15, 0, 15, 0, 257, 0]).expect_err("past 256");
    assert_eq!(error_number(&mut session, past), 0x94);
    common::select_full_palette(&mut session).expect("a full palette");
    common::vdu(&mut session, &[19, 0, 16, 0xFF, 0xFF, 0xFF]).expect("colour 0 white");
    call(
        &mut session,
        BLEND_TABLE,
        &[0, u32::MAX, 0, u32::MAX, 0, 0, TABLE],
    )
    .expect("defaults");
    assert_eq!(
        byte(&mut session, TABLE),
        0,
        "the mode's default black, the palette nought"
    );
    call(
        &mut session,
        BLEND_TABLE,
        &[0, u32::MAX, u32::MAX, u32::MAX, u32::MAX, 0, TABLE],
    )
    .expect("current");
    assert_eq!(
        byte(&mut session, TABLE),
        0xFF,
        "the current palette's white, -1, its last entry"
    );
    select_depth(&mut session, 4).expect("a deeper mode");
    let deep = call(
        &mut session,
        BLEND_TABLE,
        &[0, u32::MAX, 0, u32::MAX, 0, 128, 0],
    )
    .expect_err("no palette");
    assert_eq!(error_number(&mut session, deep), 0xA07);
}

#[test]
fn a_sprite_palette_past_256_entries_gives_a_full_colour_table_and_no_more() {
    let mut session = common::drivers(b"");
    mode(&mut session, 15);
    let (area, name) = (0x2_0000, WORDS);
    session
        .kernel
        .os()
        .write_bytes(name, b"lone\0")
        .expect("named");
    let sprite = common::lone_sprite(&mut session, area, name, 15, true).expect("made");
    common::grow_palette(&mut session, area, sprite, 260, 0xFFFF_FF00).expect("grown");
    let (colours, inverse) = (TABLE, TABLE + 0x1000);
    for at in (colours..inverse).step_by(4) {
        session
            .kernel
            .os()
            .write_word(at, 0x5555_5555)
            .expect("marked");
    }
    call(
        &mut session,
        INVERSE_TABLE + 1,
        &[colours, inverse, area, sprite],
    )
    .expect("built");
    assert_eq!(
        word(&mut session, colours + 0x3FC),
        0x7FFF,
        "the last white"
    );
    assert_eq!(
        word(&mut session, colours + 0x400),
        0x5555_5555,
        "nothing past the 256th colour"
    );
}

#[test]
fn a_reinitialised_inverse_table_or_blend_table_gives_back_all_it_held() {
    let mut session = common::drivers(b"");
    mode(&mut session, 15);
    let cycle = |session: &mut Session| {
        call(session, INVERSE_TABLE, &[]).expect("made");
        call(session, BLEND_TABLE, &[4, 15, 0, 15, 0, 128, 0]).expect("locked");
        for module in ["InverseTable", "BlendTable"] {
            common::command(session, &format!("RMReInit {module}")).expect("reinitialised");
        }
        common::rma_free(session)
    };
    let first = cycle(&mut session);
    assert_eq!(cycle(&mut session), first, "the RMA as it was");
}
