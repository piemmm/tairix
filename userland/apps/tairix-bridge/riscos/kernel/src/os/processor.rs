//! The session's processors: one per profile, made when first needed, the
//! register file and the FPA handed across at each switch, as one register
//! file serves both on a processor with both programmer's models.

use tairix_riscos_arm::cpu::{BadState, Context, Cpu, State};
use tairix_riscos_arm::profile::{Profile, BIT26, BIT32};

use crate::memory::Bits;

/// A context's processors while another context's run: the profile it was
/// running under, and each processor's context. A processor it never had
/// is at reset when it next runs.
pub(crate) struct Parked {
    current: Bits,
    cpus: [Option<Context>; 2],
}

impl Parked {
    /// A new context's: every processor at reset, `bits` current.
    pub(crate) const fn fresh(bits: Bits) -> Self {
        Self {
            current: bits,
            cpus: [None, None],
        }
    }
}

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
        self.of(self.current)
    }

    /// The 32-bit profile's processor, made if it has not yet run: the only
    /// one with VFP and Advanced SIMD, whose unit VFPSupport keeps whichever
    /// profile is running.
    pub(crate) fn with_vfp(&mut self) -> &mut Cpu {
        self.of(Bits::Bit32)
    }

    /// The processor of `bits`, made if it has not yet run.
    fn of(&mut self, bits: Bits) -> &mut Cpu {
        let memo_pages = self.memo_pages;
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
        let (fpa, fpa_active) = (*self.cpu().fpa(), self.cpu().fpa_active());
        let irq = self.irq;
        let cpu = self.of(bits);
        cpu.set_state(state)?;
        *cpu.fpa_mut() = fpa;
        cpu.set_fpa_active(fpa_active);
        cpu.clear_exclusive();
        cpu.set_irq(irq);
        if let Some(previous) = self.cpus[index(self.current)].as_mut() {
            previous.set_irq(false);
        }
        self.current = bits;
        Ok(())
    }

    /// The processors' contexts, set aside for another context's.
    pub(crate) fn park(&self) -> Parked {
        Parked {
            current: self.current,
            cpus: [&self.cpus[0], &self.cpus[1]].map(|cpu| cpu.as_ref().map(Cpu::context)),
        }
    }

    /// `parked` made the processors' contexts, the interrupt line moved to
    /// its current processor as it stands. A parked context holds only what
    /// these processors gave, so neither is refused but by a kernel fault,
    /// which leaves them part changed.
    pub(crate) fn resume(&mut self, parked: &Parked) -> Result<(), BadState> {
        let irq = self.irq;
        for bits in [Bits::Bit26, Bits::Bit32] {
            let kept = parked.cpus[index(bits)];
            if kept.is_none() && self.cpus[index(bits)].is_none() && bits != parked.current {
                continue;
            }
            let cpu = self.of(bits);
            let context = match kept {
                Some(context) => context,
                None => cpu.reset_context(),
            };
            cpu.resume(&context)?;
            cpu.set_irq(irq && bits == parked.current);
        }
        self.current = parked.current;
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
