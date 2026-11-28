//! The hourglass a program shows: its context's own and gone with it, let
//! go when a new application starts or the module is killed, and drawn as
//! the pointer the front end shows.

mod common;

use common::{call, command, drivers, launched, pointer_selected, wait};
use tairix_riscos_filing::session::Session;
use tairix_riscos_kernel::service::NEW_APPLICATION;
use tairix_riscos_kernel::swi::chunk::HOURGLASS;
use tairix_riscos_kernel::swi::os::{BYTE, SERVICE_CALL};
use tairix_riscos_vdu::display;

const ON: u32 = HOURGLASS;
const OFF: u32 = HOURGLASS + 1;
/// The pointer shapes its frames select.
const SHAPES: [u32; 2] = [3, 4];
/// Its colours 1 and 3 as the pointer wears them, as `&RRGGBB`.
const CYAN: u32 = 0x00_FF_FF;
const BLUE: u32 = 0x00_00_FF;

fn shows(session: &mut Session) -> bool {
    display::hourglass(&mut session.kernel.os())
}

/// An hourglass turned on and waited for, pointer shape 1 selected before.
fn shown(session: &mut Session) {
    call(session, BYTE, &[106, 1]).expect("a shape selected");
    call(session, ON, &[]).expect("on");
    wait(session, 40);
    assert!(shows(session), "shown once its delay passed");
}

#[test]
fn each_context_has_an_hourglass_of_its_own_which_goes_with_it() {
    let mut session = drivers(b"");
    let first = session.kernel.os().context();
    shown(&mut session);
    let second = launched(&mut session);
    assert!(!shows(&mut session), "a new context's is its own");
    assert_eq!(pointer_selected(&mut session), 0);
    session.kernel.os().switch_context(first).expect("switched");
    assert!(shows(&mut session));
    assert!(SHAPES.contains(&pointer_selected(&mut session)));
    session.kernel.os().end_context(second).expect("ended");
    assert!(!shows(&mut session));
    assert_eq!(
        session.kernel.os().deadlines().own,
        None,
        "nothing of the ended context's left to wake for"
    );
    launched(&mut session);
    assert!(!shows(&mut session));
}

#[test]
fn a_new_application_lets_go_of_the_hourglass_the_last_left_on() {
    let mut session = drivers(b"");
    shown(&mut session);
    call(&mut session, SERVICE_CALL, &[0, NEW_APPLICATION]).expect("the service");
    assert!(!shows(&mut session));
    assert_eq!(pointer_selected(&mut session), 1, "its pointer given back");
    call(&mut session, OFF, &[]).expect("an off the program owed is no error");
    wait(&mut session, 40);
    assert!(!shows(&mut session), "it stays gone");
}

#[test]
fn a_killed_module_s_hourglass_is_given_back_in_each_context_it_showed_in() {
    let mut session = drivers(b"");
    let first = session.kernel.os().context();
    shown(&mut session);
    launched(&mut session);
    command(&mut session, "RMKill Hourglass").expect("killed");
    session.kernel.os().switch_context(first).expect("switched");
    assert!(!shows(&mut session), "no incarnation shows it");
    wait(&mut session, 1);
    assert_eq!(pointer_selected(&mut session), 1, "given back once it ran");
    command(&mut session, "RMReInit Hourglass").expect("started again");
    shown(&mut session);
    call(&mut session, OFF, &[]).expect("off");
    assert!(!shows(&mut session), "the new incarnation's, matched");
}

#[test]
fn a_showing_hourglass_is_the_pointer_the_front_end_draws() {
    let mut session = drivers(b"");
    shown(&mut session);
    let mut pixels = Vec::new();
    let view = display::pointer(&mut session.kernel.os(), &mut pixels)
        .expect("read")
        .expect("a pointer");
    assert_eq!((view.width, view.height), (16, 32));
    assert_eq!(view.active, (8, 11), "its place the waist");
    assert_eq!(pixels.len(), 16 * 32);
    assert_eq!([view.colours[0], view.colours[2]], [CYAN, BLUE]);
    call(&mut session, OFF, &[]).expect("off");
    let view = display::pointer(&mut session.kernel.os(), &mut pixels)
        .expect("read")
        .expect("the program's own pointer");
    assert_ne!((view.width, view.height), (16, 32), "shape 1 again");
}
