//! WIRING Stage W6 QEMU integration test, its AP at an x2APIC id past the
//! eight bits xAPIC names: [`vertical`].

#![cfg_attr(itest_x86_64, no_std)]
#![cfg_attr(itest_x86_64, no_main)]
#![deny(missing_docs)]
// The bare-metal body narrows page-table and APIC register fields whose
// widths the hardware fixes; each site's value is masked or shifted into
// range first.
#![cfg_attr(itest_x86_64, allow(clippy::cast_possible_truncation))]

mod vertical;

/// The run's AP has an id past what xAPIC names, and the run fails if not.
#[cfg(itest_x86_64)]
const AP_PAST_XAPIC: bool = true;

#[cfg(not(itest_x86_64))]
fn main() {}
