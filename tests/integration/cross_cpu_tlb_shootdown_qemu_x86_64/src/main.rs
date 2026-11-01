//! WIRING Stage W6 QEMU integration test, its AP at an xAPIC id: [`vertical`].

#![cfg_attr(itest_x86_64, no_std)]
#![cfg_attr(itest_x86_64, no_main)]
#![deny(missing_docs)]
// The bare-metal body narrows page-table and APIC register fields whose
// widths the hardware fixes; each site's value is masked or shifted into
// range first.
#![cfg_attr(itest_x86_64, allow(clippy::cast_possible_truncation))]

mod vertical;

/// The run's AP has an id xAPIC names.
#[cfg(itest_x86_64)]
const AP_PAST_XAPIC: bool = false;

/// The run's AP lacks a protection the boot CPU holds, and must be refused.
#[cfg(itest_x86_64)]
const AP_REFUSED: bool = false;

#[cfg(not(itest_x86_64))]
fn main() {}
