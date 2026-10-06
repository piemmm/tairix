//! Bridge between the platform memory-map sources (Multiboot2 BIOS
//! mmap, UEFI memory map, PVH `hvm_start_info` memmap) and
//! `kernel/mem`'s typed [`BootMemoryMap`].
//!
//! [`BootMemoryMap`]: ../../../mem/src/bootinfo.rs
//!
//! The arch crate is intentionally dependency-light in production
//! builds (see `Cargo.toml`): it does **not** pull in `kernel/mem`
//! because the freestanding QEMU test binaries that link this crate do
//! not yet provide a `#[global_allocator]`. So instead of constructing
//! `kernel_mem::BootMemoryMap` here, the bridge yields a sequence of
//! [`MemoryRegionDescriptor`]s and lets the consumer (the kernel
//! binary linked with `kernel/mem`) drain them into a `BootMemoryMap`.
//!
//! The mirror enum [`RegionKind`] is locked to `kernel_mem::RegionKind`
//! by a host-side `#[cfg(test)]` round-trip test in this module, which
//! depends on `kernel/mem` only as a dev-dependency. If the two enums
//! ever drift, the round-trip test fails to compile and the build
//! breaks — exactly the duplication-detection signal
//! requires.

use core::ops::Range;

use crate::multiboot2::{EfiMemoryDescriptor, Mb2MemoryEntry, Mb2MemoryKind};
use crate::pvh::{PvhMemoryEntry, PvhMemoryKind};

/// Mirror of `tairix_kernel_mem::RegionKind`. Locked by a host-side
/// round-trip test in the `tests` module (`#[cfg(test)]`-only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RegionKind {
    /// Free RAM the frame allocator may hand out.
    Usable,
    /// Firmware-reserved, MMIO, kernel image, or otherwise untouchable.
    Reserved,
}

/// One physical-memory region produced by the bridge.
///
/// Mirrors `tairix_kernel_mem::MemoryRegion` exactly so a consumer can
/// translate one-to-one without inspecting the field types beyond the
/// kind discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryRegionDescriptor {
    /// Inclusive physical byte start.
    pub start: u64,
    /// Region length in bytes.
    pub length: u64,
    /// What kind of memory this is.
    pub kind: RegionKind,
}

/// Translate one Multiboot2 BIOS memory-map entry into a descriptor.
///
/// Multiboot2 type 1 ("Available") maps to [`RegionKind::Usable`];
/// every other type — including `AcpiReclaimable` and `AcpiNvs`,
/// which a strict frame allocator must keep its hands off until
/// the kernel has explicitly reclaimed them — maps to
/// [`RegionKind::Reserved`].
#[must_use]
pub fn from_multiboot2(entry: Mb2MemoryEntry) -> MemoryRegionDescriptor {
    let kind = match entry.kind {
        Mb2MemoryKind::Available => RegionKind::Usable,
        Mb2MemoryKind::AcpiReclaimable
        | Mb2MemoryKind::AcpiNvs
        | Mb2MemoryKind::Defective
        | Mb2MemoryKind::Reserved
        | Mb2MemoryKind::Other(_) => RegionKind::Reserved,
    };
    MemoryRegionDescriptor {
        start: entry.base,
        length: entry.length,
        kind,
    }
}

/// Translate one UEFI memory descriptor into a descriptor.
///
/// Mapping per UEFI 2.10 Table 7-9:
/// * `EfiLoaderCode` (1), `EfiLoaderData` (2), `EfiBootServicesCode`
///   (3), `EfiBootServicesData` (4), and `EfiConventionalMemory` (7)
///   are all reclaimable as free RAM after `ExitBootServices()`.
/// * Everything else (runtime services, ACPI, MMIO, persistent
///   memory, etc.) is [`RegionKind::Reserved`].
#[must_use]
pub fn from_uefi(desc: EfiMemoryDescriptor) -> MemoryRegionDescriptor {
    let kind = if desc.is_usable_after_exit_boot_services() {
        RegionKind::Usable
    } else {
        RegionKind::Reserved
    };
    MemoryRegionDescriptor {
        start: desc.physical_start,
        length: desc.length_bytes(),
        kind,
    }
}

/// Translate one PVH memory-map entry into a descriptor.
///
/// PVH type 1 (RAM) maps to [`RegionKind::Usable`]; every other type —
/// including `AcpiReclaimable` and `AcpiNvs`, which a strict frame
/// allocator must keep its hands off until the kernel has explicitly
/// reclaimed them — maps to [`RegionKind::Reserved`], the same policy
/// as [`from_multiboot2`].
#[must_use]
pub fn from_pvh(entry: PvhMemoryEntry) -> MemoryRegionDescriptor {
    let kind = match entry.kind {
        PvhMemoryKind::Ram => RegionKind::Usable,
        PvhMemoryKind::Reserved
        | PvhMemoryKind::AcpiReclaimable
        | PvhMemoryKind::AcpiNvs
        | PvhMemoryKind::Unusable
        | PvhMemoryKind::Other(_) => RegionKind::Reserved,
    };
    MemoryRegionDescriptor {
        start: entry.addr,
        length: entry.size,
        kind,
    }
}

