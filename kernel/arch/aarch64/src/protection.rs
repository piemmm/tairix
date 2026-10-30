//! The protection every CPU runs the kernel under where it offers it:
//! Privileged Access Never, so EL1 can neither read nor write a page EL0 may
//! access. (EL1 cannot execute one either: every user leaf is `PXN`.)
//!
//! The kernel reaches user memory only through the direct map, so it never
//! clears `PSTATE.PAN` to open a user page to itself. Each CPU sets it as it
//! starts, and with `SCTLR_EL1.SPAN` clear every exception taken to EL1 sets
//! it again: an `eret` to EL0 restores EL0's own state, and the next entry to
//! EL1 sets PAN before any kernel code runs.
//!
//! PAN is ARMv8.1 (`FEAT_PAN`); the Raspberry Pi 4's Cortex-A72 lacks it. The
//! boot CPU settles whether it is in force from what it offers, before any
//! other CPU starts, and a CPU that cannot hold it does not join.

use core::sync::atomic::{AtomicBool, Ordering};

/// Whether PAN is in force: settled once, by the boot CPU, before any other
/// CPU starts, and clear until then.
static PAN_IN_FORCE: AtomicBool = AtomicBool::new(false);

/// Whether a CPU whose `ID_AA64MMFR1_EL1` reads `mmfr1` offers PAN: its
/// field, bits 23:20, is non-zero.
#[must_use]
pub const fn pan_offered_by(mmfr1: u64) -> bool {
    (mmfr1 >> 20) & 0xF != 0
}

/// Whether the kernel holds every CPU to PAN.
#[must_use]
pub fn pan_in_force() -> bool {
    PAN_IN_FORCE.load(Ordering::Acquire)
}

/// The MMU-on `SCTLR_EL1` a CPU writes: [`crate::paging::SCTLR_MMU_ON`],
/// with `SPAN` clear while `pan` is in force, so every exception taken to EL1
/// sets PAN. Where the CPU lacks PAN, `SPAN` is RES1 and stays set.
#[must_use]
pub const fn sctlr_mmu_on(pan: bool) -> u64 {
    if pan {
        crate::paging::SCTLR_MMU_ON & !crate::paging::SCTLR_SPAN
    } else {
        crate::paging::SCTLR_MMU_ON
    }
}

/// Whether the calling CPU offers PAN.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
fn pan_offered_here() -> bool {
    let mmfr1: u64;
    // SAFETY: the feature-ID register is readable at EL1 with no
    // architectural side effect.
    unsafe {
        core::arch::asm!(
            "mrs {v}, id_aa64mmfr1_el1",
            v = out(reg) mmfr1,
            options(nomem, nostack, preserves_flags),
        );
    }
    pan_offered_by(mmfr1)
}

/// Settle whether the kernel holds every CPU to PAN: whether the boot CPU
/// offers it.
///
/// Called once, by the boot CPU, before any other CPU starts and before the
/// boot CPU holds itself to it.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub(crate) fn settle() {
    PAN_IN_FORCE.store(pan_offered_here(), Ordering::Release);
}

/// Hold the calling CPU to PAN while it is in force, or answer `false`,
/// changing nothing, on a CPU that does not offer it.
///
/// `SCTLR_EL1.SPAN` follows when the CPU enables translation
/// ([`sctlr_mmu_on`]).
///
/// # Safety
///
/// At EL1, before the CPU reaches any user page.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub(crate) unsafe fn hold() -> bool {
    if !pan_in_force() {
        return true;
    }
    if !pan_offered_here() {
        return false;
    }
    // SAFETY: the CPU offers PAN, so the `PAN` register (`S3_0_C4_C2_3`,
    // spelled generically because the toolchain's baseline is ARMv8.0)
    // exists; setting it only puts EL0-accessible pages out of EL1's reach,
    // and nothing the kernel runs touches one.
    unsafe {
        core::arch::asm!(
            "msr S3_0_C4_C2_3, {pan}",
            pan = in(reg) 1u64 << 22,
            options(nostack, preserves_flags),
        );
    }
    true
}

#[cfg(test)]
mod tests {
    use super::{pan_in_force, pan_offered_by, sctlr_mmu_on};
    use crate::paging::{SCTLR_MMU_ON, SCTLR_RES1, SCTLR_SPAN};

    #[test]
    fn any_non_zero_pan_field_offers_pan() {
        assert!(!pan_offered_by(0));
        for field in 1..=0xF_u64 {
            assert!(pan_offered_by(field << 20), "field {field}");
        }
        assert!(
            !pan_offered_by(!(0xF << 20)),
            "bits 23:20 alone are the field"
        );
    }

    #[test]
    fn a_cpu_held_to_pan_enables_translation_with_span_clear() {
        assert_eq!(SCTLR_SPAN, 1 << 23);
        assert_ne!(SCTLR_RES1 & SCTLR_SPAN, 0, "RES1 where the CPU lacks PAN");
        let held = sctlr_mmu_on(true);
        assert_eq!(
            held & SCTLR_SPAN,
            0,
            "every exception taken to EL1 sets PAN"
        );
        assert_eq!(held | SCTLR_SPAN, SCTLR_MMU_ON, "and nothing else differs");
        assert_eq!(sctlr_mmu_on(false), SCTLR_MMU_ON);
    }

    #[test]
    fn pan_is_not_in_force_until_the_boot_cpu_settles_it() {
        assert!(!pan_in_force());
    }
}
