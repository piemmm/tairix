use super::super::tests::{command_bytes, event_bytes, refused_cut_or_grown};
use super::super::{Command, Event, CREDITED_MOST, LARGEST_EVENT};
use super::*;
use crate::debug::disasm;
use crate::debug::Coprocessor;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use tairix_sandbox::proto::FRAME_HEADER_LEN;

const WATCH: Breakpoint = Breakpoint::Memory {
    first: 0x1_2340,
    len: 4,
    access: Access::Write,
};
const PORTS: Breakpoint = Breakpoint::Port {
    first: 0x60,
    last: 0x64,
    direction: Direction::Both,
};

fn every_breakpoint() -> [Breakpoint; 6] {
    [
        Breakpoint::Code(0xF_FFF0),
        WATCH,
        Breakpoint::Memory {
            first: 0,
            len: SPACE,
            access: Access::Any,
        },
        PORTS,
        Breakpoint::Interrupt {
            vector: 0x21,
            ah: Some(0x4C),
        },
        Breakpoint::Interrupt {
            vector: 0x08,
            ah: None,
        },
    ]
}

#[test]
fn every_request_reads_back_as_written_within_the_input_window() {
    let look = Look {
        number: u32::MAX,
        code: (0xF000, 0xFFF0),
        lines: (MOST_BEFORE, MOST_AFTER),
        memory: (0xB800, 0x0F00),
        bytes: MOST_MEMORY,
        stack: MOST_STACK,
    };
    let mut requests = vec![
        Debug::Attach,
        Debug::Detach,
        Debug::Break,
        Debug::Look(look),
    ];
    requests.extend(every_breakpoint().map(Debug::Add));
    requests.extend(every_breakpoint().map(Debug::Remove));
    requests.extend(GOES.map(Debug::Go));
    for request in requests {
        let bytes = command_bytes(&Command::Debug(request));
        assert_eq!(Command::decode(&bytes), Ok(Command::Debug(request)));
        assert!(
            FRAME_HEADER_LEN + bytes.len() <= CREDITED_MOST,
            "{request:?}"
        );
        refused_cut_or_grown(&bytes, |b| Command::decode(b).is_ok());
        assert!(Command::Debug(request).credited(), "in order with input");
    }
}

fn registers(coprocessor: bool) -> Registers {
    let regs = Regs {
        gpr: [1, 2, 3, 4, 5, 6, 7, 8],
        seg: [0xF000, 0x0050, 0x2000, 0xB800],
        ip: 0xFFF0,
        flags: 0xF202,
    };
    let value = |text: &str| ArrayString::try_from(text).unwrap();
    Registers {
        regs,
        cycles: u64::MAX,
        activity: Activity::Waiting,
        emulating: true,
        coprocessor: coprocessor.then(|| Coprocessor {
            control: 0x037F,
            status: 0x3800,
            tags: 0x0FFF,
            ip: SPACE - 1,
            opcode: 0x7FF,
            dp: 0x1_0000,
            stack: [
                value("-1.189731495357231765E+4932"),
                value("0"),
                value("-NaN"),
                value("Infinity"),
                value("Empty"),
                value("Empty"),
                value("Empty"),
                value("3.141592653589793239E+0000"),
            ],
        }),
    }
}

fn line(ip: u16, bytes: &[u8], text: &str) -> disasm::Line {
    disasm::Line {
        ip,
        bytes: bytes.to_vec(),
        text: String::from(text),
    }
}

#[test]
fn every_report_reads_back_as_written() {
    let hit = Hit {
        master: Master::Coprocessor,
        cycle: Cycle::Write,
        at: 0x1_2343,
        value: 0x80,
    };
    let port = Hit {
        master: Master::Dma,
        cycle: Cycle::In,
        at: 0x64,
        value: 0x1D,
    };
    let reasons = [
        Reason::Break,
        Reason::Stepped,
        Reason::Breakpoint(Breakpoint::Code(0x7C00)),
        Reason::Breakpoint(Breakpoint::Interrupt {
            vector: 0x13,
            ah: None,
        }),
        Reason::Watched { watch: WATCH, hit },
        Reason::Watched {
            watch: PORTS,
            hit: port,
        },
    ];
    let lines = [
        line(0x00FE, &[0x90], "NOP"),
        line(0x00FF, &[0x90], "NOP"),
        line(0x0100, &[0xB8, 0x00, 0x4C], "MOV AX,4C00h"),
        line(0x0103, &[0xCD, 0x21], "INT 21h"),
    ];
    let (before, after, encoded) = Listing::encode(&lines, 2);
    let memory = [0xCD, 0x20, 0x00, 0xA0];
    let seen = Said::Seen(Seen {
        number: 7,
        listing: Listing::new(before, after, &encoded),
        memory: &memory,
        stack: &[0xFE, 0xFF],
    });
    let mut reports: Vec<Said<'_>> = reasons
        .iter()
        .zip([true, false].into_iter().cycle())
        .map(|(&reason, fitted)| Said::Stopped {
            reason,
            registers: registers(fitted),
        })
        .collect();
    reports.extend([seen, Said::Reset, Said::Detached]);
    for report in reports {
        let bytes = event_bytes(&Event::Debug(report.clone()));
        assert_eq!(Event::decode(&bytes), Ok(Event::Debug(report)));
        refused_cut_or_grown(&bytes, |b| Event::decode(b).is_ok());
    }
    let decoded: Vec<_> = Listing::new(before, after, &encoded)
        .lines()
        .map(|line| (line.ip, line.text))
        .collect();
    assert_eq!(
        decoded,
        [
            (0xFE, "NOP"),
            (0xFF, "NOP"),
            (0x100, "MOV AX,4C00h"),
            (0x103, "INT 21h")
        ]
    );
}

