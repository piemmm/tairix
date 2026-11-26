//! A module's library, a module run as an application, and sub-programs in
//! a C client, held to what the same probe programs recorded on RISC OS 5.30.

mod common;

use common::{numbered, program, run_built, Build, Builder};

/// A module-form library laid in Supervisor mode by `SharedCLibrary_LibInitModuleAPCS_32`, then `_kernel_moduleinit` with the return pushed.
const MODULE_FORM: &str = r#"
LDR r1, Lclr
ADD r2, r1, #&1000
MOV r3, #0
.clrl STR r3, [r1], #4
CMP r1, r2
BLT clrl
MOV r9, r10
MOV r7, r11
SWI "OS_EnterOS"
LDR r0, Lmstubs
LDR r1, Lmws
ADD r2, r1, #&2000
LDR r4, Lmstat
ADD r3, r4, #&300
ADD r3, r3, #&1C
MOV r5, r3
MOV r6, #0
SWI "XSharedCLibrary_LibInitModuleAPCS_32"
LDR r12, Lrec
STMIA r12, {r0-r6}
MOVVS r0, #1
MOVVC r0, #0
STR r0, [r12, #28]
LDR r0, [r1, #20]
STR r0, [r12, #32]
LDR r0, [r1, #24]
STR r0, [r12, #36]
MOV r8, r1
STR r13, [r12, #40]
LDR r0, Lminit
MOV r1, r8
MOV r2, #&22
MOV r12, #&33
ADR r14, bad
STMFD r13!, {r14}
ADR r14, back
STR r14, [r13]
MOV r14, #&44
B kvec+4*38
.bad MOV r0, #&BA
.back LDR r12, Lrec
STMIA r12!, {r0-r2}
ADD r12, r12, #32
STR r10, [r12], #4
STR r11, [r12], #4
STR r13, [r12], #4
STR r14, [r12], #4
LDR r12, Lrec
STR r0, [r12, #100]
SWI "OS_LeaveOS"
MOV r10, r9
MOV r11, r7
B tail
.Lclr DCD res+2048
.Lrec DCD res+2048
.Lmws DCD res+4096
.Lmstubs DCD mstubs
.Lmstat DCD mstat
.Lminit DCD minit
.minit DCD &12345678
DCD lang
DCD langend
.mstubs DCD 1
DCD mvec
DCD mvec+4*48
DCD mstat
DCD mstat+&31C
DCD -1
"#;

/// A module's run code at &100000: a module-form library and `_kernel_moduleinit`, then `_kernel_entermodule` with R6 4096 and R8 a private word holding &8000 and the static offsets; the run code saves what it finds.
const ENTERED: &str = r#"
.start SWI "OS_EnterOS"
LDR r0, Lmstubs
LDR r1, Lws
ADD r2, r1, #&2000
LDR r4, Lmstat
ADD r3, r4, #&300
ADD r3, r3, #&1C
MOV r5, r3
MOV r6, #0
SWI "XSharedCLibrary_LibInitModuleAPCS_32"
MOV r8, r1
LDR r12, Lpwblk
MOV r0, #&8000
STR r0, [r12, #0]
LDR r0, [r8, #20]
STR r0, [r12, #4]
LDR r0, [r8, #24]
STR r0, [r12, #8]
LDR r12, Lres
STR r1, [r12, #0]
STR r2, [r12, #4]
LDR r0, Lkinit
MOV r1, r8
ADR r14, miback
STMFD r13!, {r14}
B mvec+4*38
.miback SWI "OS_LeaveOS"
LDR r0, Lkinit
MOV r6, #4096
LDR r8, Lpw
MVN r12, #0
B mvec+4*42
.Lmstubs DCD mstubs
.Lws DCD ws
.Lmstat DCD mstat
.Lpwblk DCD pwblk
.Lpw DCD pw
.Lres DCD res
.Lkinit DCD kinit
.kinit DCD &100000
DCD lang
DCD langend
.lang DCD 24
DCD start
DCD codeend
DCD langname
DCD initproc
DCD 0
.langend
.langname EQUS "Probe"
EQUB 0
ALIGN
.initproc LDR r12, Lires
STMIA r12, {r0-r11}
STR r13, [r12, #48]
STR r14, [r12, #52]
ADR r0, run
MOV pc, r14
.Lires DCD res+64
.run LDR r12, Lrres
STMIA r12, {r0-r11}
STR r13, [r12, #48]
STR r14, [r12, #52]
SUB r1, r10, #560
ADD r12, r12, #64
MOV r2, #0
.rc LDR r3, [r1, r2]
STR r3, [r12, r2]
ADD r2, r2, #4
CMP r2, #64
BLT rc
MOV r7, #0
LDR r8, Lhand
.hl MOV r0, r7
MOV r1, #0
MOV r2, #0
MOV r3, #0
SWI "XOS_ChangeEnvironment"
STMIA r8!, {r1-r3}
ADD r7, r7, #1
CMP r7, #17
BLT hl
SWI "OS_GetEnv"
LDR r12, Lres2
STR r1, [r12, #8]
MOV r0, #10
ADR r1, nres
LDR r2, Ltype
LDR r4, Lres2
ADD r5, r4, #2048
SWI "XOS_File"
MOV r0, #10
ADR r1, nstat
LDR r2, Ltype
LDR r4, Lws2
LDR r12, Lpwblk2
LDR r3, [r12, #8]
LDR r5, Lmstat2
ADD r4, r5, r3
ADD r5, r4, #&300
ADD r5, r5, #&1C
SWI "XOS_File"
MOV r0, #10
ADR r1, ndone
LDR r2, Ltype
LDR r4, Lres2
ADD r5, r4, #4
SWI "XOS_File"
SWI "OS_Exit"
.Lrres DCD res+128
.Lhand DCD res+256
.Lres2 DCD res
.Lws2 DCD ws
.Lpwblk2 DCD pwblk
.Lmstat2 DCD mstat
.Ltype DCD &FFD
.nres EQUS "HostFS:$.CRes"
EQUB 0
.nstat EQUS "HostFS:$.CStat"
EQUB 0
.ndone EQUS "HostFS:$.Done"
EQUB 0
ALIGN
.codeend
.mstubs DCD 1
DCD mvec
DCD mvec+4*48
DCD mstat
DCD mstat+&31C
DCD -1
.pw DCD pwblk
.pwblk DCD 0
DCD 0
DCD 0
"#;

/// `_kernel_system` on a built-in command, then on a program, with a marker
/// in the heap and the handlers read after; the heap's pointer read where
/// these statics keep it, &74.
const SUB_PROGRAM: &str = r#"
MOV r6, r11
MOV r11, #0
LDR r1, Lclr
ADD r2, r1, #&1000
MOV r3, #0
.clrl STR r3, [r1], #4
CMP r1, r2
BLT clrl
LDR r0, Lcmd1
MOV r1, #0
BL kvec+4*22
STR r0, [r6, #0]
MOV r0, #16
ADD r1, r6, #8
BL kvec+4*26
LDR r1, [r6, #8]
LDR r2, Lmark
STR r2, [r1]
LDR r12, Lkst
LDR r3, [r12, #&74]
STR r3, [r6, #12]
LDR r0, Lcmd2
MOV r1, #0
BL kvec+4*22
STR r0, [r6, #16]
STR r10, [r6, #36]
STR r13, [r6, #40]
LDR r1, [r6, #8]
LDR r1, [r1]
STR r1, [r6, #20]
LDR r12, Lkst
LDR r3, [r12, #&74]
STR r3, [r6, #24]
LDR r0, Lvar
ADD r1, r6, #48
MOV r2, #16
MOV r3, #0
MOV r4, #3
SWI "XOS_ReadVarVal"
STR r2, [r6, #28]
MOV r7, #0
LDR r8, Lhand3
.hl3 MOV r0, r7
MOV r1, #0
MOV r2, #0
MOV r3, #0
SWI "XOS_ChangeEnvironment"
STMIA r8!, {r1-r3}
ADD r7, r7, #1
CMP r7, #17
BLT hl3
SWI "OS_GetEnv"
STR r1, [r6, #44]
MOV r11, r6
B tail
.Lclr DCD res+2048
.Lhand3 DCD res+2048
.Lkst DCD kstat
.Lmark DCD &C0FFEE
.Lcmd1 DCD cmd1
.Lcmd2 DCD cmd2
.Lvar DCD varname
.cmd1 EQUS "Echo hello"
EQUB 0
.cmd2 EQUS "HostFS:$.Child"
EQUB 0
.varname EQUS "Sys$ReturnCode"
EQUB 0
ALIGN
"#;

/// A program saving what it is started with, then ending as `EXIT` says.
const CHILD: &str = r#"
.cstart SWI "OS_GetEnv"
ADR r12, cres
STMIA r12, {r0-r2}
MOV r7, #0
ADD r8, r12, #16
.ch MOV r0, r7
MOV r1, #0
MOV r2, #0
MOV r3, #0
SWI "XOS_ChangeEnvironment"
STMIA r8!, {r1-r3}
ADD r7, r7, #1
CMP r7, #17
BLT ch
ADR r12, cres
STR r13, [r12, #12]
MOV r0, #10
ADR r1, ncres
LDR r2, Lctype
ADR r4, cres
ADD r5, r4, #256
SWI "XOS_File"
EXIT
.Labex DCD &58454241
.Lctype DCD &FFD
.cerr DCD &1234
EQUS "child error"
EQUB 0
ALIGN
.ncres EQUS "HostFS:$.ChildRes"
EQUB 0
ALIGN
.cres
"#;

/// How the child ends: with return code 7, or with an error.
const EXITING: &str = "MOV r0, #0\nLDR r1, Labex\nMOV r2, #7\nSWI \"OS_Exit\"\n";
const ERRING: &str = "ADR r0, cerr\nSWI \"OS_GenerateError\"\n";

/// RISC OS 5.30's answer for an OS error.
const OS_ERROR: u32 = (-2_i32).cast_unsigned();

#[test]
fn moduleinit_returns_to_the_pushed_address_with_sl_above_the_base() {
    let text = program(&Build::trapping(MODULE_FORM)).replacen(
        ":res=statend:",
        ":mvec=statend:mstat=mvec+4*96:res=mstat+&31C:",
        1,
    );
    assert!(
        text.contains(":mvec=statend:"),
        "the module form's vector and statics laid past the kit's"
    );
    let probe = run_built(&[("Mk", &text)], "Run HostFS:$.CTest probe args", &[]);
    assert_eq!(probe.word(2048 + 28), 0, "LibInitModule succeeded");
    let before = probe.word(2048 + 40);
    let [sl, fp, sp, lr] = [11, 12, 13, 14].map(|n| probe.word(2048 + 4 * n));
    assert_eq!(
        sl,
        (before & !0xF_FFFF) + 560,
        "above the supervisor stack's base"
    );
    assert_eq!(sp, before, "the pushed word taken off");
    assert_eq!(lr, 0x44, "LR untouched");
    assert_ne!(fp, 0, "FP untouched");
}

#[test]
fn entermodule_lays_the_root_chunk_at_the_application_space_and_starts_the_client() {
    let text = numbered(&Builder {
        name: "Mk",
        origin: 0x10_0000,
        assembly: ENTERED,
        layout: "mvec=(P%+3) AND NOT 3:mstat=mvec+4*96:res=mstat+&31C:ws=res+2048",
        save: "SYS \"OS_File\",0,\"HostFS:$.CTest\",&100000,&100000,code%,O%",
    });
    let probe = run_built(&[("Mk", &text)], "Run HostFS:$.CTest probe args", &[]);
    let initialising = probe.words(64, 14);
    assert_eq!(
        initialising[10],
        0x8000 + 560,
        "SL above the root chunk at &8000"
    );
    assert_eq!(initialising[11], 0, "no frame");
    let root = probe.words(128 + 64, 7);
    assert_eq!(root[..5], [0xF606_90FF, 0, 0, 4096, 0], "R6's size");
    let private = probe.words(0, 2);
    assert_ne!(private[0], 0, "the module form's base");
    assert_eq!(
        probe.statics_word(0),
        0x8000,
        "the image base the application space's"
    );
}

fn sub_program(exiting: &str) -> common::Probe {
    let main = program(&Build::trapping(SUB_PROGRAM));
    let child = numbered(&Builder {
        name: "MkC",
        origin: 0x8000,
        assembly: &CHILD.replace("EXIT\n", exiting),
        layout: "REM",
        save: "SYS \"OS_File\",10,\"HostFS:$.Child\",&FF8,,code%,O%+256",
    });
    run_built(
        &[("MkC", &child), ("Mk", &main)],
        "Run HostFS:$.CTest probe args",
        &[],
    )
}

#[test]
fn a_sub_program_runs_with_the_caller_copied_out_of_its_way_and_back() {
    let probe = sub_program(EXITING);
    assert_eq!(probe.out(0), 0, "a built-in command");
    assert_eq!(probe.out(16), 0, "the program ended");
    assert_eq!(probe.out(20), 0xC0_FFEE, "the caller's heap back as it was");
    assert_eq!(probe.out(12), probe.out(24), "its heap pointer too");
    let limit = probe.out(44);
    let child_limit = probe.child_word(4);
    let heap = probe.out(12);
    let size = (heap - 0x8000).next_multiple_of(16);
    assert_eq!(
        child_limit,
        limit - size,
        "the child below the caller's copy"
    );
    let returned = &probe.results[448..448 + probe.out(28) as usize];
    assert_eq!(returned, b"7", "Sys$ReturnCode");
}

#[test]
fn a_sub_program_s_error_is_noted_and_answered_as_an_os_error() {
    let probe = sub_program(ERRING);
    assert_eq!(probe.out(16), OS_ERROR);
    assert_eq!(probe.out(20), 0xC0_FFEE);
    assert_eq!(probe.statics_word(0x120), 0x1234, "the child's error kept");
}

/// `_kernel_system` chaining `COMMAND`, a marker set first.
const CHAINED: &str = r#"
LDR r12, Lph
MOV r0, #5
STR r0, [r12]
LDR r0, Lcmd
MOV r1, #1
BL kvec+4*22
STR r0, [r11, #0]
B tail
.Lph DCD res+1296
.Lcmd DCD cmd
.cmd EQUS "COMMAND"
EQUB 0
ALIGN
"#;

#[test]
fn a_chained_built_in_command_ends_the_program_unfinalised() {
    let probe = run_built(
        &[(
            "Mk",
            &program(&Build::trapping(&CHAINED.replace("COMMAND", "Echo bye"))),
        )],
        "Run HostFS:$.CTest probe args",
        &[],
    );
    assert!(
        probe.results.iter().all(|&b| b == 0),
        "the kit's tail never ran"
    );
    assert!(matches!(
        probe.outcome,
        tairix_riscos_kernel::os::Outcome::Ended(_)
    ));
}

#[test]
fn a_chained_command_s_error_goes_to_the_handlers_put_back() {
    let probe = run_built(
        &[(
            "Mk",
            &program(&Build::trapping(
                &CHAINED.replace("COMMAND", "NoSuchCommandX"),
            )),
        )],
        "Run HostFS:$.CTest probe args",
        &[],
    );
    assert_eq!(probe.path(), 0x50);
    assert_eq!(probe.out(0), 0, "it does not return");
}
