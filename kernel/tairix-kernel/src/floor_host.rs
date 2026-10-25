//! The host the kernel lends a floor driver it opens over a device's own
//! resources: the windows, DMA and firmware channel of that device and no
//! other's, under the unlock service's capabilities and no more.

use tairix_abi::driver::dma::DmaHost;
use tairix_abi::driver::mailbox::MailboxChannel;
use tairix_abi::{
    CapabilityId, DriverHost, DriverKind, HwNode, HwResourceKind, MmioMapError, MmioMapper,
    RegisterWindow,
};
use tairix_caps::CapabilitySet;
use tairix_kernel_core::InitSpawnCtx;
use tairix_kernel_sec::captable::ProcessId;

use crate::floor_dma::{translator, Confinement};
use crate::floor_irq::{node_line, ArmedLine, LineHost};

/// A floor driver's host: maps only its node's register windows, carves
/// through the DMA host the kernel built for the node, and lends the
/// firmware channel, where the port has one, for the bring-up.
pub struct FloorDriverHost<'a> {
    caps: CapabilitySet,
    windows: NodeWindows<'a>,
    dma: Option<&'a dyn DmaHost>,
    mailbox: Option<&'a dyn MailboxChannel>,
}

impl<'a> FloorDriverHost<'a> {
    /// A host granting `caps`, mapping through `map` the windows `node`
    /// names, carving through `dma` and lending `mailbox`.
    #[must_use]
    pub fn new(
        caps: CapabilitySet,
        node: &'a HwNode,
        map: &'a dyn MmioMapper,
        dma: Option<&'a dyn DmaHost>,
        mailbox: Option<&'a dyn MailboxChannel>,
    ) -> Self {
        Self {
            caps,
            windows: NodeWindows { node, map },
            dma,
            mailbox,
        }
    }
}

impl DriverHost for FloorDriverHost<'_> {
    fn has_capability(&self, cap: CapabilityId) -> bool {
        self.caps.contains(cap)
    }

    /// The driver runs inside the kernel image, below the driver store it
    /// reads.
    fn kind(&self) -> DriverKind {
        DriverKind::InKernel
    }

    fn mmio_mapper(&self) -> Option<&dyn MmioMapper> {
        Some(&self.windows)
    }

    fn dma_host(&self) -> Option<&dyn DmaHost> {
        self.dma
    }

    fn mailbox(&self) -> Option<&dyn MailboxChannel> {
        self.mailbox
    }
}

/// The kernel's mapper, for the register windows a node names and no other:
/// a floor driver reaches its own device's registers alone.
struct NodeWindows<'a> {
    node: &'a HwNode,
    map: &'a dyn MmioMapper,
}

impl MmioMapper for NodeWindows<'_> {
    fn map_window(&self, phys_base: u64, len: usize) -> Result<RegisterWindow, MmioMapError> {
        let len_bytes = u64::try_from(len).map_err(|_| MmioMapError::InvalidRegion)?;
        let named = self.node.resources().iter().any(|resource| {
            resource.kind() == Some(HwResourceKind::Mmio) && resource.spans(phys_base, len_bytes)
        });
        if !named {
            return Err(MmioMapError::CapabilityMissing);
        }
        self.map.map_window(phys_base, len)
    }
}

/// Take `node` for the kernel, then bind its one wired interrupt line to
/// `owner` and arm it.
///
/// The claim comes first, so no process admitted as the node's driver can
/// reach the device the kernel is about to drive. A device a translation unit
/// stands in front of is refused: its driver carves physically, inside its
/// node's window.
///
/// # Errors
///
/// A driver already holds the node, a unit translates its device, or the
/// node wires no single line, or the line cannot be armed.
pub fn claim_and_bind(
    ctx: &dyn InitSpawnCtx,
    node: &HwNode,
    lines: LineHost,
    owner: ProcessId,
) -> Result<ArmedLine, &'static str> {
    ctx.claim_for_kernel(node.id())
        .map_err(|_| "floor: the node has a driver")?;
    if translator(ctx, node, Confinement::WhereTranslated)?.is_some() {
        return Err("floor: a translation unit stands in front of the device");
    }
    let (line, trigger) = node_line(node)?;
    lines.bind(line, Some(trigger), owner)
}

#[cfg(test)]
mod tests {
    use core::cell::{Cell, RefCell};
    use core::ptr::NonNull;

    use alloc::boxed::Box;
    use alloc::vec::Vec;

    use tairix_abi::{
        DriverHost, Errno, HwDeviceClass, HwNode, HwResource, IrqHandle, MmioMapError, MmioMapper,
        RegisterWindow,
    };
    use tairix_caps::CapabilitySet;
    use tairix_kernel_core::InitSpawnCtx;
    use tairix_kernel_irq::{IrqController, IrqTable, MaskError, Trigger};
    use tairix_kernel_mem::FrameAllocator;
    use tairix_kernel_sec::captable::ProcessId;

