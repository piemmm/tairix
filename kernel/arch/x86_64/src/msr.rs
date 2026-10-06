//! Model-specific registers: the one `rdmsr`/`wrmsr` pair every path reads
//! and writes them through.

/// The low and high halves of `value`: the `eax:edx` pair `wrmsr` takes it
/// as (Intel SDM Vol 2B §4.3).
#[must_use]
pub const fn halves(value: u64) -> (u32, u32) {
    let [b0, b1, b2, b3, b4, b5, b6, b7] = value.to_le_bytes();
    (
        u32::from_le_bytes([b0, b1, b2, b3]),
        u32::from_le_bytes([b4, b5, b6, b7]),
    )
}

/// The value `rdmsr` returns as `edx:eax`.
#[must_use]
pub const fn joined(low: u32, high: u32) -> u64 {
    ((high as u64) << 32) | low as u64
}

/// Read MSR `msr`.
///
/// # Safety
///
/// Ring 0, and `msr` implemented on this CPU: an unimplemented one `#GP`s.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
#[inline]
#[must_use]
pub unsafe fn read(msr: u32) -> u64 {
    let low: u32;
    let high: u32;
    // SAFETY: the caller's contract; `rdmsr` touches no memory.
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags),
        );
    }
    joined(low, high)
}

/// Write `value` to MSR `msr`, never reordered by the compiler past the
/// memory accesses around it. The CPU may: a write to an x2APIC or TSC
/// deadline register is not serialising, so a caller ordering stores before
/// it fences first.
///
/// # Safety
///
/// As for [`read`], and `value` valid for the register: a reserved bit set
/// `#GP`s, and the register's own effects are the caller's to answer for.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
#[inline]
pub unsafe fn write(msr: u32, value: u64) {
    let (low, high) = halves(value);
    // SAFETY: the caller's contract.
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") low,
            in("edx") high,
            options(nostack, preserves_flags),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_splits_into_the_halves_it_is_rejoined_from() {
        let value = 0x0123_4567_89AB_CDEF;
        assert_eq!(halves(value), (0x89AB_CDEF, 0x0123_4567));
        assert_eq!(joined(0x89AB_CDEF, 0x0123_4567), value);
        assert_eq!(halves(u64::MAX), (u32::MAX, u32::MAX));
    }
}
