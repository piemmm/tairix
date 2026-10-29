//! [`IrqParkWaiter`] — the parking [`IrqWaiter`] an in-kernel block-device
//! completion wait drives.
//!
//! A block driver hosted in the kernel (the boot virtio-blk and SD-host
//! devices behind `SharedBlock`) must give the CPU up while a device
//! completion is outstanding. Its waits run in whatever task context
//! reached the device — a user task inside an `fs_*` syscall, or an
//! in-kernel service kthread — and the wait may last milliseconds on real
//! hardware, so the discipline is the same one the `irq_wait` syscall
//! handler uses: **park the calling task off the run queue** and let the
//! device-IRQ dispatch path's [`irq_wake`](crate::waitq::irq_wake) unpark
//! it. Parking the *task* keeps the dispatch loop running everything else —
//! other tasks, the buffered console transmit top-up, the timed sweeps —
//! for the whole duration of the device wait.
//!
//! An earlier revision halted the *CPU* instead (a masked re-check + `wfi`
//! park inside the calling task). That starves the dispatch loop for the
//! whole device wait: on metal a cold-cache directory walk (`ls -lsR`)
//! stalled console output for every SD-card read and could strand the
//! system outright once the tickless one-shot was unarmed, because nothing
//! else was left to wake the halted CPU. The `wfi` shape survives here only
//! as the *fallback* for a context that cannot park (below).
//!
//! # Lost-wake interlock
//!
//! [`IrqParkWaiter::yield_now`] registers the current task on
//! [`IRQ_WAITQ`] **before** re-checking the
//! bound line's ready flag, and parks only if the flag is still clear — the same
//! register→re-test→park discipline `SleepLock` and the console read use.
//! A fire landing between the loop's consuming poll and the park is
//! therefore never lost: the flag re-check catches an early fire, and a
//! fire after the re-check finds the task registered, so the dispatcher's
//! wake (and the scheduler's wake-pending token) converts a racing park
//! into a re-ready.
//!
//! # Fallback park
//!
//! `reschedule_current` can suspend any dispatched kthread — a user task
//! inside its syscall trap and an in-kernel service kthread body alike
//! (both publish a resume handle). The only contexts it cannot suspend are
//! the boot flow before the dispatch loop runs its first task, and a host
//! test with no live dispatch loop; for those the waiter falls back to a
//! port-supplied, bounded CPU park (on aarch64 the race-free mask →
//! ready-re-check → `wfi` → unmask sequence). That boot context runs while
//! everything else is parked waiting on it, so briefly halting the CPU
//! there starves nothing; every steady-state wait takes the task-park path.
//! A waiter built with no fallback, for a task that only ever runs
//! dispatched, ends a wait it cannot park as [`IrqWaitAbort::Unparkable`].

use tairix_abi::IrqHandle;
use tairix_kernel_irq::{
    block_until_ready, IrqController, IrqTable, IrqWaitAbort, IrqWaiter, LineState, LineWait,
    WaitOutcome,
};
use tairix_kernel_sec::ProcessId;
use tairix_log::{Field, FieldValue, Level, Sink};

use crate::audit::{emit, AuditEvent};
use crate::dispatch_slot::RescheduleAction;
use crate::kthread::reschedule_current;
use crate::waitq::{nearest_timed_deadline, wait_arch, IRQ_WAITQ, LATE_WAKE_NS};

/// How long a device the kernel drives itself may take to answer — a
/// completion interrupt, or a whole operation — before the kernel records it:
/// a device answers in milliseconds.
pub(crate) const SLOW_DEVICE_NS: u64 = 250_000_000;

/// A bounded CPU-park a port supplies for contexts that cannot be
/// scheduler-parked (see the module docs). It must return once the bound
/// line has fired or promptly on any other interrupt — never spin
/// unboundedly — and must tolerate spurious returns (the caller re-polls).
pub type FallbackPark = fn(&IrqTable, IrqHandle);

