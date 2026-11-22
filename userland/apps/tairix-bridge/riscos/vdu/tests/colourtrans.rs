//! ColourTrans in a session: its match follows the palette only once told
//! the palette changed, its loadings set by `MiscOp` and by command as RISC
//! OS 5 reads them, and the colours it sets are `OS_SetColour`'s.

mod common;

use common::{call, command, error_number};
use tairix_riscos_kernel::swi::colour_trans as ct;
use tairix_riscos_kernel::swi::os::{READ_VDU_VARIABLES, SET_COLOUR};

/// Where the tests write in the application slot.
const BLOCK: u32 = 0x9000;

/// The loadings as they stand, `MiscOp` 0 setting them back.
fn loadings(session: &mut tairix_riscos_filing::session::Session) -> u32 {
    let old = call(session, ct::MISC_OP, &[0, 0x0104_0200])
        .expect("set")
        .r[2];
    call(session, ct::MISC_OP, &[0, old]).expect("restored");
    old
}

fn vdu_variable(session: &mut tairix_riscos_filing::session::Session, number: u32) -> u32 {
    let os = &mut session.kernel.os();
    os.write_word(BLOCK, number).expect("written");
    os.write_word(BLOCK + 4, u32::MAX).expect("written");
    call(session, READ_VDU_VARIABLES, &[BLOCK, BLOCK + 8]).expect("read");
    session.kernel.os().read_word(BLOCK + 8).expect("its value")
}

#[test]
fn the_current_palette_is_seen_again_only_once_the_cache_is_invalidated() {
    let mut session = common::drivers(b"");
    common::vdu(&mut session, &[22, 12]).expect("mode 12");
    let green = 0x00FF_0000;
    assert_eq!(
        call(&mut session, ct::RETURN_COLOUR_NUMBER, &[green])
            .expect("matched")
            .r[0],
        2
    );
    common::vdu(&mut session, &[19, 9, 16, 0, 0xFF, 0]).expect("colour 9 green");
    assert_eq!(
        call(&mut session, ct::RETURN_COLOUR_NUMBER, &[green])
            .expect("matched")
            .r[0],
        2,
        "the palette as last read"
    );
    call(&mut session, ct::INVALIDATE_CACHE, &[]).expect("invalidated");
    assert_eq!(
        call(&mut session, ct::RETURN_COLOUR_NUMBER, &[green])
            .expect("matched")
            .r[0],
        2,
        "the first of the two greens"
    );
    common::vdu(&mut session, &[19, 2, 16, 0, 0, 0]).expect("colour 2 black");
    call(&mut session, ct::INVALIDATE_CACHE, &[]).expect("invalidated");
    assert_eq!(
        call(&mut session, ct::RETURN_COLOUR_NUMBER, &[green])
            .expect("matched")
            .r[0],
        9
    );
    common::vdu(&mut session, &[22, 12]).expect("mode 12 again");
    assert_eq!(
        call(&mut session, ct::RETURN_COLOUR_NUMBER, &[green])
            .expect("matched")
            .r[0],
        2,
        "a mode change reads the default palette afresh"
    );
}

#[test]
fn loadings_are_set_by_misc_op_and_by_command_a_byte_a_channel() {
    let mut session = common::drivers(b"");
    assert_eq!(loadings(&mut session), 0x0104_0200);
    command(&mut session, "ColourTransLoadings &10 4_10 300").expect("set");
    assert_eq!(loadings(&mut session), 0x2C04_1000);
    for (text, number) in [
        ("2x 4 1", 0x16B),
        ("-1 4 1", 0x16B),
        ("99999999999 4 1", 0x16C),
        ("1 2", 0xDC),
    ] {
        let fail = command(&mut session, &format!("ColourTransLoadings {text}")).expect_err(text);
        assert_eq!(error_number(&mut session, fail), number, "{text}");
    }
    let fail = call(&mut session, ct::MISC_OP, &[3]).expect_err("no such reason");
    assert_eq!(error_number(&mut session, fail), 0xA04);
    let fail = call(&mut session, ct::GENERATE_TABLE + 1, &[]).expect_err("past the SWIs");
    assert_eq!(error_number(&mut session, fail), 0x110);
}

#[test]
fn a_colour_set_is_os_set_colour_s_its_action_read_back_as_risc_os_5_reads_it() {
    let mut session = common::drivers(b"");
    common::vdu(&mut session, &[22, 12]).expect("mode 12");
    call(&mut session, SET_COLOUR, &[3, 5]).expect("set");
    assert_eq!(
        vdu_variable(&mut session, 151),
        0x63,
        "the action OS_SetColour gives"
    );
    let regs = call(&mut session, ct::SET_GCOL, &[0x0000_FF00, 0, 0, 0x180, 2]).expect("set");
    assert_eq!(
        [regs.r[0], regs.r[2], regs.r[3]],
        [1, 2, 0x80],
        "red's GCOL, the depth, R3's background bit"
    );
    assert_eq!(
        vdu_variable(&mut session, 152),
        0x62,
        "the background's action"
    );
    common::vdu(&mut session, &[18, 1, 4]).expect("GCOL 1,4");
    assert_eq!(vdu_variable(&mut session, 151), 1, "VDU 18's action alone");
}

