//! ColourTrans in a session: its match follows the palette only once told
//! the palette changed, its loadings set by `MiscOp` and by command as RISC
//! OS 5 reads them, and the colours it sets are `OS_SetColour`'s.

mod common;

use common::{call, command, error_number};
use tairix_riscos_kernel::swi::chunk::FONT;
use tairix_riscos_kernel::swi::colour_trans as ct;
use tairix_riscos_kernel::swi::font::{SET_FONT_COLOURS, SET_PALETTE};
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

#[test]
fn a_mode_s_opposite_colour_is_the_current_mode_s_where_it_is_that_mode() {
    let mut session = common::drivers(b"");
    common::vdu(&mut session, &[22, 12]).expect("mode 12");
    for colour in [0, 0x00FF_0000, 0x1234_5600, 0xFFFF_FF00] {
        let number = call(&mut session, ct::RETURN_OPP_COLOUR_NUMBER, &[colour])
            .expect("matched")
            .r[0];
        let for_mode = call(
            &mut session,
            ct::RETURN_OPP_COLOUR_NUMBER_FOR_MODE,
            &[colour, 12, u32::MAX],
        )
        .expect("matched")
        .r[0];
        assert_eq!(for_mode, number, "{colour:#x}'s colour number");
        let gcol = call(&mut session, ct::RETURN_OPP_GCOL, &[colour])
            .expect("matched")
            .r[0];
        let gcol_for_mode = call(
            &mut session,
            ct::RETURN_OPP_GCOL_FOR_MODE,
            &[colour, 12, u32::MAX],
        )
        .expect("matched")
        .r[0];
        assert_eq!(gcol_for_mode, gcol, "{colour:#x}'s GCOL");
    }
}

#[test]
fn a_text_colour_set_draws_with_the_colour_number_its_match_gives() {
    const TEXT: u32 = 1 << 6;
    const READ: u32 = 1 << 7;
    let mut session = common::drivers(b"");
    common::vdu(&mut session, &[22, 12]).expect("mode 12");
    let drawn_with = |session: &mut tairix_riscos_filing::session::Session, number| {
        call(session, SET_COLOUR, &[TEXT, number]).expect("set");
        let read = call(session, SET_COLOUR, &[TEXT | READ, 0])
            .expect("read")
            .r[1];
        call(session, SET_COLOUR, &[TEXT, 0]).expect("set back");
        read
    };
    let red = 0x0000_FF00;
    let nearest = call(&mut session, ct::RETURN_COLOUR_NUMBER, &[red])
        .expect("matched")
        .r[0];
    let furthest = call(&mut session, ct::RETURN_OPP_COLOUR_NUMBER, &[red])
        .expect("matched")
        .r[0];
    let (near, far) = (
        drawn_with(&mut session, nearest),
        drawn_with(&mut session, furthest),
    );
    call(&mut session, ct::SET_TEXT_COLOUR, &[red, 0, 0, 0]).expect("set");
    let set = call(&mut session, SET_COLOUR, &[TEXT | READ, 0])
        .expect("read")
        .r[1];
    assert_eq!(set, near, "the nearest colour drawn with");
    call(&mut session, ct::SET_OPP_TEXT_COLOUR, &[red, 0, 0, 0]).expect("set");
    let set = call(&mut session, SET_COLOUR, &[TEXT | READ, 0])
        .expect("read")
        .r[1];
    assert_eq!(set, far, "the furthest colour drawn with");
}

#[test]
fn a_gcol_table_on_a_sixteen_colour_screen_is_its_colour_number_table() {
    let mut session = common::drivers(b"");
    common::vdu(&mut session, &[22, 12]).expect("mode 12");
    let (numbers, gcols) = (BLOCK + 0x100, BLOCK + 0x200);
    for source in [0, 9, 12] {
        let asked = [source, u32::MAX, u32::MAX, u32::MAX];
        let mut with_buffer = |swi: u32, at: u32| {
            let mut registers = asked.to_vec();
            registers.push(at);
            call(&mut session, swi, &registers).expect("a table");
        };
        with_buffer(ct::SELECT_TABLE, numbers);
        with_buffer(ct::SELECT_GCOL_TABLE, gcols);
        let mut os = session.kernel.os();
        let (mut a, mut b) = ([0u8; 16], [0u8; 16]);
        os.read_bytes(numbers, &mut a).expect("read");
        os.read_bytes(gcols, &mut b).expect("read");
        assert_eq!(a, b, "mode {source}'s colours");
    }
}

