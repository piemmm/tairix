//! Addressing modes, as the bus cycles each performs.
//!
//! A mode is a marker type implementing the operand flavours it supports:
//! [`Read`] for instructions that consume a value, [`Write`] for stores, and
//! [`Modify`] for read-modify-write. The flavours differ in their dummy cycles:
//! an indexed read touches the uncorrected address only when the index carries
//! into the high byte, while an indexed store or modify always does, and a
//! modify writes the unmodified value back before the result.

use super::{Bus, Cpu};

/// No operand.
pub struct Implied;
/// The accumulator.
pub struct Accumulator;
/// The byte after the opcode.
pub struct Immediate;
/// A zero-page address.
pub struct ZeroPage;
/// A zero-page address plus X.
pub struct ZeroPageX;
/// A zero-page address plus Y.
pub struct ZeroPageY;
/// A 16-bit address.
pub struct Absolute;
/// A 16-bit address plus X.
pub struct AbsoluteX;
/// A 16-bit address plus Y.
pub struct AbsoluteY;
/// `JMP (abs)`.
pub struct Indirect;
/// `(zp,X)`.
pub struct IndexedIndirect;
/// `(zp),Y`.
pub struct IndirectIndexed;
/// A branch displacement.
pub struct Relative;

/// A mode an instruction can read its operand through.
pub trait Read {
    /// Perform the mode's cycles and the final read, returning the operand.
    fn read<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> u8;
}

/// A mode an instruction can store through.
pub trait Write {
    /// Perform the mode's cycles up to, not including, the store, returning
    /// the address to store to.
    fn address<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> u16;
}

/// A mode an instruction can read, modify and write back through.
pub trait Modify {
    /// Perform the mode's cycles, applying `op` to the operand.
    fn modify<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: impl FnOnce(&mut Cpu, u8) -> u8);
}

/// Every [`Write`] mode modifies memory the same way: read, write the old value
/// back, write the new one.
fn modify_memory<B: Bus>(
    cpu: &mut Cpu,
    bus: &mut B,
    addr: u16,
    op: impl FnOnce(&mut Cpu, u8) -> u8,
) {
    let old = cpu.read(bus, addr);
    cpu.write(bus, addr, old);
    let new = op(cpu, old);
    cpu.write(bus, addr, new);
}

impl Read for Immediate {
    fn read<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> u8 {
        cpu.fetch(bus)
    }
}

macro_rules! memory_modes {
    ($($mode:ident),*) => {$(
        impl Read for $mode {
            fn read<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> u8 {
                let addr = $mode::read_address(cpu, bus);
                cpu.read(bus, addr)
            }
        }

        impl Write for $mode {
            fn address<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> u16 {
                $mode::write_address(cpu, bus)
            }
        }

        impl Modify for $mode {
            fn modify<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: impl FnOnce(&mut Cpu, u8) -> u8) {
                let addr = $mode::write_address(cpu, bus);
                modify_memory(cpu, bus, addr, op);
            }
        }
    )*};
}

memory_modes!(
    ZeroPage,
    ZeroPageX,
    ZeroPageY,
    Absolute,
    AbsoluteX,
    AbsoluteY,
    IndexedIndirect,
    IndirectIndexed
);

impl Modify for Accumulator {
    fn modify<B: Bus>(cpu: &mut Cpu, bus: &mut B, op: impl FnOnce(&mut Cpu, u8) -> u8) {
        cpu.dummy_read_pc(bus);
        let old = cpu.a;
        cpu.a = op(cpu, old);
    }
}

impl ZeroPage {
    fn read_address<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> u16 {
        u16::from(cpu.fetch(bus))
    }

    fn write_address<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> u16 {
        Self::read_address(cpu, bus)
    }
}

