//! Deterministic fuzz harness for BASIC's interpreter and assembler. A
//! program's text is a stranger's. The invariants:
//!
//! * any program of statements drawn from BASIC's keywords, operators and
//!   operands, or of any printable bytes, run by BASIC V and BASIC VI ends,
//!   waits or spends its budget without a panic;
//! * so does one that also overwrites words of BASIC's own workspace and
//!   stack, and rewrites from a function the records waiting on it;
//! * any assembler statement drawn from the mnemonics, their suffixes and
//!   every kind of operand, or of any printable bytes, assembled under any
//!   `OPT` raises an error or moves `P%` and `O%` on by no more than a string
//!   and an alignment, so it lays nothing down outside its block.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz --soak` extends it to a
//! wall-clock budget.

mod common;

use std::fmt::Write;

use common::run_within;
use tairix_fuzzseed::Prng;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 40;

/// The instructions a program may spend: enough for its lines, bounding any
/// loop it makes.
const BUDGET: u64 = 4_000_000;

/// The most bytes one assembler statement may move `P%` or `O%` on: the
/// longest string, after an alignment.
const MOST_LAID: i64 = 255 + 3;

const DIALECTS: [&str; 2] = ["BASIC", "BASIC64"];

const KEYWORDS: &[&str] = &[
    "PRINT",
    "A%=",
    "B$=",
    "C=",
    "D%()=",
    "DIM",
    "FOR",
    "TO",
    "STEP",
    "NEXT",
    "REPEAT",
    "UNTIL",
    "WHILE",
    "ENDWHILE",
    "IF",
    "THEN",
    "ELSE",
    "ENDIF",
    "CASE",
    "OF",
    "WHEN",
    "OTHERWISE",
    "ENDCASE",
    "GOSUB",
    "RETURN",
    "GOTO",
    "PROCa",
    "DEF PROCa",
    "ENDPROC",
    "FNa",
    "DEF FNa",
    "=",
    "LOCAL",
    "DATA",
    "READ",
    "RESTORE",
    "ON",
    "ERROR",
    "OFF",
    "LOCAL ERROR",
    "RESTORE ERROR",
    "LOCAL DATA",
    "RESTORE DATA",
    "END",
    "STOP",
    "CLEAR",
    "SWAP",
    "VDU",
    "COLOUR",
    "GCOL",
    "PLOT",
    "MOVE",
    "DRAW",
    "CLS",
    "CLG",
    "MODE",
    "SOUND",
    "TIME",
    "TIME$",
    "INKEY",
    "EVAL",
    "VAL",
    "STR$",
    "LEFT$(",
    "MID$(",
    "RIGHT$(",
    "INSTR(",
    "LEN",
    "CHR$",
    "ASC",
    "STRING$(",
    "SUM",
    "SGN",
    "ABS",
    "SQR",
    "LN",
    "EXP",
    "SIN",
    "RND",
    "TRUE",
    "FALSE",
    "NOT",
    "AND",
    "OR",
    "EOR",
    "DIV",
    "MOD",
    "^",
    "+",
    "-",
    "*",
    "/",
    "(",
    ")",
    ",",
    ";",
    "'",
    "~",
    "$",
    "?",
    "!",
    "|",
    "@%=",
    "P%",
    "HIMEM",
    "LOMEM",
    "PAGE",
    "TOP",
    "LIST",
    "TRACE ON",
    "TRACE OFF",
    "TRACE PROC",
    "[",
    "]",
    "OPT 2",
    "MOV R0,#1",
    "SYS",
    "CALL",
    "USR",
    "LET",
    "REM",
    "QUIT",
    "BGET#",
    "BPUT#",
    "OPENIN",
    "CLOSE#",
    "EXT#",
    "PTR#",
    "EOF#",
    "LVAR",
    "WIDTH",
    "COUNT",
    "POS",
    "VPOS",
    "TAB(",
    "SPC",
    "ERR",
    "ERL",
    "REPORT",
    "REPORT$",
    "DEG",
    "RAD",
    "PI",
    "INT",
    "MOUSE",
    "TINT",
    "ELLIPSE",
    "RECTANGLE",
    "FILL",
    "CIRCLE",
    "LINE",
    "ORIGIN",
    "OSCLI",
    "RENUMBER",
    "DELETE",
    "AUTO",
    "HELP",
];

