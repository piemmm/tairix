//! What the device manager makes of devices coming and going: when the
//! hardware present at boot has settled — its readiness — and which removable
//! devices arrived or left on a running machine, each of which it cues a
//! sound for (`plans/SOUND.md` SND22).

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use tairix_abi::sound_cue::SoundEvent;
use tairix_abi::HwNode;
use tairix_log::{log, Event, Field, FieldValue, Level, Sink};

use crate::events;

/// How long one thing is waited for before the boot is taken to have
/// settled without it: well past what a healthy bus takes to bring up what
/// is plugged in, short enough that a wedged driver does not hold the
/// machine's start back for long.
pub const SETTLE_BUDGET_NS: u64 = 5_000_000_000;

/// What the device manager does about devices settling and moving.
pub trait Presence {
    /// The monotonic clock, in nanoseconds.
    fn now_ns(&mut self) -> u64;

    /// The hardware present at boot has settled: tell the service manager.
    fn settled(&mut self);

    /// Ask for the sound of a removable device arriving or leaving.
    fn cue(&mut self, event: SoundEvent);
}

/// One thing the device manager's readiness waits on.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Pending {
    /// A node whose driver has not settled, or that waits for its supplier.
    Node(u32),
    /// Device channels a service was not yet up to take.
    Channels,
}

/// The device manager's readiness: everything present at boot matched, its
/// drivers settled and its channels handed on, or waited for long enough.
#[derive(Debug, Default)]
pub struct BootSettle {
    announced: bool,
    /// Each thing still waited on, with the instant its wait lapses.
    waiting: BTreeMap<Pending, u64>,
    /// What was waited on until its wait lapsed, never waited on again.
    lapsed: BTreeSet<Pending>,
}

impl BootSettle {
    /// Fold one reaction at `now_ns`: whether the driver catalogue is in hand
    /// and what is still `pending`. Answers `true` exactly once, when nothing
    /// is waited on any longer.
    pub fn observe(
        &mut self,
        now_ns: u64,
        catalogued: bool,
        pending: impl IntoIterator<Item = Pending>,
        sink: &dyn Sink,
    ) -> bool {
        if self.announced {
            return false;
        }
        let pending: BTreeSet<Pending> = pending
            .into_iter()
            .filter(|item| !self.lapsed.contains(item))
            .collect();
        self.waiting.retain(|item, _| pending.contains(item));
        let lapses = now_ns.saturating_add(SETTLE_BUDGET_NS);
        for item in pending {
            self.waiting.entry(item).or_insert(lapses);
        }
        let due: Vec<Pending> = self
            .waiting
            .iter()
            .filter(|&(_, &at)| now_ns >= at)
            .map(|(&item, _)| item)
            .collect();
        for item in due {
            self.waiting.remove(&item);
            self.lapsed.insert(item);
            audit_lapsed(sink, item);
        }
        if !catalogued || !self.waiting.is_empty() {
            return false;
        }
        self.announced = true;
        self.lapsed = BTreeSet::new();
        log(
            sink,
            &Event {
                level: Level::Info,
                id: events::DEVICES_SETTLED,
                message: "the hardware present at boot has settled",
                fields: &[],
            },
        );
        true
    }

    /// The soonest instant a wait lapses, for the loop to wake at.
    #[must_use]
    pub fn deadline(&self) -> Option<u64> {
        self.waiting.values().copied().min()
    }
}

fn audit_lapsed(sink: &dyn Sink, item: Pending) {
    let node = match item {
        Pending::Node(id) => FieldValue::UnsignedInt(u64::from(id)),
        Pending::Channels => FieldValue::Str("channels"),
    };
    log(
        sink,
        &Event {
            level: Level::Warn,
            id: events::SETTLE_LAPSED,
            message: "a device did not settle in time; the boot settles without it",
            fields: &[Field {
                key: "node",
                value: node,
            }],
        },
    );
}

/// Where a removable device sits: the node it hangs from, and its bus address
/// there, or the node itself where its emitter named no address.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct DeviceKey {
    parent: u32,
    unit: Unit,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum Unit {
    Address(u32),
    Node(u32),
}

impl DeviceKey {
    fn of(node: &HwNode) -> Self {
        let unit = match node.address() {
            0 => Unit::Node(node.id()),
            address => Unit::Address(address),
        };
        Self {
            parent: node.parent(),
            unit,
        }
    }
}

/// The removable devices that arrived and that left between two snapshots.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Moves {
    /// Devices that arrived on a running machine.
    pub arrived: usize,
    /// Devices that left.
    pub left: usize,
}

/// The removable devices present, so each one's arrival and departure is
/// told once, however many nodes it is published as.
#[derive(Debug, Default)]
pub struct Devices {
    known: Option<BTreeSet<DeviceKey>>,
}

impl Devices {
    /// Take the snapshot `nodes`, whose ids are `present`, after the one whose
    /// ids were `before` (both ascending), answering what moved.
    ///
    /// A device counts as arriving only when its emitter had settled — what a
    /// bus finds as it comes up is the machine starting, not a device plugged
    /// in — and when its parent was already present: a device that came with
    /// its parent arrived as part of it, and one that left with its parent
    /// left as part of it. The first snapshot moves nothing.
    pub fn update(&mut self, nodes: &[HwNode], before: &[u32], present: &[u32]) -> Moves {
        let mut current: BTreeMap<DeviceKey, bool> = BTreeMap::new();
        for node in nodes.iter().filter(|node| node.is_removable()) {
            *current.entry(DeviceKey::of(node)).or_default() |= node.is_hot_arrival();
        }
        let Some(known) = self.known.replace(current.keys().copied().collect()) else {
            return Moves::default();
        };
        let lived = |ids: &[u32], parent: u32| ids.binary_search(&parent).is_ok();
        Moves {
            arrived: current
                .iter()
                .filter(|&(key, &hot)| hot && !known.contains(key) && lived(before, key.parent))
                .count(),
            left: known
                .iter()
                .filter(|key| !current.contains_key(key) && lived(present, key.parent))
                .count(),
        }
    }
}

#[cfg(test)]
#[path = "presence_tests.rs"]
mod tests;
