//! The debugger's side in the machine worker: the probe it puts on the board
//! ([`probe`]), and what it reads of the machine at a stop — the registers,
//! the listing ([`disasm`]), memory — all of it read and written out here, so
//! the front-end is handed text and bytes it shows without decoding anything
//! a guest controls.

#[cfg(test)]
mod bench;
pub mod disasm;
mod i8080;
pub mod number;
pub mod probe;

use alloc::string::String;

use tairix_inline::ArrayString;

use crate::cpu::{Cpu, Regs};
use crate::fpu::Fpu;

/// Characters a coprocessor register's value is written in at most.
pub const MOST_VALUE: usize = 32;

/// What the processor was doing where it stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    /// About to begin an instruction.
    Executing,
    /// Halted, for an interrupt to end the HLT.
    Halted,
    /// In a WAIT, for the 8087's TEST.
    Waiting,
}

/// The processor at a stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registers {
    /// Its registers.
    pub regs: Regs,
    /// Cycles it has run.
    pub cycles: u64,
    /// What it was doing.
    pub activity: Activity,
    /// Whether a V-series part is in 8080 emulation mode.
    pub emulating: bool,
    /// The 8087, if fitted.
    pub coprocessor: Option<Coprocessor>,
}

/// The 8087 at a stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Coprocessor {
    /// The control word.
    pub control: u16,
    /// The status word, B and IR as the processor reads them.
    pub status: u16,
    /// The tag word.
    pub tags: u16,
    /// The last numeric instruction's physical address and opcode bits.
    pub ip: u32,
    /// Its eleven opcode bits.
    pub opcode: u16,
    /// The last memory operand's physical address.
    pub dp: u32,
    /// ST(0) to ST(7), each as [`number::extended`] writes it, or `Empty`.
    pub stack: [ArrayString<MOST_VALUE>; 8],
}

impl Registers {
    /// `cpu` as it stands, the 8087 beside it as `fpu`.
    #[must_use]
    pub fn of(cpu: &Cpu, fpu: Option<&Fpu>) -> Self {
        let activity = if cpu.halted() {
            Activity::Halted
        } else if cpu.waiting() {
            Activity::Waiting
        } else {
            Activity::Executing
        };
        Self {
            regs: *cpu.regs(),
            cycles: cpu.cycles(),
            activity,
            emulating: cpu.in_8080_mode(),
            coprocessor: fpu.map(Coprocessor::of),
        }
    }
}

/// The tag an empty register has.
const EMPTY: u16 = 3;

impl Coprocessor {
    fn of(fpu: &Fpu) -> Self {
        let regs = fpu.registers();
        let stack = core::array::from_fn(|i| {
            let physical = regs.physical(u8::try_from(i).unwrap_or(0));
            let text = if regs.tag(physical) == EMPTY {
                String::from("Empty")
            } else {
                number::extended(regs.data[physical])
            };
            ArrayString::from_str_truncating(&text)
        });
        Self {
            control: regs.control,
            status: fpu.status(),
            tags: regs.tags,
            ip: regs.ip,
            opcode: regs.opcode,
            dp: regs.dp,
            stack,
        }
    }
}
