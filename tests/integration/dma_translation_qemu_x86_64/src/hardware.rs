//! What the runs behind a hardware unit prove of it.

use tairix_itest_translation_witness::{Faults, Interrupts, Stage, Tables};

/// Every interrupt is its own remapping entry, in extended mode with the
/// CPUs in x2APIC mode; the unit walks tables at the second stage and raises
/// its faults as a message.
pub const UNIT: crate::vertical::Unit = (
    Interrupts::Remapped {
        extended: tairix_arch_x86_64::apic::x2apic,
    },
    Tables::Walked(Stage::Second),
    Faults::Served,
);
