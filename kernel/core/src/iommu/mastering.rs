//! Bus mastering follows ownership: a function masters DMA only while an
//! owner may reach memory through it (`plans/IOMMU.md` IOM7).
//!
//! Only the owner of a function's configuration space can turn its Bus
//! Master Enable on or off. Where that owner is the kernel, the port hands
//! the core a [`BusMastering`]; the core decides when, and records each
//! change.

use tairix_abi::IommuStreams;
use tairix_log::{Field, FieldValue, Level, Sink};

use crate::audit::{emit, AuditEvent};

/// The functions a mastering change names.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MasterTarget {
    /// The function an untranslated owner's node describes.
    Node(u32),
    /// Every handed-over function a unit knows by one of these streams: what
    /// a translated owner's domain holds, so a bus-published child of a
    /// device masters through its parent's function.
    Streams(IommuStreams),
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
    /// Let the functions `target` names master DMA, or stop them, for the
    /// owner admitted as `generation`, writing only a bit that changes and
    /// reading each back. A function a later owner has changed is left as it
    /// is: owners are admitted in generation order, so one that ends late
    /// cannot stop its successor. [`None`] where this port owns no such
    /// function, or none still answers.
    fn set_mastering(
        &self,
        target: MasterTarget,
        master: bool,
        generation: u64,
    ) -> Option<MasterChange>;

    /// How many functions behind the unit at node `unit` master DMA though
    /// `keeps` answers that firmware keeps no window for their stream.
    fn strays(&self, unit: u32, keeps: &dyn Fn(u32) -> bool) -> usize;
}

/// The kernel's bus-mastering authority: the port owning configuration
/// space, where that is the kernel, and the audit each change goes to.
#[derive(Copy, Clone)]
pub struct Mastering {
    port: Option<&'static dyn BusMastering>,
    audit: &'static (dyn Sink + Sync),
}

impl Mastering {
    /// Mastering through `port`, recorded to `audit`. With no port the kernel
    /// owns no configuration space, and every change is a no-op.
    #[must_use]
    pub const fn new(
        port: Option<&'static dyn BusMastering>,
        audit: &'static (dyn Sink + Sync),
    ) -> Self {
        Self { port, audit }
    }

    /// Let the functions `target` names master DMA for the owner admitted as
    /// `generation`. Hand what it did to [`record`](Self::record) before
    /// releasing the lock that orders `target`'s changes, so its records keep
    /// that order.
    #[must_use]
    pub fn grant(&self, target: MasterTarget, generation: u64) -> Option<MasterChange> {
        self.port?.set_mastering(target, true, generation)
    }

    /// Stop the functions `target` names mastering DMA as the owner admitted
    /// as `generation` ends. Hand what it did to [`record`](Self::record)
    /// before releasing the lock that orders `target`'s changes, so its
    /// records keep that order.
    #[must_use]
    pub fn withdraw(&self, target: MasterTarget, generation: u64) -> Option<MasterChange> {
        self.port?.set_mastering(target, false, generation)
    }

    /// Audit what turning `node`'s functions `master` did, when it did
    /// anything.
    pub fn record(&self, node: u32, master: bool, change: Option<MasterChange>) {
        let Some(change) = change.filter(|change| change.changed || change.refused) else {
            return;
        };
        let (level, outcome) = if change.refused {
            (Level::Warn, "refused")
        } else {
            (Level::Info, "applied")
        };
        emit(
            self.audit,
            level,
            AuditEvent::DmaBusMaster,
            &[
                Field {
                    key: "node",
                    value: FieldValue::UnsignedInt(u64::from(node)),
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

    /// Hand the function untranslated `node` describes to its owner admitted
    /// as `generation`: it may master DMA from now on.
    pub fn hand_over(&self, node: u32, generation: u64) {
        self.record(node, true, self.grant(MasterTarget::Node(node), generation));
    }

    /// Take the function untranslated `node` describes back from its owner
    /// admitted as `generation`, which ended.
    pub fn take_back(&self, node: u32, generation: u64) {
        self.record(
            node,
            false,
            self.withdraw(MasterTarget::Node(node), generation),
        );
    }

    /// [`BusMastering::strays`], or none where the kernel owns no
    /// configuration space.
    #[must_use]
    pub fn strays(&self, unit: u32, keeps: &dyn Fn(u32) -> bool) -> usize {
        self.port.map_or(0, |port| port.strays(unit, keeps))
    }
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;
    use alloc::vec::Vec;

    use tairix_sync::SpinLock;

    use super::*;
    use crate::test_sink::{with_log_level, CapturedEvent, TestSink};

    /// A port answering every change with `answer`, recording each call.
    struct Port {
        answer: Option<MasterChange>,
        calls: SpinLock<Vec<(MasterTarget, bool, u64)>>,
    }

    impl BusMastering for Port {
        fn set_mastering(
            &self,
            target: MasterTarget,
            master: bool,
            generation: u64,
        ) -> Option<MasterChange> {
            self.calls.lock().push((target, master, generation));
            self.answer
        }

        fn strays(&self, unit: u32, keeps: &dyn Fn(u32) -> bool) -> usize {
            usize::from(unit == 9 && !keeps(4))
        }
    }

    fn port(answer: Option<MasterChange>) -> &'static Port {
        Box::leak(Box::new(Port {
            answer,
            calls: SpinLock::new(Vec::new()),
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

    #[test]
    fn an_untranslated_hand_over_and_take_back_name_the_node() {
        let port = port(Some(APPLIED));
        let audit = sink();
        let mastering = Mastering::new(Some(port), audit);
        with_log_level(Level::Info, || {
            mastering.hand_over(7, 3);
            mastering.take_back(7, 3);
        });
        assert_eq!(
            *port.calls.lock(),
            [
                (MasterTarget::Node(7), true, 3),
                (MasterTarget::Node(7), false, 3)
            ]
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
    }

    #[test]
    fn a_change_that_changed_nothing_is_not_recorded() {
        let audit = sink();
        with_log_level(Level::Info, || {
            Mastering::new(Some(port(Some(MasterChange::default()))), audit).hand_over(7, 3);
            Mastering::new(Some(port(None)), audit).take_back(7, 3);
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
            Mastering::new(Some(port(Some(refused))), audit).hand_over(7, 3);
        });
        let events = audit.snapshot();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].level, Level::Warn);
        assert_eq!(field(&events[0], "outcome"), Some("refused"));
    }

    #[test]
    fn with_no_port_nothing_masters_and_nothing_strays() {
        let audit = sink();
        let mastering = Mastering::new(None, audit);
        assert_eq!(mastering.grant(MasterTarget::Node(7), 3), None);
        with_log_level(Level::Info, || mastering.take_back(7, 3));
        assert_eq!(mastering.strays(9, &|_| false), 0);
        assert!(audit.snapshot().is_empty());
    }

    #[test]
    fn strays_ask_the_port_with_the_firmware_test() {
        let mastering = Mastering::new(Some(port(None)), sink());
        assert_eq!(mastering.strays(9, &|_| false), 1);
        assert_eq!(mastering.strays(9, &|stream| stream == 4), 0);
    }
}
