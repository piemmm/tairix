//! The protections every CPU runs the kernel under.
//!
//! Supervisor writes honour read-only pages (`CR0.WP`) on every CPU. Where
//! the CPU offers them, the kernel also cannot execute a user page
//! (`CR4.SMEP`) or read or write one (`CR4.SMAP`), and user code cannot read
//! the descriptor-table and machine-status registers (`CR4.UMIP`).
//!
//! The kernel reaches user memory only through the direct map, so it never
//! sets `RFLAGS.AC` to open a user page to itself. What is left is the `AC`
//! user code may set, which every way into the kernel clears: `syscall`
//! through `IA32_FMASK`, each gate-entered stub with `clac` while SMAP is in
//! force, the boot entry by writing `RFLAGS` whole, and an application
//! processor by starting from INIT.
//!
//! The boot CPU settles the set from what it offers before any other CPU
//! starts, and a CPU that cannot hold all of it does not join the kernel, so
//! the stubs' `clac` is defined on every CPU that runs them.

use core::sync::atomic::{AtomicU8, Ordering};

/// `CPUID` leaf 7 sub-leaf 0 bits: `EBX` offers SMEP and SMAP, `ECX` UMIP.
const LEAF7_EBX_SMEP: u32 = 7;
const LEAF7_EBX_SMAP: u32 = 20;
const LEAF7_ECX_UMIP: u32 = 2;

/// A set of the protections the kernel may hold a CPU to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Protections(u8);

impl Protections {
    /// The kernel cannot execute a user page.
    pub const SMEP: Self = Self(1 << 0);
    /// The kernel cannot read or write a user page.
    pub const SMAP: Self = Self(1 << 1);
    /// User code cannot read the descriptor-table or machine-status
    /// registers.
    pub const UMIP: Self = Self(1 << 2);
    /// Every protection the port holds a CPU to where the CPU offers it.
    pub const ALL: Self = Self(Self::SMEP.0 | Self::SMAP.0 | Self::UMIP.0);

    /// What a CPU offers, from `CPUID` leaf 7 sub-leaf 0's `EBX` and `ECX`.
    #[must_use]
    pub const fn offered(ebx: u32, ecx: u32) -> Self {
        const fn when(register: u32, at: u32, protection: Protections) -> u8 {
            if register & (1 << at) == 0 {
                0
            } else {
                protection.0
            }
        }
        Self(
            when(ebx, LEAF7_EBX_SMEP, Self::SMEP)
                | when(ebx, LEAF7_EBX_SMAP, Self::SMAP)
                | when(ecx, LEAF7_ECX_UMIP, Self::UMIP),
        )
    }

    /// Whether every protection in `other` is in this set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// The bits of the set, as the entry stubs test them.
    #[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
    pub(crate) const fn bits(self) -> u8 {
        self.0
    }

    /// The `CR4` bits that hold a CPU to this set.
    #[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
    const fn cr4(self) -> u64 {
        const fn when(set: Protections, protection: Protections, cr4: u64) -> u64 {
            if set.contains(protection) {
                cr4
            } else {
                0
            }
        }
        when(self, Self::SMEP, 1 << 20)
            | when(self, Self::SMAP, 1 << 21)
            | when(self, Self::UMIP, 1 << 11)
    }
}

/// The protections in force, which the entry stubs read to decide whether to
/// clear `RFLAGS.AC`: settled once, by the boot CPU, before any other CPU
/// starts, and empty until then.
pub(crate) static IN_FORCE: AtomicU8 = AtomicU8::new(0);

/// The protections the kernel holds every CPU to.
#[must_use]
pub fn in_force() -> Protections {
    Protections(IN_FORCE.load(Ordering::Acquire))
}

/// What the calling CPU offers.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
fn offered_here() -> Protections {
    let leaf7 = crate::cpuid::structured_extended_features();
    Protections::offered(leaf7.ebx, leaf7.ecx)
}

