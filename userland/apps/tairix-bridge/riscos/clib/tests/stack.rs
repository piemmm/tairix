//! Stack extension, the heap, and unwinding in a C client, held to what the
//! same probe programs recorded on RISC OS 5.30: chunks claimed through the
//! default allocator or a client's, one kept in hand, the rest freed as the
//! frames that overflowed return; `_kernel_alloc`'s blocks; frames unwound
//! through the library's own record of an extension; and a language's
//! unwind and name procedures entered for the PC they hold.

mod common;

use common::{program, run, run_program, Build, Probe, PRELUDE_ERROR};

/// The chunk mark.
const MARK: u32 = 0xF606_90FF;
/// The extension stack's bytes.
const EXTENSION_STACK: u32 = 159 * 4;

/// A snapshot's chunk: its address and first seven words.
fn chunk(probe: &Probe, at: u32) -> [u32; 8] {
    let words = probe.words(at, 8);
    [
        words[0], words[1], words[2], words[3], words[4], words[5], words[6], words[7],
    ]
}

/// The chunks a snapshot walked from `at`, up to the first empty one.
fn chain(probe: &Probe, at: u32) -> Vec<[u32; 8]> {
    (0..12)
        .map(|n| chunk(probe, at + 32 * n))
        .take_while(|chunk| chunk[1] == MARK)
        .collect()
}

