//! The GIC topology an aarch64 test kernel hands `gic::init`: its CPUs'
//! slots, and a GICv3's redistributor regions read from the board's tree, or
//! the boot CPU's alone.
//!
//! The production boot collects the regions into the heap; a test kernel has
//! none, so they are kept here for as long as the kernel runs.
//!
//! Test scaffolding: nothing in TAIRiX itself links it.

#![no_std]
#![deny(missing_docs)]

#[cfg(itest_aarch64)]
mod kernel {
    use tairix_arch_aarch64::gic::{self, GicCpu, GicTopology};
    use tairix_arch_aarch64::gicv3::RedistributorRegion;
    use tairix_fdt::Fdt;
    use tairix_sync::once::OnceCell;

    /// The most redistributor regions a QEMU `virt` tree describes: a second
    /// past the first region's 123 redistributors.
    const MOST_REGIONS: usize = 2;

    /// The boot CPU's slot, for a test kernel that runs on it alone.
    static BOOT_CPU: [GicCpu; 1] = [GicCpu::new()];

    /// Bring the GIC up for a test kernel that runs on its boot CPU alone.
    ///
    /// # Safety
    ///
    /// As [`gic::init`]: once, on the boot CPU, its stack and MMU up.
    ///
    /// # Errors
    ///
    /// As [`gic::init`].
    pub unsafe fn init_boot_cpu() -> Result<(), gic::GicError> {
        // SAFETY: the caller keeps `gic::init`'s contract.
        unsafe { gic::init(GicTopology::new(&BOOT_CPU)) }
    }

    struct Regions {
        regions: [RedistributorRegion; MOST_REGIONS],
        count: usize,
    }

    static REGIONS: OnceCell<Regions> = OnceCell::new();

    /// `cpus`, and the redistributor regions a GICv3 in `fdt` describes;
    /// [`None`] for a tree naming more than a `virt` board does, or a second
    /// call.
    #[must_use]
    pub fn topology(fdt: &Fdt<'_>, cpus: &'static [GicCpu]) -> Option<GicTopology> {
        let mut found = Regions {
            regions: [RedistributorRegion { base: 0, len: 0 }; MOST_REGIONS],
            count: 0,
        };
        let mut overflow = false;
        let stride =
            gic::redistributor_regions(fdt, |region| match found.regions.get_mut(found.count) {
                Some(slot) => {
                    *slot = region;
                    found.count += 1;
                }
                None => overflow = true,
            });
        if overflow {
            return None;
        }
        REGIONS.set(found).ok()?;
        let kept = REGIONS.get().ok().flatten()?;
        Some(
            GicTopology::new(cpus)
                .with_redistributors(&kept.regions[..kept.count], stride.flatten()),
        )
    }

    /// [`topology`] for a test kernel that runs on its boot CPU alone.
    #[must_use]
    pub fn boot_cpu_topology(fdt: &Fdt<'_>) -> Option<GicTopology> {
        topology(fdt, &BOOT_CPU)
    }
}

#[cfg(itest_aarch64)]
pub use kernel::{boot_cpu_topology, init_boot_cpu, topology};
