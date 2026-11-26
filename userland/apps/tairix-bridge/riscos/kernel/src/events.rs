//! Events (`plans/RISCOS-EMULATOR.md` §4.2): each of 0 to 31 delivered
//! through `EventV` only while `OS_Byte` 14 has enabled it more often than 13
//! has disabled it; a higher number, which no count covers, always is.

use tairix_riscos_arm::psr::{self, Mode};

use crate::env::EVENT;
use crate::os::{Entry, Fail, Os, Regs, Target, Unwind};
use crate::vectors;

/// The events the kernel's own parts raise, by number.
pub(crate) mod number {
    /// An output buffer emptied.
    pub(crate) const OUTPUT_EMPTY: u32 = 0;
    /// An input buffer full, a byte lost.
    pub(crate) const INPUT_FULL: u32 = 1;
    /// A character into the keyboard buffer.
    pub(crate) const CHAR_INPUT: u32 = 2;
    /// The interval timer passing zero.
    pub(crate) const INTERVAL: u32 = 5;
    /// Escape.
    pub(crate) const ESCAPE: u32 = 6;
    /// A mouse button changing.
    pub(crate) const MOUSE: u32 = 10;
    /// A key going up or down.
    pub(crate) const KEYBOARD: u32 = 11;
}

/// The events that are counted.
const COUNTED: u32 = 32;
/// A count that has reached this stays: the event is enabled for good.
const SATURATED: u8 = 0xFF;

/// Each counted event's enable count.
pub(crate) struct Events {
    counts: [u8; COUNTED as usize],
}

impl Events {
    pub(crate) const fn new() -> Self {
        Self {
            counts: [0; COUNTED as usize],
        }
    }

    /// `OS_Byte` 14 (`enable`) or 13 on `event`: the count as it was; 255
    /// for an event past the counted. Disabling a count of zero wraps it to
    /// 255, enabled for good: RISC OS does, though the PRM has it left alone.
    pub(crate) fn change(&mut self, event: u32, enable: bool) -> u8 {
        let Some(count) = self.counts.get_mut(event as usize) else {
            return SATURATED;
        };
        let old = *count;
        if old != SATURATED {
            *count = if enable { old + 1 } else { old.wrapping_sub(1) };
        }
        old
    }

    /// Whether `event` is delivered.
    pub(crate) fn enabled(&self, event: u32) -> bool {
        self.counts
            .get(event as usize)
            .is_none_or(|&count| count != 0)
    }
}

