//! The instructions that reach memory. Every access goes through the
//! profile's alignment rule and then the bus; an instruction that aborts
//! changes no register, so it can be restarted, as the base-restored abort
//! model both profiles share requires.

use super::alu::shift_immediate;
use super::execute::{sext16, sext8};
use super::{Cpu, Flow};
use crate::bus::{Bus, Width};
use crate::cp15::Fault;
use crate::instruction::{
    BlockMode, BlockOp, DualOp, ExclusiveOp, Index, Offset, SwapOp, TransferOp,
};
use crate::psr::{self, Bank, Mode};
use crate::reg::{Reg, RegList};

const PC: Reg = Reg::PC;

/// A data access that aborted: why, and where.
pub(super) struct Aborted {
    fault: Fault,
    address: u32,
    write: bool,
}

pub(super) type Access<T> = Result<T, Aborted>;

impl Aborted {
    /// An alignment fault at `address`: an access the instruction itself
    /// requires aligned, whatever the profile's rule.
    pub(super) const fn misaligned(address: u32, write: bool) -> Self {
        Self {
            fault: Fault::Alignment,
            address,
            write,
        }
    }
}

/// The first address of a block of `count` words from `base`, and the base
/// written back.
const fn block_range(mode: BlockMode, base: u32, count: u32) -> (u32, u32) {
    let size = count.wrapping_mul(4);
    match mode {
        BlockMode::IncrementAfter => (base, base.wrapping_add(size)),
        BlockMode::IncrementBefore => (base.wrapping_add(4), base.wrapping_add(size)),
        BlockMode::DecrementAfter => (
            base.wrapping_sub(size).wrapping_add(4),
            base.wrapping_sub(size),
        ),
        BlockMode::DecrementBefore => (base.wrapping_sub(size), base.wrapping_sub(size)),
    }
}

impl Cpu {
    /// The data at `address`, under the profile's alignment rule: a fault,
    /// or a misaligned word or halfword rotated as ARMv4 cores rotated it.
    pub(super) fn load<B: Bus>(
        &self,
        bus: &mut B,
        address: u32,
        width: Width,
        privileged: bool,
    ) -> Access<u32> {
        let size = width.bytes();
        let misaligned = address & (size - 1);
        let aborted = |fault| Aborted {
            fault,
            address,
            write: false,
        };
        if misaligned != 0 && self.profile.alignment_faults {
            return Err(aborted(Fault::Alignment));
        }
        let raw = bus
            .read(address - misaligned, width, privileged)
            .map_err(|abort| aborted(Fault::Bus(abort)))?;
        Ok(self.endian(raw, width).rotate_right(8 * misaligned))
    }

    /// Stores `value` at `address`; a misaligned store faults, or writes the
    /// aligned unit the address falls in.
    pub(super) fn store<B: Bus>(
        &self,
        bus: &mut B,
        address: u32,
        width: Width,
        value: u32,
        privileged: bool,
    ) -> Access<()> {
        let size = width.bytes();
        let misaligned = address & (size - 1);
        let aborted = |fault| Aborted {
            fault,
            address,
            write: true,
        };
        if misaligned != 0 && self.profile.alignment_faults {
            return Err(aborted(Fault::Alignment));
        }
        bus.write(
            address - misaligned,
            width,
            self.endian(value, width),
            privileged,
        )
        .map_err(|abort| aborted(Fault::Bus(abort)))
    }

    /// `value` as the data's endianness has it: reversed when the CPSR's E
    /// bit asks for big-endian data.
    const fn endian(&self, value: u32, width: Width) -> u32 {
        if self.cpsr & psr::E == 0 {
            return value;
        }
        match width {
            Width::Byte => value,
            Width::Half => ((value & 0xFF) << 8) | ((value >> 8) & 0xFF),
            Width::Word => value.swap_bytes(),
        }
    }

    /// Takes the data abort an access raised.
    pub(super) fn aborted(&mut self, aborted: &Aborted) -> Flow {
        self.data_abort(aborted.fault, aborted.address, aborted.write);
        Flow::Next
    }

    /// Where a block of words at `start` begins: ARMv4 cores ignore the
    /// address's low bits, and a profile with alignment faults faults them.
    pub(super) const fn block_start(&self, start: u32) -> u32 {
        if self.profile.alignment_faults {
            start
        } else {
            start & !3
        }
    }

    /// A User-bank register, as `LDM` and `STM` with the S bit reach it.
    fn user_reg(&self, reg: Reg) -> u32 {
        let n = reg.index();
        match (self.mode.bank(), n) {
            (Bank::Fiq, 8..=12) => self.stashed_high[n - 8],
            (bank, 13 | 14) if bank != Bank::User => self.stashed[Bank::User.index()][n - 13],
            _ => self.r[n],
        }
    }

