//! x86_64 side-channel mitigations.
//!
//! x86_64 silicon is exposed to the whole class — Meltdown, Spectre v1/v2,
//! MDS, L1TF, MMIO stale data — in a mix vendor and generation decide.
//! What the port applies, and where:
//!
//! * **User-entry hardening.** Every entry stub zeroes the general-purpose
//!   registers it saved before its first call into Rust, leaving only the
//!   arguments it chose; the syscall stub hands its trampoline a pointer
//!   to the saved frame rather than the user's `rip`/`rbp` in registers.
//!   The one GS swap that depends on the interrupted privilege (the
//!   preemption path) and the ring-3 fault context's swap are each
//!   followed by `lfence`, so a mispredicted privilege test cannot run
//!   kernel code against the user's GS base (the SWAPGS variant).
//! * **Return-to-user buffer clear.** `verw` against a writable data
//!   selector is the last instruction before every `sysretq` and every
//!   `iretq` to ring 3. On an `MD_CLEAR` part it overwrites the store,
//!   fill and load-port buffers; elsewhere it is a cheap segment check.
//! * **Return-to-user speculation barrier.** An `int3` follows every
//!   `sysretq` and `iretq`, so straight-line speculation past the return
//!   stops at a trap instead of running the kernel bytes behind it.
//!
//! The templates every stub splices in are defined here once, and
//! `sidechannel_tests` pins each applied mechanism onto every entry and
//! return path in the port's sources.

use tairix_arch_api::{Mitigation, MitigationProfile, SideChannelMitigation};

/// x86_64 implementation of the Arch HAL side-channel declaration.
#[derive(Debug, Default, Clone, Copy)]
pub struct SideChannel;

impl SideChannel {
    /// Construct the x86_64 side-channel handle.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// The honest declaration for x86_64 (see the module docs).
    #[must_use]
    pub const fn declared_profile() -> MitigationProfile {
        MitigationProfile {
            address_space_isolation: Mitigation::Pending(
                "KPTI needs a per-process user translation root that maps only the entry \
                 trampolines, and a CR3 switch on every entry and return",
            ),
            user_entry_hardening: Mitigation::Applied,
            user_return_buffer_clear: Mitigation::Applied,
            user_return_speculation_barrier: Mitigation::Applied,
            context_switch_indirect_branch_barrier: Mitigation::Pending(
                "IBPB through IA32_PRED_CMD #GPs unless CPUID.(EAX=7,ECX=0):EDX[26] enumerates \
                 it; neither that probe nor a context-switch hook issuing it is wired",
            ),
        }
    }
}

impl SideChannelMitigation for SideChannel {
    fn profile(&self) -> MitigationProfile {
        Self::declared_profile()
    }
}

/// The selector every return to ring 3 hands `verw`. Any selector clears
/// the buffers on an `MD_CLEAR` part; a writable data segment is the
/// fastest form.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub(crate) static VERW_SELECTOR: u16 = crate::gdt::PerCpuGdt::selectors().kernel_ds;

/// Template text zeroing the general-purpose registers no entry stub
/// passes an argument in. A stub splices it in after its saves and before
/// its call, and zeroes any System V argument register it leaves unused
/// itself.
#[doc(hidden)]
#[macro_export]
macro_rules! scrub_nonargument_gprs {
    () => {
        concat!(
            "xorl %eax, %eax\n",
            "xorl %ebx, %ebx\n",
            "xorl %ebp, %ebp\n",
            "xorl %r10d, %r10d\n",
            "xorl %r11d, %r11d\n",
            "xorl %r12d, %r12d\n",
            "xorl %r13d, %r13d\n",
            "xorl %r14d, %r14d\n",
            "xorl %r15d, %r15d\n",
        )
    };
}

/// Template text clearing the direction flag before a stub's first call into
/// Rust. Interrupt, exception, and trap-gate delivery keep the interrupted
/// context's `DF` — only `syscall` clears it, through `IA32_FMASK` — while the
/// System V AMD64 ABI requires `DF=0` at every function's entry, and the
/// kernel's `memcpy`/`memset` (`compiler_builtins`) are `rep movs`/`rep stos`
/// loops that read it. The interrupted context's own `DF` comes back with
/// `iretq`.
#[doc(hidden)]
#[macro_export]
macro_rules! clear_direction_flag {
    () => {
        "cld\n"
    };
}

/// Template text for an interrupt return with `%rsp` at the CPU frame:
/// clear the buffers when the frame goes back to ring 3, `iretq`, and trap
/// straight-line speculation behind it. The `naked_asm!` it is spliced
/// into binds `verw_sel`.
#[doc(hidden)]
#[macro_export]
macro_rules! iret_clearing_for_ring3 {
    () => {
        concat!(
            "testb $3, 8(%rsp)\n",
            "jz 3f\n",
            "verw {verw_sel}(%rip)\n",
            "3:\n",
            "iretq\n",
            "int3\n",
        )
    };
}

#[cfg(test)]
#[path = "sidechannel_tests.rs"]
pub(crate) mod sidechannel_tests;

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
        assert_eq!(profile.user_return_buffer_clear, Mitigation::Applied);
        assert_eq!(profile.user_return_speculation_barrier, Mitigation::Applied);
        assert!(profile.address_space_isolation.is_pending());
        assert!(profile.context_switch_indirect_branch_barrier.is_pending());
        assert!(!profile.is_release_ready());
    }
}
