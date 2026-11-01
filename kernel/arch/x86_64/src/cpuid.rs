//! The `CPUID` reads more than one part of the port shares: which leaves a CPU
//! reports, who made it, its structured extended features, and lifting the
//! limit firmware can put on its basic leaves.

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
use core::arch::x86_64::{__cpuid, __cpuid_count, CpuidResult};

use tairix_sync::once::OnceCell;

/// The bit that puts a leaf in the extended range, whose highest leaf
/// `0x8000_0000` reports as leaf 0 does the basic range's.
#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
const EXTENDED_RANGE: u32 = 0x8000_0000;

/// Whether a CPU whose highest leaf in `leaf`'s range is `highest` reports
/// `leaf`. A `highest` from the other range is no report of this one.
#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
const fn reports(leaf: u32, highest: u32) -> bool {
    highest & EXTENDED_RANGE == leaf & EXTENDED_RANGE && leaf <= highest
}

/// `CPUID` leaf `leaf` sub-leaf `sub_leaf`, or `None` past the highest leaf
/// the CPU reports in its range: a leaf beyond it answers the highest basic
/// leaf's data instead (Intel SDM Vol. 2A, "CPUID"), which is no answer.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub(crate) fn leaf(leaf: u32, sub_leaf: u32) -> Option<CpuidResult> {
    reports(leaf, __cpuid(leaf & EXTENDED_RANGE).eax).then(|| __cpuid_count(leaf, sub_leaf))
}

/// `CPUID` leaf 7 sub-leaf 0, the structured extended feature flags, or no
/// flags at all on a CPU that does not report the leaf.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub(crate) fn structured_extended_features() -> CpuidResult {
    leaf(7, 0).unwrap_or(CpuidResult {
        eax: 0,
        ebx: 0,
        ecx: 0,
        edx: 0,
    })
}

/// Who made a CPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vendor {
    /// `GenuineIntel`.
    Intel,
    /// `AuthenticAMD`.
    Amd,
}

/// A vendor's name as `CPUID` leaf 0 spells it across `EBX`, `EDX` and
/// `ECX`, four bytes to each, little-endian.
const fn leaf0_words(name: &[u8; 12]) -> [u32; 3] {
    const fn word(name: &[u8; 12], at: usize) -> u32 {
        u32::from_le_bytes([name[at], name[at + 1], name[at + 2], name[at + 3]])
    }
    [word(name, 0), word(name, 4), word(name, 8)]
}

/// `GenuineIntel` in leaf 0's registers, which the boot entries also test
/// before touching an Intel-only register.
pub(crate) const INTEL_LEAF0: [u32; 3] = leaf0_words(b"GenuineIntel");
/// `AuthenticAMD` in leaf 0's registers.
const AMD_LEAF0: [u32; 3] = leaf0_words(b"AuthenticAMD");

impl Vendor {
    /// The vendor `CPUID` leaf 0 spells across `EBX`, `EDX` and `ECX`, or
    /// `None` for one outside this set: an honest unknown, never a guess.
    #[must_use]
    pub const fn from_leaf0(ebx: u32, edx: u32, ecx: u32) -> Option<Self> {
        match [ebx, edx, ecx] {
            INTEL_LEAF0 => Some(Self::Intel),
            AMD_LEAF0 => Some(Self::Amd),
            _ => None,
        }
    }

    /// The name the boot facts give the vendor.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Intel => "Intel",
            Self::Amd => "AMD",
        }
    }
}

/// Who made the calling CPU.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub(crate) fn vendor() -> Option<Vendor> {
    let leaf0 = __cpuid(0);
    Vendor::from_leaf0(leaf0.ebx, leaf0.edx, leaf0.ecx)
}

/// `IA32_MISC_ENABLE` (Intel SDM Vol. 4, Table 2-2).
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub(crate) const IA32_MISC_ENABLE: u32 = 0x1A0;

/// `IA32_MISC_ENABLE.XD Bit Disable`: while set, the CPU withholds
/// execute-disable and reports no NX. The boot entries clear it.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub(crate) const MISC_ENABLE_XD_DISABLE_BIT: u32 = 34;

