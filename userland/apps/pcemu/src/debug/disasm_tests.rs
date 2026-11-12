use super::super::bench::Bench;
use super::*;
use alloc::vec;

/// A bench holding `code` at `cs:ip`.
fn holding(cs: u16, ip: u16, code: &[u8]) -> Bench {
    let mut bench = Bench::new(CpuModel::I8088);
    bench.load(cs, ip, code);
    bench
}

const CS: u16 = 0x1234;

fn native(model: CpuModel) -> Reading {
    Reading {
        model,
        emulating: false,
    }
}

/// The text of each line of `code`, read as `reading` does from offset 0.
fn texts(reading: Reading, code: &[u8], lines: usize) -> Vec<String> {
    let ram = holding(CS, 0, code);
    listing(reading, &ram, (CS, 0), (0, lines))
        .into_iter()
        .map(|line| line.text)
        .collect()
}

#[test]
fn instructions_read_as_masm_writes_them() {
    let code = [
        0xB8, 0x00, 0x4C, // MOV AX,4C00h
        0xCD, 0x21, // INT 21h
        0x26, 0x8B, 0x40, 0x12, // MOV AX,ES:[BX+SI+12h]
        0x8B, 0x46, 0xFE, // MOV AX,[BP-02h]
        0xC6, 0x06, 0x00, 0x03, 0x7F, // MOV BYTE PTR [0300h],7Fh
        0xF3, 0xA4, // REP MOVSB
        0xF3, 0xA6, // REPE CMPSB
        0xF2, 0xAE, // REPNE SCASB
        0xF0, 0x87, 0x07, // LOCK XCHG [BX],AX
        0xEA, 0x00, 0x01, 0x00, 0xF0, // JMP 0F000h:0100h
        0xE8, 0x00, 0x00, // CALL 0022h
        0xEB, 0xFE, // JMP 0022h
        0x90, // NOP
        0xD1, 0xE0, // SHL AX,1
        0xF7, 0xF3, // DIV BX
    ];
    assert_eq!(
        texts(native(CpuModel::I8088), &code, 16),
        [
            "MOV AX,4C00h",
            "INT 21h",
            "MOV AX,ES:[BX+SI+12h]",
            "MOV AX,[BP-02h]",
            "MOV BYTE PTR [0300h],7Fh",
            "REP MOVSB",
            "REPE CMPSB",
            "REPNE SCASB",
            "LOCK XCHG [BX],AX",
            "JMP 0F000h:0100h",
            "CALL 0022h",
            "JMP 0022h",
            "NOP",
            "SHL AX,1",
            "DIV BX",
            "ADD [BX+SI],AL",
        ]
    );
}

#[test]
fn the_8087s_instructions_read_by_its_own_decoder() {
    let code = [
        0xDD, 0x07, // FLD QWORD PTR [BX]
        0xD9, 0xE8, // FLD1
        0xDE, 0xC1, // FADDP ST(1),ST
        0xD8, 0xC2, // FADD ST,ST(2)
        0xDB, 0xE3, // FNINIT
        0xDF, 0x3E, 0x00, 0x05, // FISTP QWORD PTR [0500h]
        0x9B, // WAIT
    ];
    assert_eq!(
        texts(native(CpuModel::I8086), &code, 7),
        [
            "FLD QWORD PTR [BX]",
            "FLD1",
            "FADDP ST(1),ST",
            "FADD ST,ST(2)",
            "FNINIT",
            "FISTP QWORD PTR [0500h]",
            "WAIT",
        ]
    );
}

#[test]
fn each_model_reads_its_own_instruction_set() {
    // ENTER 0010h,01h and PUSH 0005h on the 80186; on the 8088, C8 is a
    // return and 6A a jump.
    let code = [0xC8, 0x10, 0x00, 0x01, 0x6A, 0x05];
    assert_eq!(
        texts(native(CpuModel::I80186), &code, 2),
        ["ENTER 0010h,01h", "PUSH 0005h"]
    );
    assert_eq!(texts(native(CpuModel::I8088), &code, 1)[0], "RETF 0010h");
    assert_eq!(texts(native(CpuModel::V20), &[0x0F, 0x20], 1), ["ADD4S"]);
}

