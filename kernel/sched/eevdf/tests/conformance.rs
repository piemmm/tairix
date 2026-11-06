//! acceptance test: the EEVDF policy must pass the shared
//! [`tairix_kernel_sched_api::conformance`] suite.
//!
//! Every concrete scheduler runs the same suite against itself; the
//! canonical copy in `kernel/sched/api/tests/conformance.rs` drives the
//! build-selected policy. This drives the EEVDF policy purely through the
//! [`SchedulerPolicy`](tairix_kernel_sched_api::SchedulerPolicy) trait —
//! exactly as the MLFQ sibling is exercised — proving the two policies
//! are interchangeable behind the contract.

use tairix_fuzzseed::meter::{refusing_after, Metered};
use tairix_kernel_sched_api::conformance;
use tairix_kernel_sched_api::TestArch;
use tairix_kernel_sched_eevdf::Scheduler;

/// The metered allocator the refusal properties refuse through.
#[global_allocator]
static ALLOCATOR: Metered = Metered;

#[test]
fn eevdf_refuses_what_its_heap_cannot_hold() {
    conformance::run_refusals::<Scheduler<TestArch>>(&|granted: usize, call: &mut dyn FnMut()| {
        refusing_after(granted, call);
    });
}

#[test]
fn eevdf_passes_scheduler_policy_conformance() {
    conformance::run_all::<Scheduler<TestArch>>();
}

#[test]
fn eevdf_survives_scheduler_policy_races() {
    conformance::run_races::<Scheduler<TestArch>>();
}
