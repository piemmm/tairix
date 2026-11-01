//! The x86_64 floating-point isolation vertical on a CPU whose basic `CPUID`
//! leaves stop at 2, as the limit firmware can set leaves them
//! (`plans/OPEN-DEFECTS.md` D1591): one reporting no leaf 0xD names no XSAVE
//! layout, so the run requires its state saved with FXSAVE64 before it runs
//! the whole isolation workload. It fails unless the CPU really stops there.

#![cfg_attr(itest_x86_64, no_std)]
#![cfg_attr(itest_x86_64, no_main)]
#![deny(missing_docs)]

/// Whether this run's CPU model hides every basic `CPUID` leaf past 2.
#[cfg(all(itest_x86_64, feature = "test-hooks"))]
const CAPPED_CPUID: bool = true;

include!("stubs.rs");
