//! The architecture-neutral trap callbacks every port installs.
//!
//! A port's timer interrupt, its return-to-user preempt point, and its
//! reschedule IPI each need the *same* kernel-side work done; only the
//! trap plumbing that reaches them is target-divergent. These are the
//! single definitions of that work, installed directly by each port's
//! wiring rather than re-stated per port, so a new port inherits the
//! whole body and none can drive a tick that performs only part of it.
//! Three ports each carrying their own copy is how wasm32 came to drive
//! no deadline sweep at all.
//!
//! Every callback here is pure accounting except
//! [`on_user_preempt_point`], which context-switches and is therefore
//! reached only from a trap taken in user mode. That is what makes the
//! others safe on a trap taken with the kernel mid-operation.

use core::sync::atomic::Ordering;

use tairix_arch_api::CpuId;

/// Honour `cpu`'s pending reschedule on the way back to user mode.
///
/// Installed as each port's return-to-user preempt callback and reached
/// on **any** interrupt returning to user mode. It reschedules only when
/// this CPU owes one, so an interrupt that woke nothing returns straight
/// to user mode with no gratuitous context switch; a stray invocation
/// with no resumable user task published is a no-op rather than an
/// unsound switch.
pub extern "C" fn on_user_preempt_point(cpu: CpuId) {
    let _ = crate::preempt::preempt_current(cpu);
}

/// Perform every per-tick duty on `cpu`.
///
/// Installed as each port's timer-tick callback and invoked on **every**
/// fired tick, whichever privilege level it interrupted:
///
/// * [`note_preempt_tick`](crate::note_preempt_tick) latches the tick, so
///   a quantum that expires inside a syscall is honoured at that
///   syscall's completion instead of being silently lost.
/// * [`timed_wake_sweep`](crate::timed_wake_sweep) requests the
///   blocking-wait deadline sweep, which is what makes a finite timeout
///   fire when every task is parked and no preemption is owed.
/// * [`check_stall`](crate::check_stall) samples the stall watchdog: a
///   tick still fires on a CPU looping without returning to the
///   scheduler, so this is where a soft lockup becomes observable.
///
/// All three are pure accounting and never context-switch, so a tick
/// taken in kernel mode is safe; preempting a *user* task is the separate
/// [`on_user_preempt_point`].
pub extern "C" fn on_timer_tick(cpu: CpuId) {
    crate::preempt::note_preempt_tick(cpu);
    crate::waitq::timed_wake_sweep();
    crate::watchdog::check_stall(cpu);
}

/// Latch a delivered reschedule IPI as an **un-gated** yield of `cpu`.
///
/// Installed as each port's reschedule-IPI callback. A directed IPI is a
/// decision its sender has already taken — work placed on this CPU's queue,
/// or the task running here told to die or to give up its slot — so unlike a
/// quantum tick it is not re-judged against "has this CPU a competitor".
/// Routing it through the tick latch dropped exactly the case the senders
/// send it for: a lone runnable task on the target swallowed the IPI, and its
/// kill or its stop then waited on the lockup watchdog's monopoly guard.
/// Pure accounting — the context switch is [`on_user_preempt_point`]'s job.
pub extern "C" fn on_reschedule_ipi(cpu: CpuId) {
    if let Some(state) = crate::cpu_state::get(cpu) {
        state.ipi_pending_since.store(0, Ordering::Release);
    }
    crate::preempt::request_forced_yield(cpu);
}

/// Note that a reschedule IPI is about to be raised on `target`, so the
/// lockup watchdog can report one the target never takes
/// ([`AuditEvent::IpiUndelivered`](crate::audit::AuditEvent::IpiUndelivered)).
/// Called by each port before it raises the IPI; the oldest outstanding send
/// is the one kept, so a later send reads no clock. A send to the calling
/// CPU is not noted, since a port may answer it without an interrupt, and
/// nothing is noted until a watchdog cadence sample has run, since only its
/// scan reads the mark.
pub fn note_reschedule_ipi_sent(target: CpuId) {
    if !crate::watchdog::cadence_running() {
        return;
    }
    let Some(hook) = crate::waitq::wait_arch() else {
        return;
    };
    if hook.current_cpu() == Some(target) {
        return;
    }
    let Some(state) = crate::cpu_state::get(target) else {
        return;
    };
    if state.ipi_pending_since.load(Ordering::Relaxed) != 0 {
        return;
    }
    let _ = state.ipi_pending_since.compare_exchange(
        0,
        hook.now_ns().max(1),
        Ordering::AcqRel,
        Ordering::Relaxed,
    );
}

#[cfg(test)]
mod tests {
    use super::{
        note_reschedule_ipi_sent, on_reschedule_ipi, on_timer_tick, on_user_preempt_point,
    };
    use crate::preempt::{preemption_count, take_preempt_pending};
    use core::sync::atomic::Ordering;

    /// A send to another CPU is noted with when it was made, the oldest of
    /// several outstanding kept, and taking the IPI clears it.
    #[test]
    fn a_noted_send_is_cleared_when_the_ipi_is_taken() {
        let _ = crate::test_boot::claim_scheduler();
        let target = crate::test_boot::claim_cpu();
        let pending = || {
            crate::cpu_state::get(target)
                .expect("a claimed CPU has a slot")
                .ipi_pending_since
                .load(Ordering::Relaxed)
        };
        crate::watchdog::note_cadence_running();
        crate::test_boot::advance_clock(5_000);
        note_reschedule_ipi_sent(target);
        crate::test_boot::advance_clock(1_000);
        note_reschedule_ipi_sent(target);
        assert_eq!(pending(), 5_000, "the oldest send is kept");
        on_reschedule_ipi(target);
        assert_eq!(pending(), 0);
    }

    #[test]
    fn the_timer_tick_latches_the_pending_preemption() {
        let cpu = crate::test_boot::claim_cpu();
        on_timer_tick(cpu);
        assert!(
            take_preempt_pending(cpu),
            "a fired tick must latch the CPU's pending preemption"
        );
    }

    /// With nothing owed the preempt point must neither count a preemption
    /// nor fault on a CPU with no user task switched in.
    #[test]
    fn the_user_preempt_point_is_a_no_op_when_nothing_is_owed() {
        let cpu = crate::test_boot::claim_cpu();
        assert_eq!(preemption_count(cpu), 0);
        on_user_preempt_point(cpu);
        assert_eq!(preemption_count(cpu), 0);
    }
}
