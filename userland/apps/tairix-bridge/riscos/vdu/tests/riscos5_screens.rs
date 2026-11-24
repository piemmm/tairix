//! The replay: every case genuine RISC OS 5's BASIC V ran on the screen for
//! `cargo xtask riscos --reference` (`tests/riscos5/screens.txt`), framed
//! and run in order in one session as that harness ran them, and what each
//! read and drew held to what genuine RISC OS recorded
//! (`tests/riscos5/drawn.txt`), every reading and every byte of the screen.

mod replay;

use replay::MODULES;
use tairix_fscopy::fixture::Tree;
use tairix_riscos_filing::session::{command, disc_file, session, Setup, DISC, SLOT_BYTES};
use tairix_riscos_kernel::os::{Fail, Unwind};
use tairix_riscos_vdu::corpus;
use tairix_vdu::mode::var::{LOG2_BPP, X_WIND_LIMIT, Y_WIND_LIMIT};

const CASES: &str = include_str!("riscos5/screens.txt");
const DRAWN: &str = include_str!("riscos5/drawn.txt");

#[test]
fn every_screen_is_read_and_drawn_as_genuine_risc_os_did() {
    let wrong = replay::differences(CASES, DRAWN, replay::MODULES.as_slice(), &[]);
    assert!(
        wrong.is_empty(),
        "{} cases differ from genuine RISC OS:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

#[test]
fn mode_given_a_string_selects_the_mode_it_describes_and_goes_on() {
    let program = "10 MODE \"X640 Y480 C16\"\n20 F%=OPENOUT \"HostFS:$.Done\"\n30 CLOSE#F%\n";
    let tree = Tree::new()
        .dir(DISC)
        .file(&format!("{DISC}/ModeStr,fff"), program.as_bytes());
    let mut session = session(&Setup {
        vdu: Some(tairix_riscos_vdu::DRIVERS),
        modules: MODULES.as_slice(),
        bytes: SLOT_BYTES,
        keys: &[],
        disc: Some(&tree),
    });
    let entered = command(&mut session.kernel.os(), &corpus::run_command("ModeStr"));
    assert!(
        matches!(entered, Err(Fail::Unwind(Unwind::Transfer))),
        "{entered:?}"
    );
    session.run(1 << 34);
    assert!(
        disc_file(&tree, "Done").is_some(),
        "the program went on past MODE"
    );
    let variable = |session: &mut tairix_riscos_filing::session::Session, number| {
        let mut regs = tairix_riscos_kernel::os::Regs::default();
        regs.r[..2].copy_from_slice(&[u32::MAX, number]);
        session
            .kernel
            .os()
            .perform(
                tairix_riscos_kernel::swi::X | tairix_riscos_kernel::swi::os::READ_MODE_VARIABLE,
                &mut regs,
            )
            .expect("read");
        regs.r[2]
    };
    assert_eq!(
        [X_WIND_LIMIT, Y_WIND_LIMIT, LOG2_BPP].map(|number| variable(&mut session, number)),
        [639, 479, 2],
        "640 by 480 at four bits a pixel, which the session did not start in"
    );
}
