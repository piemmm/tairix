//! Relays: one left pending parks the program's SWI, performed again with
//! every answer it had; a request that changes the other side, and all after
//! it, waits in place, as one outside a program's own SWI does; and an
//! answer there is no room to keep or give again parks nothing more.

use alloc::vec::Vec;

use tairix_fuzzseed::meter::refusing_above;

use super::*;
use crate::memory::layout::RISC_OS_5;
use crate::memory::Bits;
use crate::os::{Outcome, Regs};
use crate::swi::X;
use crate::testkit::{boot_with, Session, SLOT_BYTES};

const RELAY: u32 = crate::swi::chunk::DRAW;
const CHANGE_THEN_READ: u32 = RELAY + 1;
const TWICE: u32 = RELAY + 2;

/// A built-in in the inventory's row for Draw, which another crate
/// provides, whose SWIs relay: `Relay` asks for R1's byte and answers its
/// reply's length and first byte; `ChangeThenRead` asks a request that
/// changes the other side, then one that does not; `Twice` asks for R1's
/// byte twice, answering each reply's length.
static RELAYING: crate::module::Builtin = crate::module::Builtin {
    title: "Draw",
    help_name: None,
    help_suffix: None,
    bridge: false,
    swis: Some(crate::module::Swis {
        chunk: RELAY,
        prefix: "Draw",
        names: &["Relay", "ChangeThenRead", "Twice"],
        handler: relaying,
    }),
    services: None,
    commands: &[],
    init: None,
    finalise: None,
    start: None,
    routines: &[],
    vectors: &[],
    resources: &[],
    code: None,
};

static MODULES: [&crate::module::Builtin; 1] = [&RELAYING];

fn relaying(os: &mut Os<'_>, offset: u32, regs: &mut Regs) -> Result<(), Fail> {
    let request = [regs.byte(1), b'?'];
    let mut reply = Vec::new();
    match offset {
        0 => {
            os.relay(&request, &mut reply, false)?;
            regs.r[0] = crate::os::len32(reply.len());
            regs.r[1] = reply.first().copied().map_or(0, u32::from);
        }
        1 => {
            os.relay(b"write", &mut reply, true)?;
            os.relay(&request, &mut reply, false)?;
            regs.r[0] = crate::os::len32(reply.len());
        }
        _ => {
            os.relay(&request, &mut reply, false)?;
            regs.r[0] = crate::os::len32(reply.len());
            reply.clear();
            os.relay(&request, &mut reply, false)?;
            regs.r[1] = crate::os::len32(reply.len());
        }
    }
    Ok(())
}

fn session() -> Session {
    let mut s = boot_with(&RISC_OS_5, tairix_tz::Zone::UTC, 0, &MODULES);
    s.application(Bits::Bit32, SLOT_BYTES);
    s
}

/// Each request asked of the host and whether it might have parked.
fn asked(s: &Session) -> Vec<(Vec<u8>, bool)> {
    s.relays
        .borrow()
        .asked
        .iter()
        .map(|(_, request, parking)| (request.clone(), *parking))
        .collect()
}

fn pend(s: &Session, pends: &[bool]) {
    s.relays.borrow_mut().pend = pends.iter().copied().collect();
}

#[test]
fn a_relay_left_pending_parks_its_swi_which_is_answered_from_the_reply_performed_again() {
    let mut s = session();
    pend(&s, &[true]);
    let mut r = [0; 13];
    r[1] = u32::from(b'k');
    assert_eq!(s.start_swi(X | RELAY, r), Outcome::Waiting(Wait::Relay));
    assert_eq!(asked(&s), [(b"k?".to_vec(), true)]);
    assert!(s.kernel.os().relay_answered(b"later"));
    assert!(!s.kernel.os().relay_answered(b"again"), "nothing waits now");
    assert_eq!(s.resume(), Outcome::Budget);
    let regs = s.regs();
    assert_eq!(
        (regs.r[0], regs.r[1]),
        (5, u32::from(b'l')),
        "the reply that came"
    );
    assert_eq!(asked(&s).len(), 1, "not asked again");
}

#[test]
fn every_answer_a_swi_had_is_given_again_in_order_when_it_is_performed_again() {
    let mut s = session();
    pend(&s, &[false, true]);
    let mut r = [0; 13];
    r[1] = u32::from(b'x');
    assert_eq!(s.start_swi(X | TWICE, r), Outcome::Waiting(Wait::Relay));
    assert!(s.kernel.os().relay_answered(b"second"));
    assert_eq!(s.resume(), Outcome::Budget);
    let regs = s.regs();
    assert_eq!(
        (regs.r[0], regs.r[1]),
        (2, 6),
        "the first answer, then the reply"
    );
    assert_eq!(asked(&s).len(), 2);
}

#[test]
fn a_request_that_changes_the_other_side_and_every_relay_after_it_waits_in_place() {
    let mut s = session();
    pend(&s, &[true, true]);
    let mut r = [0; 13];
    r[1] = u32::from(b'z');
    let (regs, _) = s.user_swi(X | CHANGE_THEN_READ, r);
    assert_eq!(regs.r[0], 7, "both answered");
    assert_eq!(
        asked(&s),
        [(b"write".to_vec(), false), (b"z?".to_vec(), false)],
        "neither might park"
    );
}

