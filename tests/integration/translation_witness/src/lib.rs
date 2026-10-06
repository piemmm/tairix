//! The witness a DMA-translation vertical judges its run by (`plans/IOMMU.md`).
//!
//! The run passes once `InputDelivered` `kind=key` follows `DmaTranslationUnit`
//! `outcome=translating` at the stage the run asks for with `stopped=0` and
//! `refused=0`, interrupt remapping `outcome=remapped` in the mode the board
//! needs where its unit remaps interrupts, and the keyboard's `DmaBusMaster`
//! `master=on` `outcome=applied`. Any other unit outcome (`faults_unrouted`
//! among them) or stage, a remapping record a board with nothing to remap
//! should never write, a translation fault, a grant before the unit translates
//! (and, where it remaps, before it remaps), or a key before the keyboard was
//! granted fails it.
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

use tairix_itest_finisher::fail_point;
use tairix_itest_witness::field;
use tairix_kernel_core::{AuditEvent, HwTreeSource};
pub use tairix_kernel_iommu_api::Stage;
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
    /// On wired lines no unit remaps: no remapping record may appear.
    Wired,
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

/// The witness: the order of the records a run must write.
pub struct TranslationWitness {
    interrupts: Interrupts,
    stage: Stage,
    translating: AtomicBool,
    remapped: AtomicBool,
    keyboard: AtomicU32,
    keyboard_mastered: AtomicBool,
}

/// No keyboard found yet.
const NO_NODE: u32 = u32::MAX;

impl TranslationWitness {
    /// A witness for a board whose interrupts arrive as `interrupts` says,
    /// whose unit is to translate at `stage`.
    #[must_use]
    pub const fn new(interrupts: Interrupts, stage: Stage) -> Self {
        Self {
            interrupts,
            stage,
            translating: AtomicBool::new(false),
            remapped: AtomicBool::new(false),
            keyboard: AtomicU32::new(NO_NODE),
            keyboard_mastered: AtomicBool::new(false),
        }
    }

    /// Judge `event`, asking `keyboard` for the keyboard's node once the unit
    /// translates.
    pub fn observe(&self, event: &Event<'_>, keyboard: impl FnOnce() -> Option<u32>) -> Verdict {
        let id = event.id.0;
        if id == AuditEvent::DmaTranslationUnit.id().0 {
            if !matches!(
                field(event, "outcome"),
                Some(FieldValue::Str("translating"))
            ) || !matches!(field(event, "stopped"), Some(FieldValue::UnsignedInt(0)))
                || !matches!(field(event, "refused"), Some(FieldValue::UnsignedInt(0)))
            {
                return Verdict::Fail(FAIL_UNIT);
            }
            if !matches!(field(event, "stage"), Some(FieldValue::Str(stage)) if *stage == self.stage.name())
            {
                return Verdict::Fail(FAIL_STAGE);
            }
            self.keyboard
                .store(keyboard().unwrap_or(NO_NODE), Ordering::Release);
            self.translating.store(true, Ordering::Release);
        } else if id == AuditEvent::InterruptRemapping.id().0 {
            let Interrupts::Remapped { extended } = self.interrupts else {
                return Verdict::Fail(FAIL_REMAPPING);
            };
            if !matches!(field(event, "outcome"), Some(FieldValue::Str("remapped")))
                || !self.translating.load(Ordering::Acquire)
                || !extended()
            {
                return Verdict::Fail(FAIL_REMAPPING);
            }
            self.remapped.store(true, Ordering::Release);
        } else if id == AuditEvent::DmaTranslationFault.id().0 {
            return Verdict::Fail(FAIL_FAULT);
        } else if id == AuditEvent::DmaBusMaster.id().0
            && matches!(field(event, "master"), Some(FieldValue::Str("on")))
        {
            let remapping = matches!(self.interrupts, Interrupts::Remapped { .. });
            if !self.translating.load(Ordering::Acquire)
                || (remapping && !self.remapped.load(Ordering::Acquire))
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
            if self.keyboard_mastered.load(Ordering::Acquire) {
                return Verdict::Pass;
            }
            return Verdict::Fail(FAIL_EARLY_KEY);
        }
        Verdict::Pending
    }
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
