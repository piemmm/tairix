//! The `CPUID` reads more than one part of the port shares.

/// `CPUID` leaf 7 sub-leaf 0, the structured extended feature flags, or no
/// flags at all on a CPU whose highest basic leaf stops short of it: a leaf a
/// CPU does not report offers nothing.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub(crate) fn structured_extended_features() -> core::arch::x86_64::CpuidResult {
    use core::arch::x86_64::{__cpuid, __cpuid_count, CpuidResult};
    if __cpuid(0).eax >= 7 {
        __cpuid_count(7, 0)
    } else {
        CpuidResult {
            eax: 0,
            ebx: 0,
            ecx: 0,
            edx: 0,
        }
    }
}
