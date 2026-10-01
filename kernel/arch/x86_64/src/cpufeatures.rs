//! x86_64 CPU feature detection and cycle counter.
//!
//! Implements the Arch HAL
//! [`CpuFeatures`](tairix_arch_api::CpuFeatures) and
//! [`CpuCycles`](tairix_arch_api::CpuCycles) surfaces for x86_64.
//!
//! `CPUID` is the deterministic capability source: leaf 1 reports the
//! SSE/AVX/AES-NI/`crc32`(SSE4.2)/`PCLMULQDQ`/`RDRAND` flags in
//! `ECX`/`EDX`, and leaf 7 sub-leaf 0 reports AVX2/SHA-NI/`RDSEED` in
//! `EBX` (Intel SDM Vol. 2A "CPUID"; AMD64 APM Vol. 3). The cycle
//! counter is the Time-Stamp Counter (`RDTSC`), reported as a reliable
//! time base only when the part advertises an Invariant TSC
//! ([`crate::tsc`]).
//!
//! Only the architecture port reads `CPUID`, so detection lives here.
//! The decoder (`features_from_cpuid`) is pure and host-tested, as is the
//! vendor decode it shares with CPU bring-up
//! ([`crate::cpuname::vendor_from_leaf0`]); the register reads execute only
//! on the freestanding target and the host build reports the empty set / an
//! unknown core (no fake hardware in production paths).

use tairix_arch_api::{
    CoreType, CpuCycles, CpuFeature, CpuFeatureSet, CpuFeatures, CpuId, FeatureProfile,
    FeatureSupport,
};

// --- CPUID leaf 1: ECX/EDX feature bits ---
const LEAF1_ECX_PCLMULQDQ: u32 = 1;
const LEAF1_ECX_SSSE3: u32 = 9;
const LEAF1_ECX_SSE42: u32 = 20;
const LEAF1_ECX_AESNI: u32 = 25;
const LEAF1_ECX_OSXSAVE: u32 = 27;
const LEAF1_ECX_AVX: u32 = 28;
const LEAF1_ECX_RDRAND: u32 = 30;
const LEAF1_EDX_SSE2: u32 = 26;

// --- CPUID leaf 7 sub-leaf 0: EBX feature bits ---
const LEAF7_EBX_AVX2: u32 = 5;
const LEAF7_EBX_ERMS: u32 = 9;
const LEAF7_EBX_RDSEED: u32 = 18;
const LEAF7_EBX_SHA: u32 = 29;

/// `XCR0` bits the operating system must have enabled before an AVX
/// instruction may run: the SSE state and the YMM upper halves.
const XCR0_AVX_STATE: u64 = crate::xstate::SSE | crate::xstate::AVX;

/// Decode the `CPUID` feature registers into a [`CpuFeatureSet`].
///
/// Pure and host-testable: the bare-metal probe feeds it the registers
/// it read. `leaf1_ecx`/`leaf1_edx` are `CPUID.1` `ECX`/`EDX`;
/// `leaf7_ebx` is `CPUID.7` sub-leaf 0 `EBX`; `xcr0` is the enabled state,
/// read only when `leaf1_ecx` reports `OSXSAVE`. AVX and AVX2 are reported
/// only when that state includes the YMM registers, which an AVX
/// instruction faults without.
// The `ecx`/`edx`/`ebx` register names are the canonical CPUID hardware
// identifiers; their inherent similarity is the domain's, and renaming
// them to satisfy the lint would obscure which register each value came
// from.
#[allow(clippy::similar_names)]
#[must_use]
pub fn features_from_cpuid(
    leaf1_ecx: u32,
    leaf1_edx: u32,
    leaf7_ebx: u32,
    xcr0: u64,
) -> CpuFeatureSet {
    let mut set = CpuFeatureSet::EMPTY;
    let mut on = |reg: u32, bit: u32, feature: CpuFeature| {
        if (reg >> bit) & 1 == 1 {
            set = set.with(feature);
        }
    };
    on(leaf1_edx, LEAF1_EDX_SSE2, CpuFeature::Sse2);
    on(leaf1_ecx, LEAF1_ECX_SSSE3, CpuFeature::Ssse3);
    on(leaf1_ecx, LEAF1_ECX_SSE42, CpuFeature::Sse42);
    on(leaf1_ecx, LEAF1_ECX_AESNI, CpuFeature::AesNi);
    on(leaf1_ecx, LEAF1_ECX_PCLMULQDQ, CpuFeature::Pclmulqdq);
    on(leaf1_ecx, LEAF1_ECX_RDRAND, CpuFeature::Rdrand);
    on(leaf7_ebx, LEAF7_EBX_ERMS, CpuFeature::Erms);
    on(leaf7_ebx, LEAF7_EBX_SHA, CpuFeature::ShaNi);
    on(leaf7_ebx, LEAF7_EBX_RDSEED, CpuFeature::Rdseed);
    let os_enabled = (leaf1_ecx >> LEAF1_ECX_OSXSAVE) & 1 == 1;
    if os_enabled && xcr0 & XCR0_AVX_STATE == XCR0_AVX_STATE {
        on(leaf1_ecx, LEAF1_ECX_AVX, CpuFeature::Avx);
        on(leaf7_ebx, LEAF7_EBX_AVX2, CpuFeature::Avx2);
    }
    set
}

