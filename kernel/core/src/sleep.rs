//! A kernel task's timed sleep: park off the run queue until a deadline.
//!
//! The waits a device bring-up owes the hardware — a supply's ramp, a
//! signalling switch's settle — are intervals, not events. The task
//! registers on [`SLEEP_WAITQ`] with its deadline, arms the one-shot at the
//! nearest pending deadline, and parks; the timed sweep releases it. A resume
//! before the deadline parks again for the remainder, so the sleep is never
//! short, and the CPU runs everything else meanwhile.

use crate::dispatch_slot::RescheduleAction;
use crate::kthread::reschedule_current;
use crate::waitq::{nearest_timed_deadline, wait_arch, SLEEP_WAITQ};

/// The caller cannot be parked — the boot flow before the dispatch loop runs,
/// or a context with no wait hook — so only it knows what to do instead.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct NotParkable;

/// Park the calling task until the monotonic clock reaches `deadline_ns`.
///
/// Returns at once when the deadline has already passed.
///
/// # Errors
///
/// [`NotParkable`] when the caller is not a task the scheduler can park; it
/// leaves no registration behind and has not slept to the deadline.
pub fn park_until(deadline_ns: u64) -> Result<(), NotParkable> {
    let hook = wait_arch().ok_or(NotParkable)?;
    loop {
        if hook.now_ns() >= deadline_ns {
            return Ok(());
        }
        let cpu = hook.current_cpu().ok_or(NotParkable)?;
        let task = hook.current_task(cpu).ok_or(NotParkable)?;
        SLEEP_WAITQ.register(task, deadline_ns);
        hook.set_wakeup(nearest_timed_deadline());
        let parked = reschedule_current(cpu, RescheduleAction::Park);
        SLEEP_WAITQ.deregister(task);
        hook.set_wakeup(nearest_timed_deadline());
        if !parked {
            return Err(NotParkable);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_boot;

    #[test]
    fn a_deadline_already_reached_returns_without_parking() {
        let _ = test_boot::claim_scheduler();
        test_boot::advance_clock(5_000);
        assert_eq!(park_until(5_000), Ok(()));
        assert_eq!(park_until(0), Ok(()));
    }

    #[test]
    fn a_caller_that_cannot_park_is_told_so_and_leaves_no_registration() {
        // The claimed CPU has no per-CPU state slot, so the park fails closed
        // exactly as it does for the pre-dispatch boot flow.
        let (_, task) = test_boot::claim_scheduler();
        assert_eq!(park_until(u64::MAX - 1), Err(NotParkable));
        let arch = test_boot::claimed_wait_arch().expect("claimed hook");
        assert!(
            !SLEEP_WAITQ.wake_task(arch, task),
            "the failed park deregistered itself"
        );
        assert!(test_boot::take_unparked().is_empty());
    }
}
