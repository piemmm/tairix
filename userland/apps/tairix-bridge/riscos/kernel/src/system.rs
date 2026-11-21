//! The kernel's smaller SWIs about the caller's own state and the session's
//! devices (`plans/RISCOS-EMULATOR.md` §4.5): interrupts and modes, the
//! keyboard and pointer as `KeyV` and `MouseV` deliver them, redirection, the
//! serial port the emulator lacks, and `OS_Heap`.

use tairix_riscos_arm::psr::{self, Mode};
use tairix_riscos_arm::reg::Reg;

use crate::heap::{self, Heap, HeapError};
use crate::kheap::heap_error;
use crate::memory::layout::HIGH_ZERO_PAGE;
use crate::os::{Entry, Fail, Os, Regs};
use crate::vectors::{self, Carry};
use crate::zeropage;

impl Os<'_> {
    /// `OS_IntOn`: interrupts enabled in the caller's PSR.
    #[expect(clippy::unnecessary_wraps, reason = "a SWI handler's signature")]
    pub(crate) fn int_on(&mut self, _regs: &mut Regs) -> Result<(), Fail> {
        let cpu = self.world.processor.cpu();
        let cpsr = cpu.cpsr() & !psr::I;
        let _ = cpu.set_cpsr(cpsr);
        Ok(())
    }

    /// `OS_IntOff`: interrupts disabled in the caller's PSR.
    #[expect(clippy::unnecessary_wraps, reason = "a SWI handler's signature")]
    pub(crate) fn int_off(&mut self, _regs: &mut Regs) -> Result<(), Fail> {
        let cpu = self.world.processor.cpu();
        let cpsr = cpu.cpsr() | psr::I;
        let _ = cpu.set_cpsr(cpsr);
        Ok(())
    }

    /// `OS_EnterOS`: the caller continues in Supervisor mode, 26-bit if it
    /// was, R14 its return address.
    #[expect(clippy::unnecessary_wraps, reason = "a SWI handler's signature")]
    pub(crate) fn enter_os(&mut self, _regs: &mut Regs) -> Result<(), Fail> {
        let cpu = self.world.processor.cpu();
        let back = cpu.pc();
        let mode = if cpu.mode().is_26bit() {
            Mode::Supervisor26
        } else {
            Mode::Supervisor
        };
        let cpsr = (cpu.cpsr() & !psr::M) | mode.bits();
        if cpu.set_cpsr(cpsr).is_ok() {
            cpu.set_reg(Reg::LR, back);
        }
        Ok(())
    }

    /// `OS_LeaveOS`: the caller continues in User mode, 26-bit if it was.
    #[expect(clippy::unnecessary_wraps, reason = "a SWI handler's signature")]
    pub(crate) fn leave_os(&mut self, _regs: &mut Regs) -> Result<(), Fail> {
        let cpu = self.world.processor.cpu();
        let mode = if cpu.mode().is_26bit() {
            Mode::User26
        } else {
            Mode::User
        };
        let cpsr = (cpu.cpsr() & !psr::M) | mode.bits();
        let _ = cpu.set_cpsr(cpsr);
        Ok(())
    }

    /// `OS_Mouse`, through `MouseV` with interrupts disabled.
    pub(crate) fn mouse(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        self.vectored(vectors::MOUSE, regs, Entry::SupervisorNoIrq, Carry::All)
    }

    /// `OS_ChangeRedirection`: the input and output redirection handles,
    /// each changed unless 256 or more; the old ones returned. Output
    /// redirected takes `WrchV`; output ended gives it up.
    pub(crate) fn change_redirection(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let input_at = HIGH_ZERO_PAGE + zeropage::REDIRECT_IN;
        let output_at = HIGH_ZERO_PAGE + zeropage::REDIRECT_OUT;
        let old_in = u32::from(self.read_byte(input_at)?);
        let old_out = u32::from(self.read_byte(output_at)?);
        let routine = self
            .world
            .routine_address(crate::host::Routine::RedirectOutput);
        if regs.r[1] < 0x100 {
            let mut claim = Regs::new([vectors::WRCH.0, routine, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            match (old_out, regs.r[1]) {
                (0, new) if new != 0 => self.claim(&mut claim)?,
                (old, 0) if old != 0 => self.release(&mut claim)?,
                _ => {}
            }
            self.write_byte(output_at, regs.byte(1))?;
        }
        if regs.r[0] < 0x100 {
            self.write_byte(input_at, regs.byte(0))?;
        }
        regs.r[0] = old_in;
        regs.r[1] = old_out;
        Ok(())
    }

    /// The output redirection's `WrchV` claimant: the character to the
    /// redirection's file, the call claimed. A write the file refuses ends
    /// all redirection, the character's write failing with its error.
    pub(crate) fn redirect_output(&mut self) -> Result<(), crate::os::Unwind> {
        let cpu = self.world.processor.cpu();
        let c = cpu.reg(Reg::field(0));
        let sp = cpu.reg(Reg::SP);
        let handle = self
            .peek_byte(HIGH_ZERO_PAGE + zeropage::REDIRECT_OUT)
            .unwrap_or(0);
        if handle != 0 {
            let mut put = Regs::new([c, u32::from(handle), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            match self.perform(crate::swi::X | crate::swi::os::BPUT, &mut put) {
                Ok(()) => {}
                Err(crate::os::Fail::Unwind(unwind)) => return Err(unwind),
                Err(crate::os::Fail::Error(block)) => {
                    self.end_redirection();
                    let cpu = self.world.processor.cpu();
                    cpu.set_reg(Reg::field(0), block);
                    cpu.set_nzcv(cpu.cpsr() | psr::V);
                }
            }
        }
        let back = self
            .peek_word(sp)
            .ok_or(crate::os::Unwind::Ended(crate::os::Ended::Fault))?;
        let cpu = self.world.processor.cpu();
        cpu.set_reg(Reg::SP, sp.wrapping_add(4));
        let target = if cpu.mode().is_26bit() {
            back & psr::R15_PC
        } else {
            back
        };
        cpu.set_reg(Reg::PC, target);
        Ok(())
    }

    /// `OS_SerialOp`: through `SerialV`, whose default claims and does
    /// nothing, there being no serial port.
    pub(crate) fn serial_op(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        self.vectored(vectors::SERIAL, regs, Entry::Supervisor, Carry::Vector)
    }

    /// The stack-reset routine `OS_TaskControl` hands out, entered from a
    /// privileged mode: the interrupt semaphore cleared and every privileged
    /// stack emptied, then a return.
    pub(crate) fn reset_stacks(&mut self) -> Result<(), crate::os::Unwind> {
        self.reset_semaphores();
        let bits = self.world.processor.bits();
        let state = self.world.processor.state();
        let back = self.world.processor.cpu().reg(Reg::LR);
        let mut state = self.emptied_stacks(state);
        state.pc = back;
        self.world
            .processor
            .load(bits, &state)
            .map_err(|_| crate::os::Unwind::Ended(crate::os::Ended::Fault))
    }

    /// Clears the interrupt semaphore, as emptying the privileged stacks
    /// for a new application does.
    pub(crate) fn reset_semaphores(&mut self) {
        let _ = self.poke_word(HIGH_ZERO_PAGE + zeropage::IRQSEMA, 0);
    }

    /// `OS_Heap`.
    pub(crate) fn os_heap(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let heap_at = regs.r[1];
        let result = match regs.r[0] {
            0 => heap::initialise(self, heap_at, regs.r[3]),
            1 => Heap::open(self, heap_at)
                .and_then(|h| h.describe(self))
                .map(|(largest, total)| {
                    regs.r[2] = largest;
                    regs.r[3] = total;
                }),
            2 => {
                let claimed = Heap::open(self, heap_at).and_then(|mut h| h.get(self, regs.r[3]));
                claim_answer(regs, claimed)
            }
            3 => Heap::open(self, heap_at).and_then(|mut h| h.free(self, regs.r[2])),
            4 => Heap::open(self, heap_at)
                .and_then(|mut h| h.extend_block(self, regs.r[2], regs.r[3].cast_signed()))
                .map(|block| regs.r[2] = block.unwrap_or(u32::MAX)),
            5 => match Heap::open(self, heap_at)
                .and_then(|mut h| h.extend_heap(self, regs.r[3].cast_signed()))
            {
                Ok(change) => {
                    regs.r[3] = change.cast_unsigned();
                    Ok(())
                }
                Err(HeapError::ExcessiveShrink { removed }) => {
                    regs.r[3] = removed;
                    Err(HeapError::ExcessiveShrink { removed })
                }
                Err(error) => {
                    regs.r[3] = 0;
                    Err(error)
                }
            },
            6 => Heap::open(self, heap_at)
                .and_then(|h| h.block_size(self, regs.r[2]))
                .map(|size| regs.r[3] = size),
            reason @ (7 | 8) => {
                let skew = if reason == 8 { regs.r[5] } else { heap_at };
                let claimed = Heap::open(self, heap_at)
                    .and_then(|mut h| h.get_aligned(self, regs.r[3], regs.r[2], regs.r[4], skew));
                claim_answer(regs, claimed)
            }
            _ => Err(HeapError::BadReason),
        };
        result.map_err(|error| self.error(heap_error(error)))
    }
}

/// A claim's answer: R2 the block, or zero when there is none.
fn claim_answer(regs: &mut Regs, claimed: Result<u32, HeapError>) -> Result<(), HeapError> {
    regs.r[2] = *claimed.as_ref().unwrap_or(&0);
    claimed.map(drop)
}

#[cfg(test)]
mod tests;
