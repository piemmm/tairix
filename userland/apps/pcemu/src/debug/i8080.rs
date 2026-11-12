//! The V-series' 8080 emulation mode as the listing writes it, in Intel's
//! 8080 mnemonics, each opcode read as the processor's executor reads it:
//! the undocumented `NOP`, `JMP`, `CALL` and `RET` aliases included, and
//! `ED` the prefix of `CALLN` and `RETEM`.

use alloc::format;
use alloc::string::String;

use super::disasm::hex;

/// The registers an opcode's three-bit field names; `M` is the byte at HL.
const REG: [&str; 8] = ["B", "C", "D", "E", "H", "L", "M", "A"];
/// The register pairs LXI, INX, DCX and DAD name.
const PAIR: [&str; 4] = ["B", "D", "H", "SP"];
/// The pairs PUSH and POP name.
const STACKED: [&str; 4] = ["B", "D", "H", "PSW"];
/// The conditions of Jcc, Ccc and Rcc.
const CONDITION: [&str; 8] = ["NZ", "Z", "NC", "C", "PO", "PE", "P", "M"];
const ALU: [&str; 8] = ["ADD", "ADC", "SUB", "SBB", "ANA", "XRA", "ORA", "CMP"];
const ALU_IMMEDIATE: [&str; 8] = ["ADI", "ACI", "SUI", "SBI", "ANI", "XRI", "ORI", "CPI"];

/// How an 8080 instruction leaves the code it is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Flow {
    /// A CALL, a call on condition, an RST or a CALLN: back to the next
    /// instruction, if it returns.
    Calls,
    /// RET, or RET on condition `cc`: its address popped from the 8080's
    /// stack.
    Returns(Option<u8>),
    /// RETEM: back to native code from the native stack.
    LeavesEmulation,
    /// Anything else.
    Other,
}

/// One 8080 instruction: its bytes, what they say, and its flow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Insn {
    /// Its length, one to three bytes.
    pub len: u8,
    pub text: String,
    pub flow: Flow,
}

/// Bytes an instruction takes at most.
pub(crate) const LONGEST: usize = 3;