    fn set_user_reg(&mut self, reg: Reg, value: u32) {
        let n = reg.index();
        match (self.mode.bank(), n) {
            (Bank::Fiq, 8..=12) => self.stashed_high[n - 8] = value,
            (bank, 13 | 14) if bank != Bank::User => {
                self.stashed[Bank::User.index()][n - 13] = value;
            }
            _ => self.r[n] = value,
        }
    }

    /// A single or dual transfer's address, and the base it writes back.
    fn addressed(&self, rn: Reg, up: bool, offset: Offset, index: Index) -> (u32, u32) {
        let base = self.base(rn);
        let distance = match offset {
            Offset::Immediate(value) => u32::from(value),
            Offset::Register { rm, shift } => {
                shift_immediate(self.read(rm), shift, self.cpsr & psr::C != 0).value
            }
        };
        let offset_address = if up {
            base.wrapping_add(distance)
        } else {
            base.wrapping_sub(distance)
        };
        let address = if index == Index::PostIndexed {
            base
        } else {
            offset_address
        };
        (address, offset_address)
    }

    #[allow(clippy::too_many_arguments)] // One per operand the transfer form has.
    pub(super) fn transfer<B: Bus>(
        &mut self,
        bus: &mut B,
        op: TransferOp,
        rt: Reg,
        rn: Reg,
        up: bool,
        offset: Offset,
        index: Index,
    ) -> Flow {
        use TransferOp as T;
        let (address, offset_address) = self.addressed(rn, up, offset, index);
        let privileged = self.mode.privileged() && !op.unprivileged();
        let moved = match op {
            T::Str | T::Strt => self
                .store(bus, address, Width::Word, self.stored(rt), privileged)
                .map(|()| None),
            T::Strb | T::Strbt => self
                .store(bus, address, Width::Byte, self.stored(rt), privileged)
                .map(|()| None),
            T::Strh | T::Strht => self
                .store(bus, address, Width::Half, self.stored(rt), privileged)
                .map(|()| None),
            T::Ldr | T::Ldrt => self.load(bus, address, Width::Word, privileged).map(Some),
            T::Ldrb | T::Ldrbt => self.load(bus, address, Width::Byte, privileged).map(Some),
            T::Ldrh | T::Ldrht => self.load(bus, address, Width::Half, privileged).map(Some),
            T::Ldrsb | T::Ldrsbt => self
                .load(bus, address, Width::Byte, privileged)
                .map(|byte| Some(sext8(byte))),
            // A misaligned halfword arrives rotated: its byte at the address
            // is what ARMv4 cores sign-extended.
            T::Ldrsh | T::Ldrsht => self
                .load(bus, address, Width::Half, privileged)
                .map(|half| {
                    Some(if address & 1 == 0 {
                        sext16(half)
                    } else {
                        sext8(half)
                    })
                }),
        };
        let loaded = match moved {
            Ok(loaded) => loaded,
            Err(aborted) => return self.aborted(&aborted),
        };
        let loads_pc = rt == PC && matches!(op, T::Ldr | T::Ldrt);
        if let Some(value) = loaded.filter(|_| loads_pc && self.loads_interwork()) {
            if self.misaligned_exchange(value) {
                return Flow::Next;
            }
        }
        if index != Index::Offset {
            self.write(rn, offset_address);
        }
        match loaded {
            Some(value) if loads_pc => self.load_pc(value),
            Some(value) => self.write(rt, value),
            None => {}
        }
        Flow::Next
    }

    #[allow(clippy::too_many_arguments)] // One per operand the dual form has.
    pub(super) fn dual<B: Bus>(
        &mut self,
        bus: &mut B,
        op: DualOp,
        [rt, rt2]: [Reg; 2],
        rn: Reg,
        up: bool,
        offset: Offset,
        index: Index,
    ) -> Flow {
        let (address, offset_address) = self.addressed(rn, up, offset, index);
        let second = address.wrapping_add(4);
        let privileged = self.mode.privileged();
        let moved = match op {
            DualOp::Strd => self
                .store(bus, address, Width::Word, self.read(rt), privileged)
                .and_then(|()| self.store(bus, second, Width::Word, self.read(rt2), privileged))
                .map(|()| None),
            DualOp::Ldrd => self
                .load(bus, address, Width::Word, privileged)
                .and_then(|first| {
                    Ok(Some([
                        first,
                        self.load(bus, second, Width::Word, privileged)?,
                    ]))
                }),
        };
        let loaded = match moved {
            Ok(loaded) => loaded,
            Err(aborted) => return self.aborted(&aborted),
        };
        if index != Index::Offset {
            self.write(rn, offset_address);
        }
        if let Some([first, second]) = loaded {
            self.write(rt, first);
            self.write(rt2, second);
        }
        Flow::Next
    }