    use super::{claim_and_bind, FloorDriverHost};
    use crate::floor_irq::LineHost;
    use crate::test_support::{inert_spawn_ctx, scratch_frames};

    const WINDOW: u64 = 0x1000_0000;
    const LINE: u32 = 126;
    const OWNER: ProcessId = ProcessId(7);

    /// A mapper recording the windows asked of it, over a buffer of its own.
    struct Recorder {
        backing: Vec<u32>,
        asked: RefCell<Vec<(u64, usize)>>,
    }

    impl MmioMapper for Recorder {
        fn map_window(&self, phys_base: u64, len: usize) -> Result<RegisterWindow, MmioMapError> {
            self.asked.borrow_mut().push((phys_base, len));
            let base = NonNull::new(self.backing.as_ptr().cast_mut().cast::<u8>())
                .ok_or(MmioMapError::Unsupported)?;
            // SAFETY: `base` is the start of `backing`, 4-byte aligned as a
            // `Vec<u32>` allocation is and at least `len` bytes long for
            // every window these tests ask for; it outlives the window, and
            // the tests read nothing through it.
            Ok(unsafe { RegisterWindow::from_mapping(phys_base, base, len) })
        }
    }

    fn node() -> HwNode {
        let mut node = HwNode::new(3, 0, HwDeviceClass::Storage);
        node.push_resource(HwResource::mmio(WINDOW, 0x100))
            .expect("room");
        node.push_resource(HwResource::irq_at(u64::from(LINE), 0))
            .expect("room");
        node
    }

    /// Regression: the SD bring-up's host mapped any window its driver asked
    /// for, not only its own node's.
    #[test]
    fn a_floor_driver_maps_only_its_nodes_windows() {
        let recorder = Recorder {
            backing: alloc::vec![0; 0x100 / 4],
            asked: RefCell::new(Vec::new()),
        };
        let node = node();
        let host = FloorDriverHost::new(CapabilitySet::empty(), &node, &recorder, None, None);
        let mapper = host.mmio_mapper().expect("a mapper");
        assert!(mapper.map_window(WINDOW, 0x100).is_ok());
        assert!(mapper.map_window(WINDOW + 0x80, 0x80).is_ok());
        for (base, len) in [(WINDOW + 0x80, 0x100), (WINDOW - 4, 8), (0x2000_0000, 4)] {
            assert_eq!(
                mapper.map_window(base, len).err(),
                Some(MmioMapError::CapabilityMissing),
                "{base:#x}+{len:#x}"
            );
        }
        assert_eq!(
            *recorder.asked.borrow(),
            [(WINDOW, 0x100), (WINDOW + 0x80, 0x80)],
            "a refused window never reaches the kernel's mapper"
        );
    }

    /// A controller that configures and arms every line.
    struct Accepting;

    impl IrqController for Accepting {
        fn mask(&self, _line: u32) -> Result<(), MaskError> {
            Ok(())
        }

        fn rearm(&self, _line: u32) -> Result<(), MaskError> {
            Ok(())
        }

        fn set_trigger(&self, _line: u32, _trigger: Trigger) -> Result<(), MaskError> {
            Ok(())
        }
    }

    fn park(_table: &IrqTable, _handle: IrqHandle) {}

    fn lines() -> LineHost {
        LineHost {
            table: tairix_kernel_core::forever(Box::new(IrqTable::new(255))),
            controller: &Accepting,
            park,
        }
    }

    /// A context whose claim answers as told, counting the asks.
    struct ClaimingCtx {
        frames: FrameAllocator,
        claim: Result<(), Errno>,
        claims: Cell<u32>,
    }

    impl InitSpawnCtx for ClaimingCtx {
        inert_spawn_ctx!();

        fn claim_for_kernel(&self, _node: u32) -> Result<(), Errno> {
            self.claims.set(self.claims.get() + 1);
            self.claim
        }
    }

    fn ctx(claim: Result<(), Errno>) -> ClaimingCtx {
        ClaimingCtx {
            frames: scratch_frames(),
            claim,
            claims: Cell::new(0),
        }
    }

    /// Regression: the SD bring-up bound its line without claiming its node,
    /// so a driver process could be admitted for the device the kernel drove.
    #[test]
    fn a_claimed_node_is_refused_before_its_line_is_bound() {
        let lines = lines();
        let held = ctx(Err(Errno::Busy));
        assert!(claim_and_bind(&held, &node(), lines, OWNER).is_err());
        assert_eq!(held.claims.get(), 1);
        assert!(
            lines.table.bind_exclusive(LINE, OWNER).is_ok(),
            "the line was never bound"
        );
    }

    #[test]
    fn a_free_node_is_claimed_and_its_line_bound_to_the_owner() {
        let lines = lines();
        let free = ctx(Ok(()));
        let armed = claim_and_bind(&free, &node(), lines, OWNER).expect("bound");
        assert_eq!(free.claims.get(), 1);
        assert_eq!(
            lines.table.lookup(armed.handle()).map(|entry| entry.owner),
            Some(OWNER)
        );
    }
}