#[test]
fn a_relay_outside_a_program_s_own_swi_waits_in_place_and_fonts_always_do() {
    let mut s = session();
    pend(&s, &[true]);
    let mut reply = Vec::new();
    assert_eq!(s.kernel.os().relay(b"ab", &mut reply, false), Ok(true));
    assert_eq!(reply, b"ba");
    assert!(s.kernel.os().font_relay(b"glyph", &mut reply));
    let asked = s.relays.borrow().asked.clone();
    assert_eq!(
        asked,
        [
            (Side::Files, b"ab".to_vec(), false),
            (Side::Fonts, b"glyph".to_vec(), false)
        ]
    );
}

#[test]
fn a_reply_too_large_to_keep_leaves_the_swi_to_wait_in_place() {
    let mut s = session();
    pend(&s, &[true, true]);
    let mut r = [0; 13];
    r[1] = u32::from(b'q');
    assert_eq!(s.start_swi(X | RELAY, r), Outcome::Waiting(Wait::Relay));
    assert!(s
        .kernel
        .os()
        .relay_answered(&alloc::vec![0; ANSWERS_MOST + 1]));
    assert_eq!(s.resume(), Outcome::Budget, "not parked again");
    assert_eq!(s.regs().r[0], 2, "answered in place");
    assert_eq!(asked(&s), [(b"q?".to_vec(), true), (b"q?".to_vec(), false)]);
}

#[test]
fn a_pending_relay_that_fails_fails_the_swi_performed_again_without_asking_again() {
    let mut s = session();
    pend(&s, &[true, true]);
    let mut r = [0; 13];
    r[1] = u32::from(b'f');
    assert_eq!(s.start_swi(X | RELAY, r), Outcome::Waiting(Wait::Relay));
    assert!(s.kernel.os().relay_failed());
    assert!(!s.kernel.os().relay_failed(), "nothing waits now");
    assert_eq!(s.resume(), Outcome::Budget, "not parked again");
    assert_eq!(s.regs().r[0], 0, "no reply");
    assert_eq!(asked(&s).len(), 1, "not asked again");
}

#[test]
fn a_new_swi_has_none_of_the_answers_another_had() {
    let mut s = session();
    pend(&s, &[true]);
    let mut r = [0; 13];
    r[1] = u32::from(b'm');
    assert_eq!(s.start_swi(X | RELAY, r), Outcome::Waiting(Wait::Relay));
    assert!(s.kernel.os().relay_answered(b"stale!"));
    let (regs, _) = s.user_swi(X | TWICE, r);
    assert_eq!((regs.r[0], regs.r[1]), (2, 2), "both asked afresh");
    assert_eq!(asked(&s).len(), 3);
}

#[test]
fn a_context_s_pending_relay_is_its_own() {
    let mut s = session();
    pend(&s, &[true]);
    let mut r = [0; 13];
    r[1] = u32::from(b'a');
    assert_eq!(s.start_swi(X | RELAY, r), Outcome::Waiting(Wait::Relay));
    let first = s.kernel.os().context();
    s.kernel.os().launch_context().expect("launched");
    s.application(Bits::Bit32, SLOT_BYTES);
    assert!(
        !s.kernel.os().relay_answered(b"no"),
        "this context waits on none"
    );
    r[1] = u32::from(b'b');
    let (regs, _) = s.user_swi(X | RELAY, r);
    assert_eq!(regs.r[0], 2);
    s.kernel.os().switch_context(first).expect("switched");
    assert!(s.kernel.os().relay_answered(b"yes"));
    assert_eq!(s.resume(), Outcome::Budget);
    assert_eq!(s.regs().r[1], u32::from(b'y'));
}

/// An answer longer than any request a relay makes of its own, so refusing
/// every request of its length refuses only its copy.
const LONG: [u8; 512] = [b'a'; 512];

#[test]
fn a_reply_there_is_no_room_to_keep_is_asked_again_in_place() {
    let mut s = session();
    pend(&s, &[true, true]);
    let mut r = [0; 13];
    r[1] = u32::from(b'k');
    assert_eq!(s.start_swi(X | RELAY, r), Outcome::Waiting(Wait::Relay));
    assert!(refusing_above(LONG.len() - 1, || {
        s.kernel.os().relay_answered(&LONG)
    }));
    assert_eq!(s.resume(), Outcome::Budget, "not parked again");
    assert_eq!(s.regs().r[0], 2, "answered in place");
    assert_eq!(asked(&s), [(b"k?".to_vec(), true), (b"k?".to_vec(), false)]);
}

#[test]
fn an_answer_there_is_no_room_to_give_again_fails_and_no_later_relay_parks() {
    let mut s = session();
    pend(&s, &[true, true]);
    let mut r = [0; 13];
    r[1] = u32::from(b'w');
    assert_eq!(s.start_swi(X | TWICE, r), Outcome::Waiting(Wait::Relay));
    assert!(s.kernel.os().relay_answered(&LONG));
    // Parked again, a later performance would see answered what this one
    // saw fail.
    let resumed = refusing_above(LONG.len() - 1, || s.resume());
    assert_eq!(resumed, Outcome::Budget, "not parked again");
    let regs = s.regs();
    assert_eq!(
        (regs.r[0], regs.r[1]),
        (0, 2),
        "the first failed, the second answered in place"
    );
    assert_eq!(asked(&s), [(b"w?".to_vec(), true), (b"w?".to_vec(), false)]);
}
