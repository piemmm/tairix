//! What the debugger's panes say: each line from what the worker read, the
//! values that changed since the stop before marked, and why the processor
//! stopped, in words.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::cpu::{flag, gpr, Regs, Seg};
use crate::debug::probe::{Access, Breakpoint, Cycle, Direction, Hit, Master, Reason};
use crate::debug::{Activity, Coprocessor, Registers};

/// A run of a line's text, marked where it changed since the stop before.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    /// The text.
    pub text: String,
    /// Whether what it shows changed.
    pub changed: bool,
}

/// A line of spans.
pub type Row = Vec<Span>;

fn span(text: String, changed: bool) -> Span {
    Span { text, changed }
}

const NAMES: [(&str, usize); 8] = [
    ("AX", gpr::AX),
    ("BX", gpr::BX),
    ("CX", gpr::CX),
    ("DX", gpr::DX),
    ("SI", gpr::SI),
    ("DI", gpr::DI),
    ("BP", gpr::BP),
    ("SP", gpr::SP),
];
const SEGMENTS: [(&str, Seg); 4] = [
    ("CS", Seg::Cs),
    ("DS", Seg::Ds),
    ("ES", Seg::Es),
    ("SS", Seg::Ss),
];
/// The flags as DEBUG names them: set, then clear.
const FLAGS: [(u16, &str, &str); 8] = [
    (flag::OF, "OV", "NV"),
    (flag::DF, "DN", "UP"),
    (flag::IF, "EI", "DI"),
    (flag::SF, "NG", "PL"),
    (flag::ZF, "ZR", "NZ"),
    (flag::AF, "AC", "NA"),
    (flag::PF, "PE", "PO"),
    (flag::CF, "CY", "NC"),
];

/// A register's span: its name and value, changed where `before` differs.
fn word(name: &str, now: u16, before: Option<u16>) -> Span {
    span(
        format!("{name} {now:04X}"),
        before.is_some_and(|before| before != now),
    )
}

/// The processor's lines: its registers four to a line, IP and FLAGS with
/// each flag spelled as DEBUG spells it, and what it was doing.
#[must_use]
pub fn registers(now: &Registers, before: Option<&Registers>) -> Vec<Row> {
    let regs = &now.regs;
    let was = before.map(|before| &before.regs);
    let general = |names: &[(&str, usize)]| -> Row {
        names
            .iter()
            .map(|&(name, n)| word(name, regs.gpr[n], was.map(|was| was.gpr[n])))
            .collect()
    };
    let segments: Row = SEGMENTS
        .iter()
        .map(|&(name, seg)| word(name, regs.sreg(seg), was.map(|was| was.sreg(seg))))
        .collect();
    let mut control = vec![
        word("IP", regs.ip, was.map(|was| was.ip)),
        word("FLAGS", regs.flags, was.map(|was| was.flags)),
    ];
    control.extend(flags(regs, was));
    let mut state = format!("Cycles {}", now.cycles);
    match now.activity {
        Activity::Executing => {}
        Activity::Halted => state.push_str("  Halted"),
        Activity::Waiting => state.push_str("  Waiting for the 8087"),
    }
    if now.emulating {
        state.push_str("  8080 mode");
    }
    vec![
        general(&NAMES[..4]),
        general(&NAMES[4..]),
        segments,
        control,
        vec![span(state, false)],
    ]
}

fn flags(regs: &Regs, was: Option<&Regs>) -> impl Iterator<Item = Span> {
    let (now, before) = (regs.flags, was.map(|was| was.flags));
    let trap = (now & flag::TF != 0).then_some((flag::TF, "TR", ""));
    FLAGS
        .into_iter()
        .chain(trap)
        .map(move |(mask, set, clear)| {
            let on = now & mask != 0;
            let changed = before.is_some_and(|before| (before & mask != 0) != on);
            span(String::from(if on { set } else { clear }), changed)
        })
}

/// The 8087's lines: ST(0) to ST(7), then its control, status and tag
/// words, the stack's top, and its last instruction and operand.
#[must_use]
pub fn coprocessor(now: &Coprocessor, before: Option<&Coprocessor>) -> Vec<Row> {
    let mut rows: Vec<Row> = now
        .stack
        .iter()
        .enumerate()
        .map(|(n, value)| {
            let changed = before.is_some_and(|before| before.stack[n] != *value);
            vec![span(format!("ST{n} {value}"), changed)]
        })
        .collect();
    let field = |name: &str, now: u16, before: Option<u16>| word(name, now, before);
    rows.push(vec![
        field("CW", now.control, before.map(|before| before.control)),
        field("SW", now.status, before.map(|before| before.status)),
        field("TW", now.tags, before.map(|before| before.tags)),
        span(format!("TOP {}", (now.status >> 11) & 7), false),
    ]);
    rows.push(vec![span(
        format!(
            "FIP {:05X}  FOP {:03X}  FDP {:05X}",
            now.ip, now.opcode, now.dp
        ),
        before.is_some_and(|before| {
            (before.ip, before.opcode, before.dp) != (now.ip, now.opcode, now.dp)
        }),
    )]);
    rows
}

