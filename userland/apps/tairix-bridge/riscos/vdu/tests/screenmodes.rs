//! ScreenModes in a session: a monitor definition file loaded from the disc
//! answers the mode extension service for its modes, and a selector pointer
//! running off the top of memory is answered as one it cannot read, never a
//! crash.

mod common;

use std::sync::LazyLock;

use common::SELECTOR_AT;
use tairix_fscopy::fixture::Tree;
use tairix_riscos_filing::session::{command, modules_with, session, Setup, DISC, SLOT_BYTES};
use tairix_riscos_kernel::module::Builtin;
use tairix_riscos_kernel::os::Regs;
use tairix_riscos_kernel::service::MODE_EXTENSION;
use tairix_riscos_kernel::swi::os::SERVICE_CALL;
use tairix_riscos_kernel::swi::X;

const MONITOR: &[u8] =
    b"file_format:1\nmonitor_title:Test\nstartmode\nmode_name:VGA\nx_res:640\ny_res:480\n\
pixel_rate:25175\nh_timings:96,40,8,640,8,8\nv_timings:2,25,8,480,8,2\nsync_pol:3\nendmode\n";
/// A selector for the file's mode at eight bits a pixel, any frame rate.
const SELECTOR: [u32; 6] = [1, 640, 480, 3, u32::MAX, u32::MAX];

static MODULES: LazyLock<Vec<&'static Builtin>> =
    LazyLock::new(|| modules_with(&[&tairix_riscos_vdu::MODULES]));

#[test]
fn a_loaded_file_answers_for_its_modes_and_a_wild_selector_is_passed_on() {
    let tree = Tree::new()
        .dir(DISC)
        .file(&format!("{DISC}/Monitor,fff"), MONITOR);
    let mut session = session(&Setup {
        vdu: Some(tairix_riscos_vdu::DRIVERS),
        modules: MODULES.as_slice(),
        bytes: SLOT_BYTES,
        keys: b"",
        disc: Some(&tree),
    });
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
const READ_INFO: u32 = 0x4_87C0;
const ENUMERATE_AUDIO_FORMATS: u32 = 0x4_87C1;
const FEATURES: u32 = 0x4_87C2;
const PAST_SWIS: u32 = 0x4_87C3;

/// A session whose disc holds `files`, each a name and its text.
fn with_files(files: &[(&str, &[u8])]) -> tairix_riscos_filing::session::Session {
    let mut tree = Tree::new().dir(DISC);
    for (name, text) in files {
        tree = tree.file(&format!("{DISC}/{name},fff"), text);
    }
    session(&Setup {
        vdu: Some(tairix_riscos_vdu::DRIVERS),
        modules: MODULES.as_slice(),
        bytes: SLOT_BYTES,
        keys: b"",
        disc: Some(&tree),
    })
}

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
