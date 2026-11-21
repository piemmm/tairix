//! The core from guest code: callbacks, timed calls, events, the
//! environment and `OS_Exit`, SWIs that wait for input and carry on, and
//! interrupts taken while the kernel waits.

use tairix_riscos_arm::psr;

use crate::input::InputEvent;
use crate::memory::layout::RISC_OS_5;
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
    memory.set_slot_size(slot, 0x1_0000).expect("its memory");
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