    /// A register as a store writes it: R15 as the profile stores it.
    fn stored(&self, reg: Reg) -> u32 {
        if reg == PC {
            self.stored_pc()
        } else {
            self.r[reg.index()]
        }
    }

    #[allow(clippy::too_many_arguments)] // One per operand the block form has.
    pub(super) fn block<B: Bus>(
        &mut self,
        bus: &mut B,
        op: BlockOp,
        rn: Reg,
        mode: BlockMode,
        writeback: bool,
        user: bool,
        list: RegList,
    ) -> Flow {
        let base = self.read(rn);
        let (start, written_back) = block_range(mode, base, list.len());
        let start = self.block_start(start);
        let privileged = self.mode.privileged();
        let address = |n: u32| start.wrapping_add(4 * n);
        match op {
            BlockOp::Stm => {
                for (n, reg) in (0..).zip(list.iter()) {
                    let value = if reg == PC {
                        self.stored_pc()
                    } else if user {
                        self.user_reg(reg)
                    } else {
                        self.r[reg.index()]
                    };
                    if let Err(aborted) =
                        self.store(bus, address(n), Width::Word, value, privileged)
                    {
                        return self.aborted(&aborted);
                    }
                }
                if writeback {
                    self.write(rn, written_back);
                }
            }
            BlockOp::Ldm => {
                let mut values = [0; 16];
                for (n, reg) in (0..).zip(list.iter()) {
                    match self.load(bus, address(n), Width::Word, privileged) {
                        Ok(value) => values[reg.index()] = value,
                        Err(aborted) => return self.aborted(&aborted),
                    }
                }
                let loads_pc = list.contains(PC);
                let interworks = loads_pc && !user && self.loads_interwork();
                if interworks && self.misaligned_exchange(values[PC.index()]) {
                    return Flow::Next;
                }
                if writeback {
                    self.write(rn, written_back);
                }
                for reg in list.iter().filter(|&reg| reg != PC) {
                    if user && !loads_pc {
                        self.set_user_reg(reg, values[reg.index()]);
                    } else {
                        self.r[reg.index()] = values[reg.index()];
                    }
                }
                if loads_pc {
                    self.load_block_pc(values[PC.index()], user);
                }
            }
        }
        Flow::Next
    }

    /// `LDM` loading R15: with the S bit an exception return, which in a
    /// 26-bit mode restores the PSR bits R15's layout holds.
    fn load_block_pc(&mut self, target: u32, user: bool) {
        if !user {
            self.load_pc(target);
        } else if self.mode.is_26bit() {
            self.write_r15_psr(target);
            self.branch(target);
        } else if self.restore_cpsr() {
            self.branch(target);
        } else if !self.refuse() {
            self.load_pc(target);
        }
    }

    pub(super) fn table_branch<B: Bus>(
        &mut self,
        bus: &mut B,
        half: bool,
        rn: Reg,
        rm: Reg,
    ) -> Flow {
        let (base, index) = (self.read(rn), self.read(rm));
        let (address, width) = if half {
            (base.wrapping_add(index << 1), Width::Half)
        } else {
            (base.wrapping_add(index), Width::Byte)
        };
        match self.load(bus, address, width, self.mode.privileged()) {
            Ok(entry) => {
                self.branch(self.pc_operand().wrapping_add(entry << 1));
                Flow::Next
            }
            Err(aborted) => self.aborted(&aborted),
        }
    }

    pub(super) fn swap<B: Bus>(
        &mut self,
        bus: &mut B,
        op: SwapOp,
        rt: Reg,
        rt2: Reg,
        rn: Reg,
    ) -> Flow {
        let address = self.read(rn);
        let width = match op {
            SwapOp::Swp => Width::Word,
            SwapOp::Swpb => Width::Byte,
        };
        let privileged = self.mode.privileged();
        let swapped = self.load(bus, address, width, privileged).and_then(|old| {
            self.store(bus, address, width, self.read(rt2), privileged)
                .map(|()| old)
        });
        match swapped {
            Ok(old) => {
                self.write(rt, old);
                Flow::Next
            }
            Err(aborted) => self.aborted(&aborted),
        }
    }