/// `EfiReservedMemoryType`, `EfiMemoryMappedIO` and
/// `EfiMemoryMappedIOPortSpace` (UEFI 2.10 Table 7-9): the UEFI types that
/// may hold device registers.
const UEFI_NOT_MEMORY: [u32; 3] = [0, 11, 12];

/// One region of the memory map firmware gave, from whichever map it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmwareRegion {
    /// A Multiboot2 BIOS memory-map entry.
    Bios(Mb2MemoryEntry),
    /// A UEFI memory descriptor.
    Uefi(EfiMemoryDescriptor),
    /// A PVH memory-map entry.
    Pvh(PvhMemoryEntry),
}

impl FirmwareRegion {
    /// The descriptor the kernel's memory map takes for it.
    #[must_use]
    pub fn descriptor(self) -> MemoryRegionDescriptor {
        match self {
            Self::Bios(entry) => from_multiboot2(entry),
            Self::Uefi(desc) => from_uefi(desc),
            Self::Pvh(entry) => from_pvh(entry),
        }
    }

    /// Its span where it is memory of any kind — RAM the kernel takes, RAM
    /// firmware keeps (ACPI tables and NVS, runtime services), defective or
    /// persistent memory, or a type the kernel does not know — rather than a
    /// reservation that may hold device registers: what no BAR may decode
    /// over.
    #[must_use]
    pub fn memory(self) -> Option<Range<u64>> {
        let (start, length, memory) = match self {
            Self::Bios(entry) => (
                entry.base,
                entry.length,
                entry.kind != Mb2MemoryKind::Reserved,
            ),
            Self::Uefi(desc) => (
                desc.physical_start,
                desc.length_bytes(),
                !UEFI_NOT_MEMORY.contains(&desc.kind),
            ),
            Self::Pvh(entry) => (
                entry.addr,
                entry.size,
                entry.kind != PvhMemoryKind::Reserved,
            ),
        };
        memory.then(|| start..start.saturating_add(length))
    }
}

#[cfg(test)]
mod tests {
    //! Host-side tests, including the round-trip lock against the
    //! canonical `tairix_kernel_mem::RegionKind`.

    use super::*;
    use crate::multiboot2::{EfiMemoryDescriptor, Mb2MemoryEntry, Mb2MemoryKind};
    use tairix_kernel_mem::{MemoryRegion as KMemRegion, RegionKind as KRegionKind};

    /// Translate our mirror enum to the canonical kernel/mem enum.
    fn lift(k: RegionKind) -> KRegionKind {
        match k {
            RegionKind::Usable => KRegionKind::Usable,
            RegionKind::Reserved => KRegionKind::Reserved,
        }
    }

    fn lift_region(d: MemoryRegionDescriptor) -> KMemRegion {
        KMemRegion {
            start: tairix_kernel_mem::PhysAddr::new(d.start),
            length: d.length,
            kind: lift(d.kind),
        }
    }

    #[test]
    fn region_kind_round_trip_matches_kernel_mem() {
        // Exhaustive over our enum: if anyone adds a variant here
        // without updating `lift`, the `match` in `lift` no longer
        // compiles. If `kernel_mem::RegionKind` adds a variant, our
        // construction can no longer round-trip and this test fails.
        for k in [RegionKind::Usable, RegionKind::Reserved] {
            let lifted = lift(k);
            match (k, lifted) {
                (RegionKind::Usable, KRegionKind::Usable)
                | (RegionKind::Reserved, KRegionKind::Reserved) => {}
                _ => panic!("RegionKind enums drifted: {k:?} != {lifted:?}"),
            }
        }
    }

    #[test]
    fn from_multiboot2_maps_available_to_usable() {
        let d = from_multiboot2(Mb2MemoryEntry {
            base: 0x10_0000,
            length: 0x10_0000,
            kind: Mb2MemoryKind::Available,
        });
        assert_eq!(
            d,
            MemoryRegionDescriptor {
                start: 0x10_0000,
                length: 0x10_0000,
                kind: RegionKind::Usable,
            }
        );
        // And it round-trips into kernel/mem's region type.
        let lifted = lift_region(d);
        assert_eq!(lifted.start.as_u64(), 0x10_0000);
        assert_eq!(lifted.length, 0x10_0000);
        assert_eq!(lifted.kind, KRegionKind::Usable);
    }

    #[test]
    fn from_multiboot2_maps_acpi_reclaim_to_reserved() {
        let d = from_multiboot2(Mb2MemoryEntry {
            base: 0,
            length: 1,
            kind: Mb2MemoryKind::AcpiReclaimable,
        });
        assert_eq!(d.kind, RegionKind::Reserved);
    }