/// A 32-bit guest module standing in for the Font Manager: each SWI it is
/// called with records R0 to R5 and its number within the chunk, from
/// [`FONT_RECORD`] past its base.
fn font_recorder() -> Vec<u8> {
    let header = [0, 0, 0, 0, 0x60, 0x60, 0, FONT, 0x34, 0, 0, 0, 0x40];
    let handler = [
        0xE28F_A008, // ADR R10, record
        0xE88A_083F, // STMIA R10, {R0-R5, R11}
        0xE1A0_F00E, // MOV PC, R14
    ];
    let flags = [1];
    let record = [0; 7];
    let mut image: Vec<u8> = header
        .iter()
        .chain(&handler)
        .chain(&flags)
        .chain(&record)
        .flat_map(|w: &u32| w.to_le_bytes())
        .collect();
    image.extend_from_slice(b"FontRec\0");
    image
}

/// Where [`font_recorder`] records, from its base.
const FONT_RECORD: u32 = 0x44;

/// What the stand-in Font Manager at `base` was last handed.
fn font_call(session: &mut tairix_riscos_filing::session::Session, base: u32) -> [u32; 7] {
    let os = &mut session.kernel.os();
    core::array::from_fn(|i| {
        os.read_word(base + FONT_RECORD + 4 * u32::try_from(i).expect("small"))
            .expect("readable")
    })
}

#[test]
fn set_font_colours_hands_the_font_manager_its_match_or_else_its_words() {
    let mut session = common::drivers(b"");
    let base = common::insert_module(&mut session, &font_recorder()).expect("inserted");
    let (white, black) = (0xFFFF_FF00, 0);
    common::vdu(&mut session, &[22, 12]).expect("mode 12");
    let chosen = call(
        &mut session,
        ct::RETURN_FONT_COLOURS,
        &[5, white, black, 14],
    )
    .expect("returned")
    .r;
    call(&mut session, ct::SET_FONT_COLOURS, &[5, white, black, 14]).expect("set");
    assert_eq!(
        font_call(&mut session, base),
        [
            5,
            chosen[1],
            chosen[2],
            chosen[3],
            0,
            0,
            SET_FONT_COLOURS - FONT
        ]
    );
    common::select_depth(&mut session, 5).expect("32 bits a pixel");
    call(&mut session, ct::SET_FONT_COLOURS, &[5, white, black, 14]).expect("set");
    assert_eq!(
        font_call(&mut session, base),
        [5, 0, 0, 14, white, black, SET_PALETTE - FONT]
    );
}

#[test]
fn a_table_for_a_deep_destination_holds_its_pixels_wide_or_its_gcols_narrow() {
    const WIDE: u32 = 1 << 4;
    let mut session = common::drivers(b"");
    common::vdu(&mut session, &[22, 12]).expect("mode 12");
    {
        let os = &mut session.kernel.os();
        for (i, word) in (0u32..).zip([1, 640, 480, 5, u32::MAX, u32::MAX]) {
            os.write_word(BLOCK + 4 * i, word).expect("a deep selector");
        }
    }
    let (wide, narrow) = (BLOCK + 0x100, BLOCK + 0x200);
    for (at, flags) in [(wide, WIDE), (narrow, 0)] {
        call(
            &mut session,
            ct::GENERATE_TABLE,
            &[12, 0, BLOCK, 0, at, flags],
        )
        .expect("a table");
    }
    let mut pixels = [0u8; 4 * 8];
    session
        .kernel
        .os()
        .read_bytes(wide, &mut pixels)
        .expect("read");
    let primaries = [
        0, 0xFF, 0xFF00, 0xFFFF, 0xFF_0000, 0xFF_00FF, 0xFF_FF00, 0xFF_FFFF,
    ];
    assert_eq!(
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&pixel| u32::from_le_bytes(pixel))
            .collect::<Vec<_>>(),
        primaries,
        "mode 12's first eight colours as 32-bit pixels"
    );
    let words = [
        0,
        0xFF00,
        0xFF_0000,
        0xFF_FF00,
        0xFF00_0000,
        0xFF00_FF00,
        0xFFFF_0000,
        0xFFFF_FF00,
    ];
    for (i, word) in (0u32..).zip(words) {
        let gcol = call(&mut session, ct::RETURN_GCOL_FOR_MODE, &[word, 28, 0])
            .expect("matched")
            .r[0];
        let entry = session.kernel.os().read_byte(narrow + i).expect("read");
        assert_eq!(u32::from(entry), gcol, "colour {i} as mode 28 matches it");
    }
}

