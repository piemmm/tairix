//! The fault service: one task per unit, woken by the unit's fault interrupt,
//! that drains the unit's records, records each under the unit's budget, and
//! silences a stream that storms.

use alloc::boxed::Box;

use tairix_abi::blkio::FaultDomainState;
use tairix_abi::{IrqHandle, MsiAllocation};
use tairix_inline::ArrayVec;
use tairix_kernel_iommu_api::{
    Clock, Fault, FaultBudget, FaultLimits, FaultReason, FaultRoute, FaultVerdict, IommuUnit,
    FAULT_QUEUE_RECORDS,
};
use tairix_kernel_irq::{IrqController, IrqTable, WaitOutcome};
use tairix_kernel_sched_api::TaskId;
use tairix_kernel_sec::ProcessId;
use tairix_log::{Field, FieldValue, Level, Sink};

use crate::audit::{emit, AuditEvent};
use crate::blockwait::IrqParkWaiter;
use crate::devres::KernelMsiFacility;
use crate::kthread::{KernelServiceBody, YieldHandle};

use super::{FaultSignal, Translation};

/// The kernel identity every unit's fault interrupt is bound to. It sits below
/// the task-id draw, so no process is given it and no process's exit releases
/// the binding.
pub const FAULT_OWNER: ProcessId = ProcessId(0x5b6);

const _: () = assert!(
    FAULT_OWNER.0 < tairix_kernel_sched_api::FIRST_DRAWN_TASK_ID,
    "a kernel service identity sits below the task-id draw"
);

/// What one unit's faults may cost.
///
/// A storm sits above the most records any family lets a unit hold, so the
/// records one drain finds waiting — an earlier owner's, or a backlog the
/// drain was late to — cannot storm a stream alone; the drain count bounds the
/// work, and the fault interrupts, one storming unit can cause.
pub const FAULT_LIMITS: FaultLimits = FaultLimits {
    window_ns: 1_000_000_000,
    stream_records: 4,
    unit_records: 32,
    storm: 2 * FAULT_QUEUE_RECORDS,
    drains: 1024,
    streams: 1024,
};

const _: () = assert!(
    FAULT_LIMITS.storm > FAULT_QUEUE_RECORDS,
    "a storm takes more than one queue's worth of records"
);

/// What serving the units' faults needs from the kernel.
pub struct FaultEnv {
    /// The table each unit's fault interrupt line is bound in.
    pub table: &'static IrqTable,
    /// The controller that line is re-armed through.
    pub controller: &'static (dyn IrqController + Sync),
    /// Where a unit raising its faults by message takes its vector, where the
    /// port has one to give.
    pub msi: Option<&'static dyn KernelMsiFacility>,
    /// Where faults are recorded.
    pub audit: &'static (dyn Sink + Sync),
    /// The clock the budget's windows run on.
    pub clock: &'static dyn Clock,
}

/// What a unit's fault interrupt holds while it is set up or served: its
/// line's binding and, raised by message, its vector.
#[derive(Clone, Copy)]
struct FaultInterrupt {
    table: &'static IrqTable,
    controller: &'static (dyn IrqController + Sync),
    handle: Option<IrqHandle>,
    vector: Option<(&'static dyn KernelMsiFacility, MsiAllocation)>,
}

impl FaultInterrupt {
    /// Give back what is held of `unit`'s interrupt, the unit told to raise
    /// it where `told`: its vector then only once the unit stops, as the
    /// vector's next owner would hear the unit.
    fn give_back(&self, unit: &dyn IommuUnit, told: bool) {
        let quiet = !told || unit.unroute_faults().is_ok();
        if let Some(handle) = self.handle {
            self.table
                .release_binding(handle, FAULT_OWNER, self.controller);
        }
        if let (true, Some((msi, vector))) = (quiet, self.vector) {
            msi.release(&vector);
        }
    }
}

impl Translation {
    /// Route each unit's fault interrupt, and admit through `admit` the task
    /// that drains it. A unit whose faults cannot be served still translates;
    /// the reason is audited.
    pub fn serve_faults(
        &'static self,
        env: &FaultEnv,
        mut admit: impl FnMut(KernelServiceBody) -> Option<TaskId>,
    ) {
        for index in 0..self.units.len() {
            if let Err(reason) = self.serve_unit(index, env, &mut admit) {
                self.unrouted(index, env.audit, reason);
            }
        }
    }

