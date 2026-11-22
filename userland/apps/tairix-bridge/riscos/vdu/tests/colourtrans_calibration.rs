//! ColourTrans's calibration, font colours and CIE conversions in a session,
//! as RISC OS 5 gives them.

mod common;

use common::{call, command, error_number, select_depth};
use tairix_riscos_filing::session::Session;
use tairix_riscos_kernel::swi::colour_trans as ct;

/// Where the tests write a table, and read one back.
const TABLE: u32 = 0x9000;
const READ: u32 = 0x9800;

/// A red list of three pairs, its middle one off the line between its ends.
const THREE: [u32; 10] = [
    3,
    2,
    2,
    0x1010_1000,
    0x2040_2080,
    0xFF00_00FF,
    0,
    0x00FF_00FF,
    0,
    0xFF00_00FF,
];

fn set_table(session: &mut Session, table: &[u32]) -> Result<(), tairix_riscos_kernel::os::Fail> {
    let os = &mut session.kernel.os();
    for (i, &word) in (0u32..).zip(table) {
        os.write_word(TABLE + 4 * i, word).expect("written");
    }
    call(session, ct::SET_CALIBRATION, &[TABLE]).map(drop)
}

fn read_back(session: &mut Session) -> Vec<u32> {
    let size = call(session, ct::READ_CALIBRATION, &[0]).expect("sized").r[1];
    call(session, ct::READ_CALIBRATION, &[READ]).expect("read");
    let os = &mut session.kernel.os();
    (0..size / 4)
        .map(|i| os.read_word(READ + 4 * i).expect("read"))
        .collect()
}

fn device_colour(session: &mut Session, device: u32) -> u32 {
    let regs = call(session, ct::CONVERT_DEVICE_COLOUR, &[0x5A, device, 0, 0]).expect("converted");
    assert_eq!(
        [regs.r[0], regs.r[1], regs.r[3]],
        [0x5A, device, 0],
        "the rest kept"
    );
    regs.r[2]
}

#[test]
fn a_table_set_is_read_back_and_converts_device_colours_summed_before_they_are_cut() {
    let mut session = common::drivers(b"");
    assert!(
        read_back(&mut session).is_empty(),
        "the default holds no table"
    );
    assert_eq!(
        device_colour(&mut session, 0x8080_8000),
        0x8080_8000,
        "unchanged by the default"
    );
    let manual = [
        2,
        2,
        2,
        0x0201_0300,
        0x0203_FDFF,
        0x0201_0300,
        0x03FC_02FF,
        0x0201_0300,
        0xFF03_02FF,
    ];
    set_table(&mut session, &manual).expect("set");
    assert_eq!(read_back(&mut session), manual);
    assert_eq!(device_colour(&mut session, 0x8080_8000), 0x8582_8500);
    assert_eq!(device_colour(&mut session, 0x1234_5600), 0x1836_5D00);
}

#[test]
fn a_bad_table_is_refused_and_leaves_the_default() {
    let mut session = common::drivers(b"");
    set_table(&mut session, &THREE).expect("set");
    let mut unsorted = THREE;
    unsorted[4] = 0x2040_20FF;
    let refused = set_table(&mut session, &unsorted).expect_err("refused");
    assert_eq!(error_number(&mut session, refused), 0xA00);
    assert!(read_back(&mut session).is_empty());
    set_table(&mut session, &THREE).expect("set");
    call(&mut session, ct::SET_CALIBRATION, &[0]).expect("the default");
    assert!(read_back(&mut session).is_empty());
}

#[test]
fn the_map_commands_set_a_table_from_its_sizes_blue_first_and_its_pairs_last_first() {
    let mut session = common::drivers(b"");
    command(&mut session, "ColourTransMapSize 2 2 3").expect("sized");
    command(
        &mut session,
        "ColourTransMap &FF0000FF 0 &FF00FF 0 &FF0000FF &20402080 &10101000",
    )
    .expect("mapped");
    assert_eq!(read_back(&mut session), THREE);
    let short = command(&mut session, "ColourTransMap 0 &FF").expect_err("too few");
    assert_eq!(error_number(&mut session, short), 0xDC);
}

#[test]
fn a_palettised_match_looks_for_the_wanted_colour_through_the_calibration_but_a_direct_one_does_not(
) {
    let mut session = common::drivers(b"");
    common::vdu(&mut session, &[22, 12]).expect("mode 12");
    set_table(&mut session, &THREE).expect("set");
    let red = 0x0000_FF00;
    assert_eq!(
        call(&mut session, ct::RETURN_COLOUR_NUMBER, &[red])
            .expect("matched")
            .r[0],
        4,
        "red shows as blue"
    );
    assert_eq!(
        call(&mut session, ct::RETURN_COLOUR_NUMBER, &[0x8080_8000])
            .expect("matched")
            .r[0],
        6
    );
    let named = call(
        &mut session,
        ct::RETURN_COLOUR_NUMBER_FOR_MODE,
        &[red, 12, 0],
    )
    .expect("matched");
    assert_eq!(named.r[0], 4);
    select_depth(&mut session, 4).expect("a deeper mode");
    assert_eq!(
        call(&mut session, ct::RETURN_COLOUR_NUMBER, &[red])
            .expect("matched")
            .r[0],
        0x1F
    );
}

#[test]
fn font_colours_run_through_a_ramp_and_a_deeper_mode_takes_the_words() {
    let mut session = common::drivers(b"");
    common::vdu(&mut session, &[22, 12]).expect("mode 12");
    for i in 0u8..16 {
        common::vdu(&mut session, &[19, i, 16, i * 17, i * 17, i * 17]).expect("a grey");
    }
    call(&mut session, ct::INVALIDATE_CACHE, &[]).expect("invalidated");
    let ramp = call(
        &mut session,
        ct::RETURN_FONT_COLOURS,
        &[5, 0, 0xFFFF_FF00, 255],
    )
    .expect("chosen");
    assert_eq!(ramp.r[..4], [5, 0, 1, 14]);
    let none = call(
        &mut session,
        ct::RETURN_FONT_COLOURS,
        &[0, 0, 0xFFFF_FF00, u32::MAX],
    )
    .expect("chosen");
    assert_eq!(none.r[1..4], [0, 15, 0]);
    common::vdu(&mut session, &[22, 15]).expect("mode 15");
    let deep = call(
        &mut session,
        ct::RETURN_FONT_COLOURS,
        &[0, 0x1020_3000, 0xFFFF_FF00, u32::MAX],
    )
    .expect("chosen");
    assert_eq!(deep.r[1..4], [0x1020_3000, 0, u32::MAX]);
    let past = call(
        &mut session,
        ct::RETURN_FONT_COLOURS,
        &[0, 0x1020_3000, 0xFFFF_FF00, 15],
    )
    .expect_err("refused");
    assert_eq!(error_number(&mut session, past), 0x206);
}

#[test]
fn a_cie_answer_past_a_register_is_refused() {
    let mut session = common::drivers(b"");
    let xyz = call(
        &mut session,
        ct::CONVERT_RGB_TO_CIE,
        &[0x8000, 0x4000, 0xC000, 0x5A],
    )
    .expect("converted");
    assert_eq!(xyz.r[..4], [0x71D7, 0x5B4A, 0xBB1F, 0x5A]);
    let refused =
        call(&mut session, ct::CONVERT_CIE_TO_RGB, &[0x7FFF_FFFF, 0, 0]).expect_err("refused");
    assert_eq!(error_number(&mut session, refused), 0xA01);
}
