//! `*Run` where the corpus cannot follow: a file run as the application,
//! which leaves the call, and one that cannot be loaded where it must run;
//! and scripts going on once a program one of their lines started exits.

mod common;

use common::{call, command, read, run_out, save, session, text, variable, EXITS, FAILS};
use tairix_riscos_kernel::os::{Ended, Fail, Os, Outcome, Regs, Unwind};
use tairix_riscos_kernel::swi::os::{fs_control, FILE, FS_CONTROL};
use tairix_riscos_kernel::swi::X;

/// `OS_FSControl` 4 on `name`, as it comes back.
fn run(os: &mut Os<'_>, name: &[u8]) -> Result<(), Fail> {
    let name = text(os, name);
    let mut regs = Regs::default();
    regs.r[..2].copy_from_slice(&[fs_control::RUN, name]);
    os.perform(X | FS_CONTROL, &mut regs)
}

/// The error number `outcome` refused with.
fn refusal(os: &mut Os<'_>, outcome: Result<(), Fail>) -> u32 {
    match outcome {
        Err(Fail::Error(block)) => os.read_word(block).expect("its number"),
        other => panic!("refused, not {other:?}"),
    }
}

/// `name` given untyped load and execution addresses.
fn untyped(os: &mut Os<'_>, name: &[u8], load: u32, exec: u32) {
    let name = text(os, name);
    call(os, FILE, &[1, name, load, exec, 0, 0x03]).expect("addresses kept");
}

#[test]
fn an_absolute_file_is_loaded_into_application_space_and_entered() {
    let mut kernel = common::kernel();
    session(&mut kernel, 0x1_0000);
    let mut os = kernel.os();
    let code = [0x11, 0x00, 0x00, 0xEF, 0x0E, 0xF0, 0xA0, 0xE1];
    save(&mut os, b"RAM:$.App", 0xFF8, &code).expect("saved");
    let outcome = run(&mut os, b"RAM:$.App");
    assert!(
        matches!(outcome, Err(Fail::Unwind(Unwind::Transfer))),
        "left for the application, not {outcome:?}"
    );
    assert_eq!(read(&mut os, 0x8000, code.len()), code, "loaded at &8000");
}

#[test]
fn a_file_that_cannot_be_loaded_where_it_runs_is_refused() {
    let mut kernel = common::kernel();
    let limit = session(&mut kernel, 0x1_0000);
    let mut os = kernel.os();
    save(&mut os, b"RAM:$.High", 0xFFD, &[0; 64]).expect("saved");
    untyped(&mut os, b"RAM:$.High", limit - 16, limit - 16);
    let outcome = run(&mut os, b"RAM:$.High");
    assert_eq!(refusal(&mut os, outcome), 0x400, "past the memory limit");
    save(&mut os, b"RAM:$.Low", 0xFFD, &[0; 64]).expect("saved");
    untyped(&mut os, b"RAM:$.Low", 0x4000, 0x4000);
    let outcome = run(&mut os, b"RAM:$.Low");
    assert_eq!(refusal(&mut os, outcome), 0x400, "below application space");
}

#[test]
fn an_untyped_file_runs_from_its_own_addresses() {
    let mut kernel = common::kernel();
    session(&mut kernel, 0x1_0000);
    let mut os = kernel.os();
    let code = [0xE1, 0xA0, 0x00, 0x00, 0x11, 0x00, 0x00, 0xEF];
    save(&mut os, b"RAM:$.Own", 0xFFD, &code).expect("saved");
    untyped(&mut os, b"RAM:$.Own", 0x9000, 0x9004);
    let outcome = run(&mut os, b"RAM:$.Own");
    assert!(
        matches!(outcome, Err(Fail::Unwind(Unwind::Transfer))),
        "{outcome:?}"
    );
    assert_eq!(
        read(&mut os, 0x9000, code.len()),
        code,
        "loaded at its load address"
    );
}

