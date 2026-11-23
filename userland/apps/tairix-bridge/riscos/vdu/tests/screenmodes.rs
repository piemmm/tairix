//! ScreenModes in a session: a monitor definition file loaded from the disc
//! answers the mode extension service for its modes, and a selector pointer
//! running off the top of memory is answered as one it cannot read, never a
//! crash.

mod common;

use common::{with_files, SELECTOR_AT};
use tairix_riscos_filing::session::command;
use tairix_riscos_kernel::os::Regs;
use tairix_riscos_kernel::service::MODE_EXTENSION;
use tairix_riscos_kernel::swi::chunk::SCREEN_MODES;
use tairix_riscos_kernel::swi::os::SERVICE_CALL;
use tairix_riscos_kernel::swi::X;
use tairix_vdu::mode::var::{LOG2_BPP, X_EIG, X_WIND_LIMIT, Y_EIG, Y_WIND_LIMIT};

const MONITOR: &[u8] =
    b"file_format:1\nmonitor_title:Test\nstartmode\nmode_name:VGA\nx_res:640\ny_res:480\n\
pixel_rate:25175\nh_timings:96,40,8,640,8,8\nv_timings:2,25,8,480,8,2\nsync_pol:3\nendmode\n";
/// A selector for the file's mode at eight bits a pixel, any frame rate.
const SELECTOR: [u32; 6] = [1, 640, 480, 3, u32::MAX, u32::MAX];

