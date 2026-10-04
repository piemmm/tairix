//! Bus mastering follows ownership: a function masters DMA only while an
//! owner may reach memory through it (`plans/IOMMU.md` IOM7).
//!
//! Only the owner of a function's configuration space can turn its Bus
//! Master Enable on or off. Where that owner is the kernel, the port hands
//! the core a [`BusMastering`]; the core decides when, and records each
//! change.

pub use tairix_abi::driver::pci::Quiesced;
use tairix_abi::IommuStreams;
use tairix_log::{Field, FieldValue, Level, Sink};

use crate::audit::{emit, AuditEvent};

/// The functions a mastering change names.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MasterTarget<'a> {
    /// The function an untranslated owner's node describes.
    Node(u32),
    /// Every handed-over function a unit knows by one of these streams as its
    /// own: a translated owner's requester streams, so a bus-published child
    /// of a device masters through its parent's function. An alias names no
    /// function and is never one of them.
    Streams(&'a [IommuStreams]),
}

/// The owner a mastering change is made for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MasterOwner {
    /// The node it owns.
    pub node: u32,
    /// Its admission generation, which the change's record names.
    pub generation: u64,
    /// When it began ([`Mastering::begin`]).
    pub epoch: u64,
}

/// What a mastering change did to the functions it named.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct MasterChange {
    /// A function's Bus Master Enable was written.
    pub changed: bool,
    /// A function still reports the other state afterwards.
    pub refused: bool,
}

/// The owner of functions' configuration space, which alone turns their bus
/// mastering on and off.
pub trait BusMastering: Sync {
    /// An epoch later than every one handed out before. Owners take one as
    /// they begin, so their changes are ordered by when each began, whatever
    /// generation it was admitted with.
    fn begin(&self) -> u64;

    /// Let the functions `target` names master DMA, or stop them, for an
    /// owner that began at `epoch`, writing only a bit that changes and
    /// reading each back, and hand `report` what it did before another change
    /// to them can land. A function an owner that began later has changed is
    /// left as it is, so an owner that ends late cannot stop its successor.
    /// Nothing is reported where the port owns no such function, or none still
    /// answers.
    fn set_mastering(
        &self,
        target: MasterTarget<'_>,
        master: bool,
        epoch: u64,
        report: &mut dyn FnMut(MasterChange),
    );

    /// Stop every function behind the unit at node `unit` that masters DMA
    /// though `keeps` answers that firmware keeps no window for its stream.
    fn quiesce(&self, unit: u32, keeps: &dyn Fn(u32) -> bool) -> Quiesced;
}

/// The kernel's bus-mastering authority: the port owning configuration space,
/// where that is the kernel, and the audit each change goes to.
#[derive(Copy, Clone)]
pub struct Mastering {
    port: &'static dyn BusMastering,
    audit: &'static (dyn Sink + Sync),
}

impl Mastering {
    /// Mastering through `port`, recorded to `audit`.
    #[must_use]
    pub const fn new(port: &'static dyn BusMastering, audit: &'static (dyn Sink + Sync)) -> Self {
        Self { port, audit }
    }

    /// The epoch an owner beginning now takes.
    #[must_use]
    pub fn begin(&self) -> u64 {
        self.port.begin()
    }

    /// Let the functions `target` names master DMA, or stop them, for
    /// `owner`, auditing what it did in the order the changes landed.
    pub fn set(&self, target: MasterTarget<'_>, master: bool, owner: MasterOwner) {
        self.port
            .set_mastering(target, master, owner.epoch, &mut |change| {
                record(self.audit, owner, master, change);
            });
    }

    /// [`BusMastering::quiesce`].
    #[must_use]
    pub fn quiesce(&self, unit: u32, keeps: &dyn Fn(u32) -> bool) -> Quiesced {
        self.port.quiesce(unit, keeps)
    }
}

