use super::*;
use tairix_inline::ArrayString;

fn state() -> Registers {
    let mut regs = Regs::default();
    for (n, value) in [
        (gpr::AX, 0x4C00),
        (gpr::BX, 0x0001),
        (gpr::CX, 0x00FF),
        (gpr::DX, 0x0002),
        (gpr::SI, 0x0080),
        (gpr::DI, 0x0100),
        (gpr::BP, 0x0000),
        (gpr::SP, 0xFFFE),
    ] {
        regs.gpr[n] = value;
    }
    regs.seg = [0x2000, 0xF000, 0x3000, 0x1234];
    regs.ip = 0xE05B;
    regs.flags = 0xF202;
    Registers {
        regs,
        cycles: 1234,
        activity: Activity::Executing,
        emulating: false,
        coprocessor: None,
    }
}

fn texts(row: &Row) -> Vec<&str> {
    row.iter().map(|span| span.text.as_str()).collect()
}

#[test]
fn the_registers_read_four_to_a_line_and_the_flags_as_debug_spells_them() {
    let rows = registers(&state(), None);
    assert_eq!(
        texts(&rows[0]),
        ["AX 4C00", "BX 0001", "CX 00FF", "DX 0002"]
    );
    assert_eq!(
        texts(&rows[1]),
        ["SI 0080", "DI 0100", "BP 0000", "SP FFFE"]
    );
    assert_eq!(
        texts(&rows[2]),
        ["CS F000", "DS 1234", "ES 2000", "SS 3000"]
    );
    assert_eq!(
        texts(&rows[3]),
        [
            "IP E05B",
            "FLAGS F202",
            "NV",
            "UP",
            "EI",
            "PL",
            "NZ",
            "NA",
            "PO",
            "NC"
        ]
    );
    assert_eq!(texts(&rows[4]), ["Cycles 1234"]);
    assert!(
        rows.iter().flatten().all(|span| !span.changed),
        "no stop before"
    );
}

#[test]
fn what_changed_since_the_stop_before_is_marked() {
    let before = state();
    let mut now = state();
    now.regs.gpr[gpr::CX] = 0x00FE;
    now.regs.flags |= flag::ZF | flag::TF;
    now.activity = Activity::Halted;
    now.emulating = true;
    let rows = registers(&now, Some(&before));
    let changed: Vec<&str> = rows
        .iter()
        .flatten()
        .filter(|span| span.changed)
        .map(|span| span.text.as_str())
        .collect();
    assert_eq!(changed, ["CX 00FE", "FLAGS F342", "ZR", "TR"]);
    assert_eq!(texts(&rows[3]).last(), Some(&"TR"), "the trap flag set");
    assert_eq!(texts(&rows[4]), ["Cycles 1234  Halted  8080 mode"]);
}

#[test]
fn the_8087_reads_its_stack_its_words_and_its_last_instruction() {
    let value = |text: &str| ArrayString::try_from(text).unwrap();
    let mut fpu = Coprocessor {
        control: 0x037F,
        status: 0x3800,
        tags: 0xFFFC,
        ip: 0xF_E05B,
        opcode: 0x1E8,
        dp: 0x1_2345,
        stack: [value("Empty"); 8],
    };
    fpu.stack[0] = value("1.000000000000000000E+0000");
    let before = fpu.clone();
    fpu.stack[1] = value("0");
    let rows = coprocessor(&fpu, Some(&before));
    assert_eq!(texts(&rows[0]), ["ST0 1.000000000000000000E+0000"]);
    assert!(rows[1][0].changed && !rows[0][0].changed);
    assert_eq!(texts(&rows[8]), ["CW 037F", "SW 3800", "TW FFFC", "TOP 7"]);
    assert_eq!(texts(&rows[9]), ["FIP FE05B  FOP 1E8  FDP 12345"]);
}

#[test]
fn memory_reads_as_bytes_and_as_text_control_codes_dotted() {
    let bytes = [
        0x48, 0x69, 0x00, 0x0A, 0xB0, 0xC9, 0x7F, 0xFF, 0x20, 0x41, 0, 0, 0, 0, 0, 0x7E,
    ];
    assert_eq!(
        memory_row(0x1234, 0x0010, &bytes),
        "1234:0010  48 69 00 0A B0 C9 7F FF  20 41 00 00 00 00 00 7E  Hi..\u{2591}\u{2554}.. A.....~"
    );
}