/// The report `said` encoded, then changed by `change`.
fn altered(said: &Said<'_>, change: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut bytes = event_bytes(&Event::Debug(said.clone()));
    change(&mut bytes);
    bytes
}

#[test]
fn a_report_that_could_not_be_true_is_refused() {
    // The event's tag, the report's kind, then the reason's.
    const REASON: usize = 2;
    let stopped = |reason| Said::Stopped {
        reason,
        registers: registers(false),
    };
    let refused = |bytes: Vec<u8>, why: &str| {
        assert!(Event::decode(&bytes).is_err(), "{why}");
    };
    let watched = stopped(Reason::Watched {
        watch: WATCH,
        hit: Hit {
            master: Master::Cpu,
            cycle: Cycle::Write,
            at: 0x1_2340,
            value: 0,
        },
    });
    // The hit's address follows the watch (1 + 4 + 4 + 1) and its master
    // and cycle.
    let hit_at = REASON + 1 + 10 + 2;
    refused(
        altered(&watched, |b| {
            b[hit_at..hit_at + 4].copy_from_slice(&0x1_2344u32.to_le_bytes());
        }),
        "a hit outside its watch",
    );
    refused(
        altered(&watched, |b| b[hit_at - 1] = 0),
        "a write watch reporting a read",
    );
    refused(
        altered(&watched, |b| b[hit_at - 1] = 3),
        "a memory watch reporting a port",
    );
    let interrupt = stopped(Reason::Breakpoint(Breakpoint::Interrupt {
        vector: 0x21,
        ah: None,
    }));
    refused(
        altered(&interrupt, |b| b[REASON + 4] = 0x4C),
        "an AH where none was given",
    );
    refused(
        event_bytes(&Event::Debug(stopped(Reason::Breakpoint(WATCH)))),
        "a watch reported as a breakpoint",
    );
    let lines = [line(0x0100, &[0x90], "NOP"), line(0x0102, &[0x90], "NOP")];
    let (before, after, encoded) = Listing::encode(&lines, 0);
    let skipping = Said::Seen(Seen {
        number: 0,
        listing: Listing::new(before, after, &encoded),
        memory: &[],
        stack: &[],
    });
    refused(
        event_bytes(&Event::Debug(skipping)),
        "lines that do not follow one another",
    );
    let lines = [line(0x0100, &[0x90], "NO\x07P")];
    let (before, after, encoded) = Listing::encode(&lines, 0);
    let control = Said::Seen(Seen {
        number: 0,
        listing: Listing::new(before, after, &encoded),
        memory: &[],
        stack: &[],
    });
    refused(
        event_bytes(&Event::Debug(control)),
        "a control code in a line's text",
    );
    let mut fpu = registers(true);
    if let Some(fpu) = fpu.coprocessor.as_mut() {
        fpu.ip = SPACE;
    }
    refused(
        event_bytes(&Event::Debug(Said::Stopped {
            reason: Reason::Break,
            registers: fpu,
        })),
        "an instruction pointer past the address space",
    );
}

#[test]
fn a_look_past_what_an_answer_holds_is_refused() {
    let look = |lines, bytes, stack| {
        command_bytes(&Command::Debug(Debug::Look(Look {
            number: 0,
            code: (0, 0),
            lines,
            memory: (0, 0),
            bytes,
            stack,
        })))
    };
    for (lines, bytes, stack) in [
        ((MOST_BEFORE + 1, 0), 0, 0),
        ((0, MOST_AFTER + 1), 0, 0),
        ((0, 0), MOST_MEMORY + 1, 0),
        ((0, 0), 0, MOST_STACK + 1),
    ] {
        assert!(Command::decode(&look(lines, bytes, stack)).is_err());
    }
}

#[test]
fn the_fullest_answer_fits_the_session() {
    let text = "W".repeat(MOST_TEXT);
    let count = u16::from(MOST_BEFORE) + u16::from(MOST_AFTER);
    let lines: Vec<_> = (0..count)
        .map(|n| {
            let width = u16::try_from(disasm::MOST_BYTES).unwrap();
            line(n * width, &[0x26; disasm::MOST_BYTES], &text)
        })
        .collect();
    let (before, after, encoded) = Listing::encode(&lines, usize::from(MOST_BEFORE));
    assert_eq!((before, after), (MOST_BEFORE, MOST_AFTER));
    let memory = vec![0; usize::from(MOST_MEMORY)];
    let stack = vec![0; usize::from(MOST_STACK) * 2];
    let seen = Event::Debug(Said::Seen(Seen {
        number: u32::MAX,
        listing: Listing::new(before, after, &encoded),
        memory: &memory,
        stack: &stack,
    }));
    let bytes = event_bytes(&seen);
    assert!(bytes.len() <= LARGEST_EVENT);
    assert!(bytes.len() <= 1 + MOST_SAID);
    assert_eq!(Event::decode(&bytes), Ok(seen));
}