    fn serve_unit(
        &'static self,
        index: usize,
        env: &FaultEnv,
        admit: &mut impl FnMut(KernelServiceBody) -> Option<TaskId>,
    ) -> Result<(), &'static str> {
        let mut budget =
            FaultBudget::new(FAULT_LIMITS, env.clock.now_ns()).map_err(|_| "exhausted")?;
        let unit = self.units[index].unit;
        let (line, route, trigger, vector) = match self.units[index].faults {
            FaultSignal::Wired(wired) => (
                wired.line,
                FaultRoute::Wired { place: wired.place },
                Some(wired.trigger),
                None,
            ),
            FaultSignal::Message => {
                let msi = env.msi.ok_or("no_vector")?;
                let vector = msi.allocate().map_err(|_| "no_vector")?;
                let route = FaultRoute::Message {
                    address: vector.address,
                    data: vector.data,
                };
                (vector.line, route, None, Some((msi, vector)))
            }
            FaultSignal::Unheard => return Err("no_line"),
        };
        let mut held = FaultInterrupt {
            table: env.table,
            controller: env.controller,
            handle: None,
            vector,
        };
        let refuse = |held: &FaultInterrupt, told, reason| {
            held.give_back(unit, told);
            reason
        };
        let bound = env
            .table
            .bind_exclusive(line, FAULT_OWNER)
            .map_err(|_| refuse(&held, false, "unbound"))?;
        held.handle = Some(bound.handle);
        // Only once the line is the kernel's alone, so no other owner's
        // trigger is changed under it.
        if let Some(trigger) = trigger {
            env.controller
                .activate(line)
                .map_err(|_| refuse(&held, false, "unactivated"))?;
            env.controller
                .set_trigger(line, trigger)
                .map_err(|_| refuse(&held, false, "untriggerable"))?;
        }
        // A dispatched task always parks, so the waiter needs no CPU halt.
        let waiter = IrqParkWaiter::new(env.table, bound.handle, FAULT_OWNER, env.controller, None);
        // A refused route may have reached the unit in part.
        unit.route_faults(route)
            .map_err(|_| refuse(&held, true, "refused"))?;
        let (audit, clock) = (env.audit, env.clock);
        // Taken once, so a vector is never given back twice.
        let mut serving = Some(held);
        let body: KernelServiceBody = Box::new(move |_: &mut dyn YieldHandle| {
            self.serve(index, &mut budget, &waiter, audit, clock);
            if let Some(held) = serving.take() {
                held.give_back(unit, true);
            }
        });
        // Before the task can run, so its own stop is the last word.
        self.units[index].counts.note_heard(true);
        admit(body)
            .map(|_| ())
            .ok_or_else(|| refuse(&held, true, "not_admitted"))
    }

    /// Drain unit `index` each time its fault interrupt fires, and whenever a
    /// drain says records remain; a window whose drains are spent is slept
    /// out with the line masked. Returns only when the interrupt can no
    /// longer be waited on.
    fn serve(
        &self,
        index: usize,
        budget: &mut FaultBudget,
        waiter: &IrqParkWaiter,
        audit: &(dyn Sink + Sync),
        clock: &dyn Clock,
    ) {
        loop {
            if let Err(resume_ns) = budget.drain(clock.now_ns()) {
                // The line stays masked until the window turns: a level line
                // the unit still asserts would fire straight back.
                if crate::sleep::park_until(resume_ns).is_err() {
                    return self.unrouted(index, audit, "unparkable");
                }
                continue;
            }
            if self.drain_pass(index, budget, audit, clock) {
                let _ = crate::preempt::yield_if_owed();
            } else if let Err(reason) = wait(waiter, u64::MAX) {
                return self.unrouted(index, audit, reason);
            }
        }
    }

    /// Take one drain of unit `index`'s records, charging each to `budget`,
    /// and answer whether records remain.
    pub(super) fn drain_pass(
        &self,
        index: usize,
        budget: &mut FaultBudget,
        audit: &(dyn Sink + Sync),
        clock: &dyn Clock,
    ) -> bool {
        let counts = &self.units[index].counts;
        self.units[index].unit.drain_faults(&mut |fault| {
            let charge = budget.charge(fault.stream, clock.now_ns());
            match charge.verdict {
                FaultVerdict::Suppress => counts.note_dropped(),
                FaultVerdict::Record => {
                    counts.note_recorded();
                    self.record(index, &fault, charge.suppressed, audit);
                }
                FaultVerdict::Storm { recorded } => {
                    if recorded {
                        counts.note_recorded();
                    } else {
                        counts.note_dropped();
                    }
                    self.contain(index, &fault, recorded, charge.suppressed, audit);
                }
            }
        })
    }

    fn record(&self, index: usize, fault: &Fault, suppressed: u64, audit: &(dyn Sink + Sync)) {
        let node = self.node_of(index, fault.stream);
        audit_fault(
            audit,
            AuditEvent::DmaTranslationFault,
            Level::Warn,
            &Attribution {
                unit: self.units[index].node,
                node,
                fault,
                suppressed,
            },
            None,
        );
    }