#[test]
fn a_listing_line_shows_its_address_bytes_and_text() {
    assert_eq!(
        code_row(0xF000, 0xE05B, &[0xFA], "CLI"),
        "F000:E05B  FA            CLI"
    );
    assert_eq!(
        code_row(
            0x0050,
            0,
            &[0x26, 0x26, 0x26, 0x26, 0x26, 0x26, 0x26, 0x90],
            "NOP"
        ),
        "0050:0000  262626262626\u{2026} NOP",
        "the bytes past six elided"
    );
    assert_eq!(stack_row(0x3000, 0xFFFE, 0x1234), "3000:FFFE  1234");
}

#[test]
fn each_breakpoint_reads_as_what_stops_the_machine() {
    let cases = [
        (Breakpoint::Code(0xF_E05B), "F000:E05B", "Execute F000:E05B"),
        (
            Breakpoint::Memory {
                first: 0x1_2340,
                len: 1,
                access: Access::Write,
            },
            "1234:0000",
            "Write 1234:0000, 1 byte",
        ),
        (
            Breakpoint::Memory {
                first: 0x1_2340,
                len: 16,
                access: Access::Any,
            },
            "1234:0000",
            "Read or write 1234:0000, 16 bytes",
        ),
        (
            Breakpoint::Port {
                first: 0x60,
                last: 0x60,
                direction: Direction::In,
            },
            "",
            "Port 0060h in",
        ),
        (
            Breakpoint::Port {
                first: 0x3F8,
                last: 0x3FF,
                direction: Direction::Both,
            },
            "",
            "Ports 03F8h\u{2013}03FFh in or out",
        ),
        (
            Breakpoint::Interrupt {
                vector: 0x21,
                ah: Some(0x4C),
            },
            "",
            "INT 21h, AH=4Ch",
        ),
        (
            Breakpoint::Interrupt {
                vector: 0x10,
                ah: None,
            },
            "",
            "INT 10h",
        ),
    ];
    for (breakpoint, at, expected) in cases {
        assert_eq!(super::breakpoint(&breakpoint, at), expected);
    }
}

#[test]
fn why_it_stopped_reads_in_words() {
    let at = |addr: u32| format!("{addr:05X}h");
    let mut stopped = state();
    let watch = Breakpoint::Memory {
        first: 0x1_2340,
        len: 4,
        access: Access::Write,
    };
    let wrote = Hit {
        master: Master::Coprocessor,
        cycle: Cycle::Write,
        at: 0x1_2342,
        value: 0x80,
    };
    let read_port = Hit {
        master: Master::Dma,
        cycle: Cycle::In,
        at: 0x61,
        value: 0x1D,
    };
    let ports = Breakpoint::Port {
        first: 0x61,
        last: 0x61,
        direction: Direction::In,
    };
    for (why, expected) in [
        (Reason::Break, "Stopped at F000:E05B"),
        (Reason::Stepped, "Stepped to F000:E05B"),
        (
            Reason::Breakpoint(Breakpoint::Code(0xF_E05B)),
            "Stopped at the breakpoint at F000:E05B",
        ),
        (
            Reason::Breakpoint(Breakpoint::Interrupt {
                vector: 0x21,
                ah: None,
            }),
            "Stopped entering INT 21h, AH=4Ch",
        ),
        (
            Reason::Watched { watch, hit: wrote },
            "The 8087 wrote 80h to 12342h, stopping at F000:E05B",
        ),
        (
            Reason::Watched {
                watch: ports,
                hit: read_port,
            },
            "DMA read 1Dh from port 0061h, stopping at F000:E05B",
        ),
    ] {
        assert_eq!(reason(&why, &stopped, at), expected);
    }
    stopped.activity = Activity::Waiting;
    assert_eq!(
        reason(&Reason::Break, &stopped, at),
        "Stopped at F000:E05B, waiting for the 8087"
    );
}