/// The instruction `bytes`, the three from where it starts, begin.
pub(crate) fn decode(bytes: [u8; LONGEST]) -> Insn {
    let [op, b1, b2] = bytes;
    let d16 = || hex(u16::from_le_bytes([b1, b2]), 4);
    let r = |field: u8| REG[usize::from(field & 7)];
    let p = |field: u8| PAIR[usize::from((field >> 4) & 3)];
    let cc = |field: u8| CONDITION[usize::from((field >> 3) & 7)];
    let other = |len: u8, text: String| Insn {
        len,
        text,
        flow: Flow::Other,
    };
    match op {
        0x00 | 0x08 | 0x10 | 0x18 | 0x20 | 0x28 | 0x30 | 0x38 => other(1, String::from("NOP")),
        0x01 | 0x11 | 0x21 | 0x31 => other(3, format!("LXI {},{}", p(op), d16())),
        0x02 | 0x12 => other(1, format!("STAX {}", p(op))),
        0x0A | 0x1A => other(1, format!("LDAX {}", p(op))),
        0x03 | 0x13 | 0x23 | 0x33 => other(1, format!("INX {}", p(op))),
        0x0B | 0x1B | 0x2B | 0x3B => other(1, format!("DCX {}", p(op))),
        0x09 | 0x19 | 0x29 | 0x39 => other(1, format!("DAD {}", p(op))),
        0x04 | 0x0C | 0x14 | 0x1C | 0x24 | 0x2C | 0x34 | 0x3C => {
            other(1, format!("INR {}", r(op >> 3)))
        }
        0x05 | 0x0D | 0x15 | 0x1D | 0x25 | 0x2D | 0x35 | 0x3D => {
            other(1, format!("DCR {}", r(op >> 3)))
        }
        0x06 | 0x0E | 0x16 | 0x1E | 0x26 | 0x2E | 0x36 | 0x3E => {
            other(2, format!("MVI {},{}", r(op >> 3), hex(u16::from(b1), 2)))
        }
        0x07 => other(1, String::from("RLC")),
        0x0F => other(1, String::from("RRC")),
        0x17 => other(1, String::from("RAL")),
        0x1F => other(1, String::from("RAR")),
        0x22 => other(3, format!("SHLD {}", d16())),
        0x2A => other(3, format!("LHLD {}", d16())),
        0x27 => other(1, String::from("DAA")),
        0x2F => other(1, String::from("CMA")),
        0x32 => other(3, format!("STA {}", d16())),
        0x3A => other(3, format!("LDA {}", d16())),
        0x37 => other(1, String::from("STC")),
        0x3F => other(1, String::from("CMC")),
        0x76 => other(1, String::from("HLT")),
        0x40..=0x7F => other(1, format!("MOV {},{}", r(op >> 3), r(op))),
        0x80..=0xBF => other(1, format!("{} {}", ALU[usize::from((op >> 3) & 7)], r(op))),
        0xC6 | 0xCE | 0xD6 | 0xDE | 0xE6 | 0xEE | 0xF6 | 0xFE => other(
            2,
            format!(
                "{} {}",
                ALU_IMMEDIATE[usize::from((op >> 3) & 7)],
                hex(u16::from(b1), 2)
            ),
        ),
        0xC0 | 0xC8 | 0xD0 | 0xD8 | 0xE0 | 0xE8 | 0xF0 | 0xF8 => Insn {
            len: 1,
            text: format!("R{}", cc(op)),
            flow: Flow::Returns(Some((op >> 3) & 7)),
        },
        0xC9 | 0xD9 => Insn {
            len: 1,
            text: String::from("RET"),
            flow: Flow::Returns(None),
        },
        0xC1 | 0xD1 | 0xE1 | 0xF1 => {
            other(1, format!("POP {}", STACKED[usize::from((op >> 4) & 3)]))
        }
        0xC5 | 0xD5 | 0xE5 | 0xF5 => {
            other(1, format!("PUSH {}", STACKED[usize::from((op >> 4) & 3)]))
        }
        0xC2 | 0xCA | 0xD2 | 0xDA | 0xE2 | 0xEA | 0xF2 | 0xFA => {
            other(3, format!("J{} {}", cc(op), d16()))
        }
        0xC3 | 0xCB => other(3, format!("JMP {}", d16())),
        0xC4 | 0xCC | 0xD4 | 0xDC | 0xE4 | 0xEC | 0xF4 | 0xFC => Insn {
            len: 3,
            text: format!("C{} {}", cc(op), d16()),
            flow: Flow::Calls,
        },
        0xCD | 0xDD | 0xFD => Insn {
            len: 3,
            text: format!("CALL {}", d16()),
            flow: Flow::Calls,
        },
        0xED => extended(b1, b2),
        0xC7 | 0xCF | 0xD7 | 0xDF | 0xE7 | 0xEF | 0xF7 | 0xFF => Insn {
            len: 1,
            text: format!("RST {}", (op >> 3) & 7),
            flow: Flow::Calls,
        },
        0xD3 => other(2, format!("OUT {}", hex(u16::from(b1), 2))),
        0xDB => other(2, format!("IN {}", hex(u16::from(b1), 2))),
        0xE3 => other(1, String::from("XTHL")),
        0xE9 => other(1, String::from("PCHL")),
        0xEB => other(1, String::from("XCHG")),
        0xF3 => other(1, String::from("DI")),
        0xFB => other(1, String::from("EI")),
        0xF9 => other(1, String::from("SPHL")),
    }
}

/// `ED` and the bytes after it: CALLN, RETEM, or the CALL alias.
fn extended(b1: u8, b2: u8) -> Insn {
    match b1 {
        0xED => Insn {
            len: 3,
            text: format!("CALLN {}", hex(u16::from(b2), 2)),
            flow: Flow::Calls,
        },
        0xFD => Insn {
            len: 2,
            text: String::from("RETEM"),
            flow: Flow::LeavesEmulation,
        },
        low => Insn {
            len: 3,
            text: format!("CALL {}", hex(u16::from_le_bytes([low, b2]), 4)),
            flow: Flow::Calls,
        },
    }
}

#[cfg(test)]
#[path = "i8080_tests.rs"]
mod tests;