/// Audit what turning `owner`'s functions `master` did, when it did anything.
fn record(audit: &(dyn Sink + Sync), owner: MasterOwner, master: bool, change: MasterChange) {
    if !change.changed && !change.refused {
        return;
    }
    let (level, outcome) = if change.refused {
        (Level::Warn, "refused")
    } else {
        (Level::Info, "applied")
    };
    emit(
        audit,
        level,
        AuditEvent::DmaBusMaster,
        &[
            Field {
                key: "node",
                value: FieldValue::UnsignedInt(u64::from(owner.node)),
            },
            Field {
                key: "generation",
                value: FieldValue::UnsignedInt(owner.generation),
            },
            Field {
                key: "master",
                value: FieldValue::Str(if master { "on" } else { "off" }),
            },
            Field {
                key: "outcome",
                value: FieldValue::Str(outcome),
            },
        ],
    );
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;
    use alloc::vec::Vec;

    use tairix_sync::SpinLock;

    use super::*;
    use crate::test_sink::{with_log_level, CapturedEvent, TestSink};

    /// A port answering every change with `answer`, recording each call and
    /// whether it reported while still holding its own lock.
    struct Port {
        answer: Option<MasterChange>,
        calls: SpinLock<Vec<(bool, u64)>>,
        held: SpinLock<()>,
        reported_held: SpinLock<Vec<bool>>,
    }

    impl BusMastering for Port {
        fn begin(&self) -> u64 {
            7
        }

        fn set_mastering(
            &self,
            target: MasterTarget<'_>,
            master: bool,
            epoch: u64,
            report: &mut dyn FnMut(MasterChange),
        ) {
            assert_eq!(target, MasterTarget::Node(7));
            let held = self.held.lock();
            self.calls.lock().push((master, epoch));
            if let Some(answer) = self.answer {
                report(answer);
                self.reported_held.lock().push(self.held.is_locked());
            }
            drop(held);
        }

        fn quiesce(&self, unit: u32, keeps: &dyn Fn(u32) -> bool) -> Quiesced {
            Quiesced {
                stopped: usize::from(unit == 9 && !keeps(4)),
                refused: 0,
            }
        }
    }

    fn port(answer: Option<MasterChange>) -> &'static Port {
        Box::leak(Box::new(Port {
            answer,
            calls: SpinLock::new(Vec::new()),
            held: SpinLock::new(()),
            reported_held: SpinLock::new(Vec::new()),
        }))
    }

    fn sink() -> &'static TestSink {
        Box::leak(Box::new(TestSink::new()))
    }

    fn field<'a>(event: &'a CapturedEvent, key: &str) -> Option<&'a str> {
        event
            .fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    const APPLIED: MasterChange = MasterChange {
        changed: true,
        refused: false,
    };

    const OWNER: MasterOwner = MasterOwner {
        node: 7,
        generation: 3,
        epoch: 11,
    };

    #[test]
    fn a_change_carries_its_owner_s_epoch_and_is_recorded_under_the_port_s_lock() {
        let port = port(Some(APPLIED));
        let audit = sink();
        let mastering = Mastering::new(port, audit);
        assert_eq!(mastering.begin(), 7);
        with_log_level(Level::Info, || {
            mastering.set(MasterTarget::Node(7), true, OWNER);
            mastering.set(MasterTarget::Node(7), false, OWNER);
        });
        assert_eq!(*port.calls.lock(), [(true, 11), (false, 11)]);
        assert_eq!(
            *port.reported_held.lock(),
            [true, true],
            "reported before another change could land"
        );
        let events = audit.snapshot();
        assert_eq!(events.len(), 2);
        assert!(events
            .iter()
            .all(|event| event.id == AuditEvent::DmaBusMaster.id() && event.level == Level::Info));
        assert_eq!(field(&events[0], "master"), Some("on"));
        assert_eq!(field(&events[1], "master"), Some("off"));
        assert_eq!(field(&events[0], "outcome"), Some("applied"));
        assert_eq!(field(&events[0], "node"), Some("7"));
        assert_eq!(field(&events[0], "generation"), Some("3"));
    }

    #[test]
    fn a_change_that_changed_nothing_is_not_recorded() {
        let audit = sink();
        with_log_level(Level::Info, || {
            Mastering::new(port(Some(MasterChange::default())), audit).set(
                MasterTarget::Node(7),
                true,
                OWNER,
            );
            Mastering::new(port(None), audit).set(MasterTarget::Node(7), false, OWNER);
        });
        assert!(audit.snapshot().is_empty());
    }

    #[test]
    fn a_function_that_kept_its_state_is_recorded_as_refused() {
        let audit = sink();
        let refused = MasterChange {
            changed: true,
            refused: true,
        };
        with_log_level(Level::Info, || {
            Mastering::new(port(Some(refused)), audit).set(MasterTarget::Node(7), true, OWNER);
        });
        let events = audit.snapshot();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].level, Level::Warn);
        assert_eq!(field(&events[0], "outcome"), Some("refused"));
    }

    #[test]
    fn quiesce_asks_the_port_with_the_firmware_test() {
        let mastering = Mastering::new(port(None), sink());
        assert_eq!(mastering.quiesce(9, &|_| false).stopped, 1);
        assert_eq!(mastering.quiesce(9, &|stream| stream == 4).stopped, 0);
    }
}