/// Parking [`IrqWaiter`] for one device's bound interrupt line.
///
/// One instance is built per brought-up device and lives with it for the
/// kernel's lifetime. Every consumer waits through its [`LineWait`] — the
/// virtio host's completion notifier ([`KernelVirtioHost::notify_wait`]) and
/// the SDHCI engine's completion seam alike — so there is exactly one bounded
/// device-wait shape, and a device operation cannot acquire an *unbounded*
/// one by reaching past it.
/// A wait that could not expire would strand its task inside the device
/// operation, holding the device's lock, the instant a completion interrupt
/// were lost or coalesced.
///
/// [`KernelVirtioHost::notify_wait`]: ../../tairix_kernel_virtio/struct.KernelVirtioHost.html#method.notify_wait
pub struct IrqParkWaiter {
    /// The published IRQ table the device's line is bound in.
    table: &'static IrqTable,
    /// The kernel-held handle minted when the line was bound.
    handle: IrqHandle,
    /// The process the binding belongs to.
    owner: ProcessId,
    /// The controller the line is re-armed through before every park (the
    /// dispatch path's `fire` masks it on each completion).
    controller: &'static (dyn IrqController + Sync),
    /// The port's bounded CPU-park for non-parkable contexts.
    fallback_park: Option<FallbackPark>,
    /// Where a wake delivered late is recorded.
    audit: &'static (dyn Sink + Sync),
}

impl IrqParkWaiter {
    /// Build the waiter for a bound device line.
    ///
    /// `table`/`handle` name `owner`'s binding, and `fallback_park` is the
    /// port's bounded CPU-park for contexts that cannot be
    /// scheduler-parked, or [`None`] for a waiter only a dispatched task
    /// drives. A wake delivered late is recorded through `audit`.
    #[must_use]
    pub fn new(
        table: &'static IrqTable,
        handle: IrqHandle,
        owner: ProcessId,
        controller: &'static (dyn IrqController + Sync),
        fallback_park: Option<FallbackPark>,
        audit: &'static (dyn Sink + Sync),
    ) -> Self {
        Self {
            table,
            handle,
            owner,
            controller,
            fallback_park,
            audit,
        }
    }

    /// Block the caller until the bound line fires or `timeout_ns` elapses,
    /// consuming the ready flag on a fire, and record a fire that reached
    /// this wait [`LATE_WAKE_NS`] or more late ([`AuditEvent::IrqWakeLate`]).
    ///
    /// Each park registers the loop's deadline with the timed wait queue, so
    /// a silent line ends in [`WaitOutcome::TimedOut`] rather than a task
    /// parked for ever holding its device's lock.
    fn wait_line(&self, timeout_ns: u64) -> WaitOutcome {
        let from = self.table.now_ns();
        let outcome = block_until_ready(self.table, self.handle, self.owner, timeout_ns, self);
        if let (WaitOutcome::Ready, Some(from)) = (outcome, from) {
            self.record_if_late(from);
        }
        outcome
    }

    /// Record a completion overdue on the bound line: its state at the
    /// controller, and what the CPU it is routed to is doing.
    fn record_overdue(&self) {
        let Some(line) = self.table.line_for(self.handle, self.owner) else {
            return;
        };
        let state = self.controller.line_state(line);
        let flag = |read: fn(&LineState) -> bool| {
            state
                .as_ref()
                .map_or(FieldValue::Null, |state| FieldValue::Bool(read(state)))
        };
        let target = state.and_then(|state| state.target);
        let activity = target.and_then(crate::watchdog::activity_of);
        emit(
            self.audit,
            Level::Warn,
            AuditEvent::IrqCompletionOverdue,
            &[
                Field {
                    key: "line",
                    value: FieldValue::UnsignedInt(u64::from(line)),
                },
                Field {
                    key: "overdue_ms",
                    value: FieldValue::UnsignedInt(SLOW_DEVICE_NS / 1_000_000),
                },
                Field {
                    key: "pending",
                    value: flag(|state| state.pending),
                },
                Field {
                    key: "active",
                    value: flag(|state| state.active),
                },
                Field {
                    key: "enabled",
                    value: flag(|state| state.enabled),
                },
                Field {
                    key: "target",
                    value: target.map_or(FieldValue::Null, |cpu| {
                        FieldValue::UnsignedInt(u64::from(cpu))
                    }),
                },
                Field {
                    key: "target_activity",
                    value: activity
                        .map_or(FieldValue::Null, |activity| FieldValue::Str(activity.tag())),
                },
            ],
        );
    }