/// x86_64 implementation of the Arch HAL CPU-feature surface.
///
/// Zero-sized: the detection state lives in `CPUID`, not in the handle.
#[derive(Debug, Default, Clone, Copy)]
pub struct CpuFeatureDetect;

impl CpuFeatureDetect {
    /// Construct the x86_64 CPU-feature handle.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// The honest declaration for x86_64: both detection sources are
    /// `CPUID`, which is unconditionally available on every x86_64 CPU.
    #[must_use]
    pub const fn declared_profile() -> FeatureProfile {
        FeatureProfile {
            isa_features: FeatureSupport::Supported,
            core_identity: FeatureSupport::Supported,
        }
    }
}

impl CpuFeatures for CpuFeatureDetect {
    fn detect(&self, _cpu: CpuId) -> CpuFeatureSet {
        #[cfg(all(target_arch = "x86_64", target_os = "none"))]
        {
            // `CPUID` is unconditionally available on every x86_64 CPU
            // and side-effect-free. Leaf 7 requires the max-basic-leaf
            // bound, so read leaf 0's EAX first and treat an absent leaf
            // 7 as "no leaf-7 features" (fail closed to fewer bits).
            let basic = core::arch::x86_64::__cpuid(0);
            let leaf1 = core::arch::x86_64::__cpuid(1);
            let leaf7_ebx = if basic.eax >= 7 {
                core::arch::x86_64::__cpuid_count(7, 0).ebx
            } else {
                0
            };
            // `XGETBV` exists only once the OS has set `CR4.OSXSAVE`.
            let xcr0 = if (leaf1.ecx >> LEAF1_ECX_OSXSAVE) & 1 == 1 {
                // SAFETY: `OSXSAVE` set means `XGETBV` is enabled; `ECX = 0`
                // names `XCR0`, which is always readable.
                unsafe { core::arch::x86_64::_xgetbv(0) }
            } else {
                0
            };
            features_from_cpuid(leaf1.ecx, leaf1.edx, leaf7_ebx, xcr0)
        }
        #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
        {
            CpuFeatureSet::EMPTY
        }
    }

    fn core_type(&self, _cpu: CpuId) -> CoreType {
        #[cfg(all(target_arch = "x86_64", target_os = "none"))]
        {
            let leaf0 = core::arch::x86_64::__cpuid(0);
            let leaf1 = core::arch::x86_64::__cpuid(1);
            // An unknown vendor still keys its ops table by the leaf-1
            // signature in `raw_id`.
            CoreType {
                model: crate::cpuname::vendor_from_leaf0(leaf0.ebx, leaf0.edx, leaf0.ecx),
                class: tairix_arch_api::CoreClass::Performance,
                raw_id: u64::from(leaf1.eax),
            }
        }
        #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
        {
            CoreType::UNKNOWN
        }
    }

    fn profile(&self) -> FeatureProfile {
        Self::declared_profile()
    }
}