#[test]
fn a_host_call_reads_as_one_only_in_the_emulators_rom() {
    let code = [0xFE, 0x38, 0x34, 0x12];
    let mut ram = holding(CS, 0, &code);
    let reading = native(CpuModel::I8088);
    ram.rom = linear(CS, 0)..linear(CS, 4);
    assert_eq!(
        listing(reading, &ram, (CS, 0), (0, 1))[0].text,
        "HOSTCALL 1234h"
    );
    ram.rom = 0..0;
    assert_ne!(
        listing(reading, &ram, (CS, 0), (0, 1))[0].text,
        "HOSTCALL 1234h"
    );
}

#[test]
fn in_8080_mode_the_code_reads_as_the_8080s() {
    let emulating = Reading {
        model: CpuModel::V20,
        emulating: true,
    };
    let code = [
        0x3E, 0x41, 0x3C, 0x47, 0xCD, 0x00, 0x01, 0xC0, 0xED, 0xED, 0x21, 0xED, 0xFD,
    ];
    assert_eq!(
        texts(emulating, &code, 7),
        [
            "MVI A,41h",
            "INR A",
            "MOV B,A",
            "CALL 0100h",
            "RNZ",
            "CALLN 21h",
            "RETEM"
        ]
    );
}

#[test]
fn a_line_holds_its_bytes_and_at_most_as_many_as_a_line_can() {
    let mut code = vec![0x26; 9];
    code.push(0x90);
    let ram = holding(CS, 0, &code);
    let lines = listing(native(CpuModel::I8088), &ram, (CS, 0), (0, 3));
    assert_eq!(
        lines[0].bytes.len(),
        MOST_PREFIXES + 1,
        "one prefix past the most"
    );
    assert!(lines.iter().all(|line| line.bytes.len() <= MOST_BYTES));
    let mut ip = 0;
    for line in &lines {
        assert_eq!(line.ip, ip);
        ip += u16::try_from(line.bytes.len()).unwrap();
    }
}

#[test]
fn the_lines_before_are_those_most_starts_agree_on() {
    // Code from 0100h: MOV CX,0010h; MOV SI,0200h; LODSB; ADD BL,AL;
    // LOOP 0106h; MOV AH,4Ch; INT 21h; listed from the INT.
    let code = [
        0xB9, 0x10, 0x00, 0xBE, 0x00, 0x02, 0xAC, 0x02, 0xD8, 0xE2, 0xFB, 0xB4, 0x4C, 0xCD, 0x21,
    ];
    let ram = holding(CS, 0x0100, &code);
    let lines = listing(native(CpuModel::I8088), &ram, (CS, 0x010D), (5, 1));
    let shown: Vec<_> = lines
        .iter()
        .map(|line| (line.ip, line.text.as_str()))
        .collect();
    assert_eq!(
        shown,
        [
            (0x0103, "MOV SI,0200h"),
            (0x0106, "LODSB"),
            (0x0107, "ADD BL,AL"),
            (0x0109, "LOOP 0106h"),
            (0x010B, "MOV AH,4Ch"),
            (0x010D, "INT 21h"),
        ]
    );
}

#[test]
fn stepping_knows_what_calls_and_what_returns() {
    let reading = native(CpuModel::I8088);
    for (code, len, expected) in [
        (vec![0xE8, 0x00, 0x00], 3, Flow::Calls),
        (vec![0xFF, 0x1E, 0x00, 0x01], 4, Flow::Calls),
        (vec![0xCD, 0x21], 2, Flow::Calls),
        (vec![0xF3, 0xA5], 2, Flow::Calls),
        (vec![0xA5], 1, Flow::Other),
        (vec![0xC3], 1, Flow::Returns(Return::Near(0))),
        (vec![0xC2, 0x04, 0x00], 3, Flow::Returns(Return::Near(4))),
        (vec![0xCB], 1, Flow::Returns(Return::Far(0))),
        (vec![0xCF], 1, Flow::Returns(Return::Interrupt)),
        // The 8088's undocumented alias of RET.
        (vec![0xC1], 1, Flow::Returns(Return::Near(0))),
        (vec![0xE2, 0xFE], 2, Flow::Other),
    ] {
        let ram = holding(CS, 0, &code);
        assert_eq!(flow(reading, &ram, CS, 0), (len, expected), "{code:02X?}");
    }
}
