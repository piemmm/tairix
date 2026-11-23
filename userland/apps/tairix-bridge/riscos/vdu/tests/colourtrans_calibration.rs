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
    call(&mut session, ct::SET_CALIBRATION, &[0]).expect("the default");
    command(&mut session, "ColourTransMap &FF0000FF 0 &FF00FF 0").expect("the first line");
    assert!(
        read_back(&mut session).is_empty(),
        "nothing set until all are given"
    );
    command(&mut session, "ColourTransMap &FF0000FF &20402080 &10101000").expect("the rest");
    assert_eq!(read_back(&mut session), THREE, "the lines joined");
    command(&mut session, "ColourTransMap 0 &FF").expect("a start toward another");
    let over = command(&mut session, "ColourTransMap 1 2 3 4 5 6").expect_err("too many");
    assert_eq!(error_number(&mut session, over), 0xDC);
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

#[test]
fn a_device_palette_is_converted_entry_by_entry_as_one_device_colour_is() {
    let mut session = common::drivers(b"");
    set_table(&mut session, &THREE).expect("set");
    let devices = [0, 0x8080_8000, 0x1234_5600, 0xFFFF_FF00, 0x00FF_0000];
    let (from, to) = (READ, READ + 0x100);
    for (i, &device) in (0u32..).zip(&devices) {
        session
            .kernel
            .os()
            .write_word(from + 4 * i, device)
            .expect("written");
    }
    let count = u32::try_from(devices.len()).expect("few");
    call(
        &mut session,
        ct::CONVERT_DEVICE_PALETTE,
        &[count, from, to, 0],
    )
    .expect("converted");
    for (i, &device) in (0u32..).zip(&devices) {
        let converted = session.kernel.os().read_word(to + 4 * i).expect("read");
        assert_eq!(converted, device_colour(&mut session, device), "entry {i}");
    }
}

#[test]
fn a_calibration_written_to_a_file_sets_itself_again_when_obeyed() {
    use tairix_riscos_kernel::swi::os::FIND;
    let mut session = common::with_files(&[]);
    set_table(&mut session, &THREE).expect("set");
    let name = TABLE + 0x400;
    session
        .kernel
        .os()
        .write_bytes(name, b"HostFS:$.Cal\0")
        .expect("written");
    let handle = call(&mut session, FIND, &[0x83, name]).expect("opened").r[0];
    let last = call(&mut session, ct::WRITE_CALIBRATION_TO_FILE, &[0, handle])
        .expect("written")
        .r[0];
    call(&mut session, FIND, &[0, handle]).expect("closed");
    assert_eq!(last, u32::from(b'\n'), "R0 the last character written");
    call(&mut session, ct::SET_CALIBRATION, &[0]).expect("the default");
    assert!(read_back(&mut session).is_empty());
    command(&mut session, "Obey HostFS:$.Cal").expect("obeyed");
    assert_eq!(read_back(&mut session), THREE);
}

#[test]
fn a_size_no_list_may_hold_is_refused_and_gathers_nothing() {
    let mut session = common::drivers(b"");
    let refused = command(&mut session, "ColourTransMapSize 257 2 2").expect_err("refused");
    assert_eq!(error_number(&mut session, refused), 0xA00);
    let over = command(&mut session, "ColourTransMap 0 &FF").expect_err("nothing sized");
    assert_eq!(error_number(&mut session, over), 0xDC);
}

/// The address of command `name`'s code in module `module`, as guest code
/// finds it in the module's command table.
fn command_code(session: &mut Session, module: &str, name: &str) -> u32 {
    let mut text = module.as_bytes().to_vec();
    text.push(0);
    let os = &mut session.kernel.os();
    os.write_bytes(common::COMMAND_AT, &text).expect("written");
    let base = call(
        session,
        tairix_riscos_kernel::swi::os::MODULE,
        &[18, common::COMMAND_AT],
    )
    .expect("found")
    .r[3];
    let os = &mut session.kernel.os();
    let mut entry = base + os.read_word(base + 0x18).expect("its table");
    loop {
        let found = os.read_zstring(entry, 64).expect("a name");
        assert!(!found.is_empty(), "{name} in {module}'s table");
        let code = (entry + u32::try_from(found.len()).expect("short") + 1).next_multiple_of(4);
        if found == name.as_bytes() {
            return base + os.read_word(code).expect("its code");
        }
        entry = code + 16;
    }
}

#[test]
fn more_pairs_than_were_sized_are_refused_before_any_is_read() {
    use tairix_riscos_kernel::memory::Bits;
    use tairix_riscos_kernel::os::{Entry, Regs};
    let mut session = common::drivers(b"");
    command(&mut session, "ColourTransMapSize 2 2 2").expect("sized");
    let code = command_code(&mut session, "ColourTrans", "ColourTransMap");
    session
        .kernel
        .os()
        .write_bytes(common::COMMAND_AT, b"0 &FF\r")
        .expect("written");
    let mut regs = Regs::default();
    regs.r[..2].copy_from_slice(&[common::COMMAND_AT, 7]);
    let over = session
        .kernel
        .os()
        .call_routine(code, Bits::Bit32, Entry::Supervisor, &mut regs)
        .expect_err("seven past six");
    assert_eq!(error_number(&mut session, over), 0xDC);
}

#[test]
fn a_pair_count_no_memory_backs_is_refused_before_the_host_holds_any() {
    use tairix_riscos_kernel::memory::Bits;
    use tairix_riscos_kernel::os::{Entry, Regs};
    let mut session = common::drivers(b"");
    command(&mut session, "ColourTransMapSize 2 2 2").expect("sized");
    let code = command_code(&mut session, "ColourTrans", "ColourTransMap");
    let mut regs = Regs::default();
    regs.r[..2].copy_from_slice(&[common::COMMAND_AT, 0x3FFF_FFFF]);
    let over = session
        .kernel
        .os()
        .call_routine(code, Bits::Bit32, Entry::Supervisor, &mut regs)
        .expect_err("past any list");
    assert_eq!(error_number(&mut session, over), 0xDC);
}

#[test]
fn a_calibration_the_guest_freed_is_replaced_without_complaint() {
    use tairix_riscos_kernel::swi::os::{MODULE, READ_DYNAMIC_AREA};
    let mut session = common::drivers(b"");
    set_table(&mut session, &THREE).expect("set");
    let rma = call(&mut session, READ_DYNAMIC_AREA, &[1]).expect("the RMA");
    let (base, size) = (rma.r[0], rma.r[1]);
    let mut held = vec![0u8; usize::try_from(size).expect("a size")];
    session
        .kernel
        .os()
        .read_bytes(base, &mut held)
        .expect("read");
    let words: Vec<u8> = THREE.iter().flat_map(|w| w.to_le_bytes()).collect();
    let at = held
        .windows(words.len())
        .position(|window| window == words.as_slice())
        .expect("ColourTrans's copy");
    let block = base + u32::try_from(at).expect("inside the RMA");
    call(&mut session, MODULE, &[7, 0, block]).expect("freed by the guest");
    set_table(&mut session, &THREE).expect("set again");
    assert_eq!(read_back(&mut session), THREE);
}