/// Recursion 200 deep twice with the default allocator and 300 deep twice
/// with a client's, snapshots at the deepest; `_kernel_alloc`; a big frame.
/// The heap's pointer is read where these statics keep it, &74.
const DEEP: &str = r"
MOV r9, r10
LDR r1, Lclr
ADD r2, r1, #&1000
MOV r3, #0
.clrl STR r3, [r1], #4
CMP r1, r2
BLT clrl
LDR r6, LsA1
MOV r0, #200
BL deep
LDR r6, LsA2
MOV r0, #200
BL deep
SUB r0, r9, #560
LDR r1, LcA
BL walk
LDR r12, Lkst
LDR r1, [r12, #&74]
STR r1, [r11, #0]
MOV r0, #100
ADD r1, r11, #8
BL kvec+4*26
STR r0, [r11, #4]
LDR r12, Lkst
LDR r1, [r12, #&74]
STR r1, [r11, #12]
MOV r0, #&2000
ADD r1, r11, #20
BL kvec+4*26
STR r0, [r11, #16]
LDR r12, Lkst
LDR r1, [r12, #&74]
STR r1, [r11, #24]
LDR r0, Lmya
LDR r1, Lmyf
BL kvec+4*25
LDR r6, LsB1
MOV r0, #300
BL deep
LDR r6, LsB2
MOV r0, #300
BL deep
SUB r0, r9, #560
LDR r1, LcB
BL walk
BL big
LDR r12, Lkst
LDR r1, [r12, #&74]
STR r1, [r11, #28]
SUB r0, r9, #560
LDR r1, LcC
BL walk
B tail
.Lclr DCD res+2048
.LsA1 DCD res+2048
.LsA2 DCD res+2448
.LcA DCD res+2848
.LsB1 DCD res+3232
.LsB2 DCD res+3632
.LcB DCD res+4032
.LcC DCD res+5600
.Lkst DCD kstat
.Lmya DCD myalloc
.Lmyf DCD myfree
.deep MOV r12, r13
STMFD r13!, {r11, r12, r14, pc}
SUB r11, r12, #4
CMP r13, r10
BLLT kvec+4*27
SUB r13, r13, #64
SUBS r0, r0, #1
BEQ deepend
BL deep
LDMDB r11, {r11, r13, pc}
.deepend BL snap
LDMDB r11, {r11, r13, pc}
.snap STR r10, [r6, #0]
STR r13, [r6, #4]
STR r11, [r6, #8]
LDR r12, Lkst2
LDR r1, [r12, #&74]
STR r1, [r6, #12]
SUB r1, r10, #560
ADD r2, r6, #16
MOV r3, #12
MOV r5, #8
B chunks
.Lkst2 DCD kstat
.walk MOV r2, r1
MOV r1, r0
MOV r3, #12
MOV r5, #4
.chunks STR r1, [r2], #4
LDR r4, [r1, #0]
STR r4, [r2], #4
LDR r4, [r1, #4]
STR r4, [r2], #4
LDR r4, [r1, #8]
STR r4, [r2], #4
LDR r4, [r1, #12]
STR r4, [r2], #4
LDR r4, [r1, #16]
STR r4, [r2], #4
LDR r4, [r1, #20]
STR r4, [r2], #4
LDR r4, [r1, #24]
STR r4, [r2], #4
LDR r1, [r1, r5]
CMP r1, #0
SUBNES r3, r3, #1
BNE chunks
MOV pc, r14
.big MOV r12, r13
STMFD r13!, {r11, r12, r14, pc}
SUB r11, r12, #4
SUB r12, r13, #&2000
CMP r12, r10
BLLT kvec+4*28
SUB r13, r13, #&2000
LDR r12, Lbig
STR r10, [r12, #0]
STR r13, [r12, #4]
STR r11, [r12, #8]
SUB r1, r10, #560
STR r1, [r12, #12]
LDR r2, [r1, #12]
STR r2, [r12, #16]
LDR r2, [r1, #16]
STR r2, [r12, #20]
LDR r2, [r11, #-4]
STR r2, [r12, #24]
LDR r2, [r11, #-8]
STR r2, [r12, #28]
LDMDB r11, {r11, r13, pc}
.Lbig DCD res+5448
.myalloc LDR r12, Lalog
LDR r1, [r12]
ADD r2, r12, #4
ADD r2, r2, r1, LSL #5
STMIA r2, {r0, r10, r11, r13, r14}
ADD r1, r1, #1
STR r1, [r12]
ADR r12, poolv
LDR r1, [r12]
ADD r3, r1, r0
ADD r3, r3, #7
BIC r3, r3, #7
STR r3, [r12]
STR r1, [r2, #20]
MOV r0, r1
MOV pc, r14
.poolv DCD &100000
.Lalog DCD res+4416
.myfree LDR r12, Lflog
LDR r1, [r12]
ADD r2, r12, #4
ADD r2, r2, r1, LSL #5
STMIA r2, {r0, r10, r11, r13, r14}
ADD r1, r1, #1
STR r1, [r12]
MOV pc, r14
.Lflog DCD res+4932
";

/// Three frames, the second overflowing into a new chunk, the third unwinding the stack a frame at a time; names asked for.
const UNWOUND: &str = r#"
LDR r1, Lclr
ADD r2, r1, #&1000
MOV r3, #0
.clrl STR r3, [r1], #4
CMP r1, r2
BLT clrl
LDR r6, Lrec
SUB r1, r10, #560
LDR r2, [r1, #12]
ADD r1, r1, r2
SUB r1, r1, #16
ADD r2, r6, #1024
MOV r3, #16
.pz LDR r4, [r1], #4
STR r4, [r2], #4
SUBS r3, r3, #1
BNE pz
MOV r9, r13
MOV r11, #0
ADD r13, r10, #24
MOV r4, #&31
MOV r5, #&32
BL f_one
MOV r13, r9
LDR r11, Lout3
ADR r0, f_one
STR r0, [r6, #1100]
BL kvec+4*5
STR r0, [r6, #1104]
ADR r0, f_one
ADD r0, r0, #8
BL kvec+4*5
STR r0, [r6, #1108]
ADR r0, f_three
ADD r0, r0, #60
BL kvec+4*5
STR r0, [r6, #1112]
LDR r0, Lstart
BL kvec+4*5
STR r0, [r6, #1116]
ADR r0, f_two
SUB r0, r0, #4
BL kvec+4*5
STR r0, [r6, #1120]
ADR r0, nm_one
STR r0, [r6, #1124]
ADR r0, nm_three
STR r0, [r6, #1128]
LDR r0, Llname
STR r0, [r6, #1132]
B tail
.Lclr DCD res+2048
.Lrec DCD res+2048
.Lout3 DCD res+400
.Lstart DCD &8000
.Llname DCD langname
.nm_one EQUS "f_one"
EQUB 0
ALIGN
DCD &FF000008
.f_one MOV r12, r13
STMFD r13!, {r11, r12, r14, pc}
SUB r11, r12, #4
CMP r13, r10
BLLT kvec+4*27
BL f_two
LDMDB r11, {r11, r13, pc}
.nm_two EQUS "f_two"
EQUB 0
ALIGN
DCD &FF000008
.f_two MOV r12, r13
STMFD r13!, {r4, r5, r11, r12, r14, pc}
SUB r11, r12, #4
CMP r13, r10
BLLT kvec+4*27
MOV r4, #&41
MOV r5, #&42
BL f_three
LDMDB r11, {r4, r5, r11, r13, pc}
.nm_three EQUS "f_three"
EQUB 0
ALIGN
DCD &FF000008
.f_three MOV r12, r13
STMFD r13!, {r11, r12, r14, pc}
SUB r11, r12, #4
CMP r13, r10
BLLT kvec+4*27
SUB r13, r13, #96
STMIA r13, {r4-r9}
STR r11, [r13, #24]
ADD r0, r13, #0
STR r0, [r13, #28]
ADR r0, f3pc
STR r0, [r13, #32]
STR r10, [r13, #36]
MOV r0, #0
MOV r1, #40
.fz STR r0, [r13, r1]
ADD r1, r1, #4
CMP r1, #88
BLT fz
.f3pc STR r10, [r6, #0]
STR r11, [r6, #4]
STR r13, [r6, #8]
MOV r7, #0
.uw MOV r0, r13
ADD r1, r13, #88
BL kvec+4*4
ADD r2, r6, #16
ADD r2, r2, r7, LSL #7
STR r0, [r2], #4
LDR r1, [r13, #88]
STR r1, [r2], #4
MOV r3, #0
.uc LDR r1, [r13, r3]
STR r1, [r2, r3]
ADD r3, r3, #4
CMP r3, #88
BLT uc
ADD r7, r7, #1
CMP r0, #0
BLE uwd
CMP r7, #6
BLT uw
.uwd STR r7, [r6, #12]
ADD r13, r13, #96
LDMDB r11, {r11, r13, pc}
"#;

/// A language with unwind and name procedures, asked to name and to unwind.
const NAMED: &str = r#"
LDR r1, Lclr
ADD r2, r1, #&1000
MOV r3, #0
.clrl STR r3, [r1], #4
CMP r1, r2
BLT clrl
LDR r6, Lrec
ADR r0, f_a
BL kvec+4*5
STR r0, [r6, #0]
LDR r0, Lrom
BL kvec+4*5
STR r0, [r6, #4]
LDR r0, Lstart2
BL kvec+4*5
STR r0, [r6, #8]
MOV r11, #0
BL f_a
LDR r11, Lout4
B tail
.Lclr DCD res+2048
.Lrec DCD res+2048
.Lout4 DCD res+400
.Lrom DCD &FC000000
.Lstart2 DCD &8000
.f_a MOV r12, r13
STMFD r13!, {r4, r5, r11, r12, r14, pc}
SUB r11, r12, #4
SUB r13, r13, #96
MOV r4, #&41
STMIA r13, {r4-r9}
STR r11, [r13, #24]
ADD r0, r13, #0
STR r0, [r13, #28]
ADR r0, fapc
STR r0, [r13, #32]
STR r10, [r13, #36]
MOV r0, #0
MOV r1, #40
.fz STR r0, [r13, r1]
ADD r1, r1, #4
CMP r1, #88
BLT fz
.fapc MOV r0, r13
ADD r1, r13, #88
MOV r2, #&5A
STR r2, [r1]
BL kvec+4*4
STR r0, [r6, #12]
LDR r1, [r13, #88]
STR r1, [r6, #16]
ADD r2, r6, #20
MOV r3, #0
.uc LDR r1, [r13, r3]
STR r1, [r2, r3]
ADD r3, r3, #4
CMP r3, #88
BLT uc
ADD r13, r13, #96
LDMDB r11, {r4, r5, r11, r13, pc}
.myname LDR r12, Lnrec
LDR r2, [r12]
ADD r3, r12, #4
ADD r3, r3, r2, LSL #5
STMIA r3, {r0, r1, r10, r11, r13, r14}
ADD r2, r2, #1
STR r2, [r12]
ADR r0, nmstr
MOV pc, r14
.nmstr EQUS "named"
EQUB 0
ALIGN
.Lnrec DCD res+2560
.myunwind LDR r12, Lurec
LDR r2, [r12]
ADD r3, r12, #4
ADD r3, r3, r2, LSL #5
STMIA r3, {r0, r1, r10, r11, r13, r14}
LDR r1, [r0, #32]
STR r1, [r3, #24]
LDR r1, [r0, #24]
STR r1, [r3, #28]
ADD r2, r2, #1
STR r2, [r12]
MVN r0, #6
MOV pc, r14
.Lurec DCD res+3072
"#;

/// The deep probe's records.
const SNAP_A1: u32 = 2048;
const SNAP_A2: u32 = 2448;
const CHAIN_A: u32 = 2848;
const SNAP_B1: u32 = 3232;
const CHAIN_B: u32 = 4032;
const ALLOCS: u32 = 4416;
const FREES: u32 = 4932;
const BIG: u32 = 5448;
const CHAIN_C: u32 = 5600;

#[test]
fn the_default_allocator_claims_4_kib_chunks_upward_and_never_reuses_a_freed_one() {
    let probe = run(&Build::plain(DEEP));
    let root = probe.word(4);
    let top = root + 0x1000;
    let first = chain(&probe, SNAP_A1 + 16);
    assert_eq!(first.len(), 5, "four chunks past the root at the deepest");
    let extensions: Vec<_> = first.iter().rev().skip(1).collect();
    for (n, chunk) in (0..).zip(&extensions) {
        assert_eq!(
            chunk[0],
            top + EXTENSION_STACK + 0x1000 * n,
            "chunk {n} claimed upward"
        );
        assert_eq!(chunk[4], 0x1000);
        assert_eq!(chunk[6], first[4][6], "the library offset copied");
        assert_eq!(chunk[7], 0, "the client offset copied");
    }
    let freer = extensions[0][5];
    assert!(extensions
        .iter()
        .all(|chunk| chunk[5] == freer && freer != 0));
    assert_eq!(
        probe.word(SNAP_A1 + 12),
        top + EXTENSION_STACK + 0x4000,
        "the heap past them"
    );
    let second = chain(&probe, SNAP_A2 + 16);
    assert_eq!(
        second[3][0], extensions[0][0],
        "the chunk kept in hand reused"
    );
    assert_eq!(
        second[2][0],
        top + EXTENSION_STACK + 0x4000,
        "the freed ones not"
    );
    let after = chain(&probe, CHAIN_A);
    assert_eq!(after.len(), 2, "one chunk kept in hand");
    assert_eq!(after[1][0], extensions[0][0]);
    assert_eq!(after[1][2], 0, "nothing past it");
}

#[test]
fn alloc_claims_at_least_2048_words_from_the_heap() {
    let probe = run(&Build::plain(DEEP));
    let heap = probe.out(0);
    assert_eq!(
        [probe.out(4), probe.out(8), probe.out(12)],
        [2048, heap, heap + 0x2000]
    );
    assert_eq!(
        [probe.out(16), probe.out(20), probe.out(24)],
        [0x2000, heap + 0x2000, heap + 0xA000]
    );
}

#[test]
fn a_client_s_allocator_is_asked_on_the_extension_stack_and_its_freer_remembered() {
    let probe = run(&Build::plain(DEEP));
    let top = probe.word(4) + 0x1000;
    let allocs = probe.word(ALLOCS);
    assert_eq!(allocs, 11, "ten 4 KiB chunks, then the big frame's");
    for n in 0..allocs {
        let call = probe.words(ALLOCS + 4 + 32 * n, 6);
        let size = if n == 10 { 0x2230 } else { 0x1000 };
        assert_eq!(call[0], size, "call {n}'s size");
        assert_eq!(
            (call[1], call[2]),
            (top + 560, 0),
            "SL above the extension stack, no frame"
        );
        assert_eq!(
            call[3],
            top + EXTENSION_STACK - 20,
            "five words below its top"
        );
    }
    let deepest = chain(&probe, SNAP_B1 + 16);
    let client_freer = probe.statics_word(0x2F0);
    assert!(deepest
        .iter()
        .take(5)
        .all(|chunk| chunk[5] != 0 && chunk[5] != deepest[5][5]));
    assert_ne!(
        deepest[0][5], deepest[5][5],
        "a client chunk remembers the client's freer, {client_freer:X}"
    );
    let frees = probe.word(FREES);
    assert_eq!(frees, 10, "each chunk past the one kept in hand freed");
    let first_freed: Vec<u32> = (0..5).map(|n| probe.word(FREES + 4 + 32 * n)).collect();
    assert_eq!(
        first_freed,
        [0x10_4000, 0x10_3000, 0x10_2000, 0x10_1000, 0x10_0000]
    );
    let returned_to = probe.word(FREES + 4 + 4);
    assert_eq!(returned_to, 0x10_2000 + 560, "SL the chunk returned to");
    let after = chain(&probe, CHAIN_B);
    assert_eq!(after.len(), 2);
}

#[test]
fn a_big_frame_gets_a_chunk_just_big_enough_and_returns_through_the_library() {
    let probe = run(&Build::plain(DEEP));
    let big = probe.words(BIG, 8);
    let [sl, sp, fp, chunk, size] = [big[0], big[1], big[2], big[3], big[4]];
    assert_eq!(
        size,
        0x2000 + 560,
        "the frame and the limit's distance, no more"
    );
    assert_eq!((sl, sp), (chunk + 560, chunk + 560), "SP lands on SL");
    assert_eq!(
        big[7],
        chunk + size,
        "the frame's saved SP the new chunk's top"
    );
    assert_ne!(big[6], 0, "its saved return the library's");
    assert!(fp < chunk || fp > chunk + size, "FP left in the old chunk");
    let kept = chain(&probe, CHAIN_C);
    assert_eq!(kept.len(), 2);
    assert_eq!(
        (kept[1][0], kept[1][4]),
        (chunk, size),
        "the big chunk kept in hand"
    );
}

#[test]
fn the_extension_stack_lies_at_the_root_s_top_like_a_chunk() {
    let probe = run(&Build::plain(UNWOUND));
    let base = probe.word(4) + 0x1000;
    let words = probe.words(2048 + 1024 + 16, 7);
    assert_eq!(
        words,
        [base, 0, 0, EXTENSION_STACK, 0, probe.word(800 + 20), 0]
    );
}

#[test]
fn unwinding_walks_each_frame_and_through_the_library_s_record_of_an_extension() {
    let probe = run(&Build::plain(UNWOUND));
    let record = |n: u32| probe.words(2048 + 16 + 128 * n, 24);
    assert_eq!(probe.word(2048 + 12), 5, "four frames, then the end");
    let [deepest_sl, deepest_fp] = [probe.word(2048), probe.word(2048 + 4)];
    let language = probe.word(2048 + 1132);
    let first = record(0);
    assert_eq!((first[0], first[1]), (1, language));
    assert_eq!(first[2..4], [0x41, 0x42], "R4 and R5 as f_three left them");
    assert_eq!(first[11], deepest_sl);
    let second = record(1);
    assert_eq!(
        (second[0], second[1]),
        (1, 0),
        "the library's routine in no language"
    );
    assert_eq!(second[2..4], [0x31, 0x32], "f_two's saved R4 and R5");
    assert_eq!(
        second[8],
        first[8] - 104,
        "FP its record below f_two's frame"
    );
    let third = record(2);
    assert_eq!((third[0], third[1]), (1, language));
    let root_sl = probe.word(4) + 560;
    assert_eq!(third[11], root_sl, "SL back in the root chunk");
    let fourth = record(3);
    assert_eq!(
        (fourth[0], fourth[8]),
        (1, 0),
        "f_one's caller had no frame"
    );
    assert_eq!(record(4)[0], 0, "the stack's end");
    assert_ne!(deepest_fp, 0);
}

#[test]
fn without_a_name_procedure_no_procedure_is_named() {
    let probe = run(&Build::plain(UNWOUND));
    assert_eq!(probe.words(2048 + 1104, 5), [0; 5]);
}

#[test]
fn a_language_s_name_and_unwind_procedures_are_entered_as_the_calls_continuations() {
    let build = Build {
        procedures: &[
            "finproc", "trapproc", "uncaught", "0", "0", "0", "myunwind", "myname",
        ],
        ..Build::trapping(NAMED)
    };
    let probe = run(&build);
    let named = probe.word(2048);
    assert_ne!(named, 0, "the name procedure's answer");
    assert_eq!(probe.word(2048 + 4), 0, "a PC in no language");
    assert_eq!(
        probe.word(2048 + 8),
        named,
        "the code's start in the language"
    );
    let calls = probe.word(2560);
    assert_eq!(calls, 2, "asked for both PCs in the language");
    let call = probe.words(2564, 6);
    assert_eq!(call[0], call[1], "the PC in R0 and R1");
    assert_eq!(
        probe.word(2048 + 12),
        (-7_i32).cast_unsigned(),
        "the unwind procedure's answer"
    );
    let unwinding = probe.words(3076, 6);
    assert_eq!(probe.word(3072), 1);
    assert_eq!(
        unwinding[0], unwinding[4],
        "the block in R0, below SP as the client left it"
    );
}

/// Recursion 300 deep after `SETUP`, a chunk's claims logged.
const OVERFLOWING: &str = r"
LDR r1, Lclr
ADD r2, r1, #&1000
MOV r3, #0
.clrl STR r3, [r1], #4
CMP r1, r2
BLT clrl
SETUP
MOV r0, #1
STR r0, [r11, #0]
MOV r0, #300
MOV r11, #0
BL deep
LDR r11, Lout2
MOV r0, #2
STR r0, [r11, #0]
B tail
.Lout2 DCD res+400
.Lclr DCD res+2048
.Lkst DCD kstat
.Lmya DCD myalloc
.Lmyf DCD myfree
.deep MOV r12, r13
STMFD r13!, {r11, r12, r14, pc}
SUB r11, r12, #4
CMP r13, r10
BLLT kvec+4*27
SUB r13, r13, #64
SUBS r0, r0, #1
BEQ deepend
BL deep
.deepend LDMDB r11, {r11, r13, pc}
.myalloc LDR r12, Lalog
LDR r1, [r12]
ADD r2, r12, #4
ADD r2, r2, r1, LSL #5
STMIA r2, {r0, r10, r11, r13, r14}
ADD r1, r1, #1
STR r1, [r12]
MOV r0, #0
MOV pc, r14
.Lalog DCD res+4416
.myfree MOV pc, r14
";

/// Allocation past a heap `_kernel_init` was told ends &40000 above the workspace, a slot-extension procedure registered and logging any call; the heap's pointer and limit read where these statics keep them, &74 and &2E0.
const SLOT: &str = r"
LDR r1, Lclr
ADD r2, r1, #&1000
MOV r3, #0
.clrl STR r3, [r1], #4
CMP r1, r2
BLT clrl
LDR r12, Lkst
LDR r1, [r12, #&74]
STR r1, [r11, #0]
LDR r1, [r12, #&2E0]
STR r1, [r11, #4]
LDR r0, Lmye
BL kvec+4*46
STR r0, [r11, #8]
LDR r0, Lmye
BL kvec+4*46
STR r0, [r11, #12]
MOV r0, #&10000
ADD r1, r11, #20
BL kvec+4*26
STR r0, [r11, #16]
LDR r12, Lkst
LDR r1, [r12, #&74]
STR r1, [r11, #24]
LDR r1, [r12, #&2E0]
STR r1, [r11, #28]
MOV r0, #&10000
ADD r1, r11, #36
BL kvec+4*26
STR r0, [r11, #32]
LDR r12, Lkst
LDR r1, [r12, #&74]
STR r1, [r11, #40]
LDR r1, [r12, #&2E0]
STR r1, [r11, #44]
MOV r0, #100
ADD r1, r11, #52
BL kvec+4*26
STR r0, [r11, #48]
LDR r12, Lkst
LDR r1, [r12, #&74]
STR r1, [r11, #56]
LDR r1, [r12, #&2E0]
STR r1, [r11, #60]
B tail
.Lclr DCD res+2048
.Lkst DCD kstat
.Lmye DCD myext
.myext LDR r12, Lelog
LDR r2, [r12]
ADD r3, r12, #4
ADD r3, r3, r2, LSL #5
STMIA r3, {r0, r1, r10, r11, r13, r14}
ADD r2, r2, #1
STR r2, [r12]
MOV r2, #0
CMP r2, #0
MOVEQ r0, #0
MOVEQ pc, r14
LDR r12, Lkst3
LDR r2, [r12, #&2E0]
STR r2, [r1]
STR r2, [r3, #24]
MOV pc, r14
.Lelog DCD res+4416
.Lkst3 DCD kstat
.Lwsend DCD ws+&40000
";

/// A Pascal-style procedure with four register and two stacked arguments,
/// called with the stack just above its limit so the frame it pushes
/// overflows: what it finds after `_kernel_stkovf_copyargs`, and what its
/// caller finds after it returns.
const PASCAL: &str = r"
MOV r9, r13
MOV r8, r11
LDR r6, Lrec
ADD r13, r10, #16
MOV r4, #&55
MOV r5, #&66
STMFD r13!, {r4, r5}
STR r13, [r6, #420]
MOV r0, #&11
MOV r1, #&22
MOV r2, #&33
MOV r3, #&44
MOV r4, #&A4
MOV r5, #&A5
MOV r11, #0
BL pasn
STR r0, [r6, #400]
STR r4, [r6, #404]
STR r5, [r6, #408]
STR r10, [r6, #412]
STR r13, [r6, #416]
MOV r13, r9
MOV r11, r8
B tail
.Lrec DCD res+2560
.pasn MOV r12, r13
STMFD r13!, {r0-r3}
STMFD r13!, {r4, r5, r11, r12, r14, pc}
SUB r11, r12, #20
CMP r13, r10
MOVLT r12, #6
BLLT kvec+4*29
STR r10, [r6, #0]
STR r11, [r6, #4]
STR r13, [r6, #8]
ADD r1, r11, #4
LDMIA r1, {r0-r5}
ADD r12, r6, #12
STMIA r12, {r0-r5}
MOV r0, #&77
LDMDB r11, {r4, r5, r11, r13, pc}
";

/// A frame laid by hand as a procedure saving R4, R5 and F4 lays it — the
/// storing instructions in code it points past — unwound.
const FLOATING: &str = r"
LDR r6, Lfblk
LDR r7, Lfp
LDR r0, Lspc
STR r0, [r7]
MOV r0, #0
STR r0, [r7, #-4]
STR r0, [r7, #-8]
STR r0, [r7, #-12]
MOV r0, #&B5
STR r0, [r7, #-16]
MOV r0, #&B4
STR r0, [r7, #-20]
LDR r0, Lf0
STR r0, [r7, #-32]
LDR r0, Lf1
STR r0, [r7, #-28]
LDR r0, Lf2
STR r0, [r7, #-24]
MOV r0, #0
MOV r1, #0
.fz STR r0, [r6, r1]
ADD r1, r1, #4
CMP r1, #92
BLT fz
STR r7, [r6, #24]
LDR r0, Lbody
STR r0, [r6, #32]
STR r10, [r6, #36]
MOV r0, r6
ADD r1, r6, #88
BL kvec+4*4
STR r0, [r11, #0]
B tail
.fakecode STMFD r13!, {r4, r5, r11, r12, r14, pc}
SUB r11, r12, #4
DCD &ED6D4103
.Lspc DCD fakecode+8
.Lfblk DCD res+2048
.Lfp DCD res+3000
.Lbody DCD body
.Lf0 DCD &11111111
.Lf1 DCD &22222222
.Lf2 DCD &33333333
";

/// Stack extension asked for in Supervisor mode, with the application's SL.
const PRIVILEGED: &str = "SWI \"OS_EnterOS\"\nBL kvec+4*27\nSWI \"OS_LeaveOS\"\nB tail\n";

/// The trap a stack that cannot be extended raises.
const STACK_OVERFLOW: u32 = 0x8000_0021;

#[test]
fn a_stack_no_chunk_can_be_had_for_traps_at_the_call() {
    let refusing = OVERFLOWING.replace("SETUP\n", "LDR r0, Lmya\nLDR r1, Lmyf\nBL kvec+4*25\n");
    let probe = run(&Build::trapping(&refusing));
    assert_eq!(probe.path(), 0x10);
    assert_eq!(probe.word(1024), STACK_OVERFLOW);
    assert_eq!(probe.word(4416), 1, "the client's allocator asked once");
    let exhausting = OVERFLOWING.replace(
        "SETUP\n",
        "MVN r0, #&F0000000\nADD r1, r11, #8\nBL kvec+4*26\nSTR r0, [r11, #4]\n",
    );
    let probe = run(&Build::trapping(&exhausting));
    assert_eq!(probe.path(), 0x10);
    assert_eq!(probe.word(1024), STACK_OVERFLOW);
    assert_ne!(probe.out(4), 0, "the heap's last words taken first");
}

#[test]
fn alloc_past_the_heap_gets_what_is_left_and_asks_no_procedure_outside_the_desktop() {
    let text = program(&Build::trapping(SLOT)).replacen("MOV r4, r0\n", "LDR r4, Lwsend\n", 1);
    let probe = run_program(&text, "Run HostFS:$.CTest probe args", &[]);
    let [heap, limit, before, registered] = [0, 4, 8, 12].map(|at| probe.out(at));
    assert_eq!(heap, probe.word(4) + 0x1000 + EXTENSION_STACK);
    assert_eq!(limit, probe.word(4) + 0x4_0000);
    assert_eq!(before, 0, "no procedure registered at first");
    assert_ne!(registered, 0, "the one registered answered back");
    assert_eq!(
        [probe.out(16), probe.out(20)],
        [(limit - heap) / 4, heap],
        "all that was left"
    );
    assert_eq!([probe.out(24), probe.out(28)], [limit, limit]);
    assert_eq!(
        [probe.out(32), probe.out(36)],
        [0, 0],
        "nothing left: no block"
    );
    assert_eq!(probe.word(4416), 0, "the procedure never asked");
}

#[test]
fn a_pascal_style_extension_moves_the_frame_and_its_arguments_and_returns_through_the_library() {
    let probe = run(&Build::plain(PASCAL));
    let [sl, fp, sp] = [0, 4, 8].map(|at| probe.word(2560 + at));
    let chunk = sl - 560;
    let top = chunk + 0x1000;
    assert_eq!(fp, top - 4 - 4 * 6, "the six argument words at the top");
    assert_eq!(sp, fp - 20, "the frame below them");
    assert_eq!(
        probe.words(2560 + 12, 6),
        [0x11, 0x22, 0x33, 0x44, 0x55, 0x66]
    );
    let back = probe.words(2560 + 400, 5);
    assert_eq!(
        back[..3],
        [0x77, 0xA4, 0xA5],
        "its answer, R4 and R5 as they were"
    );
    assert_eq!(back[3], probe.word(4) + 560, "SL back in the root chunk");
    assert_eq!(back[4], probe.word(2560 + 420), "SP at the arguments again");
}

#[test]
fn unwinding_restores_the_floating_point_registers_a_frame_saved() {
    let probe = run(&Build::plain(FLOATING));
    assert_eq!(probe.out(0), 1);
    let block = probe.words(2048, 22);
    assert_eq!(block[..2], [0xB4, 0xB5], "R4 and R5");
    assert_eq!(block[10..13], [0x1111_1111, 0x2222_2222, 0x3333_3333], "F4");
    assert_eq!(block[13..22], [0; 9], "F5 to F7 not saved");
}

#[test]
fn a_privileged_caller_s_overflow_is_an_uncaught_trap_error_at_once() {
    let probe = run(&Build {
        prelude: PRELUDE_ERROR,
        ..Build::plain(PRIVILEGED)
    });
    assert_eq!(probe.path(), 0x50);
    let (number, text) = probe.first_error();
    assert_eq!(number, 0x80_0E01);
    assert!(text.contains("stack could not"), "{text}");
}
