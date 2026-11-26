//! Escapes, events and upcalls in a C client, held to what the same probe
//! programs recorded on RISC OS 5.30: an escape searched for among the
//! languages' event procedures as a trap is, a real event given to the
//! first event procedure found after each fast event procedure saw it, no
//! event handed on, every upcall passed on, and a new application's upcall
//! finalising the program and putting its parent's handlers back.

mod common;

use common::{run, Build, Probe, PRELUDE_ERROR};

/// An escape (`OS_Byte` 125), event 9 with R1 &1234 and R2 &5678, and upcall &4321, each procedure's calls logged; `RETEV` the event procedures' answer.
const DISPATCHED: &str = r#"
LDR r1, Lclr
ADD r2, r1, #&1000
MOV r3, #0
.clrl STR r3, [r1], #4
CMP r1, r2
BLT clrl
MOV r0, #125
SWI "XOS_Byte"
MOV r0, #1
STR r0, [r11, #0]
BL kvec+4*43
STR r0, [r11, #4]
SWI "XOS_ReadEscapeState"
MOVCS r0, #1
MOVCC r0, #0
STR r0, [r11, #8]
MOV r0, #124
SWI "XOS_Byte"
MOV r0, #14
MOV r1, #9
SWI "XOS_Byte"
MOV r0, #9
LDR r1, L1234
LDR r2, L5678
SWI "XOS_GenerateEvent"
STR r0, [r11, #12]
MOV r0, #13
MOV r1, #9
SWI "XOS_Byte"
LDR r0, Lup
SWI "XOS_UpCall"
STR r0, [r11, #16]
BL kvec+4*43
STR r0, [r11, #20]
B tail
.Lclr DCD res+2048
.L1234 DCD &1234
.L5678 DCD &5678
.Lup DCD &4321
.myev LDR r12, Lelog
B evlog
.myuev LDR r12, Lulog
.evlog LDR r2, [r12]
ADD r3, r12, #4
MOV r2, r2, LSL #7
ADD r3, r3, r2
STMIA r3!, {r0, r1, r10, r11, r13, r14}
MOV r2, #0
.evc LDR r0, [r1, r2]
STR r0, [r3, r2]
ADD r2, r2, #4
CMP r2, #64
BLT evc
LDR r2, [r12]
ADD r2, r2, #1
STR r2, [r12]
MOV r0, #RETEV
MOV pc, r14
.Lelog DCD res+2048
.Lulog DCD res+2560
.myfast LDR r12, Lflog
LDR r2, [r12]
ADD r3, r12, #4
MOV r2, r2, LSL #6
ADD r3, r3, r2
STMIA r3!, {r0, r13, r14}
MOV r2, #0
.fc LDR r1, [r0, r2]
STR r1, [r3, r2]
ADD r2, r2, #4
CMP r2, #40
BLT fc
LDR r12, Lflog
LDR r2, [r12]
ADD r2, r2, #1
STR r2, [r12]
MOV pc, r14
.Lflog DCD res+3072
"#;

/// An event declined by the language's procedure, then a new application's upcall, the finalisation procedure logging how it was entered; `RETEV` as above.
const FINALISED: &str = r#"
LDR r1, Lclr
ADD r2, r1, #&1000
MOV r3, #0
.clrl STR r3, [r1], #4
CMP r1, r2
BLT clrl
LDR r1, Lclr2
ADD r2, r1, #&400
.clr2 STR r3, [r1], #4
CMP r1, r2
BLT clr2
MOV r0, #14
MOV r1, #9
SWI "XOS_Byte"
MOV r0, #9
LDR r1, L1111
SWI "XOS_GenerateEvent"
LDR r12, Lehl2
LDR r0, [r12]
STR r0, [r11, #0]
MOV r0, #13
MOV r1, #9
SWI "XOS_Byte"
LDR r12, Lfinl
LDR r0, [r12]
STR r0, [r11, #8]
MOV r9, r13
SUB r13, r13, #&400
MOV r0, #256
SWI "XOS_UpCall"
MOV r13, r9
STR r0, [r11, #12]
LDR r12, Lfinl
LDR r0, [r12]
STR r0, [r11, #16]
MOV r7, #0
LDR r8, Lhand2
.hl2 MOV r0, r7
MOV r1, #0
MOV r2, #0
MOV r3, #0
SWI "XOS_ChangeEnvironment"
STMIA r8!, {r1-r3}
ADD r7, r7, #1
CMP r7, #17
BLT hl2
B tail
.Lclr DCD res+2048
.Lclr2 DCD res+3584
.Lehl2 DCD res+3584
.Lfinl DCD res+3500
.Lhand2 DCD res+4096
.L1111 DCD &1111
.myev LDR r12, Lelog
LDR r2, [r12]
ADD r2, r2, #1
STR r2, [r12]
ADD r12, r12, r2, LSL #3
STMIA r12, {r0, r1}
MOV r0, #RETEV
MOV pc, r14
.Lelog DCD res+2048
.myfin LDR r12, Lfinl2
LDR r1, [r12]
ADD r1, r1, #1
STR r1, [r12]
ADD r12, r12, r1, LSL #4
STMIA r12, {r10, r11, r13, r14}
MOV pc, r14
.Lfinl2 DCD res+3500
"#;

/// An event handler set before `LibInit` that counts what reaches it.
const EVENT_HANDLER: &str = r#"
MOV r0, #10
ADR r1, myevh
MOV r2, #0
MOV r3, #0
SWI "XOS_ChangeEnvironment"
B evh_done
.myevh STMFD r13!, {r0-r3, r12}
LDR r12, Lehlog
LDR r3, [r12]
ADD r3, r3, #1
STR r3, [r12]
ADD r12, r12, r3, LSL #4
STMIA r12, {r0, r1, r2}
LDMFD r13!, {r0-r3, r12}
MOV pc, r14
.Lehlog DCD res+3584
.evh_done
"#;

/// The handlers in force before `LibInit`, R1 to R3 of each, recorded from
/// `BEFORE`.
const HANDLERS_BEFORE: &str = r#"
MOV r7, #0
LDR r8, Lbefore
.hbefore MOV r0, r7
MOV r1, #0
MOV r2, #0
MOV r3, #0
SWI "XOS_ChangeEnvironment"
STMIA r8!, {r1-r3}
ADD r7, r7, #1
CMP r7, #17
BLT hbefore
B hbefore_done
.Lbefore DCD res+7000
.hbefore_done
"#;
/// Past what the probe's body clears.
const BEFORE: u32 = 7000;
/// The handlers the library sets, which it puts back.
const SET: [u32; 10] = [1, 2, 3, 4, 6, 7, 9, 10, 11, 16];

/// The logs: each event procedure's calls, the fast procedure's, and the
/// finalisation procedure's.
const EVENT_PROC: u32 = 2048;
const UNHANDLED: u32 = 2560;
const FAST: u32 = 3072;

/// The escape's code.
const ESCAPE: u32 = u32::MAX;

/// The procedures the dispatch probe's language gives.
const DISPATCHING: [&str; 8] = [
    "finproc", "trapproc", "uncaught", "myev", "myuev", "myfast", "0", "0",
];

fn dispatched(answer: u32) -> Probe {
    let body = DISPATCHED.replace("RETEV", &answer.to_string());
    run(&Build {
        procedures: &DISPATCHING,
        ..Build::trapping(&body)
    })
}

/// Event procedure call `n` logged at `log`: R0, R1, SL, FP, SP and LR, then
/// the set.
fn call(probe: &Probe, log: u32, n: u32) -> Vec<u32> {
    probe.words(log + 4 + 128 * n, 22)
}

#[test]
fn an_escape_is_offered_to_each_event_procedure_then_the_unhandled_ones() {
    let probe = dispatched(0);
    assert_eq!(probe.word(EVENT_PROC), 2, "the escape's, then the event's");
    assert_eq!(probe.word(UNHANDLED), 1, "the escape's only");
    let escape = call(&probe, EVENT_PROC, 0);
    assert_eq!(escape[0], ESCAPE);
    assert_eq!(escape[4], escape[1] - 100, "run below the set");
    let set = &escape[6..22];
    assert_eq!(
        escape[1],
        set[13] - 68,
        "the set 68 below the interrupted SP"
    );
    assert_eq!(set[0], 125, "the registers the SWI left");
    let unhandled = call(&probe, UNHANDLED, 0);
    assert_eq!(unhandled[..2], escape[..2]);
    assert_eq!(probe.out(8), 0, "the escape acknowledged");
    assert_eq!(
        probe.out(4),
        1,
        "an escape seen, as the PRM has it; RISC OS 5.30 answered 0"
    );
}

#[test]
fn a_willing_event_procedure_takes_an_escape_from_the_unhandled_ones() {
    let probe = dispatched(1);
    assert_eq!(probe.word(UNHANDLED), 0);
}

#[test]
fn an_event_reaches_each_fast_procedure_then_the_first_event_procedure_found() {
    let probe = dispatched(0);
    assert_eq!(probe.word(FAST), 1);
    let fast = probe.words(FAST + 4, 13);
    assert_eq!(
        fast[3..6],
        [9, 0x1234, 0x5678],
        "R0 to R2 as the event came"
    );
    let event = call(&probe, EVENT_PROC, 1);
    assert_eq!(event[0], 9);
    assert_eq!(probe.out(12), 9, "OS_GenerateEvent's R0 back");
}

#[test]
fn every_other_upcall_is_passed_on() {
    let probe = dispatched(0);
    assert_eq!(probe.out(16), 0x4321);
}

fn finalised(answer: u32) -> Probe {
    let body = FINALISED.replace("RETEV", &answer.to_string());
    run(&Build {
        procedures: &["myfin", "0", "0", "myev", "0", "0", "0", "0"],
        prelude: &format!("{PRELUDE_ERROR}{EVENT_HANDLER}{HANDLERS_BEFORE}"),
        extra: "",
        ..Build::plain(&body)
    })
}

#[test]
fn an_event_no_procedure_takes_goes_to_no_handler() {
    for answer in [0, 1] {
        let probe = finalised(answer);
        assert_eq!(probe.word(2048), 1, "the event procedure entered");
        assert_eq!(probe.out(0), 0, "the handler in force before never");
    }
}

#[test]
fn a_new_application_s_upcall_finalises_on_the_root_chunk_s_top_and_is_passed_on() {
    let probe = finalised(0);
    assert_eq!(
        (probe.out(8), probe.out(16)),
        (0, 1),
        "finalised once, by the upcall"
    );
    assert_eq!(probe.out(12), 256, "passed on, unclaimed");
    let entered = probe.words(3500 + 16, 4);
    let root = probe.word(4);
    assert_eq!(
        entered[..3],
        [root + 560, 0, root + 0x1000 - 4],
        "SL, FP and SP"
    );
    let (after, before) = (probe.words(4096, 51), probe.words(BEFORE, 51));
    // A word of nought is one `OS_ChangeEnvironment` leaves as it stands.
    let differing: Vec<(u32, &[u32], &[u32])> = SET
        .into_iter()
        .map(|handler| {
            let at = 3 * handler as usize;
            (handler, &before[at..at + 3], &after[at..at + 3])
        })
        .filter(|(_, before, after)| before.iter().zip(*after).any(|(&b, &a)| b != 0 && a != b))
        .collect();
    assert!(differing.is_empty(), "not put back: {differing:x?}");
    assert_eq!(
        after[3 * 7 + 1],
        probe.word(64 + 4 * 9),
        "a callback handler's R12 of nought left the library's"
    );
}

#[test]
fn an_escape_with_no_stack_to_run_on_reaches_no_procedure_but_is_acknowledged() {
    let body = DISPATCHED.replace("RETEV", "0").replacen(
        "MOV r0, #125\nSWI \"XOS_Byte\"\n",
        "MOV r9, r10\nMOV r10, #0\nMOV r0, #125\nSWI \"XOS_Byte\"\nMOV r10, r9\n",
        1,
    );
    let probe = run(&Build {
        procedures: &DISPATCHING,
        ..Build::trapping(&body)
    });
    assert_eq!(probe.word(UNHANDLED), 0, "no unhandled procedure for it");
    assert_eq!(probe.word(EVENT_PROC), 1, "only the event's");
    assert_eq!(probe.out(8), 0, "acknowledged all the same");
}
