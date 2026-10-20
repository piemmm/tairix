//! The kernel console a display driver hosts (`console_adopt`,
//! `console_damage`, `console_relinquish`): the memory the console paints, the
//! device that reads it, and who is owed what it paints.
//!
//! The memory is kernel-owned and physically contiguous, so the console paints
//! it through the direct map; the device reaches it through its node's
//! translation domain where it has one, else untranslated. It outlives any one
//! driver: a restarted driver hosting the console at the same mode is answered
//! the same memory, mapped for its own instance. It is given back only once
//! the console has stopped painting it and its device is known to have
//! stopped reading it, so no device ever reads memory the kernel reused.

use core::ops::{Deref, DerefMut};

use tairix_abi::driver::display::DisplayMode;
use tairix_kernel_mem::{DmaCustodian, SharedMemory};
use tairix_sync::SpinLock;

use crate::devres::{SharedChunk, SharedMemFacility};
use crate::sleeplock::{SleepGuard, SleepLock};

/// The console's hosted memory and the driver instance it is hosted for.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Hosted {
    /// The adopting driver instance's custody of the device: its node and
    /// generation, and the domain its DMA is translated through.
    pub(crate) custodian: DmaCustodian,
    /// The memory the console paints.
    pub(crate) block: SharedChunk,
    /// The mode it is cut to.
    pub(crate) mode: DisplayMode,
    /// Where the device reaches the block: its IOVA in the custodian's domain
    /// when translated, else its CPU-physical base.
    pub(crate) device_base: u64,
}

impl Hosted {
    /// Whether `node`'s live driver of `generation` is the one the console is
    /// hosted for.
    pub(crate) const fn hosted_for(&self, node: u32, generation: u64) -> bool {
        self.custodian.node == node && self.custodian.generation == generation
    }
}

/// The one hosted console, or none.
///
/// Deciding a hosting carves, zeroes and maps the console's memory and moves
/// the console onto it, so the decision is held under a sleeping lock. A
/// wait-set's readiness scan may not park, so it reads who the console is
/// hosted for from a copy taken whenever the decision is let go.
pub(crate) struct ConsoleHost {
    hosted: SleepLock<Option<Hosted>>,
    host: SpinLock<Option<(u32, u64, DisplayMode)>>,
}

impl ConsoleHost {
    /// Nothing hosted.
    pub(crate) const fn new() -> Self {
        Self {
            hosted: SleepLock::new(None),
            host: SpinLock::new(None),
        }
    }

    /// The hosting state, held while it is decided.
    pub(crate) fn lock(&self) -> HostingGuard<'_> {
        HostingGuard {
            hosted: self.hosted.lock(),
            host: &self.host,
        }
    }

    /// The mode of the console hosted for `node`'s driver of `generation`.
    pub(crate) fn mode_for(&self, node: u32, generation: u64) -> Option<DisplayMode> {
        (*self.host.lock())
            .filter(|&(hosted_node, hosted_generation, _)| {
                (hosted_node, hosted_generation) == (node, generation)
            })
            .map(|(_, _, mode)| mode)
    }
}

/// The hosting state, held, and published to readers that may not wait for
/// it once it is let go.
pub(crate) struct HostingGuard<'a> {
    hosted: SleepGuard<'a, Option<Hosted>>,
    host: &'a SpinLock<Option<(u32, u64, DisplayMode)>>,
}

impl Deref for HostingGuard<'_> {
    type Target = Option<Hosted>;

    fn deref(&self) -> &Self::Target {
        &self.hosted
    }
}

impl DerefMut for HostingGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.hosted
    }
}

impl Drop for HostingGuard<'_> {
    fn drop(&mut self) {
        *self.host.lock() = (*self.hosted).map(|hosted| {
            (
                hosted.custodian.node,
                hosted.custodian.generation,
                hosted.mode,
            )
        });
    }
}

/// Hand back a block no device can reach, and the custody room reserved for
/// it when it was carved.
pub(crate) fn discard(
    facility: &dyn SharedMemFacility,
    custodian: &DmaCustodian,
    block: SharedChunk,
) {
    facility.free_region(&[block], SharedMemory::dma(custodian.coherence()));
    custodian.custody().unreserve(custodian.node);
}

/// Hand back the block `hosted` names, which the console no longer paints: to
/// the allocator once its device is known to have lost it — its translation
/// confirms the unmap, or, untranslated, its node's live driver `vouched` that
/// the device stopped reading it — else to its node's custody, which spends
/// the block's reservation and frees it only once the device is proven quiet
/// or gone.
pub(crate) fn release(facility: &dyn SharedMemFacility, hosted: &Hosted, vouched: bool) {
    let lost = match hosted.custodian.translation() {
        Some(translation) => translation
            .unmap(
                hosted.custodian.node,
                hosted.custodian.generation,
                hosted.device_base,
            )
            .is_ok(),
        None => vouched,
    };
    if lost {
        discard(facility, &hosted.custodian, hosted.block);
    } else {
        facility.surrender_region(&[hosted.block], &hosted.custodian);
    }
}
