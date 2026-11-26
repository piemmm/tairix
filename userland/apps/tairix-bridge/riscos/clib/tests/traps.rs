//! Traps, fatal errors and the program's ends in a C client, held to what
//! the same probe programs recorded on RISC OS 5.30: the trap procedure a
//! language gives entered with the code and the dumped registers, an error
//! for the parent where none takes it, and the finalisation procedures run
//! on every way out.

mod common;

use common::{program, run, run_program, Build, Probe, PRELUDE_ERROR};

/// The kit's trap procedure's record: R0 to R3, then the register set.
const TRAP: u32 = 1024;
const SET: u32 = 1040;
/// Where the kit's paths go: a trap procedure, the finalisation
/// procedure, and the prelude's error handler.
const TRAPPED: u32 = 0x10;
const FINALISED: u32 = 0x30;
const ERRED: u32 = 0x50;

fn trapped(body: &str) -> Probe {
    run(&Build::trapping(body))
}

/// `body` in a client whose language has no trap procedure, the error
/// handler set before `LibInit` taking what reaches the parent.
fn untrapped(body: &str) -> Probe {
    run(&Build {
        prelude: PRELUDE_ERROR,
        ..Build::plain(body)
    })
}

#[test]
fn a_divisor_of_nought_traps_at_its_call_with_the_registers_as_they_were() {
    let probe = trapped(
        "LDR r12, Lcall\nSTR r12, [r11, #8]\nMOV r0, #0\nMOV r1, #100\nMOV r4, #&44\nMOV r5, #&55\n\
         .call BL kvec+4*31\nB tail\n.Lcall DCD call\n",
    );
    assert_eq!(probe.path(), TRAPPED);
    assert_eq!(probe.word(TRAP), 0x8000_0020, "divide by zero");
    let set = probe.words(SET, 16);
    let call = probe.out(8);
    assert_eq!(set[15], call, "the PC wound back to the call");
    assert_eq!(set[14], call + 4, "R14 its return");
    assert_eq!((set[4], set[5]), (0x44, 0x55));
    assert_eq!(
        probe.word(TRAP + 4),
        set[13] - 68,
        "the set 68 below the SP"
    );
}

#[test]
fn a_trap_no_language_takes_is_an_uncaught_trap_error_for_the_parent() {
    let probe = untrapped("MOV r0, #0\nMOV r1, #100\nBL kvec+4*31\nB tail\n");
    assert_eq!(probe.path(), ERRED);
    let (number, text) = probe.first_error();
    assert_eq!(number, 0x80_0E01);
    assert!(text.contains("Divide by zero"), "{text}");
    let handlers = probe.words(1536, 51);
    let statics = probe.word(64 + 4 * 9);
    assert!(
        (1..=4).all(|n| handlers[3 * n] < statics || handlers[3 * n] > statics + 0x31C),
        "the exception handlers put back"
    );
}

#[test]
fn an_uncaught_exception_or_os_error_is_named_by_its_own_error_s_text() {
    let probe = untrapped("MOV r4, #&44\n.undef DCD &E7F000F0\nB tail\n");
    assert_eq!(probe.path(), ERRED);
    let (number, text) = probe.first_error();
    assert_eq!(number, 0x80_0E01);
    assert!(
        text.contains("Undefined instruction"),
        "the exception's own text: {text}"
    );
    let probe = untrapped("ADR r0, eblk\nSWI \"OS_GenerateError\"\nB tail\n.eblk DCD &1234\nEQUS \"probe error\"\nEQUB 0\nALIGN\n");
    let (_, text) = probe.first_error();
    assert!(
        text.contains("probe error"),
        "the OS error's own text: {text}"
    );
}