/// Bytes one `clflush` writes back, from `CPUID.1:EBX[15:8]`, or [`None`]
/// where the part reports none (and on the host).
#[must_use]
pub fn flush_line_bytes() -> Option<u64> {
    #[cfg(all(target_arch = "x86_64", target_os = "none"))]
    {
        flush_line_from_leaf1(core::arch::x86_64::__cpuid(1).ebx)
    }
    #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
    {
        None
    }
}

/// Decode the `clflush` line size, in bytes, from `CPUID.1:EBX`, or [`None`]
/// where the part reports none. Pure, so the decode is host-tested.
#[must_use]
pub fn flush_line_from_leaf1(ebx: u32) -> Option<u64> {
    const LINE_UNIT_BYTES: u64 = 8;
    Some(u64::from((ebx >> 8) & 0xFF) * LINE_UNIT_BYTES).filter(|&line| line != 0)
}

/// x86_64 implementation of the Arch HAL cycle-counter surface (the
/// Time-Stamp Counter).
#[derive(Debug, Default, Clone, Copy)]
pub struct CpuCycleCounter;

impl CpuCycleCounter {
    /// Construct the x86_64 cycle-counter handle.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl CpuCycles for CpuCycleCounter {
    fn cpu_cycles(&self) -> u64 {
        crate::tsc::read_tsc()
    }

