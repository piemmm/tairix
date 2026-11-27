//! The library kernel's start and its plain entries in a C client, held to
//! what the same probe programs recorded on RISC OS 5.30.

mod common;

use common::{numbered, program, run, run_built, Build, Builder, Calls};
use tairix_riscos_filing::session::disc_file;
use tairix_riscos_kernel::swi::fp_emulator::VERSION;
use tairix_riscos_kernel::vectors::UKSWI;

/// RISC OS 5.30's answer for an OS error.
const OS_ERROR: u32 = (-2_i32).cast_unsigned();

#[test]
fn init_enters_the_initialisation_procedure_then_the_run_procedure_it_offers() {
    let probe = run(&Build::plain("B tail\n"));
    assert_eq!(probe.word(28), 0, "LibInit succeeded");
    let initialising = probe.words(64, 14);
    let [desc, r5, r6, r7, r9, sl, fp, sp] = [0, 5, 6, 7, 9, 10, 11, 12].map(|n| initialising[n]);
    assert_eq!((r5, r7), (desc, 24), "the description and its size");
    assert_eq!(r6, desc + 24, "the descriptions' end");
    assert_eq!(fp, 0);
    let root = probe.word(4);
    assert_eq!(sl, root + 560);
    let top = probe.word(8);
    assert_eq!(sp, top - 28, "below the command line's copy, word-rounded");
    let running = probe.words(128, 14);
    assert_eq!(running[0], running[4], "R0 and R4 the run address");
    assert_eq!(
        (running[9], running[12]),
        (r9, sp),
        "the statics, the stack"
    );
}

#[test]
fn init_sets_the_handlers_and_lays_the_root_chunk_as_recorded() {
    let probe = run(&Build::plain("B tail\n"));
    let handlers = probe.words(192, 51);
    let handler = |n: usize| [handlers[3 * n], handlers[3 * n + 1], handlers[3 * n + 2]];
    let statics = probe.word(64 + 4 * 9);
    for (n, exception) in (1..=4).enumerate() {
        let trampoline = statics + 12 + 16 * u32::try_from(n).expect("small");
        assert_eq!(
            handler(exception),
            [trampoline, 0, 0],
            "exception {exception}"
        );
    }
    assert_eq!(
        handler(6)[1..],
        [statics, statics + 0x11C],
        "the error handler's buffer"
    );
    assert_eq!(
        handler(7)[1..],
        [statics, statics + 0x78],
        "the callback buffer"
    );
    for others in [9, 10, 11, 16] {
        assert_eq!(handler(others)[1..], [statics, 0], "handler {others}");
    }
    let root = probe.words(800, 7);
    assert_eq!(
        root[..5],
        [0xF606_90FF, 0, 0, 0x1000, 0],
        "the root chunk's header"
    );
}

#[test]
fn the_plain_entries_answer_as_recorded() {
    let mut calls = Calls::default();
    let hostos = calls.call(8, &[], false);
    let command = calls.call(7, &[], false);
    let chunk = calls.call(44, &[], false);
    let language = calls.call(6, &["LDR r0, Lp_body"], false);
    let procname = calls.call(5, &["LDR r0, Lp_body"], false);
    let disabled = calls.call(41, &[], false);
    let off = calls.call(40, &[], false);
    let disabled_after = calls.call(41, &[], false);
    let code = calls.call(2, &["MOV r0, #42"], false);
    let probe = run(&Build::plain(&calls.body("")));
    assert_eq!(probe.out(hostos), 6);
    let root = probe.word(4);
    assert_eq!(probe.out(chunk), root);
    assert_ne!(
        probe.out(language),
        0,
        "the language holding the code named"
    );
    assert_eq!(probe.out(procname), 0, "no name procedure, no name");
    assert_eq!((probe.out(disabled), probe.out(disabled_after)), (0, 0));
    assert_eq!(probe.out(off), 0);
    assert_eq!(probe.out(code), 42);
    let copy = probe.out(command);
    assert!(
        copy > root && copy < root + 0x1000,
        "the copy in the root chunk"
    );
}

