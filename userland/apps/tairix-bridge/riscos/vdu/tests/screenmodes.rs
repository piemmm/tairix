//! ScreenModes in a session: a monitor definition file loaded from the disc
//! answers the mode extension service for its modes, and a selector pointer
//! running off the top of memory is answered as one it cannot read, never a
//! crash.

use std::sync::LazyLock;

use tairix_fscopy::fixture::Tree;
use tairix_riscos_filing::session::{command, session, Setup, DISC};
use tairix_riscos_kernel::module::Builtin;
use tairix_riscos_kernel::os::Regs;
use tairix_riscos_kernel::swi::os::SERVICE_CALL;
use tairix_riscos_kernel::swi::X;

const MONITOR: &[u8] =
    b"file_format:1\nmonitor_title:Test\nstartmode\nmode_name:VGA\nx_res:640\ny_res:480\n\
pixel_rate:25175\nh_timings:96,40,8,640,8,8\nv_timings:2,25,8,480,8,2\nsync_pol:3\nendmode\n";
const MODE_EXTENSION: u32 = 0x50;
/// A selector for the file's mode at eight bits a pixel, any frame rate.
const SELECTOR: [u32; 6] = [1, 640, 480, 3, u32::MAX, u32::MAX];
const SELECTOR_AT: u32 = 0x9000;

static MODULES: LazyLock<Vec<&'static Builtin>> = LazyLock::new(|| {
    let mut modules: Vec<&'static Builtin> = tairix_riscos_filing::MODULES.to_vec();
    modules.extend(tairix_riscos_vdu::MODULES.iter().copied());
    modules
});

#[test]
fn a_loaded_file_answers_for_its_modes_and_a_wild_selector_is_passed_on() {
    let tree = Tree::new()
        .dir(DISC)
        .file(&format!("{DISC}/Monitor,fff"), MONITOR);
    let mut session = session(&Setup {
        vdu: Some(tairix_riscos_vdu::DRIVERS),
        modules: MODULES.as_slice(),
        bytes: 0x10_0000,
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