const OPERANDS: &[&str] = &[
    "0",
    "1",
    "-1",
    "2.5",
    "1E10",
    "&FF",
    "%101",
    "&7FFFFFFF",
    "-2147483648",
    "\"text\"",
    "\"\"",
    "A%",
    "B$",
    "C",
    "D%(1)",
    "x",
    "y$",
    "z%",
    "FNa",
    "TRUE",
    "(1)",
    "(",
    ")",
    "1,2",
    "\"a\",\"b\"",
    "#1",
    "#0",
    "255",
    "65280",
    "&8000",
    "PAGE",
    "HIMEM+4",
];

const STEMS: &[&str] = &[
    "MOV", "MVN", "ADD", "ADC", "SUB", "SBC", "RSB", "RSC", "AND", "ORR", "EOR", "BIC", "CMP",
    "CMN", "TST", "TEQ", "MUL", "MLA", "MLS", "UMULL", "SMLAL", "SMLABB", "LDR", "STR", "LDM",
    "STM", "LDRD", "STRD", "LDREX", "STREX", "SWP", "B", "BL", "BX", "BLX", "SWI", "SVC", "BKPT",
    "SMC", "HVC", "ERET", "ADR", "MRS", "MSR", "CLZ", "QADD", "SSAT", "USAT16", "SXTB", "UXTAH",
    "PKHBT", "SEL", "REV", "RBIT", "BFI", "BFC", "UBFX", "MOVW", "MOVT", "SDIV", "PLD", "PLI",
    "DMB", "DSB", "ISB", "CLREX", "CPSID", "SETEND", "SRSDB", "RFEIA", "NOP", "YIELD", "WFE",
    "SEV", "SEVL", "DBG", "UDF", "PUSH", "POP", "LDF", "STF", "LFM", "SFM", "ADF", "MUF", "DVF",
    "RDF", "MVF", "ABS", "SIN", "FIX", "FLT", "CMF", "CNFE", "WFS", "RFS", "LDC", "STC", "LDC2",
    "MCR", "MRC", "MCRR", "MRRC", "CDP", "CDP2", "VADD", "VSUB", "VMUL", "VMLA", "VFMA", "VDIV",
    "VSQRT", "VABS", "VNEG", "VMOV", "VMVN", "VORR", "VBIC", "VCVT", "VCVTR", "VCMP", "VCMPE",
    "VLDR", "VSTR", "VLDM", "VSTM", "VPUSH", "VPOP", "VMRS", "VMSR", "VDUP", "VEXT", "VZIP",
    "VTRN", "VTBL", "VLD1", "VLD2", "VST3", "VST4", "VSHL", "VSHR", "VQSHL", "VRSHL", "VQRSHRN",
    "VMULL", "VMOVL", "VQMOVN", "VRECPE", "VRSQRTS", "VRINTZ", "VSEL", "VMAXNM", "VCNT", "CRC32B",
    "CRC32CW", "LDA", "STL", "LDAEX", "STLEXD", "EQUB", "EQUW", "EQUD", "EQUS", "EQUFS", "EQUFD",
    "EQUFE", "DCB", "DCW", "DCD", "DCFS", "DCFD", "DCFE", "ALIGN", "OPT", "=",
];

const SUFFIXES: &[&str] = &[
    "", "", "", "EQ", "NE", "CS", "MI", "VS", "HI", "GE", "LT", "LE", "AL", "NV", "S", "B", "H",
    "SB", "T", "D", "IA", "DB", "FD", "EA", ".F32", ".F64", ".I8", ".I16", ".I32", ".I64", ".S16",
    ".U32", ".P8", ".F32.S32", ".F64.F32", ".U32.F32", ".32", ".8", "P", "E", "Z", "M", "SZ",
];

