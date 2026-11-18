//! The session's processors: one per profile, made when first needed, the
//! register file and the FPA handed across at each switch, as one register
//! file serves both on a processor with both programmer's models.

use tairix_riscos_arm::cpu::{BadState, Cpu, State};
use tairix_riscos_arm::profile::{Profile, BIT26, BIT32};

use crate::memory::Bits;

const fn profile(bits: Bits) -> &'static Profile {
    match bits {
        Bits::Bit26 => &BIT26,
        Bits::Bit32 => &BIT32,
    }
}

const fn index(bits: Bits) -> usize {
    match bits {
        Bits::Bit26 => 0,
        Bits::Bit32 => 1,
    }
}

/// The processor code runs on, by the profile it was loaded under.
pub(crate) struct Processor {
    cpus: [Option<Cpu>; 2],
    current: Bits,
    /// Decoded pages each processor keeps.
    memo_pages: usize,
    irq: bool,
}

impl Processor {
    /// A processor of `bits` at reset, keeping `memo_pages` decoded pages.
    pub(crate) fn new(bits: Bits, memo_pages: usize) -> Self {
        let mut cpus = [None, None];
        cpus[index(bits)] = Some(Cpu::new(profile(bits), memo_pages));
        Self {
            cpus,
            current: bits,
            memo_pages,
            irq: false,
        }
    }

    /// The profile current.
    pub(crate) const fn bits(&self) -> Bits {
        self.current
    }

    /// The current processor.
    pub(crate) fn cpu(&mut self) -> &mut Cpu {
        let (bits, memo_pages) = (self.current, self.memo_pages);
        self.cpus[index(bits)].get_or_insert_with(|| Cpu::new(profile(bits), memo_pages))
    }

    /// The current processor's registers.
    pub(crate) fn state(&mut self) -> State {
        self.cpu().state()
    }

    /// Makes the processor of `bits` current with `state`; refused, and
    /// nothing changed, if `state` names a mode that profile lacks.
    pub(crate) fn load(&mut self, bits: Bits, state: &State) -> Result<(), BadState> {
        if bits == self.current {
            return self.cpu().set_state(state);
        }
        let fpa = *self.cpu().fpa();
        let memo_pages = self.memo_pages;
        let cpu = self.cpus[index(bits)].get_or_insert_with(|| Cpu::new(profile(bits), memo_pages));
        cpu.set_state(state)?;
        *cpu.fpa_mut() = fpa;
        cpu.clear_exclusive();
        cpu.set_irq(self.irq);
        if let Some(previous) = self.cpus[index(self.current)].as_mut() {
            previous.set_irq(false);
        }
        self.current = bits;
        Ok(())
    }

    /// Whether the kernel's interrupt line is asserted.
    pub(crate) const fn irq(&self) -> bool {
        self.irq
    }

    /// Asserts or releases the interrupt line the kernel's timed work drives.
    pub(crate) fn set_irq(&mut self, asserted: bool) {
        self.irq = asserted;
        self.cpu().set_irq(asserted);
    }
}