#[test]
fn an_obey_file_goes_on_once_a_program_it_ran_exits() {
    let mut kernel = common::kernel();
    session(&mut kernel, 0x1_0000);
    let mut os = kernel.os();
    save(&mut os, b"RAM:$.Abs", 0xFF8, &EXITS).expect("saved");
    save(
        &mut os,
        b"RAM:$.O",
        0xFEB,
        b"Set Probe$A 1\nRun RAM:$.Abs\nSet Probe$B 2\n",
    )
    .expect("saved");
    let started = command(&mut os, b"Obey RAM:$.O");
    assert!(
        matches!(started, Err(Fail::Unwind(Unwind::Transfer))),
        "{started:?}"
    );
    assert_eq!(variable(&mut os, b"Probe$A").as_deref(), Some(&b"1"[..]));
    assert_eq!(
        variable(&mut os, b"Probe$B"),
        None,
        "not yet: the program runs first"
    );
    assert_eq!(
        run_out(&mut kernel),
        Outcome::Ended(Ended::Exit { code: 0 })
    );
    let mut os = kernel.os();
    assert_eq!(variable(&mut os, b"Probe$B").as_deref(), Some(&b"2"[..]));
}

#[test]
fn nested_obey_files_go_on_innermost_first() {
    let mut kernel = common::kernel();
    session(&mut kernel, 0x1_0000);
    let mut os = kernel.os();
    save(&mut os, b"RAM:$.Abs", 0xFF8, &EXITS).expect("saved");
    save(
        &mut os,
        b"RAM:$.Inner",
        0xFEB,
        b"Run RAM:$.Abs\nSet Probe$Log <Probe$Log>i\n",
    )
    .expect("saved");
    save(
        &mut os,
        b"RAM:$.Outer",
        0xFEB,
        b"Set Probe$Log o\nObey RAM:$.Inner\nSet Probe$Log <Probe$Log>o\n",
    )
    .expect("saved");
    let started = command(&mut os, b"Obey RAM:$.Outer");
    assert!(
        matches!(started, Err(Fail::Unwind(Unwind::Transfer))),
        "{started:?}"
    );
    assert_eq!(
        run_out(&mut kernel),
        Outcome::Ended(Ended::Exit { code: 0 })
    );
    let mut os = kernel.os();
    assert_eq!(
        variable(&mut os, b"Probe$Log").as_deref(),
        Some(&b"oio"[..])
    );
}

#[test]
fn a_program_that_fails_ends_the_scripts_it_was_started_from() {
    let mut kernel = common::kernel();
    session(&mut kernel, 0x1_0000);
    let mut os = kernel.os();
    save(&mut os, b"RAM:$.Fails", 0xFF8, &FAILS).expect("saved");
    save(&mut os, b"RAM:$.Abs", 0xFF8, &EXITS).expect("saved");
    save(
        &mut os,
        b"RAM:$.O",
        0xFEB,
        b"Run RAM:$.Fails\nSet Probe$After 1\n",
    )
    .expect("saved");
    let started = command(&mut os, b"Obey RAM:$.O");
    assert!(
        matches!(started, Err(Fail::Unwind(Unwind::Transfer))),
        "{started:?}"
    );
    assert!(matches!(
        run_out(&mut kernel),
        Outcome::Ended(Ended::Error {
            number: 0x12345,
            ..
        })
    ));
    let mut os = kernel.os();
    assert_eq!(
        variable(&mut os, b"Probe$After"),
        None,
        "the failure ended the file"
    );
    save(
        &mut os,
        b"RAM:$.P",
        0xFEB,
        b"Run RAM:$.Abs\nSet Probe$Later 1\n",
    )
    .expect("saved");
    let started = command(&mut os, b"Obey RAM:$.P");
    assert!(
        matches!(started, Err(Fail::Unwind(Unwind::Transfer))),
        "{started:?}"
    );
    assert_eq!(
        run_out(&mut kernel),
        Outcome::Ended(Ended::Exit { code: 0 })
    );
    let mut os = kernel.os();
    assert_eq!(
        variable(&mut os, b"Probe$Later").as_deref(),
        Some(&b"1"[..])
    );
    assert_eq!(
        variable(&mut os, b"Probe$After"),
        None,
        "the ended file never went on"
    );
}
