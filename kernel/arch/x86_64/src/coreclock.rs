//! x86_64 live-core-frequency source (the Arch HAL
//! [`CoreClock`](tairix_arch_api::CoreClock) slice).
//!
//! The `CpuCycleCounter` reference counter is `rdtsc`,
//! an Invariant TSC — a fixed-rate time base that advances at the CPU's
//! *base* frequency and never reflects turbo or throttling. The live core
//! clock instead comes from the `IA32_APERF`/`IA32_MPERF` feedback pair: over
//! any interval `APERF` accrues at the *actual* core frequency and `MPERF` at
//! the fixed TSC rate, so `f = ΔAPERF · tsc_hz / ΔMPERF`
//! ([`tairix_arch_api::frequency_hz`]) is the live clock, turbo and all. This
//! is Intel's "effective frequency" interface.
//!
//! The pair is present only when `CPUID.06H:ECX.0` (hardware coordination
//! feedback) is set — many virtual CPUs (QEMU TCG) do not implement it — so
//! this slice gates on that bit and on a calibrated TSC frequency
//! (`preempt::tsc_hz`, the `MPERF`/reference rate). A CPU lacking
//! either declares [`tairix_arch_api::CoreClockSupport::Unsupported`] and its
//! reads fail closed to `0` (never a `#GP` on an absent MSR, never a
//! fabricated rate). The MSRs are privileged, so the whole source is gated to
//! the freestanding target; a host build (even on an x86_64 developer
//! machine) declares itself unsupported and never executes `rdmsr`.
//!
//! Each CPU asks `CPUID` once, as it enables the counter: the instruction
//! serialises, and exits to the hypervisor under one, so it has no place on
//! the reads every preemption tick takes.

use core::sync::atomic::{AtomicU8, Ordering};

use tairix_arch_api::{CoreClock, CoreClockSupport};

/// `IA32_MPERF` — the reference (maximum-performance) counter, ticking at the
/// TSC rate. Read only on the freestanding target (privileged `rdmsr`).
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
const IA32_MPERF: u32 = 0xE7;
/// `IA32_APERF` — the actual-performance counter, ticking at the live core
/// frequency. Read only on the freestanding target (privileged `rdmsr`).
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
const IA32_APERF: u32 = 0xE8;

/// `true` if the calling CPU has the `APERF`/`MPERF`
/// hardware-coordination-feedback pair (`CPUID.06H:ECX.0`). Off the
/// freestanding target this is `false` so no privileged MSR is ever read on
/// the host.
#[must_use]
fn aperf_mperf_present() -> bool {
    #[cfg(all(target_arch = "x86_64", target_os = "none"))]
    {
        crate::cpuid::leaf(6, 0).is_some_and(|leaf| leaf.ecx & 1 != 0)
    }
    #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
    {
        false
    }
}

/// What the CPUs that have enabled the counter say of the pair, ordered so an
/// answer folds in as a maximum: unprobed, present once one has it, and
/// absent for good once any lacks it, so no CPU reads a register one lacks.
const UNPROBED: u8 = 0;
const PRESENT: u8 = 1;
const ABSENT: u8 = 2;

/// The pair's state across the CPUs that have enabled the counter.
static FEEDBACK: AtomicU8 = AtomicU8::new(UNPROBED);

/// Fold one CPU's answer into `state`. Relaxed: a CPU lacking the pair reads
/// only after its own fold, and a CPU reading the pair reads its own.
fn fold(state: &AtomicU8, present: bool) {
    state.fetch_max(if present { PRESENT } else { ABSENT }, Ordering::Relaxed);
}

/// Whether the pair may be read on the calling CPU, which has enabled the
/// counter.
fn feedback_present() -> bool {
    FEEDBACK.load(Ordering::Relaxed) == PRESENT
}

/// Read a model-specific register. Freestanding target only — the caller
/// gates on [`feedback_present`] so the MSR is known to exist.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
#[inline]
#[must_use]
fn rdmsr(msr: u32) -> u64 {
    // SAFETY: the kernel runs at ring 0 and every CPU that enabled the
    // counter, this one included, reported `CPUID.06H:ECX.0`, so `msr`
    // (APERF/MPERF) exists and the read cannot `#GP`.
    unsafe { crate::msr::read(msr) }
}

