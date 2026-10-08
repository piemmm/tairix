//! A supervisor-level APLIC domain in MSI delivery mode: each wired source
//! the machine-level domain delegated to it is sent as one identity to a
//! hart's IMSIC file (RISC-V Advanced Interrupt Architecture, chapter 4).

use crate::fdt::MAX_APLIC_SOURCES;

/// A domain's register offsets.
pub mod regs {
    /// Interrupt enable, delivery mode, endianness.
    pub const DOMAINCFG: usize = 0x0000;
    /// Pend a source by number.
    pub const SETIPNUM_LE: usize = 0x2000;
    /// Enable a source by number.
    pub const SETIENUM: usize = 0x1EDC;
    /// Disable a source by number.
    pub const CLRIENUM: usize = 0x1FDC;

    /// The configuration of `source`.
    #[must_use]
    pub const fn sourcecfg(source: u32) -> usize {
        4 * source as usize
    }

    /// Where `source`'s message goes.
    #[must_use]
    pub const fn target(source: u32) -> usize {
        0x3000 + 4 * source as usize
    }
}

const DOMAINCFG_IE: u32 = 1 << 8;
const DOMAINCFG_DM: u32 = 1 << 2;
const DOMAINCFG_BE: u32 = 1 << 0;
const TARGET_HART_SHIFT: u32 = 18;
const HART_INDEX_LIMIT: u32 = 1 << 14;
const IDENTITY_LIMIT: u32 = 1 << 11;

/// How a source signals: active-high and rising-edge, the senses a TAIRiX
/// grant can name.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Sense {
    /// A rising edge.
    Edge,
    /// A high level.
    Level,
}

impl Sense {
    /// The `sourcecfg` source mode: `Edge1` or `Level1`.
    const fn mode(self) -> u32 {
        match self {
            Self::Edge => 4,
            Self::Level => 6,
        }
    }
}

/// A domain's 32-bit registers.
pub trait AplicMmio {
    /// Read the register at `offset`.
    fn read32(&self, offset: usize) -> u32;
    /// Write `value` to the register at `offset`.
    fn write32(&self, offset: usize, value: u32);
}

/// Why a domain or source was refused.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AplicError {
    /// The domain does not deliver by MSI.
    NotMsi,
    /// A source outside `1..=sources`, or a domain of none or too many.
    SourceOutOfRange,
    /// The machine-level domain did not delegate the source here.
    NotDelegated,
    /// A hart index or identity a target cannot name.
    BadTarget,
}

/// A domain the kernel has taken over.
pub struct Aplic<M> {
    mmio: M,
    sources: u32,
}

impl<M: AplicMmio> Aplic<M> {
    /// Take the domain over in little-endian MSI delivery mode, every source
    /// inactive and disabled.
    ///
    /// # Errors
    ///
    /// [`AplicError::SourceOutOfRange`] for a source count outside
    /// `1..=1023`, and [`AplicError::NotMsi`] for a domain that will not
    /// deliver by MSI.
    pub fn take_msi(mmio: M, sources: u32) -> Result<Self, AplicError> {
        if !(1..=MAX_APLIC_SOURCES).contains(&sources) {
            return Err(AplicError::SourceOutOfRange);
        }
        mmio.write32(regs::DOMAINCFG, DOMAINCFG_DM);
        let config = mmio.read32(regs::DOMAINCFG) & (DOMAINCFG_IE | DOMAINCFG_DM | DOMAINCFG_BE);
        if config != DOMAINCFG_DM {
            return Err(AplicError::NotMsi);
        }
        for source in 1..=sources {
            mmio.write32(regs::CLRIENUM, source);
            mmio.write32(regs::sourcecfg(source), 0);
        }
        mmio.write32(regs::DOMAINCFG, DOMAINCFG_DM | DOMAINCFG_IE);
        Ok(Self { mmio, sources })
    }