const ASM_OPERANDS: &[&str] = &[
    "R0",
    "R1",
    "R7",
    "R13",
    "R15",
    "PC",
    "LR",
    "SP",
    "R16",
    "S0",
    "S31",
    "S32",
    "S100",
    "D0",
    "D16",
    "D31",
    "D32",
    "Q0",
    "Q15",
    "F0",
    "F7",
    "F8",
    "C0",
    "C15",
    "CP15",
    "P15",
    "#0",
    "#1",
    "#-1",
    "#&FF",
    "#&100",
    "#257",
    "#&F000000F",
    "#1E10",
    "#0.5",
    "#-2",
    "#3",
    "#&FFFF",
    "#&7FFFFFFF",
    "#&80000000",
    "#&FFFFFFFF",
    "[R0]",
    "[R0,#4]",
    "[R0,#-4]!",
    "[R0],#4",
    "[R0,#&7FFFFFFF]",
    "[R0,R1,LSL #2]",
    "[R1,-R2]",
    "[PC,#8]",
    "[R0,#1024]",
    "[R0@64]",
    "{R0-R3}",
    "{R0,R2,R14}^",
    "{}",
    "{R5-R2}",
    "{S0-S3}",
    "{S25-S186}",
    "{D0-D15}",
    "{D0-D16}",
    "{D0[1],D1[1]}",
    "{D0[]}",
    "D0[1]",
    "LSL #3",
    "ASR #32",
    "ROR R2",
    "RRX",
    "LSL #33",
    "0",
    "1",
    "&8000",
    "P%",
    "P%+8",
    "x%",
    "y",
    "\"text\"",
    "\"\"",
    "1+2",
    "(4)",
    "TRUE",
    "CPSR",
    "SPSR_fc",
    "CPSR_all",
    "APSR_nzcv",
    "FPSCR",
    "FPEXC",
    "SY",
    "ISH",
    "BE",
    "LE",
    "#8",
    "-R3",
    "R0!",
    "IA",
    "!",
    "^",
];

const LABELS: &[&str] = &[".a", ".b%", ".c ", ".d.", "\\ note", "; note", "REM note"];

/// Any printable text, at most `most` bytes, colons and all.
fn printable(rng: &mut Prng, most: usize) -> String {
    let length = rng.at_most(most);
    (0..length)
        .map(|_| char::from(0x20 + rng.next_u8() % 0x5F))
        .collect()
}

/// A statement of BASIC: a keyword and its operands, or printable bytes.
fn statement(rng: &mut Prng) -> String {
    if rng.below(8) == 0 {
        return printable(rng, 24);
    }
    let mut out = String::from(*rng.pick(KEYWORDS));
    for _ in 0..rng.at_most(3) {
        out.push(' ');
        out.push_str(rng.pick(OPERANDS));
        if rng.below(3) == 0 {
            out.push_str(rng.pick(KEYWORDS));
        }
    }
    out
}

/// An assembler statement: a stem, suffixes and operands, perhaps with a
/// label or a comment; or printable bytes, but for the brackets that would
/// leave the block or open another.
fn instruction(rng: &mut Prng) -> String {
    if rng.below(10) == 0 {
        return printable(rng, 32).replace(['[', ']'], "(");
    }
    let mut out = String::new();
    if rng.below(6) == 0 {
        out.push_str(rng.pick(LABELS));
        out.push(' ');
    }
    out.push_str(rng.pick(STEMS));
    for _ in 0..rng.at_most(2) {
        out.push_str(rng.pick(SUFFIXES));
    }
    let count = rng.at_most(4);
    for n in 0..count {
        out.push_str(if n == 0 {
            " "
        } else {
            rng.pick(&[",", ", ", " ", ",,"])
        });
        out.push_str(rng.pick(ASM_OPERANDS));
    }
    if rng.below(12) == 0 {
        out.push_str(rng.pick(LABELS));
    }
    out
}

/// A program of BASIC's statements, each line's errors sending it on to the
/// next.
fn program(rng: &mut Prng) -> String {
    let lines = 2 + rng.below(10);
    let last = 10 * (lines + 1);
    let mut text = format!("10 ON ERROR IF ERL<{last} THEN GOTO ERL+10 ELSE END\n");
    for n in 2..=lines + 1 {
        let statements: Vec<String> = (0..=rng.below(4)).map(|_| statement(rng)).collect();
        let _ = writeln!(text, "{} {}", 10 * n, statements.join(":"));
    }
    text
}

/// A program assembling `count` statements, each in a block of its own
/// under a random `OPT`, printing how far each moved `P%` and `O%`. The
/// first always assembles, so every run checks at least one move.
fn assembly(rng: &mut Prng, count: usize) -> String {
    let mut text = String::from("10 DIM C% 600:L%=C%+600:ON ERROR PRINT \"#E\";ERR:GOTO ERL+10\n");
    for n in 0..count {
        let opt = 2 * rng.below(8);
        let statement = if n == 0 {
            String::from("MOV R0,#1")
        } else {
            instruction(rng)
        };
        let _ = writeln!(
            text,
            "{} P%=C%:O%=C%+300:[OPT {opt}:{statement}:]:PRINT \"#P\";P%-C%;\" \";O%-C%-300",
            20 + 10 * n,
        );
    }
    let _ = writeln!(text, "{} END", 20 + 10 * count);
    text
}