/// x86_64 implementation of the Arch HAL core-clock surface: `IA32_APERF`
/// over the `IA32_MPERF`/TSC reference.
///
/// Zero-sized: the state lives in MSRs and the calibrated TSC frequency is
/// held by `super::preempt`.
#[derive(Debug, Default, Clone, Copy)]
pub struct CoreClockCounter;

impl CoreClockCounter {
    /// Construct the x86_64 core-clock handle.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// `true` when a supported live source is available: the feedback pair is
    /// present and the TSC (reference) frequency has been calibrated.
    #[must_use]
    fn available() -> bool {
        feedback_present() && calibrated_tsc_hz() != 0
    }
}

/// The calibrated TSC (reference) frequency in Hz, or `0` off the
/// freestanding target where no calibration runs. `preempt::tsc_hz` exists
/// only on the bare-metal build (it reads a boot-calibrated static), so this
/// shim keeps the host build compiling and honestly uncalibrated.
#[inline]
#[must_use]
fn calibrated_tsc_hz() -> u64 {
    #[cfg(all(target_arch = "x86_64", target_os = "none"))]
    {
        crate::preempt::tsc_hz()
    }
    #[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
    {
        0
    }
}

impl CoreClock for CoreClockCounter {
    fn enable(&self) {
        // `APERF`/`MPERF` are free-running whenever the CPU implements the
        // feedback pair, so enabling is only asking whether it does.
        fold(&FEEDBACK, aperf_mperf_present());
    }

    fn core_cycles(&self) -> u64 {
        #[cfg(all(target_arch = "x86_64", target_os = "none"))]
        {
            if feedback_present() {
                return rdmsr(IA32_APERF);
            }
        }
        0
    }

    fn reference_cycles(&self) -> u64 {
        #[cfg(all(target_arch = "x86_64", target_os = "none"))]
        {
            if feedback_present() {
                return rdmsr(IA32_MPERF);
            }
        }
        0
    }

    fn reference_hz(&self) -> u64 {
        if Self::available() {
            calibrated_tsc_hz()
        } else {
            0
        }
    }

    fn support(&self) -> CoreClockSupport {
        if Self::available() {
            CoreClockSupport::Supported
        } else {
            CoreClockSupport::Unsupported(
                "no IA32_APERF/MPERF effective-frequency pair (CPUID.06H:ECX.0) or uncalibrated TSC",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tairix_arch_api::coreclock;

    #[test]
    fn host_build_is_unsupported_and_reads_zero() {
        // Off the freestanding target the MSRs are unreachable, so the handle
        // fails closed: unsupported, zero counters — no `rdmsr` executes.
        let clock = CoreClockCounter::new();
        clock.enable();
        assert!(!clock.support().is_supported());
        assert_eq!(clock.core_cycles(), 0);
        assert_eq!(clock.reference_cycles(), 0);
        assert_eq!(clock.reference_hz(), 0);
        coreclock::conformance::run_all(&clock);
    }

    /// A CPU lacking the pair stops every CPU reading it, whatever answers
    /// come after; until one answers, nothing may read it.
    #[test]
    fn one_cpu_lacking_the_pair_keeps_every_cpu_from_reading_it() {
        let state = AtomicU8::new(UNPROBED);
        fold(&state, true);
        assert_eq!(state.load(Ordering::Relaxed), PRESENT);
        fold(&state, true);
        assert_eq!(state.load(Ordering::Relaxed), PRESENT);
        fold(&state, false);
        assert_eq!(state.load(Ordering::Relaxed), ABSENT);
        fold(&state, true);
        assert_eq!(state.load(Ordering::Relaxed), ABSENT, "absent for good");

        let first_lacks = AtomicU8::new(UNPROBED);
        fold(&first_lacks, false);
        fold(&first_lacks, true);
        assert_eq!(first_lacks.load(Ordering::Relaxed), ABSENT);
    }
}
