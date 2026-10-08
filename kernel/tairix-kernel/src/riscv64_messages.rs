//! Message-signalled interrupts on riscv64 (`plans/IOMMU.md` IOM18.4): a PCI
//! function whose host sends its messages to the boot hart's IMSIC, behind a
//! RISC-V IOMMU that confines messages to memory-resident interrupt files, is
//! given a file of its own. Its MSI-X entry writes [`VECTOR`] to the hart's
//! file page, which the unit takes as a message into the function's file; its
//! line is the `k`th from [`MESSAGE_LINE_BASE`] for file `k`; and the unit
//! then raises the file's notice in the hart's own file.
//!
//! A function no such unit confines keeps its wired line: a message reaching
//! the hart's file unconfined could raise any identity there.

use alloc::vec::Vec;

use tairix_abi::driver::msix::MsiMessage;
use tairix_abi::HwResource;
use tairix_arch_riscv64::fdt::ImsicFile;
use tairix_fdt::pci::PciHost;
use tairix_fdt::Fdt;

use crate::pci_fdt::{MessageRoute, MessageRouter};
use crate::riscv64_aia_irq::{MESSAGE_LINE_BASE, VECTOR};

/// Offers each PCI function a file of its own.
pub struct MrifPlanner {
    file: ImsicFile,
    /// Phandles of the units that confine messages to files.
    units: Vec<u32>,
    /// Each routed function's node: file `k` is the `k`th's.
    nodes: Vec<u32>,
    capacity: u32,
    offered: Option<u32>,
}

impl MrifPlanner {
    /// A planner for messages to `file`, confined by `units`, routing at most
    /// `capacity` functions.
    #[must_use]
    pub const fn new(file: ImsicFile, units: Vec<u32>, capacity: u32) -> Self {
        Self {
            file,
            units,
            nodes: Vec::new(),
            capacity,
            offered: None,
        }
    }

    /// The functions routed, in file order.
    #[must_use]
    pub fn into_nodes(self) -> Vec<u32> {
        self.nodes
    }
}

impl MessageRouter for MrifPlanner {
    fn offer(
        &mut self,
        _fdt: &Fdt<'_>,
        host: &PciHost<'_>,
        node: u32,
        requester: u16,
    ) -> Option<MessageRoute> {
        self.offered = None;
        let requester = u32::from(requester);
        let (controller, _) = host.msi_target(requester).ok()??;
        let (unit, _) = host.iommu_map().ok()??.map(requester)?;
        if controller != self.file.phandle || !self.units.contains(&unit) {
            return None;
        }
        let index = u32::try_from(self.nodes.len())
            .ok()
            .filter(|&index| index < self.capacity)?;
        // Room is made now, so the acceptance cannot fail.
        self.nodes.try_reserve(1).ok()?;
        let doorbell =
            HwResource::msi_doorbell(self.file.page, tairix_abi::PAGE_SIZE as u64).ok()?;
        self.offered = Some(node);
        Some(MessageRoute {
            message: MsiMessage {
                address: self.file.page,
                data: VECTOR,
            },
            line: MESSAGE_LINE_BASE.checked_add(index)?,
            doorbell,
        })
    }

    fn accept(&mut self) {
        if let Some(node) = self.offered.take() {
            self.nodes.push(node);
        }
    }
}

/// The functions the boot probe routed, published once.
#[cfg(all(freestanding, kernel_isa = "riscv64"))]
static ROUTED: tairix_sync::once::OnceCell<Vec<u32>> = tairix_sync::once::OnceCell::new();

/// Publish the functions the boot probe routed; the first publication wins.
#[cfg(all(freestanding, kernel_isa = "riscv64"))]
pub fn publish(nodes: Vec<u32>) {
    let _ = ROUTED.set(nodes);
}

