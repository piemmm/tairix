//! aarch64 side-channel mitigations.
//!
//! The out-of-order Cortex-A cores TAIRiX targets (the A72 and A76 of the
//! Raspberry Pi 4 and 5, and generic `ARMv8` servers) are exposed to the
//! speculative classes, Spectre v1/v2 among them; the in-order A53 of the
//! Pi 3 is not. None is exposed to the Intel buffer-sampling family (MDS,
//! L1TF, MMIO stale data). What the port applies, and where:
//!
//! * **User-entry hardening.** The trap trampoline (`vectors.s`) zeroes
//!   `x2`–`x28` once it has saved them, before the handler runs: `x0` and
//!   `x1` carry the exception kind and the frame, the forced frame pointers
//!   make the handler's prologue replace `x29`, and its `bl` replaces
//!   `x30`.
//! * **Return-to-user speculation barrier.** `dsb nsh; isb` follows every
//!   `eret`, so straight-line speculation past it cannot execute the bytes
//!   behind it. That pair is the portable form of `sb`, which only
//!   Armv8.5's `FEAT_SB` adds and neither the A72 nor the A76 has; being
//!   past the `eret`, it is never architecturally executed and costs
//!   nothing.
//!
//! `sidechannel_tests` pins both onto every path in the port's sources.

use tairix_arch_api::{Mitigation, MitigationProfile, SideChannelMitigation};

/// aarch64 implementation of the Arch HAL side-channel declaration.
#[derive(Debug, Default, Clone, Copy)]
pub struct SideChannel;

impl SideChannel {
    /// Construct the aarch64 side-channel handle.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// The honest declaration for aarch64 (see the module docs).
    #[must_use]
    pub const fn declared_profile() -> MitigationProfile {
        MitigationProfile {
            address_space_isolation: Mitigation::Pending(
                "a Meltdown-exposed core (Cortex-A75 among Arm's) needs the kernel unmapped from \
                 the user regime: a trampoline vector page and a TTBR1 switch on every entry and \
                 return",
            ),
            user_entry_hardening: Mitigation::Applied,
            user_return_buffer_clear: Mitigation::NotVulnerable(
                "MDS / L1TF / MMIO-stale-data are Intel store/fill/load-buffer sampling flaws; \
                 the ARMv8 cores TAIRiX targets do not expose those buffers",
            ),
            user_return_speculation_barrier: Mitigation::Applied,
            context_switch_indirect_branch_barrier: Mitigation::Pending(
                "the ARMv8 Spectre-v2 BHB/BTB sequence is MIDR-specific (BHB-clear loop or \
                 SMCCC firmware workaround); the MIDR/firmware-feature probe is not yet wired",
            ),
        }
    }
}

impl SideChannelMitigation for SideChannel {
    fn profile(&self) -> MitigationProfile {
        Self::declared_profile()
    }
}

#[cfg(test)]
#[path = "sidechannel_tests.rs"]
mod sidechannel_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use tairix_arch_api::sidechannel::conformance;

    #[test]
    fn passes_side_channel_conformance() {
        conformance::run_all(&SideChannel::new());
    }

    #[test]
    fn declared_profile_is_honest_and_matches_silicon() {
        let profile = SideChannel::new().profile();
        assert_eq!(profile.validate(), Ok(()));
        assert_eq!(profile.user_entry_hardening, Mitigation::Applied);
        assert_eq!(profile.user_return_speculation_barrier, Mitigation::Applied);
        assert!(matches!(
            profile.user_return_buffer_clear,
            Mitigation::NotVulnerable(_)
        ));
        assert!(profile.address_space_isolation.is_pending());
        assert!(profile.context_switch_indirect_branch_barrier.is_pending());
        assert!(!profile.is_release_ready());
    }
}