#[test]
fn division_answers_as_recorded() {
    let mut calls = Calls::default();
    let cases: [(u32, i64, i64, [u32; 2]); 7] = [
        (31, 7, 100, [14, 2]),
        (32, 7, 100, [2, 2]),
        (
            34,
            7,
            -100,
            [(-14_i32).cast_unsigned(), (-2_i32).cast_unsigned()],
        ),
        (34, -7, 100, [(-14_i32).cast_unsigned(), 2]),
        (
            35,
            7,
            -100,
            [(-2_i32).cast_unsigned(), (-2_i32).cast_unsigned()],
        ),
        (34, -1, -2_147_483_648, [0x8000_0000, 0]),
        (31, -1, 5, [0, 5]),
    ];
    let mut slots = Vec::new();
    for &(entry, divisor, dividend, answer) in &cases {
        let setup = [calls.load("r0", divisor), calls.load("r1", dividend)];
        let setup: Vec<&str> = setup.iter().map(String::as_str).collect();
        slots.push((calls.call(entry, &setup, true), answer));
    }
    let tens: [(u32, i64, [u32; 2]); 4] = [
        (33, 123, [12, 3]),
        (
            36,
            -123,
            [(-12_i32).cast_unsigned(), (-3_i32).cast_unsigned()],
        ),
        (33, -1, [0x1999_9999, 5]),
        (36, -2_147_483_648, [0xF333_3334, (-8_i32).cast_unsigned()]),
    ];
    for &(entry, dividend, answer) in &tens {
        let setup = [calls.load("r0", dividend)];
        let setup: Vec<&str> = setup.iter().map(String::as_str).collect();
        slots.push((calls.call(entry, &setup, true), answer));
    }
    let probe = run(&Build::plain(&calls.body("")));
    for (slot, answer) in slots {
        assert_eq!(
            [probe.out(slot), probe.out(slot + 4)],
            answer,
            "slot {slot}"
        );
    }
}

#[test]
fn the_os_functions_answer_as_recorded() {
    let mut calls = Calls::default();
    let byte = calls.call(10, &["MOV r0, #0", "MOV r1, #1", "MOV r2, #0"], false);
    let wrch = calls.call(12, &["MOV r0, #65"], false);
    let missing = calls.call(17, &["MOV r0, #&40", "LDR r1, Lp_nmiss"], false);
    let opened = calls.call(17, &["MOV r0, #&40", "LDR r1, Lp_nself"], false);
    calls.raw("STR r0, hand");
    let got = calls.call(13, &["LDR r0, hand"], false);
    let pointer = calls.call(19, &["MOV r0, #0", "LDR r1, hand", "MOV r2, #0"], false);
    calls.raw("LDR r0, Lp_chost");
    calls.raw("SWI \"XOS_CLI\"");
    let filing = calls.call(19, &["MOV r0, #0", "MOV r1, #0", "MOV r2, #0"], false);
    let put = calls.call(14, &["MOV r0, #65", "LDR r1, hand"], false);
    let noted = calls.call(21, &[], false);
    let closed = calls.call(17, &["MOV r0, #0", "LDR r1, hand"], false);
    let got_closed = calls.call(13, &["LDR r0, hand"], false);
    let info = calls.call(
        18,
        &["MOV r0, #5", "LDR r1, Lp_nself", "LDR r2, Lp_fblk"],
        false,
    );
    let none = calls.call(
        18,
        &["MOV r0, #5", "LDR r1, Lp_nmiss", "LDR r2, Lp_fblk"],
        false,
    );
    let bad = calls.call(
        18,
        &["MOV r0, #5", "LDR r1, Lp_nbad", "LDR r2, Lp_fblk"],
        false,
    );
    let ran = calls.call(20, &["LDR r0, Lp_cset"], false);
    let refused = calls.call(20, &["LDR r0, Lp_cbad"], false);
    let erring = calls.call(
        9,
        &["MOV r0, #&2B", "LDR r1, Lp_swiin", "LDR r2, Lp_swiout"],
        false,
    );
    let probe = run(&Build::plain(&calls.body(OS_DATA)));
    assert_eq!(probe.out(byte), 6);
    assert_eq!(probe.out(wrch), 65);
    assert_eq!(probe.out(missing), 0);
    assert_ne!(probe.out(opened), 0);
    assert_eq!(
        probe.out(got),
        0x10,
        "the file's first byte, SWI OS_GetEnv's"
    );
    assert_eq!(
        probe.out(pointer),
        1,
        "op nought on a file: its pointer, from R2"
    );
    assert_eq!(
        probe.out(filing),
        0x99,
        "HostFS, the filing system selected"
    );
    assert_eq!(
        probe.out(put),
        OS_ERROR,
        "a byte put to a file opened to read"
    );
    let statics = probe.word(64 + 4 * 9);
    assert_eq!(probe.out(noted), statics + 0x120, "the error kept");
    assert_eq!(probe.out(closed), 0);
    assert_eq!(probe.out(got_closed), OS_ERROR);
    assert_eq!(probe.out(info), 1, "a file");
    assert_eq!(probe.out(none), 0, "nothing there");
    assert_eq!(probe.out(bad), OS_ERROR);
    assert_eq!((probe.out(ran), probe.out(refused)), (1, OS_ERROR));
    assert_ne!(probe.out(erring), 0, "the error's block");
}