/// `CPUID.80000001H:EDX.NX`: the CPU offers execute-disable.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub(crate) const EXT1_EDX_NX_BIT: u32 = 20;

/// `IA32_MISC_ENABLE.Limit CPUID Maxval`: while set, leaf 0 reports no basic
/// leaf above the limit, hiding every one the CPU has beyond it.
#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
const LIMIT_CPUID_MAXVAL: u64 = 1 << 22;

/// The highest basic leaf the limit lets a CPU report, 2 on current parts and
/// 3 on early ones: a CPU reporting more is not limited.
#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
const LIMITED_HIGHEST_BASIC: u32 = 3;

/// What became of the limit firmware can put on a CPU's basic `CPUID` leaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpuidLimit {
    /// None was set.
    Unset,
    /// One was set and is now clear.
    Lifted,
    /// One was set and would not clear: every leaf above it stays hidden.
    Held,
}

/// Lift the limit on a CPU made by `vendor` whose leaf 0 reports
/// `highest_basic`, through its `IA32_MISC_ENABLE` `read` and `write`.
///
/// Only an Intel part reporting a limited leaf can be limited, so no other
/// CPU's register is touched; a set limit is cleared alone, every other
/// control keeping its setting, and the register is read back for the verdict.
#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
fn lift(
    vendor: Option<Vendor>,
    highest_basic: u32,
    mut read: impl FnMut() -> u64,
    write: impl FnOnce(u64),
) -> CpuidLimit {
    if vendor != Some(Vendor::Intel) || highest_basic > LIMITED_HIGHEST_BASIC {
        return CpuidLimit::Unset;
    }
    let controls = read();
    if controls & LIMIT_CPUID_MAXVAL == 0 {
        return CpuidLimit::Unset;
    }
    write(controls & !LIMIT_CPUID_MAXVAL);
    if read() & LIMIT_CPUID_MAXVAL == 0 {
        CpuidLimit::Lifted
    } else {
        CpuidLimit::Held
    }
}

/// Proof the calling CPU's limit is lifted, if it had one: what the CPU
/// offers past it is read only once this exists.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub(crate) struct Uncapped(CpuidLimit);

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
impl Uncapped {
    /// What became of the limit.
    pub(crate) const fn limit(&self) -> CpuidLimit {
        self.0
    }
}

/// Lift the calling CPU's limit, if firmware set one.
///
/// Both `WRMSR` and `CPUID` serialise, so every leaf read after this one
/// returns sees what the CPU reports with the limit gone.
///
/// # Safety
///
/// CPL 0.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub(crate) unsafe fn lift_limit() -> Uncapped {
    Uncapped(lift(
        vendor(),
        __cpuid(0).eax,
        // SAFETY: CPL 0 by the caller's contract, and `lift` reads only on an
        // Intel part, where the register is architectural.
        || unsafe { crate::msr::read(IA32_MISC_ENABLE) },
        // SAFETY: the value read with only the limit cleared, so no reserved
        // bit is set and every other control keeps its setting.
        |controls| unsafe { crate::msr::write(IA32_MISC_ENABLE, controls) },
    ))
}

/// What became of the boot CPU's limit.
static BOOT_CPU_LIMIT: OnceCell<CpuidLimit> = OnceCell::new();

/// Record what became of the boot CPU's limit.
///
/// # Errors
///
/// `limit` back when one is recorded already.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub(crate) fn record_boot_cpu(limit: CpuidLimit) -> Result<(), CpuidLimit> {
    BOOT_CPU_LIMIT.set(limit).map_err(|refused| refused.0)
}