#[test]
fn a_transfer_function_takes_each_colour_before_it_is_matched() {
    const TRANSFER: u32 = 1 << 2;
    let mut session = common::drivers(b"");
    common::vdu(&mut session, &[22, 12]).expect("mode 12");
    let routine = BLOCK + 0x300;
    {
        let os = &mut session.kernel.os();
        os.write_word(routine, 0xE220_0CFF)
            .expect("EOR R0, R0, #&FF00");
        os.write_word(routine + 4, 0xE1A0_F00E)
            .expect("MOV PC, R14");
    }
    let table = BLOCK + 0x100;
    let mut registers = [12, 0, 12, 0, table, TRANSFER, 0, routine];
    call(&mut session, ct::GENERATE_TABLE, &registers).expect("a table");
    let mut numbers = [0u8; 2];
    session
        .kernel
        .os()
        .read_bytes(table, &mut numbers)
        .expect("read");
    assert_eq!(numbers, [1, 0], "black made red, red made black");
    registers[5] = 0;
    call(&mut session, ct::GENERATE_TABLE, &registers).expect("a table");
    session
        .kernel
        .os()
        .read_bytes(table, &mut numbers)
        .expect("read");
    assert_eq!(numbers, [0, 1], "no function asked, none called");
}

#[test]
fn no_more_32k_tables_are_kept_than_colourtrans_keeps() {
    let mut session = common::drivers(b"");
    common::vdu(&mut session, &[22, 12]).expect("mode 12");
    {
        let os = &mut session.kernel.os();
        for (i, word) in (0u32..).zip([1, 640, 480, 5, u32::MAX, u32::MAX]) {
            os.write_word(BLOCK + 4 * i, word).expect("a deep selector");
        }
    }
    let palette = BLOCK + 0x100;
    let mut free = Vec::new();
    for n in 0..6u32 {
        {
            let os = &mut session.kernel.os();
            for i in 0..16 {
                os.write_word(palette + 4 * i, (n * 16 + i) << 12)
                    .expect("a palette of its own");
            }
        }
        call(
            &mut session,
            ct::SELECT_TABLE,
            &[BLOCK, 0, 12, palette, BLOCK + 0x200],
        )
        .expect("a 32K table");
        free.push(common::rma_free(&mut session));
    }
    assert!(free[1] < free[0], "a second table kept");
    assert!(
        free[2..].iter().all(|&f| f >= free[1]),
        "each later table taking the place of the oldest: {free:?}"
    );
}

#[test]
fn a_table_is_matched_by_the_loadings_its_transfer_function_left() {
    const TRANSFER: u32 = 1 << 2;
    const SKEWED: u32 = 0x0100_0000;
    let mut session = common::drivers(b"");
    common::vdu(&mut session, &[22, 12]).expect("mode 12");
    let routine = BLOCK + 0x300;
    let code = [
        0xE92D_4007,
        0xE3A0_0000,
        0xE59F_1004,
        0xEF00_0000 | tairix_riscos_kernel::swi::X | ct::MISC_OP,
        0xE8BD_8007,
        SKEWED,
    ];
    {
        let os = &mut session.kernel.os();
        for (i, &word) in (0u32..).zip(&code) {
            os.write_word(routine + 4 * i, word).expect("written");
        }
    }
    let table = BLOCK + 0x100;
    let built = |session: &mut tairix_riscos_filing::session::Session, flags| {
        call(
            session,
            ct::GENERATE_TABLE,
            &[15, 0, 12, 0, table, flags, 0, routine],
        )
        .expect("a table");
        let mut numbers = [0u8; 256];
        session
            .kernel
            .os()
            .read_bytes(table, &mut numbers)
            .expect("read");
        numbers
    };
    let plain = built(&mut session, 0);
    call(&mut session, ct::MISC_OP, &[0, SKEWED]).expect("skewed");
    let skewed = built(&mut session, 0);
    assert_ne!(plain, skewed, "the loadings change the match");
    call(&mut session, ct::MISC_OP, &[0, 0x0104_0200]).expect("restored");
    assert_eq!(
        built(&mut session, TRANSFER),
        skewed,
        "matched by what the function set"
    );
}

