//! acceptance test: the CFQ policy must pass the shared
//! [`tairix_kernel_sched_api::conformance`] suite.
//!
//! Every concrete scheduler runs the same suite against itself; the
//! canonical copy in `kernel/sched/api/tests/conformance.rs` drives the MLFQ
//! policy it dev-depends on. This drives the CFQ policy purely through the
//! [`SchedulerPolicy`](tairix_kernel_sched_api::SchedulerPolicy) trait —
//! exactly as the EEVDF and MLFQ siblings are exercised — proving the
//! policies are interchangeable behind the contract.

use tairix_fuzzseed::meter::{refusing_after, Metered};
use tairix_kernel_sched_api::conformance;
use tairix_kernel_sched_api::TestArch;
use tairix_kernel_sched_cfq::Scheduler;

/// The metered allocator the refusal properties refuse through.
#[global_allocator]
static ALLOCATOR: Metered = Metered;

#[test]
fn cfq_refuses_what_its_heap_cannot_hold() {
    conformance::run_refusals::<Scheduler<TestArch>>(&|granted: usize, call: &mut dyn FnMut()| {
        refusing_after(granted, call);
    });
}

#[test]
fn cfq_passes_scheduler_policy_conformance() {
    conformance::run_all::<Scheduler<TestArch>>();
}

#[test]
fn cfq_survives_scheduler_policy_races() {
    conformance::run_races::<Scheduler<TestArch>>();
}