/// What became of the boot CPU's limit, once its entry has lifted it.
#[must_use]
pub fn boot_cpu_limit() -> Option<CpuidLimit> {
    BOOT_CPU_LIMIT.get().ok().flatten().copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    extern crate std;
    use std::cell::Cell;

    #[test]
    fn a_leaf_is_reported_only_within_its_own_range() {
        assert!(reports(0, 0));
        assert!(reports(7, 0xD));
        assert!(!reports(7, 2), "past the highest basic leaf");
        assert!(reports(0x8000_0008, 0x8000_0008));
        assert!(!reports(0x8000_0008, 0x8000_0007));
        assert!(
            !reports(0x8000_0001, 0xD),
            "a CPU without the extended range answers it with a basic leaf"
        );
        assert!(!reports(7, 0x8000_0008), "a basic leaf is no extended one");
    }

    #[test]
    fn the_vendor_is_read_from_its_leaf0_spelling() {
        let leaf0 = |name: &[u8; 12]| {
            let word = |at: usize| {
                u32::from_le_bytes([name[at], name[at + 1], name[at + 2], name[at + 3]])
            };
            (word(0), word(4), word(8))
        };
        let (ebx, edx, ecx) = leaf0(b"GenuineIntel");
        assert_eq!(Vendor::from_leaf0(ebx, edx, ecx), Some(Vendor::Intel));
        let (ebx, edx, ecx) = leaf0(b"AuthenticAMD");
        assert_eq!(Vendor::from_leaf0(ebx, edx, ecx), Some(Vendor::Amd));
        assert_eq!(
            Vendor::from_leaf0(edx, ebx, ecx),
            None,
            "the registers in another order spell no vendor"
        );
        assert_eq!(Vendor::from_leaf0(0, 0, 0), None);
        assert_eq!(Vendor::Intel.name(), "Intel");
        assert_eq!(Vendor::Amd.name(), "AMD");
    }

    /// A register holding `value`, which keeps the writes it is given unless
    /// `drops` is set, and counts both.
    struct Register {
        value: Cell<u64>,
        drops: bool,
        reads: Cell<u32>,
        writes: Cell<u32>,
    }

    impl Register {
        fn holding(value: u64, drops: bool) -> Self {
            Self {
                value: Cell::new(value),
                drops,
                reads: Cell::new(0),
                writes: Cell::new(0),
            }
        }

        fn lift(&self, vendor: Option<Vendor>, highest_basic: u32) -> CpuidLimit {
            lift(
                vendor,
                highest_basic,
                || {
                    self.reads.set(self.reads.get() + 1);
                    self.value.get()
                },
                |value| {
                    self.writes.set(self.writes.get() + 1);
                    if !self.drops {
                        self.value.set(value);
                    }
                },
            )
        }
    }

    /// Fast strings, the thermal monitor, performance monitoring and XD Bit
    /// Disable beside the limit: bits 0, 3, 7, 34 and 22.
    const LIMITED: u64 = 0x0000_0004_0040_0089;

    #[test]
    fn a_set_cpuid_limit_is_cleared_alone_and_verified() {
        let register = Register::holding(LIMITED, false);
        assert_eq!(register.lift(Some(Vendor::Intel), 2), CpuidLimit::Lifted);
        assert_eq!(register.writes.get(), 1);
        assert_eq!(register.value.get(), 0x0000_0004_0000_0089);
        assert_eq!(register.reads.get(), 2, "read, then read back");
    }

    #[test]
    fn a_cpuid_limit_that_will_not_clear_is_held() {
        let register = Register::holding(LIMITED, true);
        assert_eq!(register.lift(Some(Vendor::Intel), 3), CpuidLimit::Held);
        assert_eq!(register.writes.get(), 1);
    }

    #[test]
    fn only_a_limited_intel_part_has_its_register_touched() {
        for (vendor, highest_basic) in [
            (Some(Vendor::Amd), 2),
            (None, 2),
            (Some(Vendor::Intel), LIMITED_HIGHEST_BASIC + 1),
            (Some(Vendor::Intel), 0xD),
        ] {
            let register = Register::holding(LIMITED, false);
            assert_eq!(register.lift(vendor, highest_basic), CpuidLimit::Unset);
            assert_eq!(
                (register.reads.get(), register.writes.get()),
                (0, 0),
                "{vendor:?} reporting leaf {highest_basic}"
            );
        }
        let register = Register::holding(LIMITED & !LIMIT_CPUID_MAXVAL, false);
        assert_eq!(register.lift(Some(Vendor::Intel), 2), CpuidLimit::Unset);
        assert_eq!(
            (register.reads.get(), register.writes.get()),
            (1, 0),
            "a clear limit is left as it is"
        );
    }
}
