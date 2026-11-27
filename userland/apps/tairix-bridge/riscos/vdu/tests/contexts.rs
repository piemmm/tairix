//! Each context's screen: its own mode, palette and memory, ColourTrans
//! matching against the screen of the context current, and an ended
//! context's screen given back.

mod common;

use common::{call, drivers, rma_used, vdu};
use tairix_riscos_filing::session::{application_slot, Session, SLOT_BYTES};
use tairix_riscos_kernel::memory::{FREE_POOL, SCREEN};
use tairix_riscos_kernel::swi::colour_trans as ct;
use tairix_riscos_kernel::swi::os::{BYTE, READ_DYNAMIC_AREA};

/// The screen mode, as `OS_Byte` 135 reads it.
fn mode(session: &mut Session) -> u32 {
    call(session, BYTE, &[135]).expect("read").r[2]
}

/// Where the current context's screen area is.
fn screen_base(session: &mut Session) -> u32 {
    call(session, READ_DYNAMIC_AREA, &[SCREEN])
        .expect("a screen area")
        .r[0]
}

fn colour_number(session: &mut Session, colour: u32) -> u32 {
    call(session, ct::RETURN_COLOUR_NUMBER, &[colour])
        .expect("matched")
        .r[0]
}

/// A new context, current, with an application slot.
fn launched(session: &mut Session) -> tairix_riscos_kernel::context::ContextId {
    let id = session.kernel.os().launch_context().expect("launched");
    application_slot(&mut session.kernel, SLOT_BYTES);
    id
}

#[test]
fn each_context_has_a_screen_of_its_own() {
    let mut session = drivers(b"");
    let configured = mode(&mut session);
    vdu(&mut session, &[22, 1, b'A']).expect("mode 1");
    let first = session.kernel.os().context();
    let first_base = screen_base(&mut session);
    let second = launched(&mut session);
    assert_eq!(
        mode(&mut session),
        configured,
        "a new screen in the configured mode"
    );
    let second_base = screen_base(&mut session);
    assert_ne!(second_base, first_base, "its own memory");
    vdu(&mut session, &[22, 9]).expect("mode 9");
    session.kernel.os().switch_context(first).expect("switched");
    assert_eq!(mode(&mut session), 1);
    assert_eq!(screen_base(&mut session), first_base);
    session
        .kernel
        .os()
        .switch_context(second)
        .expect("switched");
    assert_eq!(mode(&mut session), 9);
}

#[test]
fn colourtrans_matches_against_the_current_context_s_screen() {
    let green = 0x00FF_0000;
    let mut session = drivers(b"");
    vdu(&mut session, &[22, 12]).expect("mode 12");
    vdu(&mut session, &[19, 9, 16, 0, 0xFF, 0]).expect("colour 9 green");
    vdu(&mut session, &[19, 2, 16, 0, 0, 0]).expect("colour 2 black");
    assert_eq!(colour_number(&mut session, green), 9);
    let first = session.kernel.os().context();
    let second = launched(&mut session);
    vdu(&mut session, &[22, 12]).expect("mode 12");
    assert_eq!(colour_number(&mut session, green), 2, "its own palette's");
    session.kernel.os().switch_context(first).expect("switched");
    assert_eq!(colour_number(&mut session, green), 9);
    session
        .kernel
        .os()
        .switch_context(second)
        .expect("switched");
    assert_eq!(colour_number(&mut session, green), 2);
}

/// The free pool's bytes.
fn free_pool(session: &mut Session) -> u32 {
    call(session, READ_DYNAMIC_AREA, &[FREE_POOL])
        .expect("the free pool")
        .r[1]
}

#[test]
fn an_ended_context_s_screen_slot_and_copies_are_given_back() {
    let mut session = drivers(b"");
    let first = session.kernel.os().context();
    let first_base = screen_base(&mut session);
    let used = rma_used(&mut session);
    let free = free_pool(&mut session);
    let second = launched(&mut session);
    assert!(rma_used(&mut session) > used, "its mode selector's copies");
    assert!(
        free_pool(&mut session) < free,
        "its screen's and slot's pages"
    );
    session.kernel.os().end_context(first).expect("ended");
    assert_eq!(rma_used(&mut session), used, "the copies given back");
    assert_eq!(free_pool(&mut session), free, "the pages given back");
    assert_eq!(screen_base(&mut session), first_base);
    assert!(session.kernel.os().switch_context(second).is_err());
}
