//! riscv64 side-channel mitigations.
//!
//! The RISC-V cores TAIRiX targets — the QEMU `virt` board and the SiFive
//! U-series (U54 / U74) — execute strictly **in order**: they never run on
//! past a faulting load or a mispredicted branch, so the transient
//! execution every class here exploits has no window on this silicon.
//! Every slot is therefore a justified no-op. A memory-ordering `fence` is
//! no substitute: it orders accesses and stops no speculation, so emitting
//! one would defend nothing.
//!
//! Adding an out-of-order RISC-V core as a target means revisiting this
//! profile against that core's errata.

use tairix_arch_api::{Mitigation, MitigationProfile, SideChannelMitigation};

/// The justification every speculation-dependent slot shares.
const IN_ORDER: &str = "the RISC-V cores TAIRiX targets (QEMU virt, SiFive U54/U74) execute in \
                        order, so no instruction runs speculatively past a fault, a mispredicted \
                        branch or a return";

/// riscv64 implementation of the Arch HAL side-channel declaration.
#[derive(Debug, Default, Clone, Copy)]
pub struct SideChannel;

impl SideChannel {
    /// Construct the riscv64 side-channel handle.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// The honest declaration for riscv64 (see the module docs).
    #[must_use]
    pub const fn declared_profile() -> MitigationProfile {
        MitigationProfile {
            address_space_isolation: Mitigation::NotVulnerable(
                "Meltdown needs speculation past a faulting load; the in-order RISC-V cores \
                 TAIRiX targets (QEMU virt, SiFive U54/U74) never do, and page-permission \
                 isolation still applies",
            ),
            user_entry_hardening: Mitigation::NotVulnerable(IN_ORDER),
            user_return_buffer_clear: Mitigation::NotVulnerable(
                "MDS / L1TF / MMIO-stale-data are Intel store/fill/load-buffer sampling flaws; \
                 the RISC-V cores TAIRiX targets do not expose those buffers",
            ),
            user_return_speculation_barrier: Mitigation::NotVulnerable(IN_ORDER),
            context_switch_indirect_branch_barrier: Mitigation::NotVulnerable(
                "exploiting a poisoned branch predictor needs transient execution past the \
                 mispredict; the in-order RISC-V cores TAIRiX targets provide no such window",
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
mod tests {
    use super::*;
    use tairix_arch_api::sidechannel::conformance;

    #[test]
    fn passes_side_channel_conformance() {
        conformance::run_all(&SideChannel::new());
    }

    #[test]
    fn declared_profile_is_honest_and_release_ready() {
        let profile = SideChannel::new().profile();
        assert_eq!(profile.validate(), Ok(()));
        // Applied would claim code on the trap path; there is none to pin.
        assert!(profile
            .entries()
            .iter()
            .all(|entry| matches!(entry.mitigation, Mitigation::NotVulnerable(_))));
        assert!(profile.is_release_ready());
    }
}