#[test]
fn an_error_from_a_swi_traps_with_its_number_at_the_swi() {
    let probe = trapped(
        "LDR r12, Lswi\nSTR r12, [r11, #8]\nMOV r4, #&44\nADR r0, eblk\n.swi SWI \"OS_GenerateError\"\nB tail\n\
         .Lswi DCD swi\n.eblk DCD &1234\nEQUS \"probe error\"\nEQUB 0\nALIGN\n",
    );
    assert_eq!(probe.path(), TRAPPED);
    assert_eq!(probe.word(TRAP), 0x1234, "the error's number");
    let set = probe.words(SET, 16);
    assert_eq!(set[15], probe.out(8), "the PC the SWI's");
    assert_eq!(set[4], 0x44);
}

#[test]
fn an_undefined_instruction_traps_at_it() {
    let probe = trapped(
        "LDR r12, Lundef\nSTR r12, [r11, #8]\nMOV r4, #&44\n.undef DCD &E7F000F0\nB tail\n.Lundef DCD undef\n",
    );
    assert_eq!(probe.path(), TRAPPED);
    assert_eq!(probe.word(TRAP), 0x8000_0000);
    let set = probe.words(SET, 16);
    assert_eq!(set[15], probe.out(8));
    assert_eq!(set[4], 0x44);
}

#[test]
fn a_data_abort_traps_at_the_access() {
    let probe = trapped(
        "LDR r12, Lload\nSTR r12, [r11, #8]\nMOV r4, #&44\nMOV r0, #&80000000\n.load LDR r1, [r0, #-4]\nB tail\n\
         .Lload DCD load\n",
    );
    assert_eq!(probe.path(), TRAPPED);
    assert_eq!(probe.word(TRAP), 0x8000_0002);
    assert_eq!(probe.words(SET, 16)[15], probe.out(8));
}

/// Zero page, which user mode reads but may not write: `memset` there from
/// user mode aborts, as the caller's own write would.
#[test]
fn a_library_routine_reaches_memory_with_its_caller_s_rights() {
    let zero = tairix_riscos_kernel::memory::layout::HIGH_ZERO_PAGE + 0x100;
    let probe = run(&Build::c(&format!(
        "LDR r0, Lzero\nMOV r1, #0\nMOV r2, #4\nBL cvec+4*55\nB tail\n.Lzero DCD &{zero:X}\n"
    )));
    assert_eq!(probe.path(), ERRED);
    let (number, text) = probe.first_error();
    assert_eq!(number, 0x80_0E01);
    assert!(text.contains("Data transfer aborted"), "{text}");
}

#[test]
fn raising_an_error_runs_the_finalisation_procedures_first() {
    let probe = trapped(
        "ADR r0, eblk\nBL kvec+4*47\nMOV r0, #7\nSTR r0, [r11, #0]\nB tail\n\
         .eblk DCD &1234\nEQUS \"raised error\"\nEQUB 0\nALIGN\n",
    );
    assert_eq!(probe.path(), FINALISED);
    assert_eq!(probe.out(0), 0, "it does not return");
}

#[test]
fn the_program_s_own_exit_runs_the_finalisation_procedures() {
    let probe = trapped("MOV r0, #0\nMOV r1, #0\nMOV r2, #7\nSWI \"OS_Exit\"\n");
    assert_eq!(probe.path(), FINALISED);
}

#[test]
fn a_new_application_s_upcall_runs_the_finalisation_procedures() {
    let probe = trapped("MOV r0, #256\nSWI \"OS_UpCall\"\nSTR r0, [r11, #0]\n");
    assert_eq!(probe.path(), FINALISED);
}

#[test]
fn no_initialisation_procedure_offering_a_run_procedure_is_no_main_program() {
    let build = Build {
        prelude: PRELUDE_ERROR,
        ..Build::plain("B tail\n")
    };
    let text = program(&build).replacen("ADR r0, run\n", "MOV r0, #0\n", 1);
    let probe = run_program(&text, "Run HostFS:$.CTest probe args", &[]);
    assert_eq!(probe.path(), ERRED);
    assert_eq!(probe.first_error().0, 0x80_0E02);
}
