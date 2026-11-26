use alloc::vec::Vec;

use tairix_riscos_kernel::module::Code;

use super::*;
use crate::c::testing::{Client, CODE, FREE, RETURN, STATICS};

const LOG: u32 = FREE + 0x100;
const SET: u32 = FREE + 0x200;
const CALLER_FP: u32 = 0x0F0F;
const SP: u32 = FREE + 0x8000;
/// A handler logging R0, FP, SP and LR, then returning: `LDR R12, log;
/// STMIA R12, {R0, R11, R13, R14}; MOV PC, R14`.
const HANDLER: [u32; 5] = [0xE59F_C008, 0xE88C_6801, 0xE1A0_F00E, 0, LOG];

fn client() -> Client {
    let mut client = Client::new();
    start(&mut client.session.kernel.os(), Statics::of_base(STATICS)).expect("started");
    let words: Vec<u8> = HANDLER.iter().flat_map(|w| w.to_le_bytes()).collect();
    client.put(CODE, &words);
    client
}

fn handler_of(client: &mut Client, sig: u32) -> u32 {
    client.word(STATICS + at::SIGNALS + 4 * (sig - 1))
}

fn set_handler(client: &mut Client, sig: u32, handler: u32) {
    client.call(signal, &[sig, handler]);
}

/// The client's code calling `entry` with `low` from R0 on, its FP
/// [`CALLER_FP`] and SP [`SP`]: R0 to R14 after, and where it went on. A
/// call that ends the program leaves them as they stood.
fn called(client: &mut Client, entry: Code, low: &[u32]) -> ([u32; 15], u32) {
    let mut registers = [0; 15];
    registers[..low.len()].copy_from_slice(low);
    registers[10] = crate::stack_chunk::limit_of(crate::c::testing::CHUNK);
    registers[11] = CALLER_FP;
    registers[13] = SP;
    registers[14] = RETURN;
    client.running_with(&registers);
    let (_, after, pc) = client.subroutine(entry);
    (after, pc)
}

fn routine(client: &mut Client, routine: Routine) -> u32 {
    context::routine(&mut client.session.kernel.os(), routine).expect("a routine")
}

/// The guest run until it spins at [`RETURN`] or its budget ends: R0 to
/// R14 then, and where it is.
fn run(client: &mut Client) -> ([u32; 15], u32) {
    let _ = client.session.kernel.run(10_000);
    let registers = client.session.kernel.os().current_registers();
    (registers, client.session.kernel.state().pc)
}

#[test]
fn signal_answers_the_handler_it_replaces_and_sig_err_for_a_signal_there_is_not() {
    let mut client = client();
    assert_eq!(client.answer(signal, &[4, CODE]), SIG_DFL);
    assert_eq!(client.answer(signal, &[4, SIG_IGN]), CODE);
    assert_eq!(client.answer(signal, &[4, 0x1234]), SIG_IGN);
    assert_eq!(
        client.answer(signal, &[4, SIG_DFL]),
        0x1234,
        "any value kept"
    );
    for sig in [0, 11, u32::MAX] {
        assert_eq!(client.answer(signal, &[sig, CODE]), SIG_ERR, "signal {sig}");
    }
    assert_eq!(client.word(STATICS + at::ERRNO), 0, "errno untouched");
}

#[test]
fn raise_enters_the_handler_below_its_frame_record_and_answers_nought_on_its_return() {
    let mut client = client();
    set_handler(&mut client, SIGINT, CODE);
    let (entered, pc) = called(&mut client, raise, &[SIGINT]);
    let returned = routine(&mut client, Routine::RaiseReturned);
    assert_eq!(pc, CODE, "the handler");
    assert_eq!(
        [entered[0], entered[11], entered[13], entered[14]],
        [SIGINT, SP - 4, SP - 16, returned]
    );
    let marker = crate::kernel::template(&mut client.session.kernel.os()).expect("template")
        + chunk::RECORD_PC;
    let record: Vec<u32> = (0..4).map(|n| client.word(SP - 16 + 4 * n)).collect();
    assert_eq!(record, [CALLER_FP, SP, RETURN, marker]);
    assert_eq!(
        handler_of(&mut client, SIGINT),
        SIG_DFL,
        "reset before the handler runs"
    );
    let (after, pc) = run(&mut client);
    assert_eq!(pc, RETURN);
    assert_eq!(
        [after[0], after[11], after[13]],
        [0, CALLER_FP, SP],
        "raise's caller resumed"
    );
    let logged: Vec<u32> = (0..4).map(|n| client.word(LOG + 4 * n)).collect();
    assert_eq!(logged, [SIGINT, SP - 4, SP - 16, returned]);
}

#[test]
fn an_ignored_signal_returns_at_once_and_stays_ignored() {
    let mut client = client();
    set_handler(&mut client, SIGTERM, SIG_IGN);
    let (after, pc) = called(&mut client, raise, &[SIGTERM]);
    assert_eq!((after[0], pc), (0, RETURN));
    assert_eq!(handler_of(&mut client, SIGTERM), SIG_IGN);
}

#[test]
fn raise_refuses_a_signal_there_is_not_with_esignum() {
    let mut client = client();
    for sig in [0, 11] {
        assert_eq!(client.answer(raise, &[sig]), ESIGNUM);
        assert_eq!(client.word(STATICS + at::ERRNO), ESIGNUM);
    }
}