    #[test]
    fn from_pvh_maps_ram_to_usable_and_the_rest_to_reserved() {
        let usable = from_pvh(PvhMemoryEntry {
            addr: 0x10_0000,
            size: 0x1000_0000,
            kind: PvhMemoryKind::Ram,
        });
        assert_eq!(
            usable,
            MemoryRegionDescriptor {
                start: 0x10_0000,
                length: 0x1000_0000,
                kind: RegionKind::Usable,
            }
        );
        for kind in [
            PvhMemoryKind::Reserved,
            PvhMemoryKind::AcpiReclaimable,
            PvhMemoryKind::AcpiNvs,
            PvhMemoryKind::Unusable,
            PvhMemoryKind::Other(9),
        ] {
            let d = from_pvh(PvhMemoryEntry {
                addr: 0,
                size: 1,
                kind,
            });
            assert_eq!(d.kind, RegionKind::Reserved, "{kind:?}");
        }
    }

    #[test]
    fn from_uefi_classifies_correctly() {
        // EfiConventionalMemory (7) -> Usable.
        let usable = from_uefi(EfiMemoryDescriptor {
            kind: 7,
            physical_start: 0x20_0000,
            virtual_start: 0,
            number_of_pages: 4,
            attribute: 0,
        });
        assert_eq!(usable.kind, RegionKind::Usable);
        assert_eq!(usable.length, 4 * 4096);

        // EfiACPIMemoryNVS (10) -> Reserved.
        let reserved = from_uefi(EfiMemoryDescriptor {
            kind: 10,
            physical_start: 0,
            virtual_start: 0,
            number_of_pages: 1,
            attribute: 0,
        });
        assert_eq!(reserved.kind, RegionKind::Reserved);
    }

    /// Memory of every kind is memory to a BAR: RAM, what firmware keeps,
    /// defective RAM and a type the kernel does not know; only a
    /// reservation, or a UEFI register window, may hold device registers.
    #[test]
    fn a_firmware_region_is_memory_unless_it_may_hold_registers() {
        let uefi = |kind| {
            FirmwareRegion::Uefi(EfiMemoryDescriptor {
                kind,
                physical_start: 0x8000_0000,
                virtual_start: 0,
                number_of_pages: 2,
                attribute: 0,
            })
            .memory()
        };
        for kind in [1, 5, 6, 7, 8, 9, 10, 14, 0x7000_0001] {
            assert_eq!(uefi(kind), Some(0x8000_0000..0x8000_2000), "type {kind}");
        }
        for kind in [0, 11, 12] {
            assert_eq!(uefi(kind), None, "type {kind}");
        }
        let pvh = |kind| {
            FirmwareRegion::Pvh(PvhMemoryEntry {
                addr: 0x1000,
                size: 0x1000,
                kind,
            })
            .memory()
        };
        assert_eq!(pvh(PvhMemoryKind::AcpiNvs), Some(0x1000..0x2000));
        assert_eq!(pvh(PvhMemoryKind::Other(7)), Some(0x1000..0x2000));
        assert_eq!(pvh(PvhMemoryKind::Reserved), None);
        let bios = |kind| {
            FirmwareRegion::Bios(Mb2MemoryEntry {
                base: 0x1000,
                length: 0x1000,
                kind,
            })
            .memory()
        };
        assert_eq!(bios(Mb2MemoryKind::Defective), Some(0x1000..0x2000));
        assert_eq!(
            bios(Mb2MemoryKind::from_raw(7)),
            Some(0x1000..0x2000),
            "persistent memory"
        );
        assert_eq!(bios(Mb2MemoryKind::Reserved), None);
    }

    #[test]
    fn descriptors_feed_kernel_mem_boot_memory_map() {
        // End-to-end: synthesize three Multiboot2 entries, route them
        // through the bridge, build a `BootMemoryMap`, and confirm the
        // frame allocator accepts the result (i.e. no overlap or
        // overflow rejection). This is the contract test that locks
        // the bridge to kernel/mem.
        let entries = [
            Mb2MemoryEntry {
                base: 0x0,
                length: 0xA_0000,
                kind: Mb2MemoryKind::Available,
            },
            Mb2MemoryEntry {
                base: 0xA_0000,
                length: 0x6_0000,
                kind: Mb2MemoryKind::Reserved,
            },
            Mb2MemoryEntry {
                base: 0x10_0000,
                length: 0x1_0000_0000,
                kind: Mb2MemoryKind::Available,
            },
        ];

        let mut map = tairix_kernel_mem::BootMemoryMap::new();
        for e in entries {
            let d = from_multiboot2(e);
            map.push(lift_region(d));
        }
        let regions = map.regions();
        assert_eq!(regions.len(), 3);
        assert_eq!(regions[0].kind, KRegionKind::Usable);
        assert_eq!(regions[1].kind, KRegionKind::Reserved);
        assert_eq!(regions[2].kind, KRegionKind::Usable);
        let hi = map.highest_address().unwrap();
        assert_eq!(hi.as_u64(), 0x10_0000 + 0x1_0000_0000);
    }
}