/// Settle the protections the kernel holds every CPU to: what the boot CPU
/// offers.
///
/// Called once, by the boot CPU, before any other CPU starts and before the
/// boot CPU holds itself to them.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub(crate) fn settle() {
    IN_FORCE.store(offered_here().0, Ordering::Release);
}

/// The protections the calling CPU must hold to run the kernel, or [`None`]
/// on a CPU that does not offer them all.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub(crate) fn to_hold() -> Option<Protections> {
    let required = in_force();
    offered_here().contains(required).then_some(required)
}

/// Hold the calling CPU to `protections`, with supervisor write protection.
///
/// # Safety
///
/// CPL 0 with interrupts disabled and `RFLAGS.AC` clear, `protections` what
/// [`to_hold`] answered on this CPU, and no user page mapped in the active
/// root.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub(crate) unsafe fn hold(protections: Protections) {
    const CR0_WP: u64 = 1 << 16;
    // SAFETY: the CPU offers every bit set here; `CR0.WP` only makes a
    // supervisor write honour a read-only leaf, which nothing the kernel runs
    // writes through, and with `AC` clear and no user page mapped the `CR4`
    // bits fault nothing the kernel does.
    unsafe {
        core::arch::asm!(
            "mov {t}, cr0",
            "or {t}, {wp}",
            "mov cr0, {t}",
            "mov {t}, cr4",
            "or {t}, {cr4}",
            "mov cr4, {t}",
            t = out(reg) _,
            wp = const CR0_WP,
            cr4 = in(reg) protections.cr4(),
            options(nostack),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_protection_is_read_from_its_own_cpuid_bit() {
        assert_eq!(Protections::offered(1 << 7, 0), Protections::SMEP);
        assert_eq!(Protections::offered(1 << 20, 0), Protections::SMAP);
        assert_eq!(Protections::offered(0, 1 << 2), Protections::UMIP);
        assert_eq!(Protections::offered(u32::MAX, u32::MAX), Protections::ALL);
        assert_eq!(
            Protections::offered(!((1 << 7) | (1 << 20)), !(1 << 2)),
            Protections::offered(0, 0),
            "no other bit offers anything"
        );
        assert_eq!(
            Protections::offered(0, (1 << 7) | (1 << 20)),
            Protections::offered(0, 0),
            "SMEP and SMAP are EBX's, never ECX's"
        );
    }

    #[test]
    fn each_protection_sets_its_own_cr4_bit() {
        assert_eq!(Protections::SMEP.cr4(), 1 << 20);
        assert_eq!(Protections::SMAP.cr4(), 1 << 21);
        assert_eq!(Protections::UMIP.cr4(), 1 << 11);
        assert_eq!(Protections::ALL.cr4(), (1 << 20) | (1 << 21) | (1 << 11));
        assert_eq!(Protections::offered(0, 0).cr4(), 0);
    }

    /// A CPU joins only if it offers every protection in force; offering more
    /// is no bar, and it holds only the set in force.
    #[test]
    fn a_cpu_offering_less_than_the_set_in_force_cannot_hold_it() {
        let none = Protections::offered(0, 0);
        let smep_only = Protections::SMEP;
        assert!(Protections::ALL.contains(Protections::SMAP));
        assert!(Protections::ALL.contains(none), "an empty set asks nothing");
        assert!(!smep_only.contains(Protections::ALL));
        assert!(!smep_only.contains(Protections::SMAP));
        assert!(smep_only.contains(smep_only));
    }

    #[test]
    fn nothing_is_in_force_before_the_boot_cpu_settles_it() {
        assert_eq!(in_force(), Protections::offered(0, 0));
        assert!(
            !in_force().contains(Protections::SMAP),
            "no stub runs `clac`"
        );
    }

    #[test]
    fn the_stubs_test_the_smap_bit_of_the_byte_in_force() {
        assert_eq!(Protections::SMAP.bits(), 1 << 1);
        assert_eq!(
            Protections::ALL.bits() & Protections::SMAP.bits(),
            Protections::SMAP.bits()
        );
    }
}