/// The data the OS functions' calls name.
const OS_DATA: &str = r#"
.hand DCD 0
.fblk DCD 0
DCD 0
DCD 0
DCD 0
.swiin DCD errblk
DCD 0
DCD 0
DCD 0
DCD 0
DCD 0
DCD 0
DCD 0
DCD 0
DCD 0
.swiout DCD 0
DCD 0
DCD 0
DCD 0
DCD 0
DCD 0
DCD 0
DCD 0
DCD 0
DCD 0
.errblk DCD &1234
EQUS "probe error"
EQUB 0
ALIGN
.nmiss EQUS "HostFS:$.Missing"
EQUB 0
.nself EQUS "HostFS:$.CTest"
EQUB 0
.nbad EQUS "HostFS:$.a..b"
EQUB 0
.cset EQUS "Set Probe$X 1"
EQUB 0
.cbad EQUS "NoSuchCommandAtAll"
EQUB 0
.chost EQUS "HostFS"
EQUB 0
ALIGN
"#;

/// An `OS_Args` reason, whether it names the open file rather than none, its
/// R2, and what genuine's `_kernel_osargs` answered where that does not
/// depend on the filing system's own figures.
type OsArgsCase = (u32, bool, u32, Option<u32>);

/// The calls the K6 probe made of a ten-byte file and of no file, in order.
const OSARGS_CASES: [OsArgsCase; 16] = [
    (0, false, 0, Some(0x99)),
    (0, false, 0x1234, Some(0x99)),
    (0, true, 0, Some(10)),
    (1, true, 3, Some(3)),
    (0, true, 0, Some(3)),
    (2, true, 0, Some(10)),
    (3, true, 20, Some(20)),
    (2, true, 0, Some(20)),
    (4, true, 0, None),
    (5, true, 0, Some(0)),
    (6, true, 1000, None),
    (254, true, 0, None),
    (255, true, 0, Some(0)),
    (1, false, 0, Some(OS_ERROR)),
    (2, false, 0, Some(OS_ERROR)),
    (255, false, 0, Some(0)),
];

/// "Channel", which `OS_Args` refuses a reason no file answers and a closed
/// file with.
const CHANNEL: u32 = 0xDE;

/// `_kernel_osargs` beside the `OS_Args` it performs: R0 for op nought of no
/// file, R2 for every other call, and −2 where `OS_Args` refuses, its error
/// kept for `_kernel_last_oserror`, as recorded.
#[test]
fn osargs_answers_r2_but_for_the_filing_system_and_minus_two_where_refused() {
    let mut calls = Calls::default();
    for line in [
        "LDR r0, Lp_chost",
        "SWI \"XOS_CLI\"",
        "MOV r0, #&80",
        "LDR r1, Lp_nk6",
        "SWI \"XOS_Find\"",
        "STR r0, hand",
        "MOV r1, r0",
        "MOV r0, #2",
        "LDR r2, Lp_ten",
        "MOV r3, #10",
        "SWI \"XOS_GBPB\"",
    ] {
        calls.raw(line);
    }
    let answers: Vec<_> = OSARGS_CASES
        .iter()
        .map(|&(op, on_file, arg, _)| osargs_beside_os_args(&mut calls, op, on_file, arg))
        .collect();
    for line in ["MOV r0, #0", "LDR r1, hand", "SWI \"XOS_Find\""] {
        calls.raw(line);
    }
    let closed = osargs_beside_os_args(&mut calls, 2, true, 0);
    let noted = calls.call(21, &[], false);
    let probe = run(&Build::plain(&calls.body(OSARGS_DATA)));
    for (&(op, on_file, arg, recorded), ([r0, r2], [v, error], answer)) in
        OSARGS_CASES.iter().zip(&answers)
    {
        let case = format!("_kernel_osargs({op}, {on_file}, {arg:#X})");
        let expected = match probe.out(*v) {
            0 if op == 0 && !on_file => probe.out(*r0),
            0 => probe.out(*r2),
            _ => OS_ERROR,
        };
        assert_eq!(probe.out(*answer), expected, "{case}");
        if let Some(recorded) = recorded {
            assert_eq!(probe.out(*answer), recorded, "{case} as recorded");
        }
        if recorded == Some(OS_ERROR) {
            assert_eq!(probe.out(*error), CHANNEL, "{case} refused");
        }
    }
    let ([_, _], [v, error], answer) = closed;
    assert_eq!(
        (probe.out(v), probe.out(error), probe.out(answer)),
        (1, CHANNEL, OS_ERROR),
        "a closed file's extent"
    );
    assert_ne!(probe.out(noted), 0, "the refusal kept");
}