    /// The domain's sources, `1..=sources`.
    #[must_use]
    pub const fn sources(&self) -> u32 {
        self.sources
    }

    /// Make `source`, disabled, signal by `sense` as identity `identity` of
    /// the file of the hart at `hart_index`.
    ///
    /// # Errors
    ///
    /// [`AplicError::SourceOutOfRange`], [`AplicError::BadTarget`] for a hart
    /// index or identity the target register cannot hold, and
    /// [`AplicError::NotDelegated`] for a source the domain was not given.
    pub fn route(
        &self,
        source: u32,
        sense: Sense,
        hart_index: u32,
        identity: u32,
    ) -> Result<(), AplicError> {
        self.check(source)?;
        if hart_index >= HART_INDEX_LIMIT || identity == 0 || identity >= IDENTITY_LIMIT {
            return Err(AplicError::BadTarget);
        }
        self.mmio.write32(regs::CLRIENUM, source);
        self.mmio.write32(regs::sourcecfg(source), sense.mode());
        if self.mmio.read32(regs::sourcecfg(source)) != sense.mode() {
            return Err(AplicError::NotDelegated);
        }
        self.mmio.write32(
            regs::target(source),
            hart_index << TARGET_HART_SHIFT | identity,
        );
        Ok(())
    }

    /// Let `source` send its message.
    ///
    /// # Errors
    ///
    /// [`AplicError::SourceOutOfRange`].
    pub fn enable(&self, source: u32) -> Result<(), AplicError> {
        self.check(source)?;
        self.mmio.write32(regs::SETIENUM, source);
        Ok(())
    }

    /// Stop `source` sending.
    ///
    /// # Errors
    ///
    /// [`AplicError::SourceOutOfRange`].
    pub fn disable(&self, source: u32) -> Result<(), AplicError> {
        self.check(source)?;
        self.mmio.write32(regs::CLRIENUM, source);
        Ok(())
    }

    /// Pend `source` again, which takes only while its input is asserted: a
    /// level source sends one message per assertion, so one still asserted
    /// once its device was serviced would otherwise never be sent again
    /// (AIA specification 4.9.2).
    ///
    /// # Errors
    ///
    /// [`AplicError::SourceOutOfRange`].
    pub fn retrigger(&self, source: u32) -> Result<(), AplicError> {
        self.check(source)?;
        self.mmio.write32(regs::SETIPNUM_LE, source);
        Ok(())
    }

    const fn check(&self, source: u32) -> Result<(), AplicError> {
        if source == 0 || source > self.sources {
            return Err(AplicError::SourceOutOfRange);
        }
        Ok(())
    }
}

/// Bare-metal [`AplicMmio`] over a domain's register window.
#[cfg(all(target_arch = "riscv64", target_os = "none"))]
pub struct VolatileAplicMmio {
    base: usize,
}

#[cfg(all(target_arch = "riscv64", target_os = "none"))]
impl VolatileAplicMmio {
    /// An accessor over the domain at physical (identity-mapped) `base`.
    ///
    /// # Safety
    ///
    /// `base` must be the domain's register base read from the device tree,
    /// mapped readable and writable for the life of the kernel, and aliased
    /// by no other accessor.
    #[must_use]
    pub const unsafe fn new(base: usize) -> Self {
        Self { base }
    }
}

#[cfg(all(target_arch = "riscv64", target_os = "none"))]
impl AplicMmio for VolatileAplicMmio {
    fn read32(&self, offset: usize) -> u32 {
        // SAFETY: every offset `regs` names is a 4-byte-aligned register
        // inside the window the constructor's caller vouched for.
        unsafe { core::ptr::read_volatile((self.base + offset) as *const u32) }
    }

    fn write32(&self, offset: usize, value: u32) {
        // SAFETY: as `read32`.
        unsafe { core::ptr::write_volatile((self.base + offset) as *mut u32, value) }
    }
}

#[cfg(test)]
#[path = "aplic_tests.rs"]
mod tests;
