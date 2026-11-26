//! The core from guest code: callbacks, timed calls, events, the
//! environment and `OS_Exit`, SWIs that wait for input and carry on, and
//! interrupts taken while the kernel waits.

use tairix_riscos_arm::psr;

use crate::input::InputEvent;
use crate::memory::layout::{APPLICATION, RISC_OS_5};
use crate::memory::Bits;
use crate::os::{Ended, Fail, Outcome, Regs, Wait};
use crate::swi::os::exit::ABEX;
use crate::swi::os::{ADD_CALLBACK, SET_CALLBACK};
use crate::swi::os::{
    BYTE, CALL_AFTER, CALL_EVERY, CHANGE_ENVIRONMENT, CLAIM, EXIT, GENERATE_EVENT, INT_OFF, INT_ON,
    READ_C, READ_LINE32, REMOVE_TICKER_EVENT, UPCALL,
};
use crate::swi::X;
use crate::testkit::{asm, with_application, Session, CODE, DATA, SWI_AT};
use crate::vectors;

fn session() -> Session {
    with_application(&RISC_OS_5, Bits::Bit32)
}

fn registers(low: &[u32]) -> [u32; 13] {
    let mut r = [0; 13];
    r[..low.len()].copy_from_slice(low);
    r
}

/// Code at `at` counting its calls in the word its R12 names, returning
/// through R14.
fn counter(s: &mut Session, at: u32) {
    s.poke(
        at,
        &[
            asm::ldr(11, 12, 0),
            asm::add(11, 11, 1),
            asm::str(11, 12, 0),
            asm::mov_pc_lr(),
        ],
    );
}

/// User code spinning at `CODE`, for interrupts to be taken in.
fn spin(s: &mut Session) {
    s.poke(CODE + 0x80, &[asm::b(CODE + 0x80, CODE + 0x80)]);
    s.enter(Bits::Bit32, CODE + 0x80, [0; 13]);
}

#[test]
fn a_callback_added_runs_as_the_swi_returns_to_user_mode() {
    let mut s = session();
    counter(&mut s, CODE);
    let (regs, state) = s.user_swi(X | ADD_CALLBACK, registers(&[CODE, DATA]));
    assert!(!regs.flag(psr::V));
    assert_eq!(s.peek(DATA), 1);
    assert_eq!(state.pc, SWI_AT + 4, "the caller carries on after it");
    s.user_swi(X | ADD_CALLBACK, registers(&[CODE, DATA]));
    s.user_swi(X | ADD_CALLBACK, registers(&[CODE, DATA + 4]));
    assert_eq!((s.peek(DATA), s.peek(DATA + 4)), (2, 1), "each added once");
}

#[test]
fn timed_calls_fire_from_the_interrupt_once_or_every_period_until_removed() {
    let mut s = session();
    counter(&mut s, CODE);
    let (regs, _) = s.user_swi(X | CALL_AFTER, registers(&[0, CODE, DATA]));
    assert!(regs.flag(psr::V), "no interval");
    assert_eq!(s.peek(regs.r[0]), 0x1E5);
    s.user_swi(X | CALL_AFTER, registers(&[10, CODE, DATA]));
    s.user_swi(X | CALL_EVERY, registers(&[4, CODE, DATA + 4]));
    spin(&mut s);
    s.clock.set(5);
    s.run(100);
    assert_eq!(
        (s.peek(DATA), s.peek(DATA + 4)),
        (0, 1),
        "the every-call at 5"
    );
    s.clock.set(11);
    s.run(100);
    assert_eq!(
        (s.peek(DATA), s.peek(DATA + 4)),
        (1, 2),
        "the after-call at 11"
    );
    s.clock.set(16);
    s.run(100);
    assert_eq!((s.peek(DATA), s.peek(DATA + 4)), (1, 3), "once, not again");
    s.user_swi(X | REMOVE_TICKER_EVENT, registers(&[CODE, DATA + 4]));
    spin(&mut s);
    s.clock.set(40);
    s.run(100);
    assert_eq!(s.peek(DATA + 4), 3, "removed");
}

#[test]
fn an_event_reaches_eventv_only_while_enabled_and_numbers_from_32_always() {
    let mut s = session();
    counter(&mut s, CODE);
    s.user_swi(X | CLAIM, registers(&[vectors::EVENT.0, CODE, DATA]));
    s.user_swi(X | GENERATE_EVENT, registers(&[9]));
    assert_eq!(s.peek(DATA), 0, "disabled");
    let (regs, _) = s.user_swi(X | BYTE, registers(&[14, 9]));
    assert_eq!(regs.r[1], 0, "was disabled");
    s.user_swi(X | GENERATE_EVENT, registers(&[9]));
    assert_eq!(s.peek(DATA), 1);
    s.user_swi(X | BYTE, registers(&[13, 9]));
    s.user_swi(X | GENERATE_EVENT, registers(&[9]));
    assert_eq!(s.peek(DATA), 1);
    s.user_swi(X | GENERATE_EVENT, registers(&[40]));
    assert_eq!(s.peek(DATA), 2);
}

