//! The services the drivers issue, as a module offered them sees them: a
//! mode change announces output going back to the screen between
//! `Service_ModeChanging` and `Service_ModeChange`, and a calibration set or
//! refused is announced, as RISC OS 5 does both.

mod common;

use std::sync::LazyLock;

use tairix_riscos_filing::session::{modules_with, session, Session, Setup, SLOT_BYTES};
use tairix_riscos_kernel::module::{Builtin, Services};
use tairix_riscos_kernel::os::{Fail, Os, Regs, Unwind};
use tairix_riscos_kernel::service;
use tairix_riscos_kernel::swi::colour_trans as ct;

/// What the spy was offered: each service's R1 to R5.
struct Seen(Vec<[u32; 5]>);

/// The spy, in the inventory's row for Draw, whose crate is built on the
/// drivers, so a session of the drivers' modules leaves it free.
static SPY: Builtin = Builtin {
    title: "Draw",
    help_name: None,
    help_suffix: None,
    bridge: false,
    swis: None,
    services: Some(Services {
        numbers: &[
            service::PRE_MODE_CHANGE,
            service::MODE_CHANGING,
            service::SWITCHING_OUTPUT_TO_SPRITE,
            service::MODE_CHANGE,
            service::CALIBRATION_CHANGED,
        ],
        handler: offered,
    }),
    commands: &[],
    init: Some(watch),
    finalise: None,
    start: None,
    routines: &[],
    vectors: &[],
    resources: &[],
    code: None,
};

fn watch(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    os.hold_extension(Seen(Vec::new()))
}

#[expect(clippy::unnecessary_wraps, reason = "a service handler's signature")]
fn offered(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Unwind> {
    if let Some(Seen(seen)) = os.extension::<Seen>() {
        seen.push([regs.r[1], regs.r[2], regs.r[3], regs.r[4], regs.r[5]]);
    }
    Ok(())
}

static MODULES: LazyLock<Vec<&'static Builtin>> =
    LazyLock::new(|| modules_with(&[&tairix_riscos_vdu::MODULES, &[&SPY]]));

/// A session with the spy among the drivers' modules, what it saw so far
/// forgotten.
fn watched() -> Session {
    let mut session = session(&Setup {
        vdu: Some(tairix_riscos_vdu::DRIVERS),
        modules: MODULES.as_slice(),
        bytes: SLOT_BYTES,
        keys: b"",
        disc: None,
    });
    if let Some(Seen(seen)) = session.kernel.os().extension::<Seen>() {
        seen.clear();
    }
    session
}

/// The services the spy has seen, R1 to R5 each, forgetting them.
fn seen(session: &mut Session) -> Vec<[u32; 5]> {
    session
        .kernel
        .os()
        .extension::<Seen>()
        .map(|Seen(seen)| core::mem::take(seen))
        .expect("the spy watching")
}

#[test]
fn a_mode_change_announces_output_back_on_the_screen_before_it_is_done() {
    let mut session = watched();
    common::vdu(&mut session, &[22, 12]).expect("mode 12 selected");
    let seen = seen(&mut session);
    let numbers: Vec<u32> = seen.iter().map(|service| service[0]).collect();
    assert_eq!(
        numbers,
        [
            service::PRE_MODE_CHANGE,
            service::MODE_CHANGING,
            service::SWITCHING_OUTPUT_TO_SPRITE,
            service::MODE_CHANGE,
        ]
    );
    assert_eq!(
        seen[2][1..],
        [0x23C, 0, 0, 1],
        "the screen's switch: by pointer, no area or sprite, the drivers' own save area"
    );
}

#[test]
fn a_calibration_set_or_refused_is_announced_and_the_default_restored_quietly() {
    let mut session = watched();
    let table = [2, 2, 2, 0, 0xFF, 0, 0xFF, 0, 0xFF];
    let os = &mut session.kernel.os();
    os.write_words(0x9000, &table).expect("written");
    let changed = |session: &mut Session| {
        let seen = seen(session);
        seen.iter()
            .filter(|service| service[0] == service::CALIBRATION_CHANGED)
            .count()
    };
    common::call(&mut session, ct::SET_CALIBRATION, &[0x9000]).expect("set");
    assert_eq!(changed(&mut session), 1);
    session
        .kernel
        .os()
        .write_word(0x9000 + 16, 0xFE)
        .expect("written");
    common::call(&mut session, ct::SET_CALIBRATION, &[0x9000]).expect_err("refused");
    assert_eq!(changed(&mut session), 1, "the default restored");
    common::call(&mut session, ct::SET_CALIBRATION, &[0]).expect("the default");
    assert_eq!(changed(&mut session), 0);
}