#[test]
fn the_trap_procedure_declines_a_default_signal_and_raises_one_with_a_handler() {
    let mut client = client();
    let code = 0x8000_0002;
    let (after, pc) = called(&mut client, trap_handler, &[code, SET]);
    assert_eq!(
        (after[0], pc),
        (DECLINED, RETURN),
        "SIGSEGV's default: left to the uncaught procedure"
    );
    set_handler(&mut client, SIGSEGV, CODE);
    let (entered, pc) = called(&mut client, trap_handler, &[code, SET]);
    assert_eq!((entered[0], pc), (SIGSEGV, CODE));
    assert_eq!(
        entered[14],
        routine(&mut client, Routine::TrapSignalReturned)
    );
    assert_eq!(
        [client.word(SP - 24), client.word(SP - 20)],
        [code, SET],
        "the trap kept below the record"
    );
    run(&mut client);
    assert_eq!(
        client.word(STATICS + kernel_at::RETURN_CODE),
        1,
        "the handler returned: the program ends"
    );
}

#[test]
fn the_event_procedures_raise_sigint_for_an_escape_alone() {
    let mut client = client();
    let (after, _) = called(&mut client, event_handler, &[ESCAPE, SET]);
    assert_eq!(after[0], DECLINED, "the default");
    set_handler(&mut client, SIGINT, CODE);
    let (after, _) = called(&mut client, event_handler, &[9, SET]);
    assert_eq!(after[0], DECLINED, "not an escape");
    let (entered, pc) = called(&mut client, event_handler, &[ESCAPE, SET]);
    assert_eq!((entered[0], pc), (SIGINT, CODE));
    let (after, pc) = run(&mut client);
    assert_eq!(
        (after[0], pc),
        (HANDLED, RETURN),
        "handled once the handler returns"
    );
    let (after, _) = called(&mut client, unhandled_event_handler, &[9, SET]);
    assert_eq!(after[0], DECLINED);
    called(&mut client, unhandled_event_handler, &[ESCAPE, SET]);
    assert_eq!(
        client.word(STATICS + kernel_at::RETURN_CODE),
        1,
        "the default action ends it"
    );
}

#[test]
fn abort_ends_the_program_even_where_its_handler_returns() {
    let mut client = client();
    set_handler(&mut client, SIGABRT, CODE);
    let (_, pc) = called(&mut client, abort, &[]);
    assert_eq!(pc, CODE);
    run(&mut client);
    assert_eq!(client.word(STATICS + kernel_at::RETURN_CODE), 1);
}

#[test]
fn a_failed_assertion_aborts_whatever_its_texts() {
    let long = FREE + 0x1000;
    let mut client = client();
    client.put(long, &[&[b'x'; 5000][..], b"\0"].concat());
    client.put(FREE + 0x40, b"file.c\0");
    for (entry, arguments) in [
        (assert as Code, &[long, FREE + 0x40, 7][..]),
        (assert2, &[long, long, FREE + 0x40, 7]),
    ] {
        set_handler(&mut client, SIGABRT, CODE);
        let (_, pc) = called(&mut client, entry, arguments);
        assert_eq!(pc, CODE, "abort raised: {arguments:x?}");
    }
}

#[test]
fn default_messages_take_the_last_error_where_it_is_the_signal_s() {
    let mut client = client();
    let statics = Statics::of_base(STATICS);
    let note = |client: &mut Client, number: u32, text: &[u8]| {
        crate::kernel::os::note_error(&mut client.session.kernel.os(), statics, number, text)
            .expect("noted");
    };
    let text = |client: &mut Client, sig: u32| {
        default_message(&mut client.session.kernel.os(), statics, sig).expect("text")
    };
    note(&mut client, 0x117, b"An unrelated error");
    assert_eq!(
        text(&mut client, SIGFPE).as_deref(),
        Some(&b"An unrelated error"[..]),
        "always the last"
    );
    assert_eq!(
        text(&mut client, SIGOSERROR).as_deref(),
        Some(&b"An unrelated error"[..])
    );
    let generic = text(&mut client, SIGSEGV).expect("a message");
    assert_ne!(generic, b"An unrelated error", "not SIGSEGV's error");
    note(&mut client, 0x8000_0002, b"A data abort");
    assert_eq!(
        text(&mut client, SIGSEGV).as_deref(),
        Some(&b"A data abort"[..])
    );
    assert_eq!(text(&mut client, SIGSTAK), None, "silent");
    let unknown = text(&mut client, 8).expect("a message");
    assert!(unknown.windows(1).any(|w| w == b"8"), "{unknown:?}");
}

#[test]
fn trap_codes_map_to_their_signals() {
    for (code, sig) in [
        (0x8000_0020, SIGFPE),
        (0x8000_0201, SIGFPE),
        (0x8000_0000, SIGILL),
        (0x8000_0002, SIGSEGV),
        (READ_FAIL, SIGSEGV),
        (WRITE_FAIL, SIGSEGV),
        (0x8000_0021, SIGSTAK),
        (0x8000_0300, SIGOSERROR),
        (0x1234, SIGOSERROR),
    ] {
        assert_eq!(signal_of(code), sig, "{code:#x}");
    }
}
