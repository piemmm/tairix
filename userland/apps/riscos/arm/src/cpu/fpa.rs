//! The FPA's instructions on the processor, as FPEmulator executes them: R15
//! as it reads and writes it, System mode's R13 and R14 as it loses writes to
//! them, and memory as it reaches it, a word at a time without privilege
//! from User mode and otherwise as an aligned block.
//!
//! Where FPEmulator's result depends on its own workings rather than the
//! program's, the FPA's hardware decides instead: an instruction in FIQ mode,
//! which FPEmulator does not survive, or in its own Undefined mode, sees the
//! registers as the processor banks them; and an aborted transfer takes the
//! processor's data abort with its base restored, where FPEmulator takes the
//! abort inside itself.

use super::memory::Access;
use super::{Cpu, Flow, Stop};
use crate::bus::{Bus, Width};
use crate::fpa::{Exception, Format, Fpa, Precision, Rounding};
use crate::instruction::{
    CoIndex, FpaCompareOp, FpaMultipleOp, FpaOperand, FpaRegisterOp, FpaTransferOp,
};
use crate::psr::{self, Mode};
use crate::reg::Reg;

const PC: Reg = Reg::PC;

/// The words `LFM` and `SFM` move for each register: its internal format.
const MULTIPLE_WORDS: usize = 3;

/// The most registers `LFM` and `SFM` move.
const MULTIPLE_REGISTERS: usize = 4;

/// The stop an enabled exception's trap makes.
const fn trapped(exception: Exception) -> Flow {
    Flow::Stop(Stop::FloatingPoint { exception })
}

/// The run's course after an FPA instruction that writes no core register.
pub(super) const fn completed(outcome: Result<(), Exception>) -> Flow {
    match outcome {
        Ok(()) => Flow::Next,
        Err(exception) => trapped(exception),
    }
}

impl Cpu {
    /// The FPA's registers and status.
    #[must_use]
    pub const fn fpa(&self) -> &Fpa {
        &self.fpa
    }

    /// The FPA's registers and status, to change.
    pub const fn fpa_mut(&mut self) -> &mut Fpa {
        &mut self.fpa
    }

    /// `FLT`, `FIX`, `WFS` and `RFS`.
    pub(super) fn fpa_register(
        &mut self,
        op: FpaRegisterOp,
        precision: Precision,
        rounding: Rounding,
        fn_: u8,
        fm: FpaOperand,
        rd: Reg,
    ) -> Flow {
        match op {
            FpaRegisterOp::Flt => {
                let value = self.fpa_source(rd, self.float_pc());
                completed(self.fpa.float(precision, rounding, fn_, value))
            }
            FpaRegisterOp::Fix => match self.fpa.fix(rounding, fm) {
                Ok(value) => {
                    self.fpa_result(rd, value);
                    Flow::Next
                }
                Err(exception) => trapped(exception),
            },
            FpaRegisterOp::Wfs => {
                let value = self.fpa_source(rd, self.advance(self.pc, 12) | self.r15_psr());
                self.fpa.write_fpsr(value);
                Flow::Next
            }
            FpaRegisterOp::Rfs => {
                self.fpa_result(rd, self.fpa.fpsr());
                Flow::Next
            }
        }
    }

    /// `CMF`, `CNF`, `CMFE` and `CNFE`.
    pub(super) fn fpa_compare(
        &mut self,
        op: FpaCompareOp,
        fn_: u8,
        fm: FpaOperand,
        rd: Reg,
    ) -> Flow {
        match self.fpa.compare(op, fn_, fm) {
            Ok(flags) => {
                self.fpa_result(rd, flags);
                Flow::Next
            }
            Err(exception) => trapped(exception),
        }
    }

    /// `rd` as a register transfer's source, with `pc` for R15.
    fn fpa_source(&self, rd: Reg, pc: u32) -> u32 {
        self.r.get(rd.index()).copied().unwrap_or(pc)
    }

    /// R15 as `FLT` reads it: in User mode FPEmulator's saved PC, the next
    /// instruction's address, and in a privileged mode R14, the register it
    /// reaches for R15's number there.
    const fn float_pc(&self) -> u32 {
        if self.mode.privileged() {
            self.r[14]
        } else {
            self.next | self.r15_psr()
        }
    }

    /// Whether a write to `reg` lasts: FPEmulator writes System mode's R13
    /// and R14 and then restores them from what it saved on entry.
    fn fpa_keeps(&self, reg: Reg) -> bool {
        self.mode != Mode::System || !matches!(reg.index(), 13 | 14)
    }

    /// Writes a register transfer's result: to R15, its flags alone.
    fn fpa_result(&mut self, rd: Reg, value: u32) {
        if !self.fpa_keeps(rd) {
            return;
        }
        match self.r.get_mut(rd.index()) {
            Some(slot) => *slot = value,
            None => self.cpsr = (self.cpsr & !psr::NZCV) | (value & psr::NZCV),
        }
    }