    /// Silence `fault`'s stream so its device can keep neither its DMA nor
    /// the fault path, and mark its node `Offline`, recording the storm where
    /// the budget `recorded` it, or where it could not be silenced
    /// (`suppressed`, the faults it reports went unrecorded).
    fn contain(
        &self,
        index: usize,
        fault: &Fault,
        recorded: bool,
        suppressed: u64,
        audit: &(dyn Sink + Sync),
    ) {
        let outcome = match self.units[index].unit.silence(fault.stream) {
            Ok(()) => {
                self.units[index].counts.note_silenced();
                "silenced"
            }
            Err(tairix_kernel_iommu_api::IommuError::Unconfirmed) => "unconfirmed",
            Err(tairix_kernel_iommu_api::IommuError::Exhausted) => "exhausted",
            Err(_) => "refused",
        };
        let node = self.node_of(index, fault.stream);
        if let Some(node) = node {
            let _ = self.tree.set_health(node, FaultDomainState::Offline);
        }
        // A storm left uncontained is a security event whatever the share.
        if !recorded && outcome == "silenced" {
            return;
        }
        audit_fault(
            audit,
            AuditEvent::DmaTranslationStorm,
            Level::Error,
            &Attribution {
                unit: self.units[index].node,
                node,
                fault,
                suppressed,
            },
            Some(outcome),
        );
    }
}

/// Wait for the fault interrupt, at most `timeout_ns`.
fn wait(waiter: &IrqParkWaiter, timeout_ns: u64) -> Result<(), &'static str> {
    match waiter.park_wait(timeout_ns) {
        WaitOutcome::Ready | WaitOutcome::TimedOut => Ok(()),
        WaitOutcome::NotFound => Err("unbound"),
        WaitOutcome::Quarantined => Err("quarantined"),
        WaitOutcome::Aborted(_) => Err("unparkable"),
    }
}

/// One fault, and who it is laid against: the device's node where an owner
/// holds the stream, else the unit alone.
struct Attribution<'a> {
    unit: u32,
    node: Option<u32>,
    fault: &'a Fault,
    suppressed: u64,
}

fn audit_fault(
    audit: &(dyn Sink + Sync),
    event: AuditEvent,
    level: Level,
    at: &Attribution<'_>,
    outcome: Option<&'static str>,
) {
    let (reason, code) = match at.fault.reason {
        FaultReason::Blocked => ("blocked", None),
        FaultReason::Unmapped => ("unmapped", None),
        FaultReason::Denied => ("denied", None),
        FaultReason::Translated => ("translated", None),
        FaultReason::Interrupt => ("interrupt", None),
        FaultReason::Malformed => ("malformed", None),
        FaultReason::Other(code) => ("other", Some(code)),
    };
    let field = |key, value| Field { key, value };
    let unsigned = FieldValue::UnsignedInt;
    let optional = [
        code.map(|code| field("code", unsigned(u64::from(code)))),
        at.node.map(|node| field("node", unsigned(u64::from(node)))),
        outcome.map(|outcome| field("outcome", FieldValue::Str(outcome))),
    ];
    let mut fields = ArrayVec::<Field<'_>, 9>::new();
    for item in [
        field("unit", unsigned(u64::from(at.unit))),
        field("stream", unsigned(u64::from(at.fault.stream))),
        field("iova", unsigned(at.fault.iova)),
        field(
            "access",
            FieldValue::Str(if at.fault.write { "write" } else { "read" }),
        ),
        field("reason", FieldValue::Str(reason)),
        field("suppressed", unsigned(at.suppressed)),
    ]
    .into_iter()
    .chain(optional.into_iter().flatten())
    {
        // Room for every field above, so nothing is ever dropped.
        let _ = fields.try_push(item);
    }
    emit(audit, level, event, &fields);
}

impl Translation {
    /// Unit `index`'s faults are drained by nothing from now on, for
    /// `reason`.
    fn unrouted(&self, index: usize, audit: &(dyn Sink + Sync), reason: &'static str) {
        self.units[index].counts.note_heard(false);
        unrouted(audit, self.units[index].node, reason);
    }
}

fn unrouted(audit: &(dyn Sink + Sync), unit: u32, reason: &'static str) {
    emit(
        audit,
        Level::Warn,
        AuditEvent::DmaTranslationUnit,
        &[
            Field {
                key: "node",
                value: FieldValue::UnsignedInt(u64::from(unit)),
            },
            Field {
                key: "outcome",
                value: FieldValue::Str("faults_unrouted"),
            },
            Field {
                key: "reason",
                value: FieldValue::Str(reason),
            },
        ],
    );
}