fn check_program(rng: &mut Prng, hostile: bool) {
    let text = if hostile {
        hostile_program(rng)
    } else {
        program(rng)
    };
    for basic in DIALECTS {
        let _ = run_within(basic, &text, b"", BUDGET);
    }
}

/// The values a hostile statement writes over a word of BASIC's.
const HOSTILE_VALUES: &[&str] = &[
    "-1",
    "0",
    "1",
    "4",
    "10",
    "255",
    "&FFFF",
    "&10000",
    "&FFFFFFFC",
    "&7FFFFFFF",
    "&80000000",
    "PAGE",
    "TOP",
    "HIMEM",
    "HIMEM-8",
    "LOMEM",
    "&8000",
];

/// Statements whose waiting records `FNz` may rewrite, `@` its call.
const WAITING: &[&str] = &[
    "PRINT LEFT$(\"ab\",@)",
    "MOVE 0,@",
    "SYS \"OS_WriteC\",@",
    "PROCp(@)",
    "A%=INSTR(\"abc\",\"b\",@)",
    "SYS \"OS_WriteC\",65 TO B%(@)",
    "MOUSE X%,Y%,B%(@)",
    "CALL C%,B%(@)",
    "A$=MID$(\"abc\",2,@)",
    "FOR I%=1 TO 2:X=@:NEXT",
    "REPEAT:X=@:UNTIL TRUE",
    "WHILE @=0:ENDWHILE",
    "RESTORE +@",
    "ON @ GOTO 20,30",
    "Z$=STRING$(@,\"x\")",
    "VDU @",
    "B%()=@",
];

/// A program of [`statement`]s among others that overwrite a word of BASIC's
/// registers, workspace or stack, or rewrite from `FNz` a word above its
/// frame, where the records waiting on it lie.
fn hostile_program(rng: &mut Prng) -> String {
    let lines = 2 + rng.below(10);
    let last = 10 * (lines + 1);
    let mut text = format!("10 ON ERROR IF ERL<{last} THEN GOTO ERL+10 ELSE END\n");
    for n in 2..=lines + 1 {
        let statements: Vec<String> = (0..=rng.below(3))
            .map(|_| {
                let value = rng.pick(HOSTILE_VALUES);
                match rng.below(6) {
                    0 => {
                        let at = match rng.below(3) {
                            0 => 0x8700 + 4 * rng.below(0x15),
                            1 => 0x866C + 4 * rng.below(0x11),
                            _ => 0x8400 + 4 * rng.below(0x48),
                        };
                        format!("!&{at:X}={value}")
                    }
                    1 => format!("!(HIMEM-{})={value}", 4 * (1 + rng.below(32))),
                    2 | 3 => {
                        let call = format!("FNz({},{value})", 4 * rng.below(48));
                        rng.pick(WAITING).replace('@', &call)
                    }
                    _ => statement(rng),
                }
            })
            .collect();
        let _ = writeln!(text, "{} {}", 10 * n, statements.join(":"));
    }
    let _ = writeln!(text, "900 END");
    let _ = writeln!(
        text,
        "910 DEF FNz(K%,V%):LOCAL B%:DIM B% LOCAL 0:B%!K%=V%:=1"
    );
    let _ = writeln!(text, "920 DEF PROCp(A):ENDPROC");
    text.insert_str(0, "1 DIM B%(70),C% 8:!C%=&E1A0F00E\n");
    text
}

/// The moves of `P%` and `O%` checked.
fn check_assembly(rng: &mut Prng) -> u64 {
    let text = assembly(rng, 16);
    let mut checked = 0;
    for basic in DIALECTS {
        let run = run_within(basic, &text, b"", BUDGET);
        let printed = run.text();
        for line in printed.lines().filter_map(|l| l.strip_prefix("#P")) {
            let moved: Vec<i64> = line
                .split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect();
            assert!(
                moved.len() == 2 && moved.iter().all(|m| (0..=MOST_LAID).contains(m)),
                "{basic} moved P% and O% on by {line:?} in\n{text}"
            );
            checked += 1;
        }
    }
    checked
}

#[test]
fn any_program_or_assembler_statement_keeps_its_invariants() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "any_program_or_assembler_statement_keeps_its_invariants",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    let mut checked = 0;
    loop {
        check_program(&mut rng, false);
        check_program(&mut rng, true);
        checked += check_assembly(&mut rng);
        iteration += 1;
        match deadline {
            None if iteration >= SMOKE_ITERATIONS => break,
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            _ => {}
        }
    }
    assert!(
        checked >= 2 * iteration,
        "{checked} moves checked in {iteration} runs"
    );
}