    /// Record a fire that landed after `from`, when this wait began, yet
    /// reached the waiter [`LATE_WAKE_NS`] or more after it, with when the
    /// interrupt's deferred wake was performed: the table's clock is the
    /// wait clock.
    fn record_if_late(&self, from: u64) {
        // A fire after `from` cannot reach us later than the wait lasted, so a
        // prompt wake costs one clock read and no binding lookup.
        let Some(now) = self
            .table
            .now_ns()
            .filter(|&now| now.saturating_sub(from) >= LATE_WAKE_NS)
        else {
            return;
        };
        let Some(fired) = self.table.last_fire_ns(self.handle, self.owner) else {
            return;
        };
        let late = now.saturating_sub(fired);
        if fired <= from || late < LATE_WAKE_NS {
            return;
        }
        let Some(line) = self.table.line_for(self.handle, self.owner) else {
            return;
        };
        let drained = IRQ_WAITQ.drained_at_ns().filter(|&at| at >= fired);
        let mut fields = [
            Field {
                key: "line",
                value: FieldValue::UnsignedInt(u64::from(line)),
            },
            Field {
                key: "late_ms",
                value: FieldValue::UnsignedInt(late / 1_000_000),
            },
            Field {
                key: "drained_ms",
                value: FieldValue::Null,
            },
        ];
        let len = match drained {
            Some(at) => {
                fields[2].value = FieldValue::UnsignedInt((at - fired) / 1_000_000);
                3
            }
            None => 2,
        };
        emit(
            self.audit,
            Level::Warn,
            AuditEvent::IrqWakeLate,
            &fields[..len],
        );
    }
}

impl LineWait for IrqParkWaiter {
    /// A completion still missing after a quarter of a second is recorded
    /// ([`AuditEvent::IrqCompletionOverdue`]) with where its interrupt
    /// stands, and the wait goes on to the budget.
    fn await_completion(&self, budget_ns: u64) -> WaitOutcome {
        if budget_ns <= SLOW_DEVICE_NS {
            return self.wait_line(budget_ns);
        }
        match self.wait_line(SLOW_DEVICE_NS) {
            WaitOutcome::TimedOut => {
                self.record_overdue();
                self.wait_line(budget_ns - SLOW_DEVICE_NS)
            }
            outcome => outcome,
        }
    }

    fn await_event(&self, timeout_ns: u64) -> WaitOutcome {
        self.wait_line(timeout_ns)
    }
}

impl IrqWaiter for IrqParkWaiter {
    fn now_ns(&self) -> u64 {
        // Before the scheduler hook exists no wait can be timed; the
        // constant clock makes a bounded wait behave as unbounded, and the
        // fallback park below still gives the CPU up between polls.
        wait_arch().map_or(0, crate::waitq::WaitQueueArch::now_ns)
    }

    fn yield_now(&self, deadline_ns: u64) -> Result<(), IrqWaitAbort> {
        // Re-arm the line before any wait: the dispatch path's `fire`
        // masked it on the previous completion (mask-before-wake). A
        // refusal is harmless — the wait is then bounded by its deadline.
        let _ = self.table.rearm(self.handle, self.owner, self.controller);
        let Some(hook) = wait_arch() else {
            return self.fall_back();
        };
        let Some(cpu) = hook.current_cpu() else {
            return self.fall_back();
        };
        let Some(task) = hook.current_task(cpu) else {
            return self.fall_back();
        };
        // Register the bound the wait loop is polling against, so the timed
        // sweep can release this park even if the line never fires at all.
        // A caller that asked for no deadline passes the maximum, which the
        // wait queue already reads as "never released by timeout", so the two
        // conventions need no translation.
        //
        // Register before the re-test so a fire in the poll→park window is
        // never lost (see the module docs), then re-test: if the line
        // already fired, skip the park and let the loop's next poll
        // consume the flag.
        IRQ_WAITQ.register(task, deadline_ns);
        if self.table.ready_for(self.handle) {
            IRQ_WAITQ.deregister(task);
            return Ok(());
        }
        // Arm the one-shot at the nearest pending deadline across every
        // timed queue so a bounded wait fires even on an otherwise-idle
        // CPU, then park off the run queue until `irq_wake` or the sweep.
        hook.set_wakeup(nearest_timed_deadline());
        if !reschedule_current(cpu, RescheduleAction::Park) {
            // Not a parkable context (the boot flow before the dispatch
            // loop runs its first task, or a host test with no live
            // dispatch loop): take the port's bounded CPU park instead of
            // parking into the void.
            IRQ_WAITQ.deregister(task);
            return self.fall_back();
        }
        IRQ_WAITQ.deregister(task);
        // Re-point the one-shot at the nearest deadline any remaining timed
        // waiter needs (or clear it) so this finished park leaves no stale
        // arming behind.
        hook.set_wakeup(nearest_timed_deadline());
        Ok(())
    }
}

