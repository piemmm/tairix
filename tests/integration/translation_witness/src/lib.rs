//! The witness a DMA-translation vertical judges its run by (`plans/IOMMU.md`).
//!
//! The run passes once `InputDelivered` `kind=key` follows `DmaTranslationUnit`
//! `outcome=translating` keeping its translations where the run asks (the
//! stage its tables are walked at, or the unit's own) with `stopped=0` and
//! `refused=0`, the fault routing the run expects, the interrupt routing the
//! board's unit leads to, and the keyboard's `DmaBusMaster` `master=on`
//! `outcome=applied`. Any other unit outcome or stage, a `faults_unrouted`
//! the run does not expect (or one it does that never comes), a routing record
//! the board should not write, a translation fault, a grant before the unit
//! translates (and, where routing is recorded, before it is), or a key before
//! the keyboard was granted fails it.
//!
//! The keyboard is the lowest-numbered virtio-input node
//! ([`first_input_node`]), read once the unit reports translating, when the
//! tree is whole.
//!
//! Test scaffolding: nothing in TAIRiX itself links it.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::num::NonZeroU16;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use tairix_abi::{HwNode, HwResourceKind};
use tairix_itest_finisher::fail_point;
use tairix_itest_witness::field;
use tairix_kernel_core::{AuditEvent, HwTreeSource};
pub use tairix_kernel_iommu_api::{Stage, Tables};
use tairix_log::{Event, FieldValue};

/// How the board's device interrupts reach a CPU.
#[derive(Copy, Clone)]
pub enum Interrupts {
    /// Through the unit's remapping, which must report `remapped` before any
    /// device is granted, with `extended` saying the CPUs take the mode the
    /// remapping runs in.
    Remapped {
        /// Whether the interrupt controller runs in the mode remapping needs.
        extended: fn() -> bool,
    },
    /// As messages no unit can remap, the board's unit having no remapping:
    /// the routing must report `unremapped` before any device is granted.
    Unremapped,
    /// On wired lines no unit remaps: no remapping record may appear.
    Wired,
}

impl Interrupts {
    /// The routing outcome the board records, where it records one.
    const fn routed(self) -> Option<&'static str> {
        match self {
            Self::Remapped { .. } => Some("remapped"),
            Self::Unremapped => Some("unremapped"),
            Self::Wired => None,
        }
    }
}

/// Where the unit's faults reach the kernel.
#[derive(Copy, Clone, Eq, PartialEq)]
pub enum Faults {
    /// On a line or message the kernel serves.
    Served,
    /// Nowhere the platform describes: once the unit translates it must say
    /// its faults have no line (`faults_unrouted` `reason=no_line`), once.
    Unheard,
}

/// What an event made of the run.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Verdict {
    /// Undecided.
    Pending,
    /// Every witness appeared, in order.
    Pass,
    /// The run failed, at the point the code names.
    Fail(NonZeroU16),
}

/// A unit reported anything but translating with nothing stopped or refused.
pub const FAIL_UNIT: NonZeroU16 = fail_point!(1);
/// A unit translated at a stage the run did not ask for.
pub const FAIL_STAGE: NonZeroU16 = fail_point!(6);
/// Remapping reported otherwise than the board needs.
pub const FAIL_REMAPPING: NonZeroU16 = fail_point!(2);
/// A unit refused a device's access.
pub const FAIL_FAULT: NonZeroU16 = fail_point!(3);
/// A key arrived before the keyboard was granted its domain.
pub const FAIL_EARLY_KEY: NonZeroU16 = fail_point!(4);
/// A device was granted bus mastering before the unit translated, or before
/// interrupts were remapped where they must be.
pub const FAIL_EARLY_GRANT: NonZeroU16 = fail_point!(5);
/// A unit translated from registers below where its run places them, or from
/// none the tree holds.
pub const FAIL_REGISTERS: NonZeroU16 = fail_point!(7);

/// The witness: the order of the records a run must write.
pub struct TranslationWitness {
    interrupts: Interrupts,
    tables: Tables,
    faults: Faults,
    translating: AtomicBool,
    unheard: AtomicBool,
    routed: AtomicBool,
    keyboard: AtomicU32,
    keyboard_mastered: AtomicBool,
}

/// No keyboard found yet.
const NO_NODE: u32 = u32::MAX;

impl TranslationWitness {
    /// A witness for a board whose interrupts arrive as `interrupts` says,
    /// whose unit is to keep its translations as `tables` says and raise its
    /// faults as `faults` says.
    #[must_use]
    pub const fn new(interrupts: Interrupts, tables: Tables, faults: Faults) -> Self {
        Self {
            interrupts,
            tables,
            faults,
            translating: AtomicBool::new(false),
            unheard: AtomicBool::new(false),
            routed: AtomicBool::new(false),
            keyboard: AtomicU32::new(NO_NODE),
            keyboard_mastered: AtomicBool::new(false),
        }
    }