/// A planner over every unit in `fdt` that confines messages to files, for
/// the hart file of `aia`, routing no more functions than leave each of the
/// domain's sources an identity of its own. Each unit's registers are where
/// the walk that emitted `nodes` translated its window to.
#[cfg(all(freestanding, kernel_isa = "riscv64"))]
#[must_use]
pub fn discover(
    fdt: &Fdt<'_>,
    aia: &tairix_arch_riscv64::fdt::Aia,
    nodes: &[tairix_abi::HwNode],
) -> MrifPlanner {
    use tairix_abi::HwResourceKind;
    use tairix_kernel_iommu_riscv::{Capabilities, CAPABILITIES, COMPATIBLE};

    let mut units = Vec::new();
    for (id, node, _) in tairix_arch_api::fdtwalk::emitted(fdt) {
        if !node.is_compatible(COMPATIBLE) {
            continue;
        }
        let base = nodes
            .iter()
            .find(|entry| entry.id() == id)
            .and_then(|entry| {
                entry
                    .resources()
                    .iter()
                    .find(|resource| resource.kind() == Some(HwResourceKind::Mmio))
            })
            .map(tairix_abi::HwResource::base);
        let (Some(phandle), Some(base)) = (node.phandle(), base) else {
            continue;
        };
        let Some(regs) = crate::riscv64::boot::device_registers(base + CAPABILITIES as u64, 8)
        else {
            continue;
        };
        // SAFETY: the unit's capabilities register, a read-only identification
        // register of the window the tree names, inside the identity window.
        let caps = Capabilities(unsafe { regs.cast::<u64>().as_ptr().read_volatile() });
        if caps.message_files() && units.try_reserve(1).is_ok() {
            units.push(phandle);
        }
    }
    MrifPlanner::new(
        aia.imsic,
        units,
        aia.imsic.ids.saturating_sub(aia.aplic.sources),
    )
}

/// Give every published function its file: the files drawn from `frames`,
/// taken by the AIA controller, and each function's streams confined to its
/// own through `translation`. Run once, after the units translate and the
/// controller is up, before any driver is admitted.
#[cfg(all(freestanding, kernel_isa = "riscv64"))]
pub fn route(
    translation: Option<&'static tairix_kernel_core::iommu::Translation>,
    frames: &'static tairix_kernel_mem::FrameAllocator,
    log: &dyn tairix_log::Sink,
) -> tairix_kernel_core::iommu::InterruptRouting {
    use tairix_arch_api::PageTableFrames;
    use tairix_kernel_core::iommu::InterruptRouting;
    use tairix_kernel_iommu_api::Notice;

    use crate::pci_fdt::{log_unrouted, routing_refused};
    use crate::riscv64_aia_irq::Mrif;

    let Some(nodes) = ROUTED
        .get()
        .ok()
        .flatten()
        .filter(|nodes| !nodes.is_empty())
    else {
        return InterruptRouting::Native;
    };
    let refused = |reason| routing_refused(log, reason);
    let (Some(translation), Some(aia), Some(page)) = (
        translation,
        crate::riscv64::irq::aia_controller(),
        crate::riscv64::irq::file_page(),
    ) else {
        return refused("no_unit");
    };
    let Ok(tables) = crate::riscv64::spawn_producer::page_table_source(frames) else {
        return refused("no_frame_source");
    };
    let pages = (nodes.len() * core::mem::size_of::<Mrif>()).div_ceil(tairix_abi::PAGE_SIZE);
    let order = pages.next_power_of_two().trailing_zeros();
    let Some(phys) = tables.alloc_block(order) else {
        return refused("no_memory");
    };
    let Some(base) = tables.block_at(phys, order) else {
        tables.free_block(phys, order);
        return refused("no_memory");
    };
    // A page-aligned block is aligned past any file.
    #[allow(clippy::cast_ptr_alignment)]
    let base = base.cast::<Mrif>();
    // SAFETY: a fresh, zeroed, page-aligned block of at least `nodes.len()`
    // files, the kernel's for good once taken: the unit writes it for as long
    // as the files are confined. Refused, it is freed unused. A file is
    // atomics, valid as zeroes.
    let files: &'static [Mrif] = unsafe { core::slice::from_raw_parts(base, nodes.len()) };
    let Some(first) = aia.take_files(files) else {
        // Refused, the files are no one's.
        tables.free_block(phys, order);
        return refused("no_identities");
    };
    let mut failed = 0u32;
    for (k, &node) in (0u32..).zip(nodes) {
        let notice = Notice {
            address: page,
            data: first + k,
        };
        let file = phys + u64::from(k) * tairix_kernel_iommu_api::MESSAGE_FILE_BYTES;
        if translation
            .confine_messages(node, page, file, notice)
            .is_err()
        {
            failed = failed.saturating_add(1);
            log_unrouted(log, "refused");
        }
    }
    InterruptRouting::with_unrouted(failed)
}

#[cfg(test)]
#[path = "riscv64_messages_tests.rs"]
mod tests;