/// A selector for a deep mode at [`BLOCK`].
fn deep_selector(session: &mut tairix_riscos_filing::session::Session, log2_bpp: u32) {
    let os = &mut session.kernel.os();
    for (i, word) in (0u32..).zip([1, 640, 480, log2_bpp, u32::MAX, u32::MAX]) {
        os.write_word(BLOCK + 4 * i, word).expect("a deep selector");
    }
}

#[test]
fn a_size_asked_of_a_32k_table_builds_none() {
    let mut session = common::drivers(b"");
    common::vdu(&mut session, &[22, 15]).expect("mode 15");
    deep_selector(&mut session, 4);
    let before = common::rma_free(&mut session);
    let sized = call(
        &mut session,
        ct::SELECT_TABLE,
        &[BLOCK, u32::MAX, u32::MAX, u32::MAX, 0, 0],
    )
    .expect("sized");
    assert_eq!(sized.r[4], 12, "the descriptor's three words");
    assert_eq!(common::rma_free(&mut session), before, "no table built");
}

#[test]
fn a_32k_table_the_guest_freed_is_given_back_quietly() {
    use tairix_riscos_kernel::swi::os::MODULE;
    let mut session = common::drivers(b"");
    common::vdu(&mut session, &[22, 15]).expect("mode 15");
    deep_selector(&mut session, 4);
    let descriptor = BLOCK + 0x100;
    call(
        &mut session,
        ct::SELECT_TABLE,
        &[BLOCK, u32::MAX, u32::MAX, u32::MAX, descriptor, 0],
    )
    .expect("a 32K table");
    let table = session
        .kernel
        .os()
        .read_word(descriptor + 4)
        .expect("its table");
    call(&mut session, MODULE, &[7, 0, table]).expect("freed by the guest");
    common::command(&mut session, "RMReInit ColourTrans").expect("reinitialised");
}

#[test]
fn a_colour_is_matched_by_the_loadings_a_palette_reading_left() {
    const SKEWED: u32 = 0x0100_0000;
    const ROUTINE: u32 = BLOCK + 0x800;
    let mut session = common::drivers(b"");
    common::vdu(&mut session, &[22, 12]).expect("mode 12");
    let red = 0x0000_FF00;
    let number = |session: &mut tairix_riscos_filing::session::Session| {
        call(session, ct::RETURN_COLOUR_NUMBER, &[red])
            .expect("matched")
            .r[0]
    };
    let plain = number(&mut session);
    call(&mut session, ct::MISC_OP, &[0, SKEWED]).expect("skewed");
    let skewed = number(&mut session);
    assert_ne!(plain, skewed, "the loadings change the match");
    call(&mut session, ct::MISC_OP, &[0, 0x0104_0200]).expect("restored");
    let (hook, slot) = common::palette_v_hook();
    let base = common::insert_module(&mut session, &hook).expect("inserted");
    common::write_words(
        &mut session,
        ROUTINE,
        &common::once_routine(base + slot, ct::MISC_OP, [0, SKEWED, 0, 0, 0, 0, 0, 0]),
    );
    common::write_words(&mut session, base + slot, &[ROUTINE]);
    call(&mut session, ct::INVALIDATE_CACHE, &[]).expect("made stale");
    assert_eq!(
        number(&mut session),
        skewed,
        "matched once the palette is read"
    );
    assert_eq!(
        session.kernel.os().read_word(base + slot).expect("read"),
        0,
        "the loadings set while the palette was read"
    );
}