impl IrqParkWaiter {
    fn fall_back(&self) -> Result<(), IrqWaitAbort> {
        let park = self.fallback_park.ok_or(IrqWaitAbort::Unparkable)?;
        park(self.table, self.handle);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tairix_kernel_irq::{MaskError, MonotonicClock};

    use crate::test_boot;
    use crate::test_sink::{with_log_level, TestSink};

    /// Permissive controller so `IrqTable::fire` can mask and the waiter
    /// can re-arm without an architecture port.
    struct OkController;
    impl IrqController for OkController {
        fn mask(&self, _line: u32) -> Result<(), MaskError> {
            Ok(())
        }
    }
    static CONTROLLER: OkController = OkController;

    /// Fallback that advances this test's own wait clock, so a bounded wait
    /// with a never-firing line reaches its deadline instead of looping.
    fn ticking_fallback(_table: &IrqTable, _handle: IrqHandle) {
        test_boot::advance_clock(100);
    }

    /// Fallback that raises the line itself, standing in for a completion
    /// arriving while the caller is parked.
    fn firing_fallback(table: &IrqTable, _handle: IrqHandle) {
        table.fire(LINE, &OkController).expect("fires");
    }

    /// Fallback proving a wait never parks at all.
    fn no_park(_table: &IrqTable, _handle: IrqHandle) {
        panic!("this wait must not park at all");
    }

    /// Fallback that raises the line, then lets its wake take `LATE_WAKE_NS`
    /// to reach the waiter.
    fn late_firing_fallback(table: &IrqTable, _handle: IrqHandle) {
        test_boot::advance_clock(1_000);
        table.fire(LINE, &OkController).expect("fires");
        test_boot::advance_clock(LATE_WAKE_NS);
    }

    /// Fallback for a line that stays silent past `LATE_WAKE_NS`, then fires
    /// and is woken at once.
    fn idle_then_firing_fallback(table: &IrqTable, _handle: IrqHandle) {
        test_boot::advance_clock(2 * LATE_WAKE_NS);
        table.fire(LINE, &OkController).expect("fires");
    }

    /// Fallback for a completion that comes only once it is overdue: each
    /// park passes `SLOW_DEVICE_NS` of silence, and the first park
    /// after the checkpoint ends with the line firing.
    fn overdue_then_firing_fallback(table: &IrqTable, _handle: IrqHandle) {
        let before = wait_arch().map_or(0, crate::waitq::WaitQueueArch::now_ns);
        test_boot::advance_clock(SLOW_DEVICE_NS);
        if before >= SLOW_DEVICE_NS {
            table.fire(LINE, &OkController).expect("fires");
        }
    }

    /// Fallback for a line that never fires, each park passing a whole
    /// overdue checkpoint of silence.
    fn silent_overdue_fallback(_table: &IrqTable, _handle: IrqHandle) {
        test_boot::advance_clock(SLOW_DEVICE_NS);
    }

    /// A controller that reads `LINE` back raised and unmasked but never
    /// taken, routed to `target`.
    struct ReadBack {
        target: u32,
    }

    impl IrqController for ReadBack {
        fn mask(&self, _line: u32) -> Result<(), MaskError> {
            Ok(())
        }

        fn line_state(&self, line: u32) -> Option<LineState> {
            (line == LINE).then_some(LineState {
                pending: true,
                active: false,
                enabled: true,
                target: Some(self.target),
            })
        }
    }

    /// The wait clock, which boot installs as the table's.
    struct WaitClock;
    impl MonotonicClock for WaitClock {
        fn now_ns(&self) -> u64 {
            wait_arch().map_or(0, crate::waitq::WaitQueueArch::now_ns)
        }
    }
    static WAIT_CLOCK: WaitClock = WaitClock;

    const LINE: u32 = 9;
    const OWNER: ProcessId = ProcessId(31);

    /// A fresh table with `LINE` bound, and a waiter over it driving
    /// `fallback`.
    ///
    /// The test claims its own wait hook, so `yield_now` runs the whole
    /// register → re-test → suspend interlock; the claimed CPU has no
    /// per-CPU state slot, so the suspend then fails closed there — the
    /// not-parkable answer a host test with no dispatch loop must get — and
    /// the fallback is reached deterministically.
    fn bound_waiter(fallback: FallbackPark) -> (&'static IrqTable, IrqHandle, IrqParkWaiter) {
        let (table, handle, waiter, _) = recorded_waiter(fallback);
        (table, handle, waiter)
    }

    /// As [`bound_waiter`], the table on the wait clock as at boot, with the
    /// sink the waiter records through.
    fn recorded_waiter(
        fallback: FallbackPark,
    ) -> (
        &'static IrqTable,
        IrqHandle,
        IrqParkWaiter,
        &'static TestSink,
    ) {
        recorded_waiter_on(&CONTROLLER, fallback)
    }

    /// As [`recorded_waiter`], the line behind `controller`.
    fn recorded_waiter_on(
        controller: &'static (dyn IrqController + Sync),
        fallback: FallbackPark,
    ) -> (
        &'static IrqTable,
        IrqHandle,
        IrqParkWaiter,
        &'static TestSink,
    ) {
        let _ = test_boot::claim_scheduler();
        let table: &'static IrqTable = crate::forever(alloc::boxed::Box::new(IrqTable::new(31)));
        table.set_clock(&WAIT_CLOCK).expect("a fresh table");
        let out = table.bind(LINE, OWNER).expect("binds");
        let sink: &'static TestSink = crate::forever(alloc::boxed::Box::new(TestSink::new()));
        let waiter = IrqParkWaiter::new(table, out.handle, OWNER, controller, Some(fallback), sink);
        (table, out.handle, waiter, sink)
    }

    #[test]
    fn a_pre_fired_line_is_consumed_without_any_park() {
        let (table, handle, waiter) = bound_waiter(no_park);
        table.fire(LINE, &OkController).expect("fires");
        assert_eq!(waiter.await_event(1_000_000), WaitOutcome::Ready);
        // The fire was consumed: the flag is clear for the next wait.
        assert!(!table.ready_for(handle), "ready flag must be consumed");
    }

    #[test]
    fn a_silent_line_times_out_instead_of_waiting_forever() {
        let (_table, _handle, waiter) = bound_waiter(ticking_fallback);
        // 250 ns budget, 100 ns per fallback tick: the deadline is reached
        // after a bounded number of parks — the fail-closed outcome a dead
        // controller must produce.
        assert_eq!(waiter.await_event(250), WaitOutcome::TimedOut);
    }

    #[test]
    fn a_fire_during_the_wait_wakes_and_consumes() {
        // The consuming re-poll runs after each fallback park.
        let (table, handle, waiter) = bound_waiter(firing_fallback);
        assert_eq!(waiter.await_event(u64::MAX), WaitOutcome::Ready);
        assert!(!table.ready_for(handle), "ready flag must be consumed");
    }

    #[test]
    fn a_waiter_with_no_fallback_ends_a_wait_it_cannot_park() {
        let _ = test_boot::claim_scheduler();
        let table: &'static IrqTable = crate::forever(alloc::boxed::Box::new(IrqTable::new(31)));
        let out = table.bind(LINE, OWNER).expect("binds");
        let waiter = IrqParkWaiter::new(
            table,
            out.handle,
            OWNER,
            &CONTROLLER,
            None,
            crate::forever(alloc::boxed::Box::new(TestSink::new())),
        );
        assert_eq!(
            waiter.await_event(u64::MAX),
            WaitOutcome::Aborted(IrqWaitAbort::Unparkable),
            "never a spin"
        );
    }

    #[test]
    fn a_fire_whose_wake_reaches_the_waiter_late_is_recorded() {
        let (_table, _handle, waiter, sink) = recorded_waiter(late_firing_fallback);
        with_log_level(Level::Trace, || {
            assert_eq!(waiter.await_event(u64::MAX), WaitOutcome::Ready);
        });
        let events = sink.snapshot();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, AuditEvent::IrqWakeLate.id());
        assert_eq!(events[0].level, Level::Warn);
        assert_eq!(
            events[0].fields[..2],
            [
                ("line".into(), "9".into()),
                ("late_ms".into(), "250".into()),
            ]
        );
    }