    /// `LDF` and `STF`. A load writes its base back before it converts, so a
    /// packed load its trap stops has done so, as FPEmulator's has.
    #[allow(clippy::too_many_arguments)] // One per operand the transfer form has.
    pub(super) fn fpa_transfer<B: Bus>(
        &mut self,
        bus: &mut B,
        op: FpaTransferOp,
        format: Format,
        fd: u8,
        rn: Reg,
        up: bool,
        index: CoIndex,
        offset: u8,
    ) -> Flow {
        let (address, written_back) = self.fpa_address(rn, up, index, offset);
        let mut words = [0; 4];
        let count = self.fpa.transfer_words(format);
        match op {
            FpaTransferOp::Ldf => {
                if let Err(aborted) = self.fpa_read(bus, address, &mut words[..count]) {
                    return self.aborted(&aborted);
                }
                self.fpa_write_back(rn, written_back);
                completed(self.fpa.load(format, fd, words))
            }
            FpaTransferOp::Stf => {
                words = match self.fpa.store(format, fd) {
                    Ok(words) => words,
                    Err(exception) => return trapped(exception),
                };
                if let Err(aborted) = self.fpa_write(bus, address, &words[..count]) {
                    return self.aborted(&aborted);
                }
                self.fpa_write_back(rn, written_back);
                Flow::Next
            }
        }
    }

    /// `LFM` and `SFM`: `count` registers from `fd`, wrapping past F7, each
    /// as its three words of the internal format.
    #[allow(clippy::too_many_arguments)] // One per operand the multiple form has.
    pub(super) fn fpa_multiple<B: Bus>(
        &mut self,
        bus: &mut B,
        op: FpaMultipleOp,
        fd: u8,
        count: u8,
        rn: Reg,
        up: bool,
        index: CoIndex,
        offset: u8,
    ) -> Flow {
        let (address, written_back) = self.fpa_address(rn, up, index, offset);
        let registers = (0..count).map(|n| fd.wrapping_add(n) & 7);
        let mut words = [[0; MULTIPLE_WORDS]; MULTIPLE_REGISTERS];
        let held = words.len().min(usize::from(count));
        let span = &mut words[..held];
        match op {
            FpaMultipleOp::Sfm => {
                for (slot, register) in span.iter_mut().zip(registers) {
                    *slot = self.fpa.store_multiple(register);
                }
                if let Err(aborted) = self.fpa_write(bus, address, span.as_flattened()) {
                    return self.aborted(&aborted);
                }
            }
            FpaMultipleOp::Lfm => {
                if let Err(aborted) = self.fpa_read(bus, address, span.as_flattened_mut()) {
                    return self.aborted(&aborted);
                }
                for (register, &slot) in registers.zip(span.iter()) {
                    self.fpa.load_multiple(register, slot);
                }
            }
        }
        self.fpa_write_back(rn, written_back);
        Flow::Next
    }

    /// A transfer's address and the base it writes back, as FPEmulator
    /// computes them: R15 is the instruction's address plus 8, and is never
    /// written back.
    fn fpa_address(&self, rn: Reg, up: bool, index: CoIndex, offset: u8) -> (u32, Option<u32>) {
        let base = self.read(rn);
        let distance = 4 * u32::from(offset);
        let moved = if up {
            base.wrapping_add(distance)
        } else {
            base.wrapping_sub(distance)
        };
        let address = match index {
            CoIndex::Offset | CoIndex::PreIndexed => moved,
            CoIndex::PostIndexed | CoIndex::Unindexed => base,
        };
        let writes_back = matches!(index, CoIndex::PreIndexed | CoIndex::PostIndexed);
        (address, (writes_back && rn != PC).then_some(moved))
    }

    fn fpa_write_back(&mut self, rn: Reg, written_back: Option<u32>) {
        if let Some(base) = written_back.filter(|_| self.fpa_keeps(rn)) {
            self.write(rn, base);
        }
    }

    /// Where a transfer's words start, and with what privilege it reaches
    /// them: from User mode FPEmulator moves each word as `LDRT` and `STRT`
    /// do, and otherwise as `LDM` and `STM` move a block.
    const fn fpa_access(&self, address: u32) -> (u32, bool) {
        let privileged = self.mode.privileged();
        if privileged {
            (self.block_start(address), true)
        } else {
            (address, false)
        }
    }

    fn fpa_read<B: Bus>(&self, bus: &mut B, address: u32, words: &mut [u32]) -> Access<()> {
        let (start, privileged) = self.fpa_access(address);
        for (n, word) in (0u32..).zip(words.iter_mut()) {
            *word = self.load(bus, start.wrapping_add(4 * n), Width::Word, privileged)?;
        }
        Ok(())
    }

    fn fpa_write<B: Bus>(&self, bus: &mut B, address: u32, words: &[u32]) -> Access<()> {
        let (start, privileged) = self.fpa_access(address);
        for (n, &word) in (0u32..).zip(words) {
            self.store(
                bus,
                start.wrapping_add(4 * n),
                Width::Word,
                word,
                privileged,
            )?;
        }
        Ok(())
    }
}