/// A zero-page base plus an index: the base is read while the index is added,
/// and the sum wraps within page zero.
fn zero_page_indexed<B: Bus>(cpu: &mut Cpu, bus: &mut B, index: u8) -> u16 {
    let base = cpu.fetch(bus);
    cpu.read(bus, u16::from(base));
    u16::from(base.wrapping_add(index))
}

impl ZeroPageX {
    fn read_address<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> u16 {
        let index = cpu.x;
        zero_page_indexed(cpu, bus, index)
    }

    fn write_address<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> u16 {
        Self::read_address(cpu, bus)
    }
}

impl ZeroPageY {
    fn read_address<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> u16 {
        let index = cpu.y;
        zero_page_indexed(cpu, bus, index)
    }

    fn write_address<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> u16 {
        Self::read_address(cpu, bus)
    }
}

impl Absolute {
    fn read_address<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> u16 {
        cpu.fetch_word(bus)
    }

    fn write_address<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> u16 {
        Self::read_address(cpu, bus)
    }
}

/// `base + index`, and the address the CPU puts on the bus first: the low byte
/// already carries the index, the high byte does not yet carry its carry.
pub(super) fn indexed(base: u16, index: u8) -> (u16, u16) {
    let target = base.wrapping_add(u16::from(index));
    let uncorrected = (base & 0xFF00) | (target & 0x00FF);
    (target, uncorrected)
}

/// An indexed read: the uncorrected address is read only when it differs.
fn index_for_read<B: Bus>(cpu: &mut Cpu, bus: &mut B, base: u16, index: u8) -> u16 {
    let (target, uncorrected) = indexed(base, index);
    if target != uncorrected {
        cpu.read(bus, uncorrected);
    }
    target
}

/// An indexed store or modify: the uncorrected address is always read.
fn index_for_write<B: Bus>(cpu: &mut Cpu, bus: &mut B, base: u16, index: u8) -> u16 {
    let (target, uncorrected) = indexed(base, index);
    cpu.read(bus, uncorrected);
    target
}

impl AbsoluteX {
    fn read_address<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> u16 {
        let base = cpu.fetch_word(bus);
        let index = cpu.x;
        index_for_read(cpu, bus, base, index)
    }

    fn write_address<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> u16 {
        let base = cpu.fetch_word(bus);
        let index = cpu.x;
        index_for_write(cpu, bus, base, index)
    }
}

impl AbsoluteY {
    fn read_address<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> u16 {
        let base = cpu.fetch_word(bus);
        let index = cpu.y;
        index_for_read(cpu, bus, base, index)
    }

    fn write_address<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> u16 {
        let base = cpu.fetch_word(bus);
        let index = cpu.y;
        index_for_write(cpu, bus, base, index)
    }
}

/// The pointer a zero-page address holds, its high byte read from the next
/// address within page zero.
pub(super) fn zero_page_pointer<B: Bus>(cpu: &mut Cpu, bus: &mut B, at: u8) -> u16 {
    let lo = cpu.read(bus, u16::from(at));
    let hi = cpu.read(bus, u16::from(at.wrapping_add(1)));
    u16::from_le_bytes([lo, hi])
}

impl IndexedIndirect {
    fn read_address<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> u16 {
        let at = cpu.fetch(bus);
        cpu.read(bus, u16::from(at));
        let index = cpu.x;
        zero_page_pointer(cpu, bus, at.wrapping_add(index))
    }

    fn write_address<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> u16 {
        Self::read_address(cpu, bus)
    }
}

impl IndirectIndexed {
    fn read_address<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> u16 {
        let at = cpu.fetch(bus);
        let base = zero_page_pointer(cpu, bus, at);
        let index = cpu.y;
        index_for_read(cpu, bus, base, index)
    }

    fn write_address<B: Bus>(cpu: &mut Cpu, bus: &mut B) -> u16 {
        let at = cpu.fetch(bus);
        let base = zero_page_pointer(cpu, bus, at);
        let index = cpu.y;
        index_for_write(cpu, bus, base, index)
    }
}