/// A byte as the memory pane's text column shows it: code page 437's glyph,
/// or a dot for a control code.
fn shown_byte(byte: u8) -> char {
    match byte {
        0x20..=0x7E | 0x80..=0xFE => tairix_cp437::glyph_of(byte),
        _ => '.',
    }
}

/// `byte`'s two hexadecimal digits, onto `out`.
fn push_hex(out: &mut String, byte: u8) {
    for nibble in [byte >> 4, byte & 0xF] {
        let digit = char::from_digit(u32::from(nibble), 16).unwrap_or('?');
        out.push(digit.to_ascii_uppercase());
    }
}

/// A memory row: `segment:offset`, the bytes in hexadecimal, a gap after
/// the eighth, and what they read as.
#[must_use]
pub fn memory_row(segment: u16, offset: u16, bytes: &[u8]) -> String {
    let mut row = format!("{segment:04X}:{offset:04X} ");
    for (n, &byte) in bytes.iter().enumerate() {
        row.push_str(if n == 8 { "  " } else { " " });
        push_hex(&mut row, byte);
    }
    row.push_str("  ");
    row.extend(bytes.iter().map(|&byte| shown_byte(byte)));
    row
}

/// A listing line: its address, its bytes (as many as fit, the rest
/// elided) and what they say.
#[must_use]
pub fn code_row(cs: u16, ip: u16, bytes: &[u8], text: &str) -> String {
    const SHOWN: usize = 6;
    let mut hex = String::with_capacity(2 * SHOWN + 1);
    for &byte in bytes.iter().take(SHOWN) {
        push_hex(&mut hex, byte);
    }
    if bytes.len() > SHOWN {
        hex.push('\u{2026}');
    }
    format!("{cs:04X}:{ip:04X}  {hex:<13} {text}")
}

/// A stack row: the word's address and the word.
#[must_use]
pub fn stack_row(segment: u16, offset: u16, word: u16) -> String {
    format!("{segment:04X}:{offset:04X}  {word:04X}")
}

/// `bytes` as a count of them.
fn bytes(len: u32) -> String {
    if len == 1 {
        String::from("1 byte")
    } else {
        format!("{len} bytes")
    }
}

/// A breakpoint as the list writes it, its address as `at` names it.
#[must_use]
pub fn breakpoint(breakpoint: &Breakpoint, at: &str) -> String {
    match *breakpoint {
        Breakpoint::Code(_) => format!("Execute {at}"),
        Breakpoint::Memory { len, access, .. } => {
            let verb = match access {
                Access::Write => "Write",
                Access::Any => "Read or write",
            };
            format!("{verb} {at}, {}", bytes(len))
        }
        Breakpoint::Port {
            first,
            last,
            direction,
        } => {
            let ports = if first == last {
                format!("Port {first:04X}h")
            } else {
                format!("Ports {first:04X}h\u{2013}{last:04X}h")
            };
            let way = match direction {
                Direction::In => "in",
                Direction::Out => "out",
                Direction::Both => "in or out",
            };
            format!("{ports} {way}")
        }
        Breakpoint::Interrupt { vector, ah } => match ah {
            Some(ah) => format!("INT {vector:02X}h, AH={ah:02X}h"),
            None => format!("INT {vector:02X}h"),
        },
    }
}

/// Who made a cycle, as a sentence begins.
const fn master(master: Master) -> &'static str {
    match master {
        Master::Cpu => "The processor",
        Master::Coprocessor => "The 8087",
        Master::Dma => "DMA",
    }
}

/// What a watch saw, in words: a memory address as `place` writes it.
fn hit(hit: Hit, place: impl Fn(u32) -> String) -> String {
    let who = master(hit.master);
    let value = hit.value;
    match hit.cycle {
        Cycle::Read => format!("{who} read {value:02X}h at {}", place(hit.at)),
        Cycle::Write => format!("{who} wrote {value:02X}h to {}", place(hit.at)),
        Cycle::In => format!("{who} read {value:02X}h from port {:04X}h", hit.at),
        Cycle::Out => format!("{who} wrote {value:02X}h to port {:04X}h", hit.at),
    }
}

/// Why the processor stopped, in words, it standing as `registers` says; a
/// memory address as `place` writes it.
#[must_use]
pub fn reason(reason: &Reason, registers: &Registers, place: impl Fn(u32) -> String) -> String {
    let regs = &registers.regs;
    let here = format!("{:04X}:{:04X}", regs.sreg(Seg::Cs), regs.ip);
    let mut said = match reason {
        Reason::Break => format!("Stopped at {here}"),
        Reason::Stepped => format!("Stepped to {here}"),
        Reason::Breakpoint(Breakpoint::Interrupt { vector, .. }) => {
            format!(
                "Stopped entering INT {vector:02X}h, AH={:02X}h",
                regs.get8(4)
            )
        }
        Reason::Breakpoint(_) => format!("Stopped at the breakpoint at {here}"),
        Reason::Watched { hit: seen, .. } => format!("{}, stopping at {here}", hit(*seen, place)),
    };
    match registers.activity {
        Activity::Executing => {}
        Activity::Halted => said.push_str(", halted"),
        Activity::Waiting => said.push_str(", waiting for the 8087"),
    }
    said
}

#[cfg(test)]
#[path = "shown_tests.rs"]
mod tests;