    #[test]
    fn a_prompt_wake_or_a_fire_before_the_wait_is_not_recorded() {
        let (_table, _handle, waiter, sink) = recorded_waiter(firing_fallback);
        with_log_level(Level::Trace, || {
            assert_eq!(waiter.await_event(u64::MAX), WaitOutcome::Ready);
        });
        let (table, _handle, waiter, early) = recorded_waiter(no_park);
        table.fire(LINE, &OkController).expect("fires");
        test_boot::advance_clock(LATE_WAKE_NS);
        with_log_level(Level::Trace, || {
            assert_eq!(waiter.await_event(u64::MAX), WaitOutcome::Ready);
        });
        assert!(sink.snapshot().is_empty(), "a prompt wake");
        assert!(early.snapshot().is_empty(), "a fire the wait found waiting");
    }

    #[test]
    fn a_long_idle_wait_woken_promptly_is_not_recorded() {
        let (_table, _handle, waiter, sink) = recorded_waiter(idle_then_firing_fallback);
        with_log_level(Level::Trace, || {
            assert_eq!(waiter.await_event(u64::MAX), WaitOutcome::Ready);
        });
        assert!(sink.snapshot().is_empty(), "a long wait is not a late wake");
    }

    /// The record says where the interrupt stands — here raised and unmasked
    /// yet untaken while its target idles — and the wait still completes.
    #[test]
    fn an_overdue_completion_is_recorded_with_its_line_and_still_waited_for() {
        let cpu = test_boot::claim_cpu();
        crate::watchdog::set_activity(cpu, crate::watchdog::WatchdogActivity::Idle);
        let controller = crate::forever(alloc::boxed::Box::new(ReadBack { target: cpu }));
        let (_table, _handle, waiter, sink) =
            recorded_waiter_on(controller, overdue_then_firing_fallback);
        with_log_level(Level::Trace, || {
            assert_eq!(
                waiter.await_completion(8 * SLOW_DEVICE_NS),
                WaitOutcome::Ready
            );
        });
        let events = sink.snapshot();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].id, AuditEvent::IrqCompletionOverdue.id());
        assert_eq!(events[0].level, Level::Warn);
        let expected = [
            ("line", "9".into()),
            ("overdue_ms", "250".into()),
            ("pending", "true".into()),
            ("active", "false".into()),
            ("enabled", "true".into()),
            ("target", alloc::format!("{cpu}")),
            ("target_activity", "idle".into()),
        ]
        .map(|(key, value): (&str, alloc::string::String)| (key.into(), value));
        assert_eq!(events[0].fields, expected);
    }

    /// A controller that cannot read its line back leaves those fields null
    /// rather than guessed, and a silent line still times out at the budget.
    #[test]
    fn an_overdue_completion_on_an_unreadable_controller_names_nothing_it_cannot_see() {
        let (_table, _handle, waiter, sink) = recorded_waiter(silent_overdue_fallback);
        with_log_level(Level::Trace, || {
            assert_eq!(
                waiter.await_completion(2 * SLOW_DEVICE_NS),
                WaitOutcome::TimedOut
            );
        });
        let events = sink.snapshot();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].id, AuditEvent::IrqCompletionOverdue.id());
        let unread = ["pending", "active", "enabled", "target", "target_activity"];
        for (key, value) in &events[0].fields {
            if unread.contains(&key.as_str()) {
                assert_eq!(value, "null", "{key}");
            }
        }
    }

    #[test]
    fn a_completion_within_the_checkpoint_or_a_budget_inside_it_is_not_recorded() {
        let (_table, _handle, waiter, sink) = recorded_waiter(firing_fallback);
        with_log_level(Level::Trace, || {
            assert_eq!(
                waiter.await_completion(8 * SLOW_DEVICE_NS),
                WaitOutcome::Ready
            );
        });
        let (_table, _handle, short, inside) = recorded_waiter(ticking_fallback);
        with_log_level(Level::Trace, || {
            assert_eq!(short.await_completion(250), WaitOutcome::TimedOut);
        });
        assert!(sink.snapshot().is_empty(), "a prompt completion");
        assert!(
            inside.snapshot().is_empty(),
            "a budget inside the checkpoint"
        );
    }

    #[test]
    fn a_released_binding_fails_closed_as_not_found() {
        let (table, _handle, waiter) = bound_waiter(no_park);
        table.release_for(OWNER, &CONTROLLER);
        assert_eq!(waiter.await_event(1_000), WaitOutcome::NotFound);
    }
}
