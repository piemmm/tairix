//! x86_64 floating-point isolation vertical (`plans/OPEN-DEFECTS.md`
//! D359/D360/D362): two hardware-isolated ring-3 tasks fill the whole SSE/x87
//! register file and `MXCSR` with different patterns and timeshare one CPU,
//! and neither may see the other's values; a third task from the entry-hygiene
//! fixture proves first entry to user mode leaks no kernel register state; and
//! the kernel does floating point in the dispatch loop under a probe task's
//! hostile `MXCSR` (round-toward-zero, an unmasked exception) and must round to
//! nearest and take no `#XM`. Any shortfall flips `qemu_exit::exit_failure` or
//! times out, so the run fails loudly.

#![cfg_attr(itest_x86_64, no_std)]
#![cfg_attr(itest_x86_64, no_main)]
#![deny(missing_docs)]

/// Whether this run's CPU model hides every basic `CPUID` leaf past 2.
#[cfg(all(itest_x86_64, feature = "test-hooks"))]
const CAPPED_CPUID: bool = false;

include!("stubs.rs");