/// `OS_Args` of `op`, the file or none, and `arg`, then `_kernel_osargs` of
/// the same: where the SWI's R0 and R2, its V flag and error number, and the
/// function's answer are stored.
fn osargs_beside_os_args(
    calls: &mut Calls,
    op: u32,
    on_file: bool,
    arg: u32,
) -> ([u32; 2], [u32; 2], u32) {
    let setup = [
        calls.load("r0", op.into()),
        if on_file {
            "LDR r1, hand"
        } else {
            "MOV r1, #0"
        }
        .to_string(),
        calls.load("r2", arg.into()),
    ];
    for line in setup.iter().map(String::as_str).chain([
        "SWI \"XOS_Args\"",
        "MOVVS r3, #1",
        "LDRVS r4, [r0]",
        "MOVVC r3, #0",
        "MOVVC r4, #0",
    ]) {
        calls.raw(line);
    }
    let performed = [calls.stored("r0"), calls.stored("r2")];
    let refused = [calls.stored("r3"), calls.stored("r4")];
    let setup: Vec<&str> = setup.iter().map(String::as_str).collect();
    (performed, refused, calls.call(19, &setup, false))
}

/// The data the `_kernel_osargs` calls name.
const OSARGS_DATA: &str = r#"
.hand DCD 0
.nk6 EQUS "HostFS:$.K6File"
EQUB 0
.chost EQUS "HostFS"
EQUB 0
.ten EQUS "0123456789"
ALIGN
"#;