#[test]
fn a_loaded_file_answers_for_its_modes_and_a_wild_selector_is_passed_on() {
    let mut session = with_files(&[("Monitor", MONITOR)]);
    let os = &mut session.kernel.os();
    command(os, "LoadModeFile HostFS:$.Monitor").expect("the file loaded");
    for (i, word) in (0u32..).zip(SELECTOR) {
        os.write_word(SELECTOR_AT + 4 * i, word).expect("written");
    }

    let mut regs = Regs::new([0, MODE_EXTENSION, SELECTOR_AT, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    os.perform(X | SERVICE_CALL, &mut regs).expect("issued");
    assert_eq!(regs.r[1], 0, "claimed for the file's mode");
    let list = regs.r[3];
    assert_eq!(
        os.read_word(list).expect("the list"),
        3,
        "a type 3 VIDC list"
    );
    assert_eq!(os.read_word(list + 4).expect("its depth"), 3);

    for wild in [0xFFFF_FFF0, 0xFFFF_FFFC, u32::MAX] {
        let mut regs = Regs::new([0, MODE_EXTENSION, wild, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        os.perform(X | SERVICE_CALL, &mut regs).expect("issued");
        assert_eq!(
            regs.r[1], MODE_EXTENSION,
            "{wild:#x} names no mode the file has"
        );
    }
}

/// ScreenModes' SWIs and the first offset past them.
const READ_INFO: u32 = SCREEN_MODES;
const ENUMERATE_AUDIO_FORMATS: u32 = SCREEN_MODES + 1;
const FEATURES: u32 = SCREEN_MODES + 2;
const PAST_SWIS: u32 = SCREEN_MODES + 3;

/// SWI `number` with R0 to R2 `low`: what it leaves in R0 to R2, or its
/// error's number.
fn call(
    os: &mut tairix_riscos_kernel::os::Os<'_>,
    number: u32,
    low: [u32; 3],
) -> Result<[u32; 3], u32> {
    let mut regs = Regs::new([low[0], low[1], low[2], 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    match os.perform(X | number, &mut regs) {
        Ok(()) => Ok([regs.r[0], regs.r[1], regs.r[2]]),
        Err(tairix_riscos_kernel::os::Fail::Error(block)) => {
            Err(os.read_word(block).expect("a number"))
        }
        Err(other) => panic!("SWI &{number:X}: {other:?}"),
    }
}

/// Each answer recorded from genuine RISC OS 5's ScreenModes 0.74, with no
/// file loaded and then with one.
#[test]
fn screenmodes_swis_answer_as_risc_os_5_s_with_and_without_a_file() {
    let dpms = b"file_format:1\nmonitor_title:Probe Two\nDPMS_state:2\nstartmode\nmode_name:VGA\n\
pixel_rate:25175\nh_timings:96,40,8,640,8,8\nv_timings:2,25,8,480,8,2\nsync_pol:3\nendmode\n";
    let mut session = with_files(&[("Monitor", MONITOR), ("Dpms", dpms)]);
    let os = &mut session.kernel.os();
    for reason in 0..3 {
        assert_eq!(call(os, READ_INFO, [reason, 7, 8]), Err(0xBF4), "{reason}");
    }
    assert_eq!(
        call(os, READ_INFO, [3, 7, 8]),
        Err(0xBF6),
        "the reason first"
    );
    assert_eq!(
        call(os, ENUMERATE_AUDIO_FORMATS, [5, 7, 8]),
        Err(0xBF4),
        "the file first"
    );
    assert_eq!(
        call(os, FEATURES, [7, 0x1234, 0x5678]),
        Ok([1, 0x1234, 0x5678])
    );
    assert_eq!(call(os, PAST_SWIS, [0, 0, 0]), Err(0x1E6));

    command(os, "LoadModeFile HostFS:$.Monitor").expect("loaded");
    let title = call(os, READ_INFO, [0, 7, 8]).expect("the title");
    assert_eq!(os.read_zstring(title[0], 64).expect("its text"), b"Test");
    assert_eq!(title[1..], [7, 8]);
    assert_eq!(
        call(os, READ_INFO, [1, 7, 8]),
        Ok([u32::MAX, 7, 8]),
        "no DPMS state"
    );
    assert_eq!(
        call(os, READ_INFO, [2, 7, 8]),
        Ok([0, 0, 8]),
        "no speakers, none known"
    );
    assert_eq!(call(os, READ_INFO, [3, 7, 8]), Err(0xBF6));
    for form in [0, 1] {
        assert_eq!(
            call(os, ENUMERATE_AUDIO_FORMATS, [form, 5, 7]),
            Ok([form, u32::MAX, u32::MAX]),
            "no formats"
        );
    }
    assert_eq!(call(os, ENUMERATE_AUDIO_FORMATS, [2, 0, 0]), Err(0xBFC));

    command(os, "LoadModeFile HostFS:$.Dpms").expect("loaded");
    assert_eq!(call(os, READ_INFO, [1, 7, 8]), Ok([2, 7, 8]));
    let title = call(os, READ_INFO, [0, 7, 8]).expect("the title")[0];
    assert_eq!(os.read_zstring(title, 64).expect("its text"), b"Probe Two");
}

#[test]
fn a_monitor_file_risc_os_5_refuses_is_refused_with_its_error() {
    let blank = b"file_format:1\nmonitor_title:   \nstartmode\nendmode\n";
    let mut session = with_files(&[("Blank", blank)]);
    let os = &mut session.kernel.os();
    match command(os, "LoadModeFile HostFS:$.Blank") {
        Err(tairix_riscos_kernel::os::Fail::Error(block)) => {
            assert_eq!(
                os.read_word(block).expect("a number"),
                0xBEF,
                "a blank title"
            );
        }
        other => panic!("loaded: {other:?}"),
    }
    assert_eq!(
        call(os, READ_INFO, [0, 0, 0]),
        Err(0xBF4),
        "still none loaded"
    );
}

#[test]
fn a_reinitialised_screenmodes_closes_the_messages_its_errors_opened() {
    let mut session = with_files(&[]);
    let os = &mut session.kernel.os();
    let mut cycle = || {
        assert!(
            call(os, READ_INFO, [9, 0, 0]).is_err(),
            "a reason past the last"
        );
        command(os, "RMReInit ScreenModes").expect("reinitialised");
        let mut described = Regs::new([5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        os.perform(X | tairix_riscos_kernel::swi::os::MODULE, &mut described)
            .expect("the RMA described");
        described.r[3]
    };
    let first = cycle();
    assert_eq!(cycle(), first, "the RMA as it was");
}

#[test]
fn a_teletext_mode_two_banks_of_which_pass_a_word_is_refused_not_a_crash() {
    let huge = b"file_format:1\nmonitor_title:Huge\nstartmode\nmode_name:Huge\nx_res:16384\n\
y_res:16384\npixel_rate:250000\nh_timings:96,40,8,16384,8,8\nv_timings:2,25,8,16384,8,2\n\
sync_pol:3\nendmode\n";
    let mut session = with_files(&[("Huge", huge)]);
    let os = &mut session.kernel.os();
    command(os, "LoadModeFile HostFS:$.Huge").expect("the file loaded");
    let mut shadow = Regs::new([114, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    os.perform(X | tairix_riscos_kernel::swi::os::BYTE, &mut shadow)
        .expect("the shadow bank asked for");
    let selector = [
        1,
        16384,
        16384,
        2,
        u32::MAX,
        0,
        2,
        6,
        65536,
        1,
        39,
        2,
        24,
        u32::MAX,
    ];
    for (i, word) in (0u32..).zip(selector) {
        os.write_word(SELECTOR_AT + 4 * i, word).expect("written");
    }
    let mut regs = Regs::new([0, SELECTOR_AT, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let selected = os.perform(X | tairix_riscos_kernel::swi::os::SCREEN_MODE, &mut regs);
    assert!(
        matches!(selected, Err(tairix_riscos_kernel::os::Fail::Error(_))),
        "{selected:?}"
    );
}

#[test]
fn a_screen_loaded_from_a_newer_sprite_takes_a_mode_of_its_pixels() {
    use tairix_riscos_kernel::swi::os::{READ_MODE_VARIABLE, SCREEN_MODE, SPRITE_OP};
    let mut session = with_files(&[]);
    let os = &mut session.kernel.os();
    for (i, word) in (0u32..).zip([1, 640, 480, 5, u32::MAX, u32::MAX]) {
        os.write_word(SELECTOR_AT + 4 * i, word).expect("written");
    }
    call(os, SCREEN_MODE, [0, SELECTOR_AT, 0]).expect("32 bits a pixel");
    let name = SELECTOR_AT + 0x100;
    os.write_bytes(name, b"HostFS:$.Shot\0").expect("written");
    call(os, SPRITE_OP, [2, 0, name]).expect("saved");
    call(os, SCREEN_MODE, [0, 12, 0]).expect("mode 12");
    call(os, SPRITE_OP, [3, 0, name]).expect("loaded");
    let read = |os: &mut tairix_riscos_kernel::os::Os<'_>, variable: u32| {
        call(os, READ_MODE_VARIABLE, [u32::MAX, variable, 0]).expect("read")[2]
    };
    let (x_eig, y_eig, log2_bpp, right, top) = (X_EIG, Y_EIG, LOG2_BPP, X_WIND_LIMIT, Y_WIND_LIMIT);
    assert_eq!(
        [x_eig, y_eig, log2_bpp, right, top].map(|variable| read(os, variable)),
        [1, 1, 5, 639, 479]
    );
}

#[test]
fn a_numbered_mode_on_a_file_s_monitor_is_its_nearest_mode_at_that_depth() {
    use tairix_riscos_kernel::swi::os::{READ_MODE_VARIABLE, SCREEN_MODE};
    let mut session = with_files(&[("Monitor", MONITOR)]);
    let os = &mut session.kernel.os();
    command(os, "LoadModeFile HostFS:$.Monitor").expect("the file loaded");
    call(os, SCREEN_MODE, [0, 12, 0]).expect("mode 12");
    let read = |os: &mut tairix_riscos_kernel::os::Os<'_>, variable: u32| {
        call(os, READ_MODE_VARIABLE, [u32::MAX, variable, 0]).expect("read")[2]
    };
    let (log2_bpp, right, top) = (LOG2_BPP, X_WIND_LIMIT, Y_WIND_LIMIT);
    assert_eq!(
        [log2_bpp, right, top].map(|variable| read(os, variable)),
        [2, 639, 479],
        "the file's 640 by 480, at mode 12's depth"
    );
}

#[test]
fn a_screen_saved_with_a_full_palette_is_loaded_with_one() {
    use tairix_riscos_kernel::swi::os::{READ_MODE_VARIABLE, READ_PALETTE, SCREEN_MODE, SPRITE_OP};
    let mut session = with_files(&[]);
    common::select_full_palette(&mut session).expect("256 colours, each its own");
    common::vdu(&mut session, &[19, 200, 16, 0x12, 0x34, 0x56]).expect("colour 200 set");
    let os = &mut session.kernel.os();
    let name = SELECTOR_AT + 0x100;
    os.write_bytes(name, b"HostFS:$.Full\0").expect("written");
    let mut save = Regs::new([2, 0, name, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    os.perform(X | SPRITE_OP, &mut save)
        .expect("saved with its palette");
    call(os, SCREEN_MODE, [0, 12, 0]).expect("mode 12");
    call(os, SPRITE_OP, [3, 0, name]).expect("loaded");
    let flags = call(
        os,
        READ_MODE_VARIABLE,
        [u32::MAX, tairix_vdu::mode::var::MODE_FLAGS, 0],
    )
    .expect("read")[2];
    let full = tairix_vdu::mode::Flags::FULL_PALETTE.bits();
    assert_eq!(flags & full, full, "a full palette selected");
    let colour = call(os, READ_PALETTE, [200, 16, 0]).expect("read")[2];
    assert_eq!(
        colour & 0xFFFF_FF00,
        0x5634_1200,
        "colour 200 as it was saved"
    );
}