#[test]
fn os_set_colour_draws_with_its_colour_but_leaves_the_gcol_variables() {
    let mut session = common::drivers(b"");
    common::vdu(&mut session, &[22, 1]).expect("mode 1");
    call(&mut session, SET_COLOUR, &[0, 2]).expect("set");
    call(&mut session, SET_COLOUR, &[0x40, 5]).expect("text set");
    assert_eq!(
        vdu_variable(&mut session, 153),
        3,
        "GFCOL as the mode left it"
    );
    assert_eq!(vdu_variable(&mut session, 155), 3, "TForeCol likewise");
    call(&mut session, SET_COLOUR, &[0x80, BLOCK + 0x100]).expect("read");
    assert_eq!(
        session.kernel.os().read_word(BLOCK + 0x100).expect("read"),
        0xAAAA_AAAA,
        "colour 2 drawn"
    );
    let text = call(&mut session, SET_COLOUR, &[0xC0]).expect("read").r[1];
    assert_eq!(text, 5, "the text colour as given, not masked");
    common::vdu(&mut session, &[22, 1]).expect("mode 1 again");
    let text = call(&mut session, SET_COLOUR, &[0xC0]).expect("read").r[1];
    assert_eq!(text, 0xFFFF_FFFF, "COLOUR's colour replicated");
}

#[test]
fn a_grouped_256_colour_mode_takes_os_set_colour_s_colour_as_a_gcol() {
    let mut session = common::drivers(b"");
    common::vdu(&mut session, &[22, 15]).expect("mode 15");
    call(&mut session, SET_COLOUR, &[0x40, 0xC8]).expect("text set");
    assert_eq!(
        call(&mut session, SET_COLOUR, &[0xC0]).expect("read").r[1],
        0x98
    );
    common::select_full_palette(&mut session).expect("a full palette");
    call(&mut session, SET_COLOUR, &[0x40, 0xC8]).expect("text set");
    assert_eq!(
        call(&mut session, SET_COLOUR, &[0xC0]).expect("read").r[1],
        0xC8
    );
    let red = 0x0000_FF00;
    let set = call(&mut session, ct::SET_GCOL, &[red, 0, 0, 0, 0])
        .expect("set")
        .r[0];
    let opposite = call(&mut session, ct::SET_OPP_GCOL, &[red, 0, 0, 0, 0])
        .expect("set")
        .r[0];
    let returned = call(&mut session, ct::RETURN_GCOL, &[red])
        .expect("matched")
        .r[0];
    assert_eq!(
        [set, returned],
        [0x0D, 0x15],
        "SetGCOL's GCOL in the grouped form"
    );
    assert_eq!(
        opposite,
        call(&mut session, ct::RETURN_OPP_COLOUR_NUMBER, &[red])
            .expect("matched")
            .r[0]
    );
}

#[test]
fn output_to_a_sprite_with_no_palette_matches_against_the_screen() {
    use tairix_riscos_kernel::swi::os::SPRITE_OP;
    let mut session = common::drivers(b"");
    common::vdu(&mut session, &[22, 12]).expect("mode 12");
    let area = 0x1_0000;
    let names = BLOCK + 0x200;
    {
        let os = &mut session.kernel.os();
        os.write_word(area, 0x4000).expect("written");
        os.write_word(area + 8, 16).expect("written");
        os.write_bytes(names, b"bare\0with\0").expect("written");
    }
    call(&mut session, SPRITE_OP, &[0x109, area]).expect("area");
    call(
        &mut session,
        SPRITE_OP,
        &[0x10F, area, names, 0, 16, 16, 15],
    )
    .expect("bare");
    call(
        &mut session,
        SPRITE_OP,
        &[0x10F, area, names + 5, 1, 16, 16, 15],
    )
    .expect("with a palette");
    let green = 0x00FF_0000;
    for (name, number) in [(names, 2), (names + 5, 0x62)] {
        let switched = call(&mut session, SPRITE_OP, &[0x13C, area, name, 0]).expect("switched");
        call(&mut session, ct::INVALIDATE_CACHE, &[]).expect("invalidated");
        let matched = call(&mut session, ct::RETURN_COLOUR_NUMBER, &[green])
            .expect("matched")
            .r[0];
        assert_eq!(matched, number, "output to {name:x}");
        call(&mut session, SPRITE_OP, &switched.r[..4]).expect("switched back");
    }
}

#[test]
fn a_reinitialised_colourtrans_gives_back_its_32k_tables() {
    let mut session = common::drivers(b"");
    common::vdu(&mut session, &[22, 15]).expect("mode 15");
    {
        let os = &mut session.kernel.os();
        for (i, word) in (0u32..).zip([1, 640, 480, 4, u32::MAX, u32::MAX]) {
            os.write_word(BLOCK + 4 * i, word).expect("a deep selector");
        }
    }
    let cycle = |session: &mut tairix_riscos_filing::session::Session| {
        call(
            session,
            ct::SELECT_TABLE,
            &[BLOCK, u32::MAX, u32::MAX, u32::MAX, BLOCK + 0x100, 0],
        )
        .expect("a 32K table");
        common::command(session, "RMReInit ColourTrans").expect("reinitialised");
        common::rma_free(session)
    };
    let first = cycle(&mut session);
    assert_eq!(cycle(&mut session), first, "the RMA as it was");
}