    /// Judge `event`, asking `keyboard` for the keyboard's node once the unit
    /// translates.
    pub fn observe(&self, event: &Event<'_>, keyboard: impl FnOnce() -> Option<u32>) -> Verdict {
        let id = event.id.0;
        if id == AuditEvent::DmaTranslationUnit.id().0
            && self.faults == Faults::Unheard
            && matches!(
                field(event, "outcome"),
                Some(FieldValue::Str("faults_unrouted"))
            )
        {
            if !matches!(field(event, "reason"), Some(FieldValue::Str("no_line")))
                || !self.translating.load(Ordering::Acquire)
                || self.unheard.swap(true, Ordering::AcqRel)
            {
                return Verdict::Fail(FAIL_UNIT);
            }
        } else if id == AuditEvent::DmaTranslationUnit.id().0 {
            if !matches!(
                field(event, "outcome"),
                Some(FieldValue::Str("translating"))
            ) || !matches!(field(event, "stopped"), Some(FieldValue::UnsignedInt(0)))
                || !matches!(field(event, "refused"), Some(FieldValue::UnsignedInt(0)))
            {
                return Verdict::Fail(FAIL_UNIT);
            }
            if !matches!(field(event, "stage"), Some(FieldValue::Str(stage)) if *stage == self.tables.name())
            {
                return Verdict::Fail(FAIL_STAGE);
            }
            self.keyboard
                .store(keyboard().unwrap_or(NO_NODE), Ordering::Release);
            self.translating.store(true, Ordering::Release);
        } else if id == AuditEvent::InterruptRemapping.id().0 {
            let Some(wanted) = self.interrupts.routed() else {
                return Verdict::Fail(FAIL_REMAPPING);
            };
            let extended = match self.interrupts {
                Interrupts::Remapped { extended } => extended(),
                Interrupts::Unremapped | Interrupts::Wired => true,
            };
            if !matches!(field(event, "outcome"), Some(FieldValue::Str(outcome)) if *outcome == wanted)
                || !self.translating.load(Ordering::Acquire)
                || !extended
            {
                return Verdict::Fail(FAIL_REMAPPING);
            }
            self.routed.store(true, Ordering::Release);
        } else if id == AuditEvent::DmaTranslationFault.id().0 {
            return Verdict::Fail(FAIL_FAULT);
        } else if id == AuditEvent::DmaBusMaster.id().0
            && matches!(field(event, "master"), Some(FieldValue::Str("on")))
        {
            let routing = self.interrupts.routed().is_some();
            if !self.translating.load(Ordering::Acquire)
                || (routing && !self.routed.load(Ordering::Acquire))
            {
                return Verdict::Fail(FAIL_EARLY_GRANT);
            }
            let keyboard = u64::from(self.keyboard.load(Ordering::Acquire));
            if matches!(field(event, "node"), Some(FieldValue::UnsignedInt(node)) if *node == keyboard)
                && matches!(field(event, "outcome"), Some(FieldValue::Str("applied")))
            {
                self.keyboard_mastered.store(true, Ordering::Release);
            }
        } else if id == AuditEvent::InputDelivered.id().0
            && matches!(field(event, "kind"), Some(FieldValue::Str("key")))
        {
            if !self.keyboard_mastered.load(Ordering::Acquire) {
                return Verdict::Fail(FAIL_EARLY_KEY);
            }
            if self.faults == Faults::Unheard && !self.unheard.load(Ordering::Acquire) {
                return Verdict::Fail(FAIL_UNIT);
            }
            return Verdict::Pass;
        }
        Verdict::Pending
    }
}

/// Whether the unit `event` reports translating keeps every register window
/// at or above `floor`, `unit` finding its node; true of any other event. A
/// unit with no window, or no node, is not.
#[must_use]
pub fn registers_from(
    event: &Event<'_>,
    floor: u64,
    unit: impl FnOnce(u32) -> Option<HwNode>,
) -> bool {
    if event.id.0 != AuditEvent::DmaTranslationUnit.id().0
        || !matches!(
            field(event, "outcome"),
            Some(FieldValue::Str("translating"))
        )
    {
        return true;
    }
    let Some(FieldValue::UnsignedInt(node)) = field(event, "node") else {
        return false;
    };
    u32::try_from(*node)
        .ok()
        .and_then(unit)
        .is_some_and(|node| {
            let mut windows = node
                .resources()
                .iter()
                .filter(|resource| resource.kind() == Some(HwResourceKind::Mmio))
                .peekable();
            windows.peek().is_some() && windows.all(|window| window.base() >= floor)
        })
}

/// The lowest-numbered virtio-input node in `tree`: the keyboard, the first
/// input function a probe publishes.
#[must_use]
pub fn first_input_node(tree: &dyn HwTreeSource) -> Option<u32> {
    let snapshot = tree.snapshot().ok()?;
    let key = tairix_abi::HwMatchKey::virtio(tairix_virtio_input::VIRTIO_INPUT_DEVICE_ID);
    tairix_abi::hwtree::snapshot_nodes(&snapshot)?
        .filter(|node| node.match_keys().contains(&key))
        .map(|node| node.id())
        .min()
}

#[cfg(test)]
mod tests;