    #[allow(clippy::too_many_arguments)] // One per operand the exclusive form has.
    pub(super) fn exclusive<B: Bus>(
        &mut self,
        bus: &mut B,
        op: ExclusiveOp,
        rd: Option<Reg>,
        rt: Reg,
        rt2: Option<Reg>,
        rn: Reg,
        offset: u16,
    ) -> Flow {
        use ExclusiveOp as X;
        let address = self.read(rn).wrapping_add(u32::from(offset));
        let privileged = self.mode.privileged();
        let width = match op {
            X::Ldrexb | X::Strexb | X::Ldab | X::Stlb | X::Ldaexb | X::Stlexb => Width::Byte,
            X::Ldrexh | X::Strexh | X::Ldah | X::Stlh | X::Ldaexh | X::Stlexh => Width::Half,
            _ => Width::Word,
        };
        let alignment = if rt2.is_some() { 8 } else { width.bytes() };
        if !address.is_multiple_of(alignment) {
            let aborted = Aborted {
                fault: Fault::Alignment,
                address,
                write: rd.is_some() || matches!(op, X::Stl | X::Stlb | X::Stlh),
            };
            return self.aborted(&aborted);
        }
        let marks = matches!(
            op,
            X::Ldrex
                | X::Ldrexb
                | X::Ldrexh
                | X::Ldrexd
                | X::Ldaex
                | X::Ldaexb
                | X::Ldaexh
                | X::Ldaexd
        );
        let loads = marks || matches!(op, X::Lda | X::Ldab | X::Ldah);
        if loads {
            let loaded = self
                .load(bus, address, width, privileged)
                .and_then(|first| {
                    let second = match rt2 {
                        Some(_) => {
                            self.load(bus, address.wrapping_add(4), Width::Word, privileged)?
                        }
                        None => 0,
                    };
                    Ok((first, second))
                });
            let (first, second) = match loaded {
                Ok(values) => values,
                Err(aborted) => return self.aborted(&aborted),
            };
            if marks {
                self.exclusive = Some(address);
            }
            self.write(rt, first);
            if let Some(rt2) = rt2 {
                self.write(rt2, second);
            }
            return Flow::Next;
        }
        let Some(status) = rd else {
            return match self.store(bus, address, width, self.read(rt), privileged) {
                Ok(()) => Flow::Next,
                Err(aborted) => self.aborted(&aborted),
            };
        };
        let held = self.exclusive.take() == Some(address);
        if held {
            let stored = self
                .store(bus, address, width, self.read(rt), privileged)
                .and_then(|()| match rt2 {
                    Some(rt2) => self.store(
                        bus,
                        address.wrapping_add(4),
                        Width::Word,
                        self.read(rt2),
                        privileged,
                    ),
                    None => Ok(()),
                });
            if let Err(aborted) = stored {
                return self.aborted(&aborted);
            }
        }
        self.write(status, u32::from(!held));
        Flow::Next
    }

    pub(super) fn save_state<B: Bus>(
        &mut self,
        bus: &mut B,
        mode: BlockMode,
        writeback: bool,
        bank: u8,
    ) -> Flow {
        let target = Mode::from_bits(u32::from(bank)).filter(|target| !target.is_26bit());
        let (Some(target), Some(spsr)) = (target, self.spsr()) else {
            self.refuse();
            return Flow::Next;
        };
        let target = target.bank();
        let current = self.mode.bank();
        let sp = if target == current {
            self.r[13]
        } else {
            self.stashed[target.index()][0]
        };
        let (start, written_back) = block_range(mode, sp, 2);
        let privileged = self.mode.privileged();
        let saved = self
            .store(bus, start, Width::Word, self.r[14], privileged)
            .and_then(|()| self.store(bus, start.wrapping_add(4), Width::Word, spsr, privileged));
        if let Err(aborted) = saved {
            return self.aborted(&aborted);
        }
        if writeback {
            if target == current {
                self.r[13] = written_back;
            } else {
                self.stashed[target.index()][0] = written_back;
            }
        }
        Flow::Next
    }

    pub(super) fn return_from_exception<B: Bus>(
        &mut self,
        bus: &mut B,
        rn: Reg,
        mode: BlockMode,
        writeback: bool,
    ) -> Flow {
        if !self.mode.privileged() && self.refuse() {
            return Flow::Next;
        }
        let base = self.read(rn);
        let (start, written_back) = block_range(mode, base, 2);
        let privileged = self.mode.privileged();
        let loaded = self
            .load(bus, start, Width::Word, privileged)
            .and_then(|pc| {
                Ok((
                    pc,
                    self.load(bus, start.wrapping_add(4), Width::Word, privileged)?,
                ))
            });
        let (target, status) = match loaded {
            Ok(words) => words,
            Err(aborted) => return self.aborted(&aborted),
        };
        if writeback {
            self.write(rn, written_back);
        }
        self.write_cpsr(status, u32::MAX, true);
        self.branch(target);
        Flow::Next
    }
}
