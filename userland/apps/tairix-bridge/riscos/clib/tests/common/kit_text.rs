//! The probe kit: the BBC BASIC builder of a C client that initialises the
//! library through `SharedCLibrary_LibInitAPCS_32` and `_kernel_init`, runs
//! a test's body in its run code with R11 the out area, and saves its
//! results (`CRes`) and kernel statics (`CStat`) to HostFS before
//! `_kernel_exit` — the same programs genuine RISC OS 5.30 ran, so a test
//! holds the library to what was recorded there.
//!
//! The results' layout: +0 `LibInit`'s R0 to R6, +28 its V; +64 the
//! initialisation procedure's R0 to R11, SP and LR; +128 the run
//! procedure's; +192 the 17 handlers' R1 to R3; +400 the out area; +800 the
//! root chunk's first 16 words; +1024 a trap procedure's R0 to R3 and set,
//! +1200 an uncaught trap procedure's; +1296 the phase; +1300 the phase the
//! finalisation procedure saw; +1400 the prelude's error buffer; +1536 the
//! handlers at that error; +1744 the first error's first 12 words; +1792
//! the first path taken.

/// The client's start: `LibInit` and `_kernel_init`, up to its language's
/// description.
pub const START: &str = r#"
.start SWI "OS_GetEnv"
MOV r9, r1
LDR r0, Lstubs
LDR r1, Lws
MOV r2, r9
MVN r3, #0
MOV r4, #0
MVN r5, #0
MOV r6, #&40000
SWI "XSharedCLibrary_LibInitAPCS_32"
LDR r8, Lres
STMIA r8, {r0-r6}
MOV r7, #0
STR r7, [r8, #1792]
STR r7, [r8, #1296]
STR r7, [r8, #1300]
MOVVS r7, #1
MOVVC r7, #0
STR r7, [r8, #28]
MOV r4, r0
ADR r0, kinit
MOV r3, #0
LDR pc, Lkinit
.Lstubs DCD stubs
.Lws DCD ws
.Lres DCD res
.Lkinit DCD kvec
.kinit DCD &8000
DCD lang
DCD langend
"#;

/// The run procedure, after the initialisation procedure: its records and
/// the handlers', then the body.
pub const RUN: &str = r#"
.Lires DCD res+64
.run LDR r12, Lrres
STMIA r12, {r0-r11}
STR r13, [r12, #48]
STR r14, [r12, #52]
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
B body
.Lrres DCD res+128
.Lhand DCD res+192
.Lout DCD res+400
.body LDR r11, Lout
"#;

/// The run code's end: the files saved, then `_kernel_exit`.
pub const TAIL: &str = r#"
LDR r12, Lpm2
LDR r8, [r12]
CMP r8, #0
MOVEQ r8, #&40
STREQ r8, [r12]
B tail
.Lpm2 DCD res+1792
.tail LDR r12, Lres2
MOV r0, #2
STR r0, [r12, #1296]
LDR r12, Lres2
LDR r0, [r12, #4]
ADD r1, r12, #800
MOV r2, #0
.cl LDR r3, [r0, r2]
STR r3, [r1, r2]
ADD r2, r2, #4
CMP r2, #64
BLT cl
MOV r0, #10
ADR r1, nres
LDR r2, Ltype
LDR r4, Lres2
ADD r5, r4, #RESB
SWI "XOS_File"
MOV r0, #10
ADR r1, nstat
LDR r2, Ltype
LDR r4, Lkstat
LDR r5, Lstatend
SWI "XOS_File"
MOV r0, #10
ADR r1, ndone
LDR r2, Ltype
LDR r4, Lres2
ADD r5, r4, #4
SWI "XOS_File"
LDR pc, Lexit
.Lres2 DCD res
.Ltype DCD &FFD
.Lkstat DCD kstat
.Lstatend DCD statend
.Lexit DCD kvec+4
.nres EQUS "HostFS:$.CRes"
EQUB 0
.nstat EQUS "HostFS:$.CStat"
EQUB 0
.ndone EQUS "HostFS:$.Done"
EQUB 0
ALIGN
"#;

/// A finalisation, trap and uncaught trap procedure, each marking the path taken.
pub const TRAPS: &str = r"
.finproc LDR r12, Lfin
LDR r0, [r12, #-4]
STR r0, [r12]
MOV r0, #&30
B pathmark
.Lfin DCD res+1300
.pathmark LDR r12, Lpm
LDR r8, [r12]
CMP r8, #0
STREQ r0, [r12]
B tail
.Lpm DCD res+1792
.trapproc LDR r12, Ltr
MOV r9, #&10
B trapped
.uncaught LDR r12, Lun
MOV r9, #&20
.trapped STMIA r12, {r0-r3}
MOV r2, #0
.tc LDR r3, [r1, r2]
ADD r12, r12, #16
STR r3, [r12, r2]
SUB r12, r12, #16
ADD r2, r2, #4
CMP r2, #64
BLT tc
MOV r0, r9
B pathmark
.Ltr DCD res+1024
.Lun DCD res+1200
";

/// An error handler set before `LibInit`, keeping the first error and the handlers then.
pub const PRELUDE_ERROR: &str = r#"
MOV r0, #6
ADR r1, myerr
LDR r2, Lres3
LDR r3, Lebuf
SWI "XOS_ChangeEnvironment"
B prelude_done
.Lres3 DCD res
.Lebuf DCD res+1400
.myerr MOV r12, r0
LDR r11, Lres4
LDR r8, [r11, #1792]
CMP r8, #0
BNE myerr2
MOV r8, #&50
STR r8, [r11, #1792]
ADD r8, r11, #1392
ADD r8, r8, #8
ADD r9, r11, #1744
MOV r7, #12
.myerr1 LDR r6, [r8], #4
STR r6, [r9], #4
SUBS r7, r7, #1
BNE myerr1
.myerr2
LDR r11, Lres4
MOV r7, #0
ADD r8, r11, #1536
.mh MOV r0, r7
MOV r1, #0
MOV r2, #0
MOV r3, #0
SWI "XOS_ChangeEnvironment"
STMIA r8!, {r1-r3}
ADD r7, r7, #1
CMP r7, #17
BLT mh
B tail
.Lres4 DCD res
.prelude_done
"#;

/// The stub description: chunk 1, the vector and statics the builder lays.
pub const STUBS: &str = r"
.codeend
.stubs DCD 1
DCD kvec
DCD kvec+4*48
DCD kstat
DCD kstat+&31C
DCD -1
";

/// A C program's stub description: chunk 2 after chunk 1.
pub const C_STUBS: &str = r"
.codeend
.stubs DCD 1
DCD kvec
DCD kvec+4*48
DCD kstat
DCD kstat+&31C
DCD 2
DCD cvec
DCD cvec+4*183
DCD cstat
DCD cstat+&B48
DCD -1
";

/// The kit's initialisation procedure, its registers recorded.
pub const INIT_PROCEDURE: &str = r"
.initproc LDR r12, Lires
STMIA r12, {r0-r11}
STR r13, [r12, #48]
STR r14, [r12, #52]
ADR r0, run
MOV pc, r14
";

/// A C program's: its registers recorded, then `_clib_initialise` called.
pub const C_INIT_PROCEDURE: &str = r"
.initproc STMFD r13!, {r14}
LDR r12, Lires
STMIA r12, {r0-r11}
STR r13, [r12, #48]
STR r14, [r12, #52]
BL cvec+4*20
LDMFD r13!, {r14}
ADR r0, run
MOV pc, r14
";