impl Os<'_> {
    /// Whether `event` is delivered: what a source of an event costly to
    /// time asks before arranging it.
    #[must_use]
    pub fn event_enabled(&self, event: u32) -> bool {
        self.world.events.enabled(event)
    }

    /// Raises the event `regs` names in R0, its parameters in R1 onward:
    /// through `EventV` with interrupts disabled if it is enabled. Whether it
    /// was.
    ///
    /// # Errors
    ///
    /// The unwind a claimant starts.
    pub fn raise_event(&mut self, regs: &mut Regs) -> Result<bool, Unwind> {
        if !self.world.events.enabled(regs.r[0]) {
            return Ok(false);
        }
        let entry = self.event_entry();
        let mut call = *regs;
        self.call_vector(vectors::EVENT, &mut call, entry)?;
        Ok(true)
    }

    /// The entry code an event reaches is called with: IRQ mode's from IRQ
    /// mode, otherwise Supervisor mode's with interrupts disabled.
    #[must_use]
    pub fn event_entry(&mut self) -> Entry {
        if self.world.processor.cpu().mode() == Mode::Irq {
            Entry::Irq
        } else {
            Entry::SupervisorNoIrq
        }
    }

    /// `OS_GenerateEvent`: the caller's registers and flags come back as
    /// they went, so it cannot tell whether the event was enabled.
    pub(crate) fn generate_event(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let mut event = *regs;
        self.raise_event(&mut event)?;
        Ok(())
    }

    /// `EventV`'s default claimant: the event handler, with the event in R0
    /// and its R12; one returning with R12 1 asks for the legacy callback.
    pub(crate) fn default_event(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let slot = self.world.env.slot(EVENT);
        if self.world.env.is_default(EVENT) {
            return Ok(());
        }
        let mut call = *regs;
        call.r[12] = slot.workspace;
        let entry = self.event_entry();
        let target = Target {
            address: slot.address,
            bits: slot.bits,
        };
        self.call(target, &mut call, entry)?;
        if call.r[12] == 1 {
            self.world.callbacks.request_old_style();
        }
        Ok(())
    }

    /// `OS_Byte` 13 and 14.
    pub(crate) fn byte_event(&mut self, enable: bool, regs: &mut Regs) {
        regs.r[1] = u32::from(self.world.events.change(regs.r[1], enable));
    }

    /// The escape handler, told the escape condition is now `set`: one
    /// returning with R12 1 asks for the legacy callback.
    pub(crate) fn tell_escape_handler(&mut self, set: bool) -> Result<(), Unwind> {
        if self.world.env.is_default(crate::env::ESCAPE) {
            return Ok(());
        }
        let slot = self.world.env.slot(crate::env::ESCAPE);
        let mut call = Regs::default();
        call.r[11] = if set { 0xFF } else { 0 };
        call.r[12] = slot.workspace;
        let target = Target {
            address: slot.address,
            bits: slot.bits,
        };
        self.call(target, &mut call, Entry::Supervisor)?;
        if call.r[12] == 1 {
            self.world.callbacks.request_old_style();
        }
        Ok(())
    }

    /// `UpCall` 6 on the poll word at `word`: whether a task switcher took
    /// it, to hand control back once the word is not nought.
    ///
    /// # Errors
    ///
    /// What the `UpCall` vector's claimants give.
    pub fn sleep_on(&mut self, word: u32) -> Result<bool, Fail> {
        let mut regs = Regs::default();
        regs.r[..2].copy_from_slice(&[crate::swi::os::upcall::SLEEP, word]);
        self.perform(crate::swi::X | crate::swi::os::UPCALL, &mut regs)?;
        Ok(regs.r[0] == 0)
    }

    /// The `UpCall` vector's default claimant: the `UpCall` handler, with the
    /// call's registers and its R12.
    pub(crate) fn default_upcall(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        if self.world.env.is_default(crate::env::UPCALL) {
            return Ok(());
        }
        let slot = self.world.env.slot(crate::env::UPCALL);
        let mut call = *regs;
        call.r[12] = slot.workspace;
        let target = Target {
            address: slot.address,
            bits: slot.bits,
        };
        self.call(target, &mut call, Entry::Supervisor)?;
        regs.r[..10].copy_from_slice(&call.r[..10]);
        regs.flags = call.flags;
        Ok(())
    }

    /// `OS_UpCall`: through `UpCallV` with the caller's interrupt state; the
    /// caller's N, Z and C come back, as no reason defines them.
    pub(crate) fn up_call(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let kept = regs.flags & (psr::N | psr::Z | psr::C);
        self.call_vector(vectors::UPCALL, regs, Entry::Supervisor)?;
        if regs.flag(psr::V) {
            return Err(Fail::Error(regs.r[0]));
        }
        regs.flags = kept;
        Ok(())
    }

    /// UKSWIV's default claimant: the unused SWI handler, R11 the SWI's
    /// number with bit 17 clear. It returns through R14 like a module's SWI
    /// handler, not to its caller's own PC as RISC OS 3's did: a SWI the
    /// kernel performs in host code cannot hand a nested caller back that way.
    pub(crate) fn default_unknown_swi(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let slot = self.world.env.slot(crate::env::UNUSED_SWI);
        let number = regs.r[11] & !crate::swi::X;
        if self.world.env.is_default(crate::env::UNUSED_SWI) {
            let name = crate::env::hex(number);
            return Err(self.error_with(crate::env::NO_SUCH_SWI, &[name.as_bytes()]));
        }
        let mut call = *regs;
        call.r[11] = number;
        call.r[12] = slot.workspace;
        let target = Target {
            address: slot.address,
            bits: slot.bits,
        };
        self.call(target, &mut call, Entry::Supervisor)?;
        regs.r[..10].copy_from_slice(&call.r[..10]);
        regs.flags = call.flags;
        if call.flag(psr::V) {
            return Err(Fail::Error(call.r[0]));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