#[test]
fn exit_with_abex_ends_with_its_code_and_a_code_past_the_limit_is_an_error() {
    let mut s = session();
    let ended = s.start_swi(EXIT, registers(&[0, ABEX, 7]));
    assert_eq!(ended, Outcome::Ended(Ended::Exit { code: 7 }));
    let mut s = session();
    let ended = s.start_swi(EXIT, registers(&[0, 0, 7]));
    assert_eq!(ended, Outcome::Ended(Ended::Exit { code: 0 }), "no ABEX");
    let mut s = session();
    match s.start_swi(EXIT, registers(&[0, ABEX, 300])) {
        Outcome::Ended(Ended::Error { number, block }) => {
            assert_eq!(number, 0x1E2);
            assert_eq!(s.error_text(block), b"Return code limit exceeded");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_changed_exit_handler_is_entered_in_user_mode_with_its_workspace() {
    let mut s = session();
    s.poke(CODE, &[asm::str(2, 12, 0), asm::b(CODE + 4, CODE + 4)]);
    let (regs, _) = s.user_swi(X | CHANGE_ENVIRONMENT, registers(&[11, CODE, DATA, 0]));
    assert!(!regs.flag(psr::V));
    assert_ne!(regs.r[1], 0, "the old handler");
    s.start_swi(EXIT, registers(&[0, ABEX, 42]));
    assert_eq!(s.peek(DATA), 42);
    let state = s.kernel.state();
    assert_eq!(psr::Mode::from_bits(state.cpsr), Some(psr::Mode::User));
    let (regs, _) = s.user_swi(X | CHANGE_ENVIRONMENT, registers(&[17]));
    assert!(regs.flag(psr::V));
    assert_eq!(s.peek(regs.r[0]), 0x1B0);
}

#[test]
fn read_c_waits_for_a_key_and_is_performed_again_when_one_comes() {
    let mut s = session();
    assert_eq!(
        s.start_swi(X | READ_C, [0; 13]),
        Outcome::Waiting(Wait::Input { until: None })
    );
    assert_eq!(s.kernel.state().pc, SWI_AT, "the SWI to be performed again");
    s.arrive(50, InputEvent::Key(b'q'));
    s.clock.set(50);
    assert_eq!(s.resume(), Outcome::Budget);
    let regs = s.regs();
    assert_eq!(regs.r[0], u32::from(b'q'));
    assert!(!regs.flag(psr::C));
}

#[test]
fn a_timed_read_keeps_its_deadline_across_waits_and_times_out() {
    let mut s = session();
    let outcome = s.start_swi(X | BYTE, registers(&[129, 100, 0]));
    assert_eq!(outcome, Outcome::Waiting(Wait::Input { until: Some(100) }));
    s.clock.set(60);
    assert_eq!(
        s.resume(),
        Outcome::Waiting(Wait::Input { until: Some(100) }),
        "the same deadline, not a new one"
    );
    s.clock.set(100);
    assert_eq!(s.resume(), Outcome::Budget);
    let regs = s.regs();
    assert_eq!((regs.r[1], regs.r[2]), (0xFF, 0xFF));
    assert!(regs.flag(psr::C));
}

#[test]
fn a_line_read_across_waits_keeps_what_it_has_and_a_fresh_one_starts_over() {
    let mut s = session();
    let line = registers(&[DATA, 20, 32, 126, 0]);
    s.arrive(10, InputEvent::Key(b'A'));
    s.arrive(10, InputEvent::Key(b'B'));
    assert!(matches!(
        s.start_swi(X | READ_LINE32, line),
        Outcome::Waiting(_)
    ));
    s.clock.set(10);
    assert!(
        matches!(s.resume(), Outcome::Waiting(_)),
        "two keys, then a wait"
    );
    s.arrive(20, InputEvent::Key(b'C'));
    s.arrive(20, InputEvent::Key(b'\r'));
    s.clock.set(20);
    assert_eq!(s.resume(), Outcome::Budget);
    assert_eq!(s.regs().r[1], 3);
    assert_eq!(s.read(DATA, 4), b"ABC\r");
    // Waiting with "A" typed, then another SWI at the same place: the line
    // read after it starts over.
    s.arrive(30, InputEvent::Key(b'A'));
    s.clock.set(30);
    assert!(matches!(
        s.start_swi(X | READ_LINE32, line),
        Outcome::Waiting(_)
    ));
    s.user_swi(X | 0x42, [0; 13]);
    s.arrive(40, InputEvent::Key(b'Z'));
    s.arrive(40, InputEvent::Key(b'\r'));
    s.clock.set(40);
    assert_eq!(s.start_swi(X | READ_LINE32, line), Outcome::Budget);
    assert_eq!(s.regs().r[1], 1);
    assert_eq!(s.read(DATA, 2), b"Z\r");
}

#[test]
fn interrupts_are_taken_while_a_claimant_s_swi_waits_for_input() {
    let mut s = session();
    counter(&mut s, CODE);
    s.user_swi(X | CALL_EVERY, registers(&[9, CODE, DATA]));
    // A ByteV claimant reading a key, which it answers with.
    let reader = CODE + 0x40;
    s.poke(
        reader,
        &[
            asm::push(1 << 14),
            asm::swi(X | READ_C),
            asm::pull(1 << 14),
            asm::claim(),
        ],
    );
    s.user_swi(X | CLAIM, registers(&[vectors::BYTE.0, reader, 0]));
    s.arrive(50, InputEvent::Key(b'k'));
    let (regs, _) = s.user_swi(X | BYTE, registers(&[0x80]));
    assert_eq!(regs.r[0], u32::from(b'k'));
    assert_eq!(
        s.peek(DATA),
        5,
        "the timed call ran at 10, 20, 30, 40 and 50"
    );
}

#[test]
fn an_exception_s_error_swallowed_by_errorv_leaves_later_swis_outermost() {
    let mut s = session();
    // An ErrorV claimant claiming every error.
    s.poke(CODE, &[asm::claim()]);
    s.user_swi(X | CLAIM, registers(&[1, CODE, 0]));
    // An undefined instruction in User mode; its error is swallowed and the
    // program carries on after it.
    s.poke(DATA, &[0xE7F0_00F0, asm::b(DATA + 4, DATA + 4)]);
    s.enter(Bits::Bit32, DATA, [0; 13]);
    s.resume();
    // A SWI that waits must still be able to unwind to be performed again.
    assert_eq!(
        s.start_swi(X | READ_C, [0; 13]),
        Outcome::Waiting(Wait::Input { until: None })
    );
}

#[test]
fn guest_code_looping_on_a_swi_inside_a_call_still_stops_when_the_host_says() {
    let mut s = session();
    // A ByteV claimant that never returns, a SWI every other instruction.
    s.poke(CODE, &[asm::swi(X | 0x42), asm::b(CODE + 4, CODE)]);
    s.user_swi(X | CLAIM, registers(&[vectors::BYTE.0, CODE, 0]));
    s.stop_after.set(Some(2));
    assert_eq!(s.start_swi(X | BYTE, registers(&[0x80])), Outcome::Stopped);
}

#[test]
fn a_rem_v_claimant_supplying_keys_for_ever_does_not_keep_the_host_from_stopping() {
    let mut s = session();
    // RemV answering 'x' every time, C clear: a line that never ends.
    s.poke(
        CODE,
        &[
            asm::mov(0, 0x78),
            asm::mov(1, 0x100),
            asm::cmp(0, 1),
            asm::mov(2, 0x78),
            asm::claim(),
        ],
    );
    s.user_swi(X | CLAIM, registers(&[0x15, CODE, 0]));
    s.stop_after.set(Some(2));
    let line = registers(&[DATA, 10, 32, 126, 0]);
    assert_eq!(s.start_swi(X | READ_LINE32, line), Outcome::Stopped);
}

#[test]
fn a_line_is_read_through_rdchv_when_it_is_claimed() {
    let mut s = session();
    // RdchV answering CR, C clear, counting its calls.
    s.poke(
        CODE,
        &[
            asm::ldr(11, 12, 0),
            asm::add(11, 11, 1),
            asm::str(11, 12, 0),
            asm::mov(0, 13),
            asm::mov(1, 0x100),
            asm::cmp(0, 1),
            asm::claim(),
        ],
    );
    s.user_swi(X | CLAIM, registers(&[4, CODE, DATA + 0x100]));
    let (regs, _) = s.user_swi(X | READ_LINE32, registers(&[DATA, 10, 32, 126, 0]));
    assert!(!regs.flag(psr::C));
    assert_eq!(regs.r[1], 0, "the claimant's CR ended it at once");
    assert_eq!(s.peek(DATA + 0x100), 1, "asked once");
}

#[test]
fn a_26_bit_caller_s_read_line_takes_its_flags_from_r0_and_returns_the_address() {
    let mut s = with_application(&crate::memory::layout::SHARED, Bits::Bit26);
    s.capture_output();
    s.arrive(0, InputEvent::Key(b'p'));
    s.arrive(0, InputEvent::Key(b'\r'));
    // Bit 30: echo R4's character in place of each one stored.
    let r0 = DATA | (1 << 30);
    let (regs, _) = s.user_swi(X | 0x0E, registers(&[r0, 10, 32, 126, u32::from(b'*')]));
    assert_eq!(
        (regs.r[0], regs.r[1], regs.r[4]),
        (DATA, 1, u32::from(b'*'))
    );
    assert_eq!(s.read(DATA, 2), b"p\r");
    assert_eq!(s.output(), b"*\n\r");
}

#[test]
fn a_32_bit_caller_s_read_line_ignores_r0_s_flags_as_risc_os_5_does() {
    let mut s = with_application(&crate::memory::layout::RISC_OS_5, Bits::Bit32);
    s.capture_output();
    s.arrive(0, InputEvent::Key(b'p'));
    s.arrive(0, InputEvent::Key(b'\r'));
    let r0 = DATA | (1 << 30);
    let (regs, _) = s.user_swi(X | 0x0E, registers(&[r0, 10, 32, 126, u32::from(b'*')]));
    assert_eq!(
        (regs.r[0], regs.r[1]),
        (DATA, 1),
        "the address, less the flags"
    );
    assert_eq!(s.read(DATA, 2), b"p\r");
    assert_eq!(s.output(), b"p\n\r", "its own character echoed");
}

#[test]
fn a_dynamic_area_change_from_an_interrupt_is_refused() {
    let mut s = session();
    // A timed call that tries to move a page into the free pool, keeping
    // the error it gets at DATA.
    s.poke(
        CODE,
        &[
            asm::push(1 << 14),
            asm::mov(0, 6),
            asm::mov(1, 0x1000),
            asm::swi(X | 0x2A),
            asm::str(0, 12, 0),
            asm::pull(1 << 14),
            asm::mov_pc_lr(),
        ],
    );
    s.user_swi(X | CALL_AFTER, registers(&[1, CODE, DATA]));
    spin(&mut s);
    s.clock.set(5);
    s.resume();
    let error = s.peek(DATA);
    assert_eq!(s.peek(error), 0x1C1, "refused from the interrupt");
}

#[test]
fn a_transient_callback_runs_while_read_c_waits_for_a_key() {
    let mut s = session();
    counter(&mut s, CODE);
    let ticker = CODE + 0x40;
    s.poke(
        ticker,
        &[
            asm::push(0b0100_0000_0000_0011),
            asm::mov(0, CODE),
            asm::mov(1, DATA),
            asm::swi(X | ADD_CALLBACK),
            asm::pull(0b1000_0000_0000_0011),
        ],
    );
    s.user_swi(X | CALL_AFTER, registers(&[5, ticker, 0]));
    assert!(matches!(
        s.start_swi(X | READ_C, [0; 13]),
        Outcome::Waiting(_)
    ));
    assert_eq!(s.peek(DATA), 0);
    s.stop_without_input.set(true);
    s.clock.set(10);
    s.resume();
    assert_eq!(s.peek(DATA), 1, "called back with no key come");
}

#[test]
fn an_upcall_keeps_its_callers_interrupt_state_and_flags() {
    let mut s = session();
    s.poke(
        CODE,
        &[
            asm::mrs(1),
            asm::str(1, 12, 0),
            asm::set_flags(0),
            asm::mov_pc_lr(),
        ],
    );
    s.user_swi(X | CLAIM, registers(&[vectors::UPCALL.0, CODE, DATA]));
    let at = CODE + 0x40;
    s.poke(
        at,
        &[
            asm::swi(X | INT_OFF),
            asm::set_flags(psr::N | psr::Z | psr::C),
            asm::mov(0, 0x7F),
            asm::swi(X | UPCALL),
            asm::b(at + 16, at + 16),
        ],
    );
    s.enter(Bits::Bit32, at, [0; 13]);
    s.run(100);
    assert_ne!(
        s.peek(DATA) & psr::I,
        0,
        "UpCallV saw the caller's interrupts off"
    );
    assert_eq!(s.peek(DATA) & psr::F, 0, "and fast interrupts enabled");
    let regs = s.regs();
    assert!(!regs.flag(psr::V));
    assert!(
        regs.flag(psr::N) && regs.flag(psr::Z) && regs.flag(psr::C),
        "the caller's flags came back"
    );
}

#[test]
fn a_26_bit_callback_handler_finds_r15_with_its_psr_in_its_buffer() {
    let mut s = with_application(&crate::memory::layout::SHARED, Bits::Bit26);
    let handler = CODE + 0x40;
    s.poke(handler, &[asm::b(handler, handler)]);
    let buffer = DATA + 0x100;
    s.poke(buffer + 64, &[0xEEEE_EEEE]);
    s.user_swi(X | CHANGE_ENVIRONMENT, registers(&[7, handler, 0, buffer]));
    let at = CODE + 0x80;
    s.poke(
        at,
        &[
            asm::set_flags(psr::N),
            asm::swi(X | SET_CALLBACK),
            asm::swi(X | INT_ON),
            asm::b(at + 12, at + 12),
        ],
    );
    s.enter(Bits::Bit26, at, [0; 13]);
    s.run(100);
    assert_eq!(s.kernel.state().pc, handler, "the handler entered");
    let r15 = s.peek(buffer + 60);
    assert_eq!(r15 & psr::R15_PC, at + 12, "where the caller resumes");
    assert_eq!(
        r15 & !psr::R15_PC,
        psr::N,
        "User mode, interrupts enabled, N set"
    );
    assert_eq!(s.peek(buffer + 64), 0xEEEE_EEEE, "sixteen words, no more");
}

#[test]
fn the_unused_swi_handler_is_given_the_number_with_bit_17_clear() {
    let mut s = session();
    s.poke(CODE, &[asm::str(11, 12, 0), asm::mov_pc_lr()]);
    s.user_swi(X | CHANGE_ENVIRONMENT, registers(&[12, CODE, DATA, 0]));
    let (regs, _) = s.user_swi(X | 0x5_FFC1, [0; 13]);
    assert!(!regs.flag(psr::V));
    assert_eq!(s.peek(DATA), 0x5_FFC1);
}

#[test]
fn tickerv_is_called_once_a_centisecond_and_a_late_interrupt_makes_up_a_second_at_most() {
    let mut s = session();
    counter(&mut s, CODE);
    s.user_swi(X | CLAIM, registers(&[vectors::TICKER.0, CODE, DATA]));
    spin(&mut s);
    s.clock.set(5);
    s.run(100);
    assert_eq!(s.peek(DATA), 5);
    s.clock.set(1005);
    s.run(100);
    assert_eq!(s.peek(DATA), 105, "a second made up, the rest dropped");
}

#[test]
fn the_application_space_handler_holds_the_top_of_application_space() {
    let mut s = session();
    let (limit, _) = s.user_swi(X | CHANGE_ENVIRONMENT, registers(&[0]));
    let (space, _) = s.user_swi(X | CHANGE_ENVIRONMENT, registers(&[14]));
    assert_ne!(space.r[1], 0);
    assert_eq!(space.r[1], limit.r[1], "no less than the memory limit");
}

#[test]
fn a_26_bit_break_point_dumps_sixteen_words_the_psr_in_r15() {
    let mut s = with_application(&crate::memory::layout::SHARED, Bits::Bit26);
    let handler = CODE + 0x40;
    s.poke(handler, &[asm::b(handler, handler)]);
    let buffer = DATA + 0x100;
    s.poke(buffer + 64, &[0xEEEE_EEEE]);
    s.user_swi(X | CHANGE_ENVIRONMENT, registers(&[8, handler, 0, buffer]));
    let at = CODE + 0x80;
    s.poke(
        at,
        &[
            asm::mov(13, 0x1300),
            asm::set_flags(psr::C),
            asm::swi(X | 0x17),
        ],
    );
    s.enter(Bits::Bit26, at, [0; 13]);
    s.run(100);
    assert_eq!(s.kernel.state().pc, handler, "the handler entered");
    assert_eq!(s.peek(buffer + 52), 0x1300, "the User R13");
    let r15 = s.peek(buffer + 60);
    assert_eq!(r15 & psr::R15_PC, at + 8, "the break point's own address");
    assert_eq!(r15 & !psr::R15_PC, psr::C, "User mode, C set");
    assert_eq!(s.peek(buffer + 64), 0xEEEE_EEEE, "sixteen words, no more");
}

#[test]
fn an_error_handler_s_buffer_is_written_no_further_than_256_bytes() {
    let mut s = session();
    let handler = CODE + 0x40;
    s.poke(handler, &[asm::b(handler, handler)]);
    let buffer = DATA + 0x200;
    s.poke(buffer + 256, &[0xEEEE_EEEE]);
    s.user_swi(X | CHANGE_ENVIRONMENT, registers(&[6, handler, 0, buffer]));
    let block = DATA + 0x800;
    s.poke(block, &[0x1234]);
    let mut text = alloc::vec![b'e'; 252];
    text.push(0);
    s.write(block + 4, &text);
    s.start_swi(0x2B, registers(&[block]));
    assert_eq!(s.kernel.state().pc, handler, "the handler entered");
    assert_eq!(s.peek(buffer + 4), 0x1234);
    assert_eq!(s.peek(buffer + 256), 0xEEEE_EEEE, "nothing past the buffer");
    assert_eq!(s.read(buffer + 8 + 247, 1), [0], "the text cut to fit");
}

/// A built-in whose routines answer in R0 whether their caller may read the
/// word at R0, and write it, reaching it as they reach what a caller hands
/// them; in the inventory's row for Draw, which another crate provides.
static PEEKS: crate::module::Builtin = crate::module::Builtin {
    title: "Draw",
    help_name: None,
    help_suffix: None,
    bridge: false,
    swis: None,
    services: None,
    commands: &[],
    init: None,
    finalise: None,
    start: None,
    routines: &[peeks, writes],
    vectors: &[],
    resources: &[],
    code: None,
};

static PEEKING: [&crate::module::Builtin; 1] = [&PEEKS];

#[expect(clippy::unnecessary_wraps, reason = "a module routine's signature")]
fn peeks(os: &mut super::Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] = u32::from(os.as_caller().peek_word(regs.r[0]).is_some());
    Ok(())
}

fn writes(os: &mut super::Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.as_caller().write_word(regs.r[0], 0)?;
    regs.r[0] = 1;
    Ok(())
}

#[test]
fn a_routine_reads_what_its_caller_hands_it_with_its_caller_s_rights() {
    let mut s = crate::testkit::boot_with(&RISC_OS_5, tairix_tz::Zone::UTC, 0, &PEEKING);
    s.application(Bits::Bit32, crate::testkit::SLOT_BYTES);
    let mut os = s.kernel.os();
    let stack_word = os.supervisor_stack() - 4;
    let entry = os.routine_of(&PEEKS, 0).expect("active");
    assert_eq!(os.as_caller().peek_word(DATA + 1), None, "out of alignment");
    let back = CODE + 12;
    for privileged in [false, true] {
        let enter = if privileged {
            asm::swi(crate::swi::os::ENTER_OS)
        } else {
            asm::mov_reg(0, 0)
        };
        s.poke(
            CODE,
            &[
                enter,
                asm::adr(14, CODE + 4, back),
                asm::mov_reg(15, 1),
                asm::b(back, back),
            ],
        );
        for (address, answer) in [(DATA, 1), (stack_word, u32::from(privileged))] {
            s.enter(Bits::Bit32, CODE, registers(&[address, entry]));
            s.run(100);
            assert_eq!(
                s.kernel.os().current_registers()[0],
                answer,
                "{address:#x}, privileged: {privileged}"
            );
        }
    }
    let writer = s.kernel.os().routine_of(&PEEKS, 1).expect("active");
    let zero = crate::memory::layout::HIGH_ZERO_PAGE + 0x100;
    s.poke(
        CODE,
        &[
            asm::mov_reg(0, 0),
            asm::adr(14, CODE + 4, back),
            asm::mov_reg(15, 1),
            asm::b(back, back),
        ],
    );
    s.enter(Bits::Bit32, CODE, registers(&[zero, writer]));
    s.run(100);
    let abort = s.kernel.os().world.last_abort;
    assert_eq!(
        (abort.address, abort.pc),
        (zero, writer),
        "aborted at the user-mode caller's write, at the routine"
    );
}

#[test]
fn an_fpa_register_round_trips_in_extended_form_as_stfe_and_ldfe_move_it() {
    let mut s = session();
    let mut os = s.kernel.os();
    let one = [0x3FFF, 0x8000_0000, 0];
    os.set_fpa_extended(4, one).expect("loaded");
    assert_eq!(os.fpa_extended(4), Ok(one));
    assert_eq!(os.fpa_double(4), Ok(1.0_f64.to_bits()), "as a double, 1");
    os.set_fpa_double(5, (-0.5_f64).to_bits());
    assert_eq!(
        os.fpa_extended(5),
        Ok([0x8000_3FFE, 0x8000_0000, 0]),
        "the sign in the top bit"
    );
}

#[test]
fn position_finds_the_first_byte_that_ends_within_reach_page_by_page() {
    let mut s = session();
    let page = tairix_riscos_arm::mmu::PAGE_SIZE;
    let start = DATA + page - 3;
    s.write(start, b"abc,de");
    let mut os = s.kernel.os();
    assert_eq!(
        os.position(start, 16, |b| b == b','),
        Ok(Some(3)),
        "across a page"
    );
    assert_eq!(
        os.position(start, 3, |b| b == b','),
        Ok(None),
        "not within reach"
    );
    assert_eq!(os.position(start, 0, |_| true), Ok(None));
}

#[test]
fn an_error_handler_is_entered_with_the_failing_swi_s_registers_but_r0() {
    let mut s = session();
    let handler = CODE + 0x40;
    s.poke(handler, &[asm::b(handler, handler)]);
    s.user_swi(
        X | CHANGE_ENVIRONMENT,
        registers(&[6, handler, 0x1212_1212, DATA + 0x200]),
    );
    s.poke(DATA + 0x800, &[0x1234]);
    s.write(DATA + 0x804, b"e\0");
    let mut given = [0; 13];
    for (n, slot) in (0..).zip(given.iter_mut()) {
        *slot = 0x100 + n;
    }
    given[0] = DATA + 0x800;
    s.start_swi(0x2B, given);
    assert_eq!(s.kernel.state().pc, handler, "the handler entered");
    let regs = s.regs();
    assert_eq!(regs.r[0], 0x1212_1212, "its workspace");
    assert_eq!(regs.r[1..], given[1..], "the rest as at the SWI");
}

#[test]
fn an_error_reaching_the_error_handler_ends_redirection_and_closes_its_file() {
    let mut s = session();
    let file = s.one_file(b"");
    let handler = CODE + 0x40;
    s.poke(handler, &[asm::b(handler, handler)]);
    s.user_swi(
        X | CHANGE_ENVIRONMENT,
        registers(&[6, handler, 0, DATA + 0x200]),
    );
    s.write(DATA + 0x400, b"out\r");
    let (opened, _) = s.user_swi(X | 0x0D, registers(&[0x80, DATA + 0x400]));
    let handle = opened.r[0];
    s.user_swi(X | 0x5E, registers(&[handle, handle]));
    s.poke(DATA + 0x800, &[0x99]);
    s.write(DATA + 0x804, b"m\0");
    s.start_swi(0x2B, registers(&[DATA + 0x800]));
    assert_eq!(s.kernel.state().pc, handler, "the handler entered");
    let (now, _) = s.user_swi(X | 0x5E, registers(&[u32::MAX, u32::MAX]));
    assert_eq!(now.r[..2], [0, 0], "neither direction redirected");
    assert_eq!(
        file.word(&mut s, crate::testkit::OneFile::HANDLE),
        0,
        "the file closed"
    );
}

#[test]
fn guest_code_has_its_stacks_before_any_application_starts() {
    let mut s = crate::testkit::boot(&RISC_OS_5);
    let memory = s.kernel.memory();
    let slot = memory.create_slot(Bits::Bit32).expect("a slot");
    memory
        .set_slot_size(slot, crate::testkit::SLOT_BYTES)
        .expect("its memory");
    memory.switch_slot(slot).expect("made current");
    // A claimant that keeps R14 on the stack round an erring SWI, then claims.
    s.poke(
        CODE,
        &[
            0xE92D_4000,
            asm::swi(X | 0x4_307F),
            0xE8BD_4000,
            0xE8BD_8000,
        ],
    );
    let mut claim = Regs::new([0x21, CODE, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    s.kernel
        .os()
        .perform(X | 0x1F, &mut claim)
        .expect("claimed");
    let mut regs = Regs::new([0, 0, 0, 0, 0, 0, 0, 0, 0, 0x21, 0, 0, 0]);
    let called = s.kernel.os().perform(X | 0x34, &mut regs);
    assert!(
        matches!(called, Err(Fail::Error(_))),
        "the SWI's error returned, not a fault: {called:?}"
    );
}

#[test]
fn a_copy_between_overlapping_ranges_moves_the_bytes_whole_either_way() {
    let mut s = session();
    let length: u32 = 10_000;
    let pattern: alloc::vec::Vec<u8> = (0..length).map(|i| (i % 251) as u8).collect();
    for (shift, up) in [(100, true), (5000, true), (100, false), (5000, false)] {
        let (from, to) = if up {
            (DATA, DATA + shift)
        } else {
            (DATA + shift, DATA)
        };
        s.write(from, &pattern);
        s.kernel.os().copy_bytes(from, to, length).expect("copied");
        assert_eq!(s.read(to, length as usize), pattern, "{shift} {up}");
    }
}

#[test]
fn a_read_into_host_memory_aborts_before_the_host_grants_room_no_guest_memory_backs() {
    let mut s = session();
    s.write(DATA, b"abcdef");
    let mut os = s.kernel.os();
    assert_eq!(os.read_vec(DATA, 6).expect("read"), b"abcdef");
    let mut grown = b"xy".to_vec();
    os.read_onto(&mut grown, DATA + 2, 3).expect("appended");
    assert_eq!(grown, b"xycde");
    let end = APPLICATION + crate::testkit::SLOT_BYTES;
    let refused = os.read_vec(end - 4, usize::MAX);
    assert!(
        matches!(refused, Err(Fail::Unwind(_))),
        "the abort, not a refused grant: {refused:?}"
    );
    assert_eq!(os.world.last_abort.address, end, "at the first page past");
    let mut kept = b"z".to_vec();
    assert!(os.read_onto(&mut kept, end - 1, 2).is_err());
    assert_eq!(kept, b"z", "nothing appended by a read that aborts");
}

#[test]
fn a_range_is_writable_only_where_every_page_takes_a_privileged_write() {
    let mut s = session();
    let mut os = s.kernel.os();
    assert!(os.writable(DATA, u64::from(DATA) + 0x2000));
    let host = RISC_OS_5.host.start;
    assert!(
        !os.writable(host, u64::from(host) + 4),
        "the host pages are read-only"
    );
    assert!(
        !os.writable(host - 4, u64::from(host) + 4),
        "a range running onto them"
    );
    assert!(
        !os.writable(0xFFFF_FFF0, 0x1_0000_0010),
        "past the top of memory"
    );
}

#[test]
fn bytes_cross_a_page_at_every_alignment_and_leave_their_neighbours_alone() {
    let mut s = session();
    let boundary = DATA + tairix_riscos_arm::mmu::PAGE_SIZE;
    for start in boundary - 9..boundary + 2 {
        for length in 0..14 {
            s.write(boundary - 16, &[0xEE; 40]);
            let bytes: alloc::vec::Vec<u8> = (0..length).map(|n| 0x40 + n).collect();
            let mut os = s.kernel.os();
            os.write_bytes(start, &bytes).expect("written");
            let mut back = alloc::vec![0; usize::from(length)];
            os.read_bytes(start, &mut back).expect("read");
            assert_eq!(back, bytes, "{start:#x} {length}");
            let around = s.read(boundary - 16, 40);
            let first = (start - (boundary - 16)) as usize;
            for (n, &value) in around.iter().enumerate() {
                if !(first..first + usize::from(length)).contains(&n) {
                    assert_eq!(value, 0xEE, "{start:#x} {length}: byte {n} kept");
                }
            }
        }
    }
}

#[test]
fn a_routine_acting_for_a_user_mode_caller_reaches_memory_with_its_rights() {
    let mut s = session();
    let zero = crate::memory::layout::HIGH_ZERO_PAGE + 0x100;
    let mut os = s.kernel.os();
    let stack = os.memory.layout().stacks.supervisor.start;
    os.world.user_caller = true;
    assert!(
        os.as_caller().read_word(zero).is_ok(),
        "user mode reads zero page"
    );
    assert!(
        os.as_caller().write_word(zero, 1).is_err(),
        "but may not write it"
    );
    assert_eq!(os.world.last_abort.address, zero, "aborting there");
    assert!(
        os.as_caller().read_word(stack).is_err(),
        "nor reach a stack"
    );
    assert_eq!(os.as_caller().peek_word(stack), None);
    assert!(
        os.as_caller().write_word(DATA, 7).is_ok(),
        "its own memory it writes"
    );
    assert!(
        os.write_word(zero, 1).is_ok(),
        "the kernel's own reach is privileged"
    );
    os.world.user_caller = false;
    assert!(
        os.as_caller().write_word(zero, 0).is_ok(),
        "as a privileged caller's is"
    );
}

#[test]
fn words_cross_a_page_at_every_alignment_and_stop_at_the_first_page_refused() {
    let mut s = session();
    let boundary = DATA + tairix_riscos_arm::mmu::PAGE_SIZE;
    let words = [
        0x0302_0100,
        0x0706_0504,
        0x0B0A_0908,
        0x0F0E_0D0C,
        0x1312_1110,
        0x1716_1514,
    ];
    for start in boundary - 12..boundary + 4 {
        s.write(boundary - 24, &[0xEE; 56]);
        let mut os = s.kernel.os();
        os.write_words(start, &words).expect("written");
        let mut back = [0; 6];
        os.read_words(start, &mut back).expect("read");
        assert_eq!(back, words, "{start:#x}");
        let first = usize::try_from(start - (boundary - 24)).expect("near");
        let laid: alloc::vec::Vec<u8> = (0..56)
            .map(|n| {
                if (first..first + 24).contains(&n) {
                    u8::try_from(n - first).expect("a byte")
                } else {
                    0xEE
                }
            })
            .collect();
        assert_eq!(
            s.read(boundary - 24, 56),
            laid,
            "{start:#x}: little-endian, alone"
        );
    }
    let end = APPLICATION + crate::testkit::SLOT_BYTES;
    let mut os = s.kernel.os();
    assert!(os.write_words(end - 8, &words).is_err());
    assert_eq!(os.world.last_abort.address, end, "at the first page past");
    let mut back = [0; 2];
    os.read_words(end - 8, &mut back)
        .expect("the words before it");
    assert_eq!(back, [words[0], words[1]], "written up to the page refused");
    assert!(os.read_words(end - 8, &mut [0; 3]).is_err());
    assert_eq!(os.world.last_abort.address, end);
}

#[test]
fn a_string_is_read_across_pages_to_its_end_or_the_byte_after_the_most() {
    let mut s = session();
    let boundary = DATA + tairix_riscos_arm::mmu::PAGE_SIZE;
    s.write(boundary - 3, b"abcdef\0");
    let mut os = s.kernel.os();
    let zero = |b: u8| b == 0;
    assert_eq!(
        os.read_until(boundary - 3, 100, zero).expect("read"),
        (b"abcdef".to_vec(), true)
    );
    assert_eq!(
        os.read_until(boundary - 3, 6, zero).expect("read"),
        (b"abcdef".to_vec(), true),
        "the terminator just past the most still ends it"
    );
    assert_eq!(
        os.read_until(boundary - 3, 4, zero).expect("read"),
        (b"abcd".to_vec(), false)
    );
    assert_eq!(
        os.read_until(boundary - 3, 0, zero).expect("read"),
        (alloc::vec::Vec::new(), false)
    );
    let end = APPLICATION + crate::testkit::SLOT_BYTES;
    s.write(end - 2, b"xy");
    let mut os = s.kernel.os();
    assert!(os.read_until(end - 2, 100, zero).is_err());
    assert_eq!(os.world.last_abort.address, end, "at the first page past");
}

/// A built-in whose one routine calls itself through its entry, each call
/// counted in R0; in the inventory's row for Draw, which another crate
/// provides.
static NESTS: crate::module::Builtin = crate::module::Builtin {
    title: "Draw",
    help_name: None,
    help_suffix: None,
    bridge: false,
    swis: None,
    services: None,
    commands: &[],
    init: None,
    finalise: None,
    start: None,
    routines: &[nests],
    vectors: &[],
    resources: &[],
    code: None,
};

static NESTING: [&crate::module::Builtin; 1] = [&NESTS];

fn nests(os: &mut super::Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] += 1;
    let entry = os.routine_of(&NESTS, 0).expect("active");
    os.call_routine(entry, Bits::Bit32, super::Entry::Supervisor, regs)
}

#[test]
fn a_routine_the_host_calls_into_itself_meets_the_nesting_bound() {
    let mut s = crate::testkit::boot_with(&RISC_OS_5, tairix_tz::Zone::UTC, 0, &NESTING);
    s.application(Bits::Bit32, crate::testkit::SLOT_BYTES);
    let mut os = s.kernel.os();
    let entry = os.routine_of(&NESTS, 0).expect("active");
    let mut regs = Regs::default();
    let called = os.call_routine(entry, Bits::Bit32, super::Entry::Supervisor, &mut regs);
    assert!(matches!(called, Err(Fail::Unwind(_))), "{called:?}");
    assert_eq!(
        regs.r[0],
        super::call::MAX_DEPTH,
        "each call within the bound made"
    );
    assert_eq!(os.world.routines_nested, 0, "every call it made over");
}