/// A file written and read through `_kernel_osbput` and `_kernel_osgbpb`,
/// and SWIs that fail through `_kernel_swi` and `_kernel_swi_c`.
const GBPB: &str = r#"
LDR r1, Lclr
ADD r2, r1, #&400
MOV r3, #0
.clrl STR r3, [r1], #4
CMP r1, r2
BLT clrl
MOV r0, #&80
LDR r1, Lnfile
BL kvec+4*17
STR r0, [r11, #0]
MOV r9, r0
MOV r0, #65
MOV r1, r9
BL kvec+4*14
STR r0, [r11, #4]
LDR r8, Lblk
LDR r0, Lstr10
MOV r1, #10
MOV r2, #1
STMIA r8, {r0-r2}
MOV r0, #2
MOV r1, r9
MOV r2, r8
BL kvec+4*15
STR r0, [r11, #8]
LDMIA r8, {r0-r4}
ADD r12, r11, #12
STMIA r12, {r0-r4}
LDR r0, Lbuf
MOV r1, #4
MOV r2, #0
STMIA r8, {r0-r2}
MOV r0, #4
MOV r1, r9
MOV r2, r8
BL kvec+4*15
STR r0, [r11, #32]
LDMIA r8, {r0-r4}
ADD r12, r11, #36
STMIA r12, {r0-r4}
LDR r0, Lbuf
MOV r1, #4
MOV r2, #2
STMIA r8, {r0-r2}
MOV r0, #3
MOV r1, r9
MOV r2, r8
BL kvec+4*15
STR r0, [r11, #56]
LDMIA r8, {r0-r4}
ADD r12, r11, #60
STMIA r12, {r0-r4}
LDR r0, Lbuf
LDR r0, [r0]
STR r0, [r11, #80]
LDR r0, Lbuf
MOV r1, #20
MOV r2, #5
STMIA r8, {r0-r2}
MOV r0, #3
MOV r1, r9
MOV r2, r8
BL kvec+4*15
STR r0, [r11, #84]
LDMIA r8, {r0-r4}
ADD r12, r11, #88
STMIA r12, {r0-r4}
MOV r0, #0
MOV r1, r9
BL kvec+4*17
STR r0, [r11, #108]
MOV r0, #4
MOV r1, r9
MOV r2, r8
BL kvec+4*15
STR r0, [r11, #112]
LDR r7, Lin
MOV r0, #5
LDR r1, Lnbad
STMIA r7, {r0, r1}
LDR r6, Lout5
LDR r0, Lmark
MOV r1, #0
.ml STR r0, [r6, r1]
ADD r1, r1, #4
CMP r1, #80
BLT ml
MOV r0, #8
MOV r1, r7
MOV r2, r6
BL kvec+4*9
STR r0, [r11, #116]
ADD r12, r11, #120
LDMIA r6, {r0-r9}
STMIA r12, {r0-r9}
LDR r7, Lin
LDR r6, Lout5
ADD r6, r6, #40
MOV r0, #&77
STR r0, [r6, #44]
MOV r0, #8
MOV r1, r7
MOV r2, r6
ADD r3, r6, #44
BL kvec+4*45
STR r0, [r11, #160]
LDR r0, [r6, #44]
STR r0, [r11, #164]
LDR r0, [r6, #0]
STR r0, [r11, #168]
B tail
.Lclr DCD res+2048
.Lblk DCD res+2048
.Lbuf DCD res+2272
.Lin DCD res+2112
.Lout5 DCD res+2160
.Lmark DCD &5A5A5A5A
.Lnfile DCD nfile
.Lstr10 DCD str10
.Lnbad DCD nbad
.nfile EQUS "HostFS:$.T22"
EQUB 0
.nbad EQUS "a..b"
EQUB 0
.str10 EQUS "0123456789"
EQUB 0
ALIGN
"#;

#[test]
fn osgbpb_and_the_swi_calls_answer_as_recorded() {
    let probe = run(&Build::plain(GBPB));
    let block = |at: u32| probe.words(400 + at, 5);
    assert_eq!(probe.out(4), 65, "osbput: the character");
    assert_eq!(probe.out(8), 2, "a write: R0 as the SWI left it");
    let written = block(12);
    assert_eq!(written[1..3], [0, 11], "all written, the pointer past it");
    assert_eq!(probe.out(32), u32::MAX, "a read at the end: C set");
    assert_eq!(block(36)[1], 4, "nothing read");
    assert_eq!(probe.out(56), 3);
    assert_eq!(block(60)[1..3], [0, 6]);
    assert_eq!(probe.out(84), u32::MAX, "a read past the end");
    assert_eq!(block(88)[1..3], [14, 11], "fourteen short, at the end");
    assert_eq!(
        (probe.out(108), probe.out(112)),
        (0, OS_ERROR),
        "closed, then refused"
    );
    let error = probe.out(116);
    assert_ne!(error, 0);
    assert_eq!(probe.out(120), error, "the out block filled, R0 the error");
    assert_eq!(probe.out(160), probe.out(168), "swi_c's out block too");
    assert_eq!(probe.out(164), 0, "and its carry");
}

/// In Supervisor mode, where an escape's callback waits: the IRQ state set
/// and asked, then a character read with an escape pending.
const SUPERVISED: &str = r#"
SWI "OS_EnterOS"
BL kvec+4*40
BL kvec+4*41
STR r0, [r11, #0]
BL kvec+4*39
BL kvec+4*41
STR r0, [r11, #4]
MOV r0, #125
SWI "XOS_Byte"
BL kvec+4*11
STR r0, [r11, #8]
SWI "OS_LeaveOS"
SWI "XOS_ReadEscapeState"
MOVCS r0, #1
MOVCC r0, #0
STR r0, [r11, #16]
B tail
"#;

#[test]
fn a_privileged_caller_sets_the_irq_state_and_a_read_an_escape_ends_answers_minus_it() {
    let probe = run(&Build::trapping(SUPERVISED));
    assert_eq!(
        (probe.out(0), probe.out(4)),
        (0x80, 0),
        "the I bit, then none"
    );
    assert_eq!(
        probe.out(8),
        (-27_i32).cast_unsigned(),
        "minus the escape character"
    );
    assert_eq!(
        probe.out(16),
        0,
        "the escape acknowledged on the way back to User mode"
    );
}

/// A start whose `FPEmulator_Version` reaches guest code that exits passes
/// the exit on: the client is never entered, where a swallowed exit would
/// take it for an emulator missing and run it.
#[test]
fn init_passes_on_an_exit_its_emulator_query_meets() {
    let exits = numbered(&Builder {
        name: "Exits",
        origin: 0,
        assembly: &format!(
            "LDR R10,version\nCMP R11,R10\nMOVNE PC,R14\nSWI \"OS_Exit\"\n.version DCD &{VERSION:X}\n"
        ),
        layout: "",
        save: &format!(
            "SYS \"OS_Module\",6,,,O%-code% TO ,,b%:FOR i%=0 TO O%-code%-4 STEP 4:b%!i%=code%!i%:NEXT:SYS \"OS_Claim\",&{:X},b%,0:OSCLI \"RMKill FPEmulator\"",
            UKSWI.0
        ),
    });
    let probe = run_built(
        &[
            ("Mk", &program(&Build::plain("B tail\n"))),
            ("Exits", &exits),
        ],
        "Run HostFS:$.CTest probe args",
        &[],
    );
    assert!(
        disc_file(&probe.disc, "CRes").is_none(),
        "the client never ran"
    );
}