    fn cycles_monotonic_hint(&self) -> bool {
        // The TSC is a reliable, constant-rate time base only when the
        // part advertises an Invariant TSC; otherwise it may change rate
        // with P-states. The harness still uses it (non-decreasing), but
        // treats a non-invariant reading with more caution.
        crate::tsc::detect_invariant_tsc()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tairix_arch_api::{cpucycles, cpufeatures};

    #[test]
    fn no_flags_decodes_to_the_empty_set() {
        assert_eq!(features_from_cpuid(0, 0, 0, 0), CpuFeatureSet::EMPTY);
    }

    #[test]
    fn the_flush_line_is_eight_bytes_per_unit_and_never_zero() {
        assert_eq!(flush_line_from_leaf1(0x0001_0800), Some(64));
        assert_eq!(flush_line_from_leaf1(0xFFFF_10FF), Some(128));
        assert_eq!(flush_line_from_leaf1(0xFFFF_00FF), None);
        assert_eq!(flush_line_bytes(), None, "the host reads no CPUID");
    }

    #[test]
    fn individual_flags_decode_to_their_features() {
        // SSE2 in leaf-1 EDX bit 26.
        let set = features_from_cpuid(0, 1 << 26, 0, 0);
        assert!(set.contains(CpuFeature::Sse2));
        assert!(!set.contains(CpuFeature::AesNi));

        // SSE4.2 (carries crc32) in leaf-1 ECX bit 20.
        let set = features_from_cpuid(1 << 20, 0, 0, 0);
        assert!(set.contains(CpuFeature::Sse42));

        // AES-NI (25) + PCLMULQDQ (1) + AVX (28) + RDRAND (30) in ECX, with
        // the OS having enabled the YMM state (OSXSAVE, 27).
        let ecx = (1 << 25) | (1 << 1) | (1 << 28) | (1 << 30) | (1 << 27);
        let set = features_from_cpuid(ecx, 0, 0, XCR0_AVX_STATE);
        assert!(set.contains(CpuFeature::AesNi));
        assert!(set.contains(CpuFeature::Pclmulqdq));
        assert!(set.contains(CpuFeature::Avx));
        assert!(set.contains(CpuFeature::Rdrand));

        // AVX2 (5) + ERMS (9) + RDSEED (18) + SHA-NI (29) in leaf-7 EBX.
        let ebx = (1 << 5) | (1 << 9) | (1 << 18) | (1 << 29);
        let set = features_from_cpuid(1 << LEAF1_ECX_OSXSAVE, 0, ebx, XCR0_AVX_STATE);
        assert!(set.contains(CpuFeature::Avx2));
        assert!(set.contains(CpuFeature::Erms));
        assert!(set.contains(CpuFeature::Rdseed));
        assert!(set.contains(CpuFeature::ShaNi));
    }

    #[test]
    fn masking_a_field_off_removes_the_bit() {
        // A full ECX with SSE4.2 set, then cleared: the bit disappears.
        let with = features_from_cpuid(1 << 20, 0, 0, 0);
        let without = features_from_cpuid(0, 0, 0, 0);
        assert!(with.contains(CpuFeature::Sse42));
        assert!(!without.contains(CpuFeature::Sse42));
    }

    #[test]
    fn typical_haswell_class_part_decodes() {
        // A Haswell-era part: SSE2, SSSE3, SSE4.2, AVX, AES-NI,
        // PCLMULQDQ, RDRAND in ECX/EDX; AVX2, RDSEED in leaf-7 EBX.
        let ecx = (1 << LEAF1_ECX_SSSE3)
            | (1 << LEAF1_ECX_SSE42)
            | (1 << LEAF1_ECX_AVX)
            | (1 << LEAF1_ECX_AESNI)
            | (1 << LEAF1_ECX_PCLMULQDQ)
            | (1 << LEAF1_ECX_RDRAND)
            | (1 << LEAF1_ECX_OSXSAVE);
        let edx = 1 << LEAF1_EDX_SSE2;
        let ebx = (1 << LEAF7_EBX_AVX2) | (1 << LEAF7_EBX_RDSEED);
        let set = features_from_cpuid(ecx, edx, ebx, XCR0_AVX_STATE);
        for f in [
            CpuFeature::Sse2,
            CpuFeature::Ssse3,
            CpuFeature::Sse42,
            CpuFeature::Avx,
            CpuFeature::Avx2,
            CpuFeature::AesNi,
            CpuFeature::Pclmulqdq,
            CpuFeature::Rdrand,
            CpuFeature::Rdseed,
        ] {
            assert!(set.contains(f), "expected {f:?}");
        }
        // No SHA-NI on this synthetic part.
        assert!(!set.contains(CpuFeature::ShaNi));
    }

    /// A CPU with AVX whose OS has not enabled the YMM state faults on the
    /// first AVX instruction, so the feature is not there to dispatch on.
    #[test]
    fn avx_needs_the_os_to_have_enabled_the_ymm_state() {
        let ecx = 1 << LEAF1_ECX_AVX;
        let ebx = 1 << LEAF7_EBX_AVX2;
        let osxsave = 1 << LEAF1_ECX_OSXSAVE;
        let x87_sse = crate::xstate::X87 | crate::xstate::SSE;
        for (leaf1_ecx, xcr0) in [
            (ecx, XCR0_AVX_STATE),
            (ecx | osxsave, 0),
            (ecx | osxsave, x87_sse),
        ] {
            let set = features_from_cpuid(leaf1_ecx, 0, ebx, xcr0);
            assert!(!set.contains(CpuFeature::Avx), "{leaf1_ecx:#x} {xcr0:#x}");
            assert!(!set.contains(CpuFeature::Avx2), "{leaf1_ecx:#x} {xcr0:#x}");
        }
        let set = features_from_cpuid(ecx | osxsave, 0, ebx, x87_sse | crate::xstate::AVX);
        assert!(set.contains(CpuFeature::Avx) && set.contains(CpuFeature::Avx2));
        // Nothing but the AVX pair depends on the OS state.
        let sha = features_from_cpuid(0, 0, 1 << LEAF7_EBX_SHA, 0);
        assert!(sha.contains(CpuFeature::ShaNi));
    }

    #[test]
    fn declared_profile_is_honest_and_release_ready() {
        let profile = CpuFeatureDetect::new().profile();
        assert_eq!(profile.validate(), Ok(()));
        assert!(profile.is_release_ready());
        assert!(profile.isa_features.is_supported());
        assert!(profile.core_identity.is_supported());
    }

    #[test]
    fn passes_cpufeatures_conformance() {
        cpufeatures::conformance::run_all(&CpuFeatureDetect::new());
        let dynamic: &dyn CpuFeatures = &CpuFeatureDetect::new();
        cpufeatures::conformance::run_all(dynamic);
    }

    #[test]
    fn passes_cpucycles_conformance() {
        cpucycles::conformance::run_all(&CpuCycleCounter::new());
        let dynamic: &dyn CpuCycles = &CpuCycleCounter::new();
        cpucycles::conformance::run_all(dynamic);
    }
}
