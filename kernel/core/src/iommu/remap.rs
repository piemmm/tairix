//! Interrupt remapping through the translating units (`plans/IOMMU.md`
//! IOM11).
//!
//! An MSI is a DMA write; without remapping, a device that can write memory
//! forges any vector to any CPU. With it, every interrupt a device raises
//! names an entry the kernel wrote, which delivers only the vector its owner
//! was given and only for the requester ids it admits.
//!
//! Remapping is the whole machine's or no one's: a unit that cannot remap
//! would pass its devices' forged messages whatever the others refuse, so a
//! machine with any unit that cannot remap, or that translates nothing at
//! all, keeps compatibility delivery and says so. The port drives it — it alone knows its APIC mode,
//! its vectors and its interrupt sources — over this mechanism.

use core::sync::atomic::Ordering;

use tairix_kernel_iommu_api::{
    InterruptRemapping, InterruptSource, InterruptTarget, IommuError, Remapped,
};

use super::Translation;

/// Why an interrupt could not be remapped.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RemapError {
    /// No unit translates, one that does cannot remap, or a discovered unit
    /// translates nothing.
    Unsupported,
    /// The interrupt's unit is not one translating here, or remapping was
    /// not prepared.
    UnknownUnit,
    /// The unit refused or could not confirm.
    Unit(IommuError),
    /// A unit could not turn remapping back off after another could not
    /// turn it on: it refuses its devices' compatibility interrupts.
    Stranded,
}

/// How the port routed the interrupt sources it set up at boot.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum InterruptRouting {
    /// Every source raises only the entry the kernel wrote for it.
    Remapped,
    /// No unit translates, or one cannot remap: sources deliver in the
    /// platform's compatibility format, and a device can forge any.
    Unremapped,
    /// Remapping could not be turned on, for this reason: sources deliver in
    /// compatibility format.
    Refused(RemapError),
    /// Remapping is on, but these many sources could be given no entry —
    /// the requester ids their messages carry are unknown, or their unit
    /// refused one — and were left unrouted rather than raise what they were
    /// not given.
    Unrouted(u32),
    /// The port has no sources a unit remaps.
    Native,
}

impl InterruptRouting {
    /// The audit trail's name for the outcome.
    #[must_use]
    pub const fn outcome(self) -> &'static str {
        match self {
            Self::Remapped => "remapped",
            Self::Unremapped => "unremapped",
            Self::Refused(RemapError::Stranded) => "stranded",
            Self::Refused(_) => "refused",
            Self::Unrouted(_) => "unrouted",
            Self::Native => "native",
        }
    }
}

/// One remapped interrupt: the unit it was made on, by index, and what its
/// source is programmed with. Not `Copy`: releasing it consumes it, so an
/// entry handed back to its unit cannot be released twice.
#[derive(Debug, Eq, PartialEq)]
pub struct RemapEntry {
    unit: usize,
    /// The entry and the messages that raise it.
    pub remapped: Remapped,
}

impl Translation {
    fn remapping_of(&self, index: usize) -> Option<&dyn InterruptRemapping> {
        self.units.get(index)?.unit.interrupt_remapping()
    }

    /// Whether every discovered unit translates and can remap.
    fn remaps_whole_machine(&self) -> bool {
        !self.units.is_empty()
            && !self.strands()
            && (0..self.units.len()).all(|index| self.remapping_of(index).is_some())
    }
}

/// The kernel's interrupt remapping, as the port drives it.
impl Translation {
    /// Whether every discovered unit translates and can remap, and to 32-bit
    /// x2APIC destinations: what [`Self::prepare`] needs to succeed in
    /// extended mode.
    #[must_use]
    pub fn extended(&self) -> bool {
        self.remaps_whole_machine()
            && (0..self.units.len()).all(|index| {
                self.remapping_of(index)
                    .is_some_and(InterruptRemapping::supports_extended)
            })
    }

    /// Build every translating unit's remapping table, `entries` entries
    /// each, for 32-bit x2APIC destinations where `extended` says so.
    ///
    /// # Errors
    ///
    /// [`RemapError::Unsupported`] unless every discovered unit translates
    /// and can remap, else the first unit's refusal.
    pub fn prepare(&self, extended: bool, entries: u32) -> Result<(), RemapError> {
        if !self.remaps_whole_machine() {
            return Err(RemapError::Unsupported);
        }
        for index in 0..self.units.len() {
            self.remapping_of(index)
                .ok_or(RemapError::Unsupported)?
                .prepare_remapping(extended, entries)
                .map_err(RemapError::Unit)?;
        }
        self.remap_prepared.store(true, Ordering::Release);
        Ok(())
    }

    /// An entry on the unit at hardware-tree node `unit` delivering
    /// `target`, raisable only by `source`.
    ///
    /// # Errors
    ///
    /// [`RemapError::UnknownUnit`] for a unit not translating here or not
    /// prepared, else the unit's refusal.
    pub fn remap(
        &self,
        unit: u32,
        source: InterruptSource,
        target: InterruptTarget,
    ) -> Result<RemapEntry, RemapError> {
        let index = self.unit_index(unit).ok_or(RemapError::UnknownUnit)?;
        if !self.remap_prepared.load(Ordering::Acquire) {
            return Err(RemapError::UnknownUnit);
        }
        let remapped = self
            .remapping_of(index)
            .ok_or(RemapError::UnknownUnit)?
            .remap_interrupt(source, target)
            .map_err(RemapError::Unit)?;
        Ok(RemapEntry {
            unit: index,
            remapped,
        })
    }

    /// Release `entry`. One the unit could not confirm gone is never reused,
    /// by the unit's own rule.
    // Taken by value so an entry is released once.
    #[allow(clippy::needless_pass_by_value)]
    pub fn release(&self, entry: RemapEntry) {
        if let Some(remapping) = self.remapping_of(entry.unit) {
            let _ = remapping.release_interrupt(entry.remapped.entry);
        }
    }

    /// Turn remapping on at every unit, once each source the port routes has
    /// its entry: from then on a compatibility interrupt is refused. The
    /// machine remaps whole or not at all, so a unit's refusal turns it back
    /// off at every unit it had reached.
    ///
    /// # Errors
    ///
    /// The first unit's refusal, with remapping off everywhere again, or
    /// [`RemapError::Stranded`] where it could not be turned back off.
    pub fn enable(&self) -> Result<(), RemapError> {
        if !self.remap_prepared.load(Ordering::Acquire) {
            return Err(RemapError::UnknownUnit);
        }
        for index in 0..self.units.len() {
            let refused = match self.remapping_of(index) {
                Some(remapping) => remapping.enable_remapping().err().map(RemapError::Unit),
                None => Some(RemapError::Unsupported),
            };
            if let Some(refused) = refused {
                // Including the unit that refused: a timed-out enable may
                // still land. Every unit is tried, whichever fails.
                let mut stranded = false;
                for undo in 0..=index {
                    stranded |= self
                        .remapping_of(undo)
                        .is_some_and(|remapping| remapping.disable_remapping().is_err());
                }
                return Err(if stranded {
                    RemapError::Stranded
                } else {
                    refused
                });
            }
        }
        Ok(())
    }
}
