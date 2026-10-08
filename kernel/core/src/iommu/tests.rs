extern crate std;

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::blkio::FaultDomainState;
use tairix_abi::{
    DmaCoherence, Errno, HwDeviceClass, HwResource, IommuGroup, MsiAllocation, HW_NODE_ROOT_ID,
};
use tairix_kernel_iommu_api::conformance::TranslationProbe;
use tairix_kernel_iommu_api::model::{Behaviour, ModelUnit};
use tairix_kernel_iommu_api::{
    Access, DomainId, Fault, FaultBudget, FaultLimits, Notice, UnitProfile,
};
use tairix_kernel_irq::{IrqController, IrqTable, MaskError};
use tairix_kernel_mem::{Frame, FrameBlock, PhysAddr};

use super::*;
use crate::audit::AuditEvent;
use crate::devres::KernelMsiFacility;
use crate::hwtree::HwNodeLiveness;
use crate::test_sink::{with_log_level, CapturedEvent, TestSink};

/// A tree holding the nodes a test put in it, and the health it was told.
struct Tree {
    nodes: SpinLock<Vec<HwNode>>,
    health: SpinLock<Vec<(u32, FaultDomainState)>>,
    /// The next lookup that finds its node removes it, as a removal racing
    /// the caller would.
    vanish: core::sync::atomic::AtomicBool,
}

impl Tree {
    const fn new() -> Self {
        Self {
            nodes: SpinLock::new(Vec::new()),
            health: SpinLock::new(Vec::new()),
            vanish: core::sync::atomic::AtomicBool::new(false),
        }
    }

    fn add(&self, node: &HwNode) {
        self.nodes.lock().push(*node);
    }

    fn drop_node(&self, id: u32) {
        self.nodes.lock().retain(|node| node.id() != id);
    }

    fn health_of(&self, id: u32) -> Option<FaultDomainState> {
        self.health
            .lock()
            .iter()
            .rev()
            .find(|(node, _)| *node == id)
            .map(|(_, health)| *health)
    }
}

impl HwNodeLiveness for Tree {
    fn is_live(&self, node_id: u32) -> bool {
        self.nodes.lock().iter().any(|node| node.id() == node_id)
    }
}

impl HwTreeSource for Tree {
    fn generation(&self) -> Result<u64, Errno> {
        Ok(1)
    }

    fn snapshot(&self) -> Result<Vec<u8>, Errno> {
        Err(Errno::NotImplemented)
    }

    fn publish(&self, _parent_id: u32, _node: HwNode) -> Result<u32, Errno> {
        Err(Errno::NotImplemented)
    }

    fn remove(&self, _parent_id: u32, _node_id: u32) -> Result<Vec<u32>, Errno> {
        Err(Errno::NotImplemented)
    }

    fn node_endpoints(&self, _parent_id: u32, _node_id: u32) -> Result<Vec<u64>, Errno> {
        Err(Errno::NotImplemented)
    }

    fn set_health(&self, node_id: u32, health: FaultDomainState) -> Result<(), Errno> {
        if !self.is_live(node_id) {
            return Err(Errno::NotFound);
        }
        self.health.lock().push((node_id, health));
        Ok(())
    }

    fn node(&self, node_id: u32) -> Result<Option<HwNode>, Errno> {
        let mut nodes = self.nodes.lock();
        let found = nodes.iter().find(|node| node.id() == node_id).copied();
        if found.is_some()
            && self
                .vanish
                .swap(false, core::sync::atomic::Ordering::Relaxed)
        {
            nodes.retain(|node| node.id() != node_id);
        }
        Ok(found)
    }
}

/// A reference unit, its frames and a tree, in statics of this expansion's
/// own: the facility holds all three for the kernel's life.
macro_rules! rig {
    ($behaviour:expr) => {{
        static FRAMES: tairix_kernel_iommu_api::hostmem::HostFrames =
            tairix_kernel_iommu_api::hostmem::HostFrames::new(0x1_0000_0000);
        static UNIT: ModelUnit<'static> = ModelUnit::new(&FRAMES, $behaviour);
        static TREE: Tree = Tree::new();
        (&UNIT, &TREE)
    }};
}

const UNIT_NODE: u32 = 100;
const DEVICE: u32 = 7;
const STREAM: u32 = 0x0010;
const PAGE: u64 = 0x1000;

/// A node mastering DMA as `stream`, alone in a group of its own.
fn device_node(id: u32, stream: u32) -> HwNode {
    grouped(id, stream, stream)
}

/// A node mastering DMA as `stream`, in group `group`.
fn grouped(id: u32, stream: u32, group: u32) -> HwNode {
    let mut node = HwNode::new(id, HW_NODE_ROOT_ID, HwDeviceClass::Storage);
    node.push_resource(HwResource::iommu_stream(
        IommuStreams::new(UNIT_NODE, stream, 1).unwrap(),
    ))
    .unwrap();
    node.push_resource(HwResource::iommu_group_member(IommuGroup::new(
        UNIT_NODE, group,
    )))
    .unwrap();
    node
}

const REGISTERS: Range<u64> = 0xFED9_0000..0xFED9_1000;

fn unit(model: &'static ModelUnit<'static>, reserved: Vec<IommuReservedWindow>) -> Unit {
    Unit {
        node: UNIT_NODE,
        unit: model,
        reserved,
        faults: FaultSignal::Message,
        family: DmaUnitFamily::Vtd,
        counts: FaultCounts::default(),
    }
}

fn block(phys: u64) -> FrameBlock {
    FrameBlock {
        frame: Frame::containing(PhysAddr::new(phys)),
        order: 0,
    }
}

fn started(
    model: &'static ModelUnit<'static>,
    tree: &'static Tree,
    reserved: Vec<IommuReservedWindow>,
) -> Translation {
    started_with(model, tree, reserved, None, audit_sink())
}

/// [`started`], its owners' functions mastering through `port`, audited to
/// `audit`.
fn started_with(
    model: &'static ModelUnit<'static>,
    tree: &'static Tree,
    reserved: Vec<IommuReservedWindow>,
    port: Option<&'static Port>,
    audit: &'static TestSink,
) -> Translation {
    tree.add(&device_node(DEVICE, STREAM));
    let mastering = port.map(|port| Mastering::new(port, audit));
    let (translation, outcomes) = Translation::started(
        vec![unit(model, reserved)],
        vec![REGISTERS],
        tree,
        audit,
        mastering,
    );
    assert_eq!(
        outcomes,
        [(
            UNIT_NODE,
            UnitOutcome::Translating(
                Quiesced::default(),
                tairix_kernel_iommu_api::Tables::Walked(tairix_kernel_iommu_api::Stage::Second),
            )
        )]
    );
    translation
}

/// A port that records each mastering change with the domain its stream was
/// attached to at that moment, and answers that it changed; and the
/// functions behind the unit it reports mastering at take-over.
struct Port {
    model: &'static ModelUnit<'static>,
    calls: SpinLock<Vec<MasterCall>>,
    epochs: core::sync::atomic::AtomicU64,
    mastering_at_take_over: Vec<u32>,
}

/// What a change named: a node, or the requester streams of an owner.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Named {
    Node(u32),
    Streams(Vec<IommuStreams>),
}

/// A change a port was asked for: what it named, the state asked, the domain
/// then holding [`STREAM`], and the owner's epoch.
type MasterCall = (Named, bool, Option<DomainId>, u64);

impl Port {
    fn new(model: &'static ModelUnit<'static>, mastering_at_take_over: Vec<u32>) -> &'static Self {
        Box::leak(Box::new(Self {
            model,
            calls: SpinLock::new(Vec::new()),
            epochs: core::sync::atomic::AtomicU64::new(1),
            mastering_at_take_over,
        }))
    }

    fn calls(&self) -> Vec<MasterCall> {
        self.calls.lock().clone()
    }
}

impl BusMastering for Port {
    fn begin(&self) -> u64 {
        self.epochs
            .fetch_add(1, core::sync::atomic::Ordering::Relaxed)
    }

    fn set_mastering(
        &self,
        target: MasterTarget<'_>,
        master: bool,
        epoch: u64,
        report: &mut dyn FnMut(MasterChange),
    ) {
        let named = match target {
            MasterTarget::Node(node) => Named::Node(node),
            MasterTarget::Streams(streams) => Named::Streams(streams.to_vec()),
        };
        self.calls
            .lock()
            .push((named, master, self.model.attached(STREAM), epoch));
        report(MasterChange {
            changed: true,
            refused: false,
        });
    }

    fn quiesce(&self, unit: u32, keeps: &dyn Fn(u32) -> bool) -> Quiesced {
        if unit != UNIT_NODE {
            return Quiesced::default();
        }
        Quiesced {
            stopped: self
                .mastering_at_take_over
                .iter()
                .filter(|&&stream| !keeps(stream))
                .count(),
            refused: 0,
        }
    }

    fn set_wired_interrupt(&self, _node: u32, _raise: bool) {}
}

fn streams(stream: u32) -> Named {
    Named::Streams(vec![IommuStreams::new(UNIT_NODE, stream, 1).unwrap()])
}

#[test]
fn a_node_naming_a_stream_on_a_started_unit_is_translated() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    assert!(model.enabled());
    assert_eq!(translation.units(), 1);
    assert_eq!(
        translation.dma_path(DEVICE),
        DmaPath::Translated {
            output_limit: model.profile().reach.output_limit()
        }
    );
    tree.add(&HwNode::new(8, HW_NODE_ROOT_ID, HwDeviceClass::Network));
    assert_eq!(
        translation.dma_path(8),
        DmaPath::Untranslated,
        "a node naming no stream"
    );
    assert_eq!(
        translation.dma_path(9),
        DmaPath::Stranded { unit: None },
        "a node the tree does not hold masters nothing"
    );
    assert!(!translation.strands());
}

#[test]
fn a_carve_is_reachable_through_its_owner_s_domain_alone() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    assert_eq!(
        model.access(STREAM, 0x4000, false),
        None,
        "blocked until owned"
    );
    let iova = translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    assert_eq!(model.access(STREAM, iova + 8, true), Some(0x8000_0008));
    assert_eq!(model.access(0x0018, iova, true), None, "another stream");
    assert_ne!(
        iova, 0x8000_0000,
        "the device is handed an IOVA, not the frame"
    );
}

/// No carve is handed an IOVA a PCI segment decodes, where its device's DMA
/// could reach a peer before the unit sees it: a device reaching 4 GiB, whose
/// carve would otherwise land in the window below that line, lands below it.
#[test]
fn no_carve_lands_where_a_pci_segment_decodes() {
    let window = 0xC000_0000..0x1_0000_0000;
    let (model, tree) = rig!(Behaviour::Correct);
    let free = started(model, tree, Vec::new());
    let iova = free.map(DEVICE, 1, &[block(0x8000_0000)], 1 << 32).unwrap();
    assert!(
        window.contains(&iova),
        "{iova:#x}: what the window keeps out"
    );
    let (model, tree) = rig!(Behaviour::Correct);
    let avoiding = started(model, tree, Vec::new()).avoiding(vec![
        0xFFF0_0000..0xFFFF_F800,
        0xC000_0010..0xD000_0000,
        0xD000_0000..0xFFF1_0000,
    ]);
    let iova = avoiding
        .map(DEVICE, 1, &[block(0x8000_0000)], 1 << 32)
        .unwrap();
    assert!(iova + 0x1000 <= window.start, "{iova:#x}");
    assert_eq!(avoiding.peers, [window], "widened to pages and joined");
}

/// A carve of several blocks, largest first, is one IOVA run its device
/// reaches end to end, and its unmap takes all of it.
#[test]
fn a_carve_of_several_blocks_is_one_run_of_its_domain() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    let large = FrameBlock {
        frame: Frame::containing(PhysAddr::new(0x8000_0000)),
        order: 1,
    };
    let iova = translation
        .map(DEVICE, 1, &[large, block(0x9000_0000)], 0)
        .unwrap();
    assert_eq!(model.access(STREAM, iova + 8, true), Some(0x8000_0008));
    assert_eq!(model.access(STREAM, iova + 0x1008, true), Some(0x8000_1008));
    assert_eq!(
        model.access(STREAM, iova + 0x2008, true),
        Some(0x9000_0008),
        "the second block lies straight after the first"
    );
    assert_eq!(model.access(STREAM, iova + 0x3000, true), None);
    assert_eq!(translation.unmap(DEVICE, 1, iova), Ok(()));
    for offset in [0, 0x1000, 0x2000] {
        assert_eq!(model.access(STREAM, iova + offset, false), None);
    }
}

#[test]
fn revocation_blocks_the_stream_and_leaves_every_carve_unreachable() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    let iova = translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    assert!(translation.revoke(DEVICE, 1));
    assert_eq!(model.access(STREAM, iova, true), None);
    assert_eq!(model.attached(STREAM), None);
    assert_eq!(translation.unmap(DEVICE, 1, iova), Ok(()));
    assert_eq!(model.domains(), 0);
}

#[test]
fn a_successor_gets_a_fresh_domain_and_its_predecessor_stays_unreachable() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    let first = translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    translation.revoke(DEVICE, 1);
    let second = translation
        .map(DEVICE, 2, &[block(0x9000_0000)], 0)
        .unwrap();
    assert_eq!(model.access(STREAM, second, false), Some(0x9000_0000));
    assert_eq!(translation.unmap(DEVICE, 1, first), Ok(()));
    assert_eq!(
        translation.map(DEVICE, 1, &[block(0x8000_0000)], 0),
        Err(DmaError::DeviceGone),
        "a revoked generation carves nothing more"
    );
}

#[test]
fn an_owner_never_revoked_is_retired_before_its_successor_carves() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    let first = translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    translation
        .map(DEVICE, 2, &[block(0x9000_0000)], 0)
        .unwrap();
    assert_eq!(model.domains(), 1);
    assert_eq!(
        model
            .access(STREAM, first, false)
            .filter(|&p| p == 0x8000_0000),
        None
    );
}

#[test]
fn an_unmap_is_confirmed_and_a_second_is_harmless() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    let iova = translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    assert!(model.access(STREAM, iova, false).is_some());
    assert_eq!(translation.unmap(DEVICE, 1, iova), Ok(()));
    assert_eq!(model.access(STREAM, iova, false), None);
    assert_eq!(translation.unmap(DEVICE, 1, iova), Ok(()));
}

#[test]
fn an_unconfirmed_revocation_keeps_its_generation_unconfirmed_for_good() {
    let (model, tree) = rig!(Behaviour::UnconfirmedBlock);
    let translation = started(model, tree, Vec::new());
    let iova = translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    assert!(!translation.revoke(DEVICE, 1));
    assert_eq!(
        translation.unmap(DEVICE, 1, iova),
        Err(DmaError::Unconfirmed)
    );
}

#[test]
fn a_revoked_generation_carves_nothing_more() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    translation.revoke(DEVICE, 1);
    assert_eq!(
        translation.map(DEVICE, 1, &[block(0x9000_0000)], 0),
        Err(DmaError::DeviceGone)
    );
    assert_eq!(model.attached(STREAM), None, "no domain was made for it");
}

#[test]
fn an_unconfirmed_end_takes_no_successor() {
    let (model, tree) = rig!(Behaviour::UnconfirmedBlock);
    let translation = started(model, tree, Vec::new());
    let iova = translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    translation.revoke(DEVICE, 1);
    assert_eq!(
        translation.map(DEVICE, 2, &[block(0x9000_0000)], 0),
        Err(DmaError::Translation),
        "refused before it maps, so its block is free to go back"
    );
    assert_eq!(
        translation.unmap(DEVICE, 1, iova),
        Err(DmaError::Unconfirmed),
        "the refused successor did not displace the record"
    );
}

#[test]
fn the_kernel_s_own_device_takes_no_driver() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    let iova = translation
        .map(DEVICE, KERNEL_OWNER, &[block(0x8000_0000)], 0)
        .unwrap();
    assert_eq!(
        translation.map(DEVICE, 1, &[block(0x9000_0000)], 0),
        Err(DmaError::KernelOwned)
    );
    assert!(translation.revoke(DEVICE, 1));
    assert_eq!(
        model.access(STREAM, iova, true),
        Some(0x8000_0000),
        "a driver's end leaves the kernel's domain alone"
    );
}

#[test]
fn a_removed_node_is_forgotten_and_its_carves_are_unreachable() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    let iova = translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    tree.drop_node(DEVICE);
    assert!(translation.forget(DEVICE));
    assert_eq!(model.access(STREAM, iova, true), None);
    assert_eq!(translation.unmap(DEVICE, 1, iova), Ok(()));
    assert_eq!(
        translation.map(DEVICE, 1, &[block(0x9000_0000)], 0),
        Err(DmaError::DeviceGone)
    );
    assert_eq!(translation.reserve(DEVICE), Err(DmaError::DeviceGone));
}

#[test]
fn a_removed_node_whose_end_is_unconfirmed_stays_recorded() {
    let (model, tree) = rig!(Behaviour::UnconfirmedBlock);
    let translation = started(model, tree, Vec::new());
    let iova = translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    tree.drop_node(DEVICE);
    assert!(!translation.forget(DEVICE), "the unit could not confirm it");
    assert_eq!(
        translation.unmap(DEVICE, 1, iova),
        Err(DmaError::Unconfirmed)
    );
}

#[test]
fn custody_is_taken_only_for_a_node_the_tree_holds() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    assert_eq!(translation.reserve(DEVICE), Ok(()));
    assert_eq!(translation.reserve(9), Err(DmaError::DeviceGone));
}

#[test]
fn a_failed_adoption_gives_every_stream_its_firmware_windows_back() {
    let (model, tree) = rig!(Behaviour::Correct);
    let shared = STREAM + 1;
    let window = firmware_window(STREAM);
    let mut pair = HwNode::new(8, HW_NODE_ROOT_ID, HwDeviceClass::Storage);
    pair.push_resource(HwResource::iommu_stream(
        IommuStreams::new(UNIT_NODE, STREAM, 2).unwrap(),
    ))
    .unwrap();
    pair.push_resource(HwResource::iommu_group_member(IommuGroup::new(
        UNIT_NODE, STREAM,
    )))
    .unwrap();
    tree.add(&pair);
    let translation = started(model, tree, vec![window]);
    // A tree whose groups disagree on who shares a stream: the unit itself
    // still refuses the second owner the stream.
    tree.add(&grouped(9, shared, shared));
    translation.map(9, 1, &[block(0x9000_0000)], 0).unwrap();

    assert_eq!(
        translation.map(8, 1, &[block(0x8000_0000)], 0),
        Err(DmaError::Translation),
        "a stream another owner holds"
    );
    assert_eq!(
        model.access(STREAM, 0x7B80_0040, false),
        Some(0x7B80_0040),
        "the stream the adoption took from firmware has its windows again"
    );
}

/// A node the unit has no endpoint for is not behind it, so it carves nothing
/// translated; one with an alias the unit has no endpoint for is translated
/// through the stream it has.
#[test]
fn a_node_the_unit_has_no_endpoint_for_carves_nothing() {
    const ALIASED: u32 = 0x40;
    let (model, tree) = rig!(Behaviour::NoEndpointFrom(ALIASED));
    let translation = started(model, tree, Vec::new());
    tree.add(&grouped(8, ALIASED + 1, ALIASED + 1));
    assert_eq!(
        translation.map(8, 1, &[block(0x8000_0000)], 0),
        Err(DmaError::Translation)
    );
    let mut aliased = HwNode::new(9, HW_NODE_ROOT_ID, HwDeviceClass::Storage);
    aliased
        .push_resource(HwResource::iommu_stream(
            IommuStreams::new(UNIT_NODE, ALIASED - 1, 2).unwrap(),
        ))
        .unwrap();
    aliased
        .push_resource(HwResource::iommu_group_member(IommuGroup::new(
            UNIT_NODE,
            ALIASED - 1,
        )))
        .unwrap();
    tree.add(&aliased);
    let iova = translation.map(9, 1, &[block(0x9000_0000)], 0).unwrap();
    assert_eq!(model.access(ALIASED - 1, iova, true), Some(0x9000_0000));
}

#[test]
fn firmware_windows_stay_reachable_before_during_and_after_an_owner() {
    let (model, tree) = rig!(Behaviour::Correct);
    let window = firmware_window(STREAM);
    let translation = started(model, tree, vec![window]);
    let inside = 0x7B80_0040;
    assert_eq!(
        model.access(STREAM, inside, true),
        Some(inside),
        "before an owner"
    );
    let iova = translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    assert_eq!(
        model.access(STREAM, inside, true),
        Some(inside),
        "under the owner"
    );
    assert!(
        !(0x7B80_0000..0x7B90_0000).contains(&iova),
        "the window is no IOVA"
    );
    translation.revoke(DEVICE, 1);
    assert_eq!(
        model.access(STREAM, inside, true),
        Some(inside),
        "after the owner"
    );
    assert_eq!(model.access(STREAM, iova, true), None);
}

/// A window firmware keeps for reading alone is kept for reading alone,
/// before an owner and under one.
#[test]
fn a_firmware_window_keeps_only_the_access_firmware_allows() {
    let (model, tree) = rig!(Behaviour::Correct);
    let window =
        IommuReservedWindow::new(STREAM, 0x7B80_0000, 0x10_0000, ReservedAccess::Read).unwrap();
    let translation = started(model, tree, vec![window]);
    let inside = 0x7B80_0040;
    assert_eq!(model.access(STREAM, inside, false), Some(inside));
    assert_eq!(model.access(STREAM, inside, true), None, "before an owner");
    translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    assert_eq!(model.access(STREAM, inside, false), Some(inside));
    assert_eq!(model.access(STREAM, inside, true), None, "under the owner");
}

/// A node whose messages are written to `DOORBELL`, mastering DMA as
/// `DOORBELLED_STREAM` in a group of its own.
const DOORBELLED: u32 = 8;
const DOORBELLED_STREAM: u32 = 0x0011;
const DOORBELL: Range<u64> = 0x0809_0000..0x0809_1000;

fn doorbelled_node() -> HwNode {
    let mut node = device_node(DOORBELLED, DOORBELLED_STREAM);
    node.push_resource(HwResource::msi_doorbell(DOORBELL.start, PAGE).unwrap())
        .unwrap();
    node
}

/// A node's doorbell is mapped at its own address in its domain, for writes
/// alone where the unit can leave the read out, and is no IOVA a carve gets.
#[test]
fn a_node_s_doorbell_takes_its_writes_in_its_domain_and_nothing_else() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    tree.add(&doorbelled_node());
    let inside = DOORBELL.start + 0x40;
    assert_eq!(
        model.access(DOORBELLED_STREAM, inside, true),
        None,
        "a node with no owner reaches no doorbell"
    );
    let iova = translation
        .map(DOORBELLED, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    assert_eq!(model.access(DOORBELLED_STREAM, inside, true), Some(inside));
    assert_eq!(
        model.access(DOORBELLED_STREAM, inside, false),
        None,
        "a doorbell is written, never read"
    );
    assert!(!DOORBELL.contains(&iova), "the doorbell is no IOVA");
    assert_eq!(
        model.access(STREAM, inside, true),
        None,
        "another node's domain maps no doorbell it was not given"
    );
    translation.revoke(DOORBELLED, 1);
    assert_eq!(model.access(DOORBELLED_STREAM, inside, true), None);
}

/// Where the unit's tables cannot grant a write alone, the doorbell is
/// mapped readable too rather than the node refused its domain.
#[test]
fn a_doorbell_is_readable_too_where_the_unit_grants_no_write_alone() {
    let (model, tree) = rig!(Behaviour::NoWriteOnly);
    let translation = started(model, tree, Vec::new());
    tree.add(&doorbelled_node());
    translation
        .map(DOORBELLED, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    let inside = DOORBELL.start + 0x40;
    assert_eq!(model.access(DOORBELLED_STREAM, inside, true), Some(inside));
    assert_eq!(model.access(DOORBELLED_STREAM, inside, false), Some(inside));
}

/// Where the unit takes a write to the doorbell as a message, the domain
/// keeps the doorbell's page clear of carves and maps nothing there, and the
/// node's streams are confined to the file the port names.
#[test]
fn a_unit_intercepting_messages_keeps_the_doorbell_clear_and_confines_the_node() {
    const STREAMLESS: u32 = 9;
    let (model, tree) = rig!(Behaviour::ConfinesMessages);
    let translation = started(model, tree, Vec::new());
    tree.add(&doorbelled_node());
    tree.add(&HwNode::new(
        STREAMLESS,
        HW_NODE_ROOT_ID,
        HwDeviceClass::Network,
    ));
    let notice = Notice {
        address: DOORBELL.start,
        data: 3,
    };
    translation
        .confine_messages(DOORBELLED, DOORBELL.start, 0x9000_0200, notice)
        .unwrap();
    assert_eq!(
        model.confined(DOORBELLED_STREAM),
        Some((DOORBELL.start, 0x9000_0200, notice))
    );
    assert_eq!(
        translation.confine_messages(STREAMLESS, DOORBELL.start, 0x9000_0400, notice),
        Err(RemapError::Unsupported),
        "a node naming no stream"
    );
    let iova = translation
        .map(DOORBELLED, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    assert!(!DOORBELL.contains(&iova), "the doorbell is no IOVA");
    let inside = DOORBELL.start + 0x40;
    assert_eq!(
        model.access(DOORBELLED_STREAM, inside, false),
        None,
        "nothing is mapped there"
    );
}

/// A bridged function's messages arrive as its alias, which is confined with
/// its own streams; a stream named twice is confined once.
#[test]
fn a_node_s_aliases_are_confined_with_its_own_streams() {
    let (model, tree) = rig!(Behaviour::ConfinesMessages);
    let translation = started(model, tree, Vec::new());
    let node = aliased(aliased(doorbelled_node(), ALIAS), DOORBELLED_STREAM);
    tree.add(&node);
    let notice = Notice {
        address: DOORBELL.start,
        data: 3,
    };
    translation
        .confine_messages(DOORBELLED, DOORBELL.start, 0x9000_0200, notice)
        .unwrap();
    for stream in [DOORBELLED_STREAM, ALIAS] {
        assert_eq!(
            model.confined(stream),
            Some((DOORBELL.start, 0x9000_0200, notice)),
            "{stream}"
        );
    }
}

/// A unit that cannot confine messages is refused, the node unconfined.
#[test]
fn a_unit_that_cannot_confine_messages_refuses_the_node() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    tree.add(&doorbelled_node());
    let notice = Notice {
        address: DOORBELL.start,
        data: 3,
    };
    assert_eq!(
        translation.confine_messages(DOORBELLED, DOORBELL.start, 0x9000_0200, notice),
        Err(RemapError::Unsupported)
    );
    assert_eq!(model.confined(DOORBELLED_STREAM), None);
}

/// A unit noting, at each attach, whether the owner it watches held its state.
struct Watching {
    model: &'static ModelUnit<'static>,
    owner: SpinLock<Option<Arc<Owner>>>,
    attaches: SpinLock<Vec<(u32, bool)>>,
}

impl IommuUnit for Watching {
    fn profile(&self) -> UnitProfile {
        self.model.profile()
    }

    fn enable(&self) -> Result<(), IommuError> {
        self.model.enable()
    }

    fn create_domain(&self) -> Result<DomainId, IommuError> {
        self.model.create_domain()
    }

    fn destroy_domain(&self, domain: DomainId) -> Result<(), IommuError> {
        self.model.destroy_domain(domain)
    }

    fn attach(&self, stream: u32, domain: DomainId) -> Result<(), IommuError> {
        if let Some(owner) = &*self.owner.lock() {
            self.attaches.lock().push((stream, owner.state.is_locked()));
        }
        self.model.attach(stream, domain)
    }

    fn block(&self, stream: u32) -> Result<(), IommuError> {
        self.model.block(stream)
    }

    fn silence(&self, stream: u32) -> Result<(), IommuError> {
        self.model.silence(stream)
    }

    fn map(
        &self,
        domain: DomainId,
        iova: u64,
        phys: u64,
        len: u64,
        access: Access,
    ) -> Result<(), IommuError> {
        self.model.map(domain, iova, phys, len, access)
    }

    fn unmap(&self, domain: DomainId, iova: u64, len: u64) -> Result<(), IommuError> {
        self.model.unmap(domain, iova, len)
    }

    fn sync(&self, domain: DomainId) -> Result<(), IommuError> {
        self.model.sync(domain)
    }

    fn route_faults(&self, route: tairix_kernel_iommu_api::FaultRoute) -> Result<(), IommuError> {
        self.model.route_faults(route)
    }

    fn unroute_faults(&self) -> Result<(), IommuError> {
        self.model.unroute_faults()
    }

    fn drain_faults(&self, sink: &mut dyn FnMut(Fault)) -> bool {
        self.model.drain_faults(sink)
    }
}

#[test]
fn an_end_is_published_only_once_its_streams_are_back_with_firmware() {
    let (model, tree) = rig!(Behaviour::Correct);
    let watching: &'static Watching = Box::leak(Box::new(Watching {
        model,
        owner: SpinLock::new(None),
        attaches: SpinLock::new(Vec::new()),
    }));
    tree.add(&device_node(DEVICE, STREAM));
    let (translation, outcomes) = Translation::started(
        vec![Unit {
            node: UNIT_NODE,
            unit: watching,
            reserved: vec![firmware_window(STREAM)],
            faults: FaultSignal::Message,
            family: DmaUnitFamily::Vtd,
            counts: FaultCounts::default(),
        }],
        vec![REGISTERS],
        tree,
        audit_sink(),
        None,
    );
    assert_eq!(
        outcomes,
        [(
            UNIT_NODE,
            UnitOutcome::Translating(
                Quiesced::default(),
                tairix_kernel_iommu_api::Tables::Walked(tairix_kernel_iommu_api::Stage::Second),
            )
        )]
    );
    translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    *watching.owner.lock() = translation.owners.lock().nodes.get(&DEVICE).cloned();

    assert!(translation.revoke(DEVICE, 1));
    assert_eq!(
        *watching.attaches.lock(),
        [(STREAM, true)],
        "a successor waiting on the end cannot adopt the stream mid-restore"
    );
    assert_eq!(
        model.access(STREAM, 0x7B80_0040, false),
        Some(0x7B80_0040),
        "back in its firmware domain"
    );
}

#[test]
fn a_unit_that_will_not_enable_is_dropped_and_translates_nothing() {
    let (model, tree) = rig!(Behaviour::RefusesEnable);
    tree.add(&device_node(DEVICE, STREAM));
    let audit = audit_sink();
    let (translation, outcomes) = Translation::started(
        vec![unit(model, Vec::new())],
        vec![REGISTERS],
        tree,
        audit,
        None,
    );
    assert_eq!(
        outcomes,
        [(
            UNIT_NODE,
            UnitOutcome::Stranded(Refusal::Unit(IommuError::Hardware), Quiesced::default())
        )]
    );
    assert_eq!(translation.units(), 0);
    assert_eq!(
        translation.dma_path(DEVICE),
        DmaPath::Stranded {
            unit: Some(UNIT_NODE)
        },
        "behind a unit that translates nothing, no DMA at all"
    );
    assert!(translation.strands());
}

#[test]
fn a_unit_s_registers_are_guarded() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    assert!(translation.guards(0xFED9_0000, PAGE));
    assert!(
        translation.guards(0xFED8_F000, 2 * PAGE),
        "a window reaching in"
    );
    assert!(translation.guards(0xFED9_0FFC, 4));
    assert!(!translation.guards(0xFED9_1000, PAGE));
    assert!(!translation.guards(0xFED8_F000, PAGE));
}

/// A unit node keyed `compatible`, stating that its DMA snoops, as discovery
/// states every unit's.
fn snooping_unit(compatible: &[u8]) -> HwNode {
    let mut unit = HwNode::new(UNIT_NODE, HW_NODE_ROOT_ID, HwDeviceClass::Iommu);
    unit.push_match_key(HwMatchKey::compatible(compatible).unwrap())
        .unwrap();
    unit.push_resource(HwResource::dma(0, 0, DmaCoherence::Snooped))
        .unwrap();
    unit
}

/// Every family keeps a unit's tables, queues and records through the CPU's
/// caches, so a unit that does not snoop them, or whose node does not say it
/// does, is never taken over: the devices behind it master nothing.
#[test]
fn a_unit_that_does_not_say_it_snoops_is_refused() {
    use tairix_abi::sysinfo::DmaUnitState;
    let env = UnitEnv {
        mmio: &|_, _| panic!("no register is reached"),
        frames: &NO_FRAMES,
        coherence: None,
        clock: &NoClock,
        function: None,
    };
    let silent = {
        let mut unit = HwNode::new(UNIT_NODE, HW_NODE_ROOT_ID, HwDeviceClass::Iommu);
        unit.push_match_key(
            HwMatchKey::compatible(tairix_kernel_iommu_smmuv3::COMPATIBLE).unwrap(),
        )
        .unwrap();
        unit
    };
    let mut unsnooped = silent;
    unsnooped
        .push_resource(HwResource::dma(0, 0, DmaCoherence::Unsnooped))
        .unwrap();
    let mut mixed = snooping_unit(tairix_kernel_iommu_smmuv3::COMPATIBLE);
    mixed
        .push_resource(HwResource::dma(0, 0, DmaCoherence::Unsnooped))
        .unwrap();
    for unit in [silent, unsnooped, mixed] {
        assert_eq!(take_over(&unit, &env).err(), Some(Refusal::Unsnooped));
    }
    let (_, tree) = rig!(Behaviour::Correct);
    let translation = Translation::start(
        Vec::new(),
        vec![Stranded::of(&unsnooped, Refusal::Unsnooped)],
        Vec::new(),
        tree,
        audit_sink(),
        None,
        &mut |_, _| {},
    );
    assert_eq!(units(&translation)[0].state, DmaUnitState::Unsnooped);
}

#[test]
fn a_unit_no_family_drives_or_whose_registers_are_unreachable_is_refused() {
    let env = UnitEnv {
        mmio: &|_, _| None,
        frames: &NO_FRAMES,
        coherence: None,
        clock: &NoClock,
        function: None,
    };
    let mut other = HwNode::new(UNIT_NODE, HW_NODE_ROOT_ID, HwDeviceClass::Iommu);
    other
        .push_match_key(HwMatchKey::compatible(b"example,iommu").unwrap())
        .unwrap();
    assert_eq!(take_over(&other, &env).err(), Some(Refusal::Unmatched));

    let mut vtd = snooping_unit(tairix_kernel_iommu_vtd::COMPATIBLE);
    assert_eq!(take_over(&vtd, &env).err(), Some(Refusal::NoRegisters));
    vtd.push_resource(HwResource::mmio(0xFED9_0000, PAGE))
        .unwrap();
    assert_eq!(
        take_over(&vtd, &env).err(),
        Some(Refusal::NoRegisters),
        "a port that cannot map the registers"
    );
}

/// The register window is built over the port's mapping and the family reads
/// the unit through it: registers reporting no queued invalidation are a unit
/// this family cannot drive.
#[test]
fn a_mapped_unit_its_family_cannot_drive_is_refused() {
    #[repr(C, align(4096))]
    struct Registers([u64; 512]);
    // Outlives every access: the refused unit is dropped inside `take_over`.
    let mut registers = Registers([0; 512]);
    let window = NonNull::from(&mut registers.0).cast::<u8>();
    let env = UnitEnv {
        mmio: &move |base, len| {
            (base == 0xFED9_0000 && usize::try_from(PAGE) == Ok(len)).then_some(window)
        },
        frames: &NO_FRAMES,
        coherence: None,
        clock: &NoClock,
        function: None,
    };
    let mut vtd = snooping_unit(tairix_kernel_iommu_vtd::COMPATIBLE);
    vtd.push_resource(HwResource::mmio(0xFED9_0000, PAGE))
        .unwrap();
    assert_eq!(
        take_over(&vtd, &env).err(),
        Some(Refusal::Unit(IommuError::OutOfRange))
    );
}

/// A virtio-mmio slot named a translation unit but holding none is a take-over
/// its family failed, and its record is one a client decodes.
#[test]
fn a_virtio_mmio_slot_holding_no_virtio_iommu_lists_as_failed() {
    use tairix_abi::sysinfo::{DmaUnitFamily, DmaUnitState};
    #[repr(C, align(4096))]
    struct Registers([u64; 512]);
    // A slot whose magic reads zero: no virtio device at all.
    let mut registers = Registers([0; 512]);
    let window = NonNull::from(&mut registers.0).cast::<u8>();
    let env = UnitEnv {
        mmio: &move |base, len| {
            (base == 0xFED9_0000 && usize::try_from(PAGE) == Ok(len)).then_some(window)
        },
        frames: &NO_FRAMES,
        coherence: None,
        clock: &NoClock,
        function: None,
    };
    let mut slot = snooping_unit(tairix_virtio::transport_mmio::COMPATIBLE.as_bytes());
    slot.push_resource(HwResource::mmio(0xFED9_0000, PAGE))
        .unwrap();
    let refusal = take_over(&slot, &env)
        .err()
        .expect("no virtio-iommu answers");
    assert_eq!(refusal, Refusal::Unit(IommuError::Hardware));

    let (_, tree) = rig!(Behaviour::Correct);
    let translation = Translation::start(
        Vec::new(),
        vec![Stranded::of(&slot, refusal)],
        Vec::new(),
        tree,
        audit_sink(),
        None,
        &mut |_, _| {},
    );
    let listed = units(&translation);
    assert_eq!(listed[0].family, DmaUnitFamily::VirtioMmio);
    assert_eq!(listed[0].state, DmaUnitState::Failed);
    assert_eq!(
        Stranded::of(&slot, Refusal::Unmatched).family,
        DmaUnitFamily::Unmatched,
        "a unit no family drives names none"
    );
}

/// An AMD-Vi node is taken over by its own family, which reads the unit
/// through the mapped window: one without host translation is refused, one
/// whose tables cannot be had is exhausted, and one whose can is kept.
#[test]
fn an_amd_vi_unit_is_taken_over_by_its_own_family() {
    const LEN: u64 = 0x4000;
    #[repr(C, align(4096))]
    struct Registers([u64; 0x800]);
    // Leaked: a unit taken over keeps reaching its registers.
    let registers = Box::leak(Box::new(Registers([0; 0x800])));
    let window = NonNull::from(&mut registers.0).cast::<u8>();
    let mut amdvi = snooping_unit(tairix_kernel_iommu_amdvi::COMPATIBLE);
    amdvi
        .push_resource(HwResource::mmio(0xFED8_0000, LEN))
        .unwrap();
    let starved: &'static tairix_kernel_iommu_api::hostmem::HostFrames = Box::leak(Box::new(
        tairix_kernel_iommu_api::hostmem::HostFrames::new(0x3_0000_0000),
    ));
    starved.limit(0);
    for (features, frames, refusal) in [
        (0b11 << 10, &NO_FRAMES, Some(IommuError::OutOfRange)),
        (0, starved, Some(IommuError::Exhausted)),
        (0, &NO_FRAMES, None),
    ] {
        // SAFETY: the window is this test's own, and no access through it is
        // live while the features word is written.
        unsafe { window.cast::<u64>().add(6).write_volatile(features) };
        let env = UnitEnv {
            mmio: &move |base, len| {
                (base == 0xFED8_0000 && usize::try_from(LEN) == Ok(len)).then_some(window)
            },
            frames,
            coherence: None,
            clock: &NoClock,
            function: None,
        };
        match take_over(&amdvi, &env) {
            Ok(unit) => {
                assert_eq!(refusal, None);
                assert_eq!(unit.node, UNIT_NODE);
                assert_eq!(unit.unit.profile().reach.input_bits, 48);
            }
            Err(refused) => assert_eq!(Some(refused), refusal.map(Refusal::Unit)),
        }
    }
}

struct NoClock;

impl Clock for NoClock {
    fn now_ns(&self) -> u64 {
        0
    }
}

static NO_FRAMES: tairix_kernel_iommu_api::hostmem::HostFrames =
    tairix_kernel_iommu_api::hostmem::HostFrames::new(0x2_0000_0000);

fn audit_sink() -> &'static TestSink {
    Box::leak(Box::new(TestSink::new()))
}

fn recorded(sink: &TestSink, event: AuditEvent) -> Vec<CapturedEvent> {
    sink.snapshot()
        .into_iter()
        .filter(|captured| captured.id == event.id())
        .collect()
}

fn field<'a>(event: &'a CapturedEvent, key: &str) -> Option<&'a str> {
    event
        .fields
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

const SMALL: FaultLimits = FaultLimits {
    window_ns: 1_000_000_000,
    stream_records: 1,
    unit_records: 8,
    storm: 3,
    drains: 8,
    streams: 4,
};

#[test]
fn a_fault_on_an_owned_stream_is_recorded_against_its_device() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    assert_eq!(model.access(STREAM, PAGE, true), None);
    let sink = audit_sink();
    let mut budget = FaultBudget::new(FAULT_LIMITS, 0).unwrap();
    assert!(!translation.drain_pass(0, &mut budget, sink, &NoClock));
    let faults = recorded(sink, AuditEvent::DmaTranslationFault);
    assert_eq!(faults.len(), 1);
    for (key, value) in [
        ("unit", "100"),
        ("node", "7"),
        ("stream", "16"),
        ("iova", "4096"),
        ("access", "write"),
        ("reason", "unmapped"),
        ("suppressed", "0"),
    ] {
        assert_eq!(field(&faults[0], key), Some(value), "{key}");
    }
}

#[test]
fn a_full_queue_of_one_stream_s_records_drained_at_once_storms_nothing() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    for _ in 0..tairix_kernel_iommu_api::FAULT_QUEUE_RECORDS {
        assert_eq!(model.access(STREAM, PAGE, true), None);
    }
    let sink = audit_sink();
    let mut budget = FaultBudget::new(FAULT_LIMITS, 0).unwrap();
    translation.drain_pass(0, &mut budget, sink, &NoClock);
    assert!(
        recorded(sink, AuditEvent::DmaTranslationStorm).is_empty(),
        "a backlog one drain finds is no storm"
    );
    assert!(!model.silenced(STREAM));
}

fn units(translation: &Translation) -> Vec<tairix_abi::sysinfo::DmaUnitRecord> {
    let bytes = translation.unit_records(0, 16).unwrap();
    bytes
        .chunks(tairix_abi::sysinfo::DmaUnitRecord::WIRE_LEN)
        .map(|chunk| tairix_abi::sysinfo::DmaUnitRecord::from_bytes(chunk).unwrap())
        .collect()
}

#[test]
fn an_administrator_reads_each_unit_its_owners_and_what_its_faults_came_to() {
    use tairix_abi::sysinfo::{DmaFaultSignal, DmaTables, DmaUnitState};
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    let [unit] = units(&translation)[..] else {
        panic!("one unit");
    };
    assert_eq!(unit.node, UNIT_NODE);
    assert_eq!(unit.state, DmaUnitState::Translating);
    assert_eq!(
        unit.faults,
        DmaFaultSignal::Unheard,
        "nothing drains them yet"
    );
    assert_eq!(unit.tables, DmaTables::SecondStage);
    assert_eq!(unit.owners, 0);

    translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    let issued = 3;
    for _ in 0..issued {
        assert_eq!(model.access(STREAM, PAGE, true), None);
    }
    let sink = audit_sink();
    let mut budget = FaultBudget::new(SMALL, 0).unwrap();
    translation.drain_pass(0, &mut budget, sink, &NoClock);
    let storms = recorded(sink, AuditEvent::DmaTranslationStorm).len() as u64;
    let audited = recorded(sink, AuditEvent::DmaTranslationFault).len() as u64 + storms;
    let unit = units(&translation)[0];
    assert_eq!(unit.owners, 1);
    assert_eq!(unit.faults_recorded, audited);
    assert_eq!(unit.faults_dropped, issued - audited);
    assert!(unit.faults_dropped > 0, "the stream's share ran out");
    assert_eq!(
        unit.streams_silenced, storms,
        "the storm silenced its stream"
    );
    assert!(model.silenced(STREAM));
    assert!(
        translation.unit_records(1, 16).unwrap().is_empty(),
        "paged past the end"
    );
}

/// A unit is listed by how its faults reach the kernel only while a task
/// drains them: one whose message could not be had is heard by nothing.
#[test]
fn a_unit_is_listed_by_how_its_faults_are_heard() {
    use tairix_abi::sysinfo::DmaFaultSignal;
    for (msi, heard) in [
        (
            Some(vector() as &'static dyn KernelMsiFacility),
            DmaFaultSignal::Message,
        ),
        (None, DmaFaultSignal::Unheard),
    ] {
        let (model, tree) = rig!(Behaviour::Correct);
        let translation: &'static Translation =
            Box::leak(Box::new(started(model, tree, Vec::new())));
        let env = FaultEnv {
            table: Box::leak(Box::new(IrqTable::new(31))),
            controller: &OK_CONTROLLER,
            msi,
            audit: audit_sink(),
            clock: &NoClock,
        };
        translation.serve_faults(&env, |_body| Some(0x77));
        assert_eq!(units(translation)[0].faults, heard);
    }
}

#[test]
fn a_stranded_unit_is_listed_after_those_translating_with_why() {
    use tairix_abi::sysinfo::{DmaFaultSignal, DmaUnitFamily, DmaUnitState};
    let (model, tree) = rig!(Behaviour::Correct);
    tree.add(&device_node(DEVICE, STREAM));
    let translation = Translation::start(
        vec![unit(model, Vec::new())],
        vec![
            Stranded {
                node: UNIT_NODE + 1,
                family: DmaUnitFamily::Smmuv3,
                refusal: Refusal::NoRegisters,
            },
            Stranded {
                node: UNIT_NODE + 2,
                family: DmaUnitFamily::Vtd,
                refusal: Refusal::Unit(IommuError::Hardware),
            },
            Stranded {
                node: UNIT_NODE + 3,
                family: DmaUnitFamily::Unmatched,
                refusal: Refusal::Unmatched,
            },
        ],
        vec![REGISTERS],
        tree,
        audit_sink(),
        None,
        &mut |_, _| {},
    );
    // Every stranded kind decodes through the same fail-closed reader a
    // client uses, so the kernel never lists one the reader would refuse.
    let listed = units(&translation);
    assert_eq!(listed.len(), 4);
    assert_eq!(listed[0].state, DmaUnitState::Translating);
    assert_eq!(listed[1].node, UNIT_NODE + 1);
    assert_eq!(listed[1].state, DmaUnitState::NoRegisters);
    assert_eq!(listed[1].faults, DmaFaultSignal::None);
    assert_eq!(
        listed[1].family,
        DmaUnitFamily::Smmuv3,
        "the family is the one discovery matched, though the tree holds no such node now"
    );
    assert_eq!(
        (listed[2].state, listed[2].family),
        (DmaUnitState::Failed, DmaUnitFamily::Vtd)
    );
    assert_eq!(
        (listed[3].state, listed[3].family),
        (DmaUnitState::Unmatched, DmaUnitFamily::Unmatched)
    );
    let second = translation.unit_records(1, 1).unwrap();
    let second = tairix_abi::sysinfo::DmaUnitRecord::from_bytes(&second).unwrap();
    assert_eq!(second.node, UNIT_NODE + 1, "paged one by one");
}

#[test]
fn a_node_s_translation_state_and_its_group_s_holder_are_read_from_its_owner() {
    use tairix_abi::sysinfo::{DmaGroupRecord, DmaNodeRecord, DmaOwnerState};
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    assert!(translation.node_records(0, 8).unwrap().is_empty());
    translation
        .map(DEVICE, 4, &[block(0x8000_0000)], 0)
        .unwrap();
    translation
        .map(DEVICE, 4, &[block(0x9000_0000)], 0)
        .unwrap();
    let node = DmaNodeRecord::from_bytes(&translation.node_records(0, 8).unwrap()).unwrap();
    assert_eq!(node.node, DEVICE);
    assert_eq!(node.unit, UNIT_NODE);
    assert_eq!(node.group, STREAM);
    assert_eq!(node.state, DmaOwnerState::Live);
    assert_eq!(node.generation, 4);
    assert_eq!(node.streams, 1);
    assert_eq!(node.mappings, 2);
    assert_eq!(node.mapped_bytes, 2 * PAGE);
    let group = DmaGroupRecord::from_bytes(&translation.group_records(0, 8).unwrap()).unwrap();
    assert_eq!(
        (group.unit, group.group, group.holder, group.generation),
        (UNIT_NODE, STREAM, DEVICE, 4)
    );
    assert!(translation.revoke(DEVICE, 4));
    let node = DmaNodeRecord::from_bytes(&translation.node_records(0, 8).unwrap()).unwrap();
    assert_eq!(node.state, DmaOwnerState::Ended);
    assert_eq!((node.mappings, node.mapped_bytes), (0, 0));
}

#[test]
fn a_fault_no_owner_holds_is_recorded_against_the_unit() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    assert_eq!(model.access(0x0018, PAGE, false), None);
    let sink = audit_sink();
    let mut budget = FaultBudget::new(FAULT_LIMITS, 0).unwrap();
    translation.drain_pass(0, &mut budget, sink, &NoClock);
    let faults = recorded(sink, AuditEvent::DmaTranslationFault);
    assert_eq!(faults.len(), 1);
    assert_eq!(field(&faults[0], "unit"), Some("100"));
    assert_eq!(field(&faults[0], "node"), None);
    assert_eq!(field(&faults[0], "reason"), Some("blocked"));
}

#[test]
fn a_dead_driver_s_device_is_still_named_and_a_forgotten_node_s_is_not() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    translation.revoke(DEVICE, 1);
    let sink = audit_sink();
    let mut budget = FaultBudget::new(FAULT_LIMITS, 0).unwrap();
    assert_eq!(model.access(STREAM, PAGE, true), None);
    translation.drain_pass(0, &mut budget, sink, &NoClock);
    tree.drop_node(DEVICE);
    assert!(translation.forget(DEVICE));
    assert_eq!(model.access(STREAM, 2 * PAGE, true), None);
    translation.drain_pass(0, &mut budget, sink, &NoClock);
    let faults = recorded(sink, AuditEvent::DmaTranslationFault);
    assert_eq!(field(&faults[0], "node"), Some("7"));
    assert_eq!(field(&faults[1], "node"), None);
}

#[test]
fn a_storm_silences_the_stream_marks_its_node_offline_and_is_recorded_once() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    for page in 1..=5 {
        let _ = model.access(STREAM, page * PAGE, true);
    }
    let sink = audit_sink();
    let mut budget = FaultBudget::new(SMALL, 0).unwrap();
    translation.drain_pass(0, &mut budget, sink, &NoClock);
    assert_eq!(recorded(sink, AuditEvent::DmaTranslationFault).len(), 1);
    let storms = recorded(sink, AuditEvent::DmaTranslationStorm);
    assert_eq!(storms.len(), 1);
    assert_eq!(field(&storms[0], "outcome"), Some("silenced"));
    assert_eq!(field(&storms[0], "node"), Some("7"));
    assert_eq!(field(&storms[0], "suppressed"), Some("1"));
    assert!(model.silenced(STREAM));
    assert_eq!(tree.health_of(DEVICE), Some(FaultDomainState::Offline));
    assert_eq!(model.access(STREAM, PAGE, true), None);
    sink.clear();
    translation.drain_pass(0, &mut budget, sink, &NoClock);
    assert!(
        sink.snapshot().is_empty(),
        "a silenced stream raises nothing"
    );
}

/// A storm the unit refuses to silence is recorded even once the unit's
/// share of the window is spent: containment failing is never anonymous.
#[test]
fn a_storm_left_uncontained_is_recorded_whatever_the_share() {
    let (model, tree) = rig!(Behaviour::RefusesSilence);
    let translation = started(model, tree, Vec::new());
    translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    for page in 1..=5 {
        let _ = model.access(STREAM, page * PAGE, true);
    }
    let sink = audit_sink();
    let mut budget = FaultBudget::new(
        FaultLimits {
            unit_records: 1,
            ..SMALL
        },
        0,
    )
    .unwrap();
    translation.drain_pass(0, &mut budget, sink, &NoClock);
    assert_eq!(recorded(sink, AuditEvent::DmaTranslationFault).len(), 1);
    let storms = recorded(sink, AuditEvent::DmaTranslationStorm);
    assert_eq!(storms.len(), 1, "past the share, yet recorded");
    assert_eq!(field(&storms[0], "outcome"), Some("refused"));
}

/// An adoption that fails puts the node's record back, so a revoked
/// generation still carves nothing, and leaves the stream's attribution with
/// the owner that holds it.
#[test]
fn a_failed_adoption_puts_the_revoked_predecessor_back() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    // Disagreeing groups on one stream, so only the unit can refuse it.
    tree.add(&grouped(9, STREAM, STREAM + 1));
    translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    assert!(translation.revoke(DEVICE, 1));
    translation.map(9, 1, &[block(0x9000_0000)], 0).unwrap();
    assert_eq!(
        translation.map(DEVICE, 2, &[block(0xA000_0000)], 0),
        Err(DmaError::Translation),
        "another node's owner holds the stream"
    );
    assert_eq!(
        translation.map(DEVICE, 1, &[block(0xA000_0000)], 0),
        Err(DmaError::DeviceGone),
        "the revoked generation is the record again"
    );
    assert_eq!(
        translation.node_of(0, STREAM),
        Some(9),
        "the refused adoption touched no attribution"
    );
}

struct OkController;

impl IrqController for OkController {
    fn mask(&self, _line: u32) -> Result<(), MaskError> {
        Ok(())
    }
}

static OK_CONTROLLER: OkController = OkController;

/// A port's one vector, counting each time it is given back.
struct OneVector {
    vector: MsiAllocation,
    released: core::sync::atomic::AtomicUsize,
}

impl OneVector {
    fn released(&self) -> usize {
        self.released.load(core::sync::atomic::Ordering::Relaxed)
    }
}

impl KernelMsiFacility for OneVector {
    fn allocate(&self) -> Result<MsiAllocation, Errno> {
        Ok(self.vector)
    }

    fn release(&self, allocation: &MsiAllocation) {
        assert_eq!(*allocation, self.vector, "only what was allocated");
        self.released
            .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    }
}

const LINE: u32 = 9;

/// A vector of the test's own, so the releases counted are its alone.
fn vector() -> &'static OneVector {
    Box::leak(Box::new(OneVector {
        vector: MsiAllocation::new(0xFEE0_0000, 0x41, LINE),
        released: core::sync::atomic::AtomicUsize::new(0),
    }))
}

fn serving(
    translation: Translation,
    msi: Option<&'static dyn KernelMsiFacility>,
) -> (&'static IrqTable, &'static TestSink, usize) {
    serving_through(translation, msi, &OK_CONTROLLER)
}

fn serving_through(
    translation: Translation,
    msi: Option<&'static dyn KernelMsiFacility>,
    controller: &'static (dyn IrqController + Sync),
) -> (&'static IrqTable, &'static TestSink, usize) {
    let translation: &'static Translation = Box::leak(Box::new(translation));
    let table: &'static IrqTable = Box::leak(Box::new(IrqTable::new(31)));
    let sink = audit_sink();
    let env = FaultEnv {
        table,
        controller,
        msi,
        audit: sink,
        clock: &NoClock,
    };
    let mut admitted = 0;
    translation.serve_faults(&env, |_body| {
        admitted += 1;
        Some(0x77)
    });
    (table, sink, admitted)
}

/// A controller that counts the lines re-armed through it.
#[derive(Default)]
struct Rearms(core::sync::atomic::AtomicUsize);

impl IrqController for Rearms {
    fn mask(&self, _line: u32) -> Result<(), MaskError> {
        Ok(())
    }

    fn rearm(&self, _line: u32) -> Result<(), MaskError> {
        self.0.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

struct Spins;

impl crate::kthread::YieldHandle for Spins {
    fn yield_now(&mut self) {}

    fn park(&mut self) {}
}

/// A unit whose records never run out spends the window's drains; its
/// service then sleeps the window out with the line still masked, never
/// re-arming a line the unit may still assert.
#[test]
fn a_window_whose_drains_are_spent_is_slept_out_with_the_line_masked() {
    let (model, tree) = rig!(Behaviour::EndlessFaults);
    let translation: &'static Translation = Box::leak(Box::new(started(model, tree, Vec::new())));
    let table: &'static IrqTable = Box::leak(Box::new(IrqTable::new(31)));
    let controller: &'static Rearms = Box::leak(Box::new(Rearms::default()));
    let sink = audit_sink();
    let env = FaultEnv {
        table,
        controller,
        msi: Some(vector()),
        audit: sink,
        clock: &NoClock,
    };
    let mut bodies = Vec::new();
    translation.serve_faults(&env, |body| {
        bodies.push(body);
        Some(0x77)
    });
    let mut body = bodies.pop().expect("one unit served");
    body(&mut Spins);
    assert_eq!(
        controller.0.load(core::sync::atomic::Ordering::Relaxed),
        0,
        "no line re-armed while its window's drains are spent"
    );
}

#[test]
fn each_unit_s_fault_interrupt_is_routed_bound_to_the_kernel_and_served() {
    let (model, tree) = rig!(Behaviour::Correct);
    let msi = vector();
    let (table, sink, admitted) = serving(started(model, tree, Vec::new()), Some(msi));
    assert_eq!(admitted, 1);
    assert_eq!(msi.released(), 0, "held while it is served");
    assert_eq!(
        model.routed(),
        Some(tairix_kernel_iommu_api::FaultRoute::Message {
            address: 0xFEE0_0000,
            data: 0x41
        })
    );
    assert_eq!(table.owner_of_line(LINE), Some(FAULT_OWNER));
    assert!(recorded(sink, AuditEvent::DmaTranslationUnit).is_empty());
}

#[test]
fn a_unit_that_refuses_its_route_is_audited_and_its_line_released() {
    let (model, tree) = rig!(Behaviour::RefusesRoute);
    let msi = vector();
    let (table, sink, admitted) = serving(started(model, tree, Vec::new()), Some(msi));
    assert_eq!(admitted, 0);
    assert_eq!(table.owner_of_line(LINE), None);
    assert_eq!(msi.released(), 1, "told to stop, it gives its vector back");
    let unit = recorded(sink, AuditEvent::DmaTranslationUnit);
    assert_eq!(field(&unit[0], "outcome"), Some("faults_unrouted"));
    assert_eq!(field(&unit[0], "reason"), Some("refused"));
}

/// Serve `translation`'s one unit, admitting `admit` services, with a
/// vector of its own and the line `held` already bound to another owner.
fn served_once(
    translation: Translation,
    admit: bool,
    held: bool,
) -> (
    &'static OneVector,
    &'static TestSink,
    Vec<crate::kthread::KernelServiceBody>,
) {
    let translation: &'static Translation = Box::leak(Box::new(translation));
    let table: &'static IrqTable = Box::leak(Box::new(IrqTable::new(31)));
    if held {
        table
            .bind_exclusive(LINE, tairix_kernel_sec::ProcessId(0x99))
            .unwrap();
    }
    let msi = vector();
    let sink = audit_sink();
    let env = FaultEnv {
        table,
        controller: &OK_CONTROLLER,
        msi: Some(msi),
        audit: sink,
        clock: &NoClock,
    };
    let mut bodies = Vec::new();
    translation.serve_faults(&env, |body| {
        if admit {
            bodies.push(body);
            Some(0x77)
        } else {
            None
        }
    });
    if admit {
        let _ = table.release_for(FAULT_OWNER, &OK_CONTROLLER);
    }
    (msi, sink, bodies)
}

/// A vector no unit was told is the port's again at once; one a unit was
/// told comes back only once the unit is told to stop raising it, and a unit
/// that will not stop keeps it.
#[test]
fn a_unit_s_vector_comes_back_only_once_nothing_can_raise_it() {
    let (model, tree) = rig!(Behaviour::Correct);
    let (msi, sink, _) = served_once(started(model, tree, Vec::new()), true, true);
    assert_eq!(model.routed(), None, "never told");
    assert_eq!(msi.released(), 1);
    let unit = recorded(sink, AuditEvent::DmaTranslationUnit);
    assert_eq!(field(&unit[0], "reason"), Some("unbound"));

    let (model, tree) = rig!(Behaviour::Correct);
    let (msi, sink, _) = served_once(started(model, tree, Vec::new()), false, false);
    assert_eq!(model.routed(), None, "told to stop");
    assert_eq!(msi.released(), 1);
    let unit = recorded(sink, AuditEvent::DmaTranslationUnit);
    assert_eq!(field(&unit[0], "reason"), Some("not_admitted"));

    let (model, tree) = rig!(Behaviour::RefusesUnroute);
    let (msi, _, _) = served_once(started(model, tree, Vec::new()), false, false);
    assert!(model.routed().is_some(), "it will not stop");
    assert_eq!(msi.released(), 0, "so its vector is kept");
}

/// A service that can no longer wait on its interrupt tells its unit to stop
/// raising it, then gives its vector back, once however often it is run.
#[test]
fn a_service_that_can_no_longer_wait_stops_its_unit_and_gives_its_vector_back() {
    let (model, tree) = rig!(Behaviour::Correct);
    let (msi, sink, mut bodies) = served_once(started(model, tree, Vec::new()), true, false);
    assert!(model.routed().is_some());
    assert_eq!(msi.released(), 0);
    let mut body = bodies.pop().expect("one unit served");
    body(&mut Spins);
    body(&mut Spins);
    assert_eq!(model.routed(), None);
    assert_eq!(msi.released(), 1);
    let unit = recorded(sink, AuditEvent::DmaTranslationUnit);
    assert_eq!(field(&unit[0], "reason"), Some("unbound"));
}

/// Records each line it is asked to trigger, and gives an edge only where it
/// can.
struct Triggering {
    edge: bool,
    asked: SpinLock<Vec<(u32, tairix_kernel_irq::Trigger)>>,
}

impl IrqController for Triggering {
    fn mask(&self, _line: u32) -> Result<(), MaskError> {
        Ok(())
    }

    fn set_trigger(&self, line: u32, trigger: tairix_kernel_irq::Trigger) -> Result<(), MaskError> {
        self.asked.lock().push((line, trigger));
        if trigger == tairix_kernel_irq::Trigger::Edge && !self.edge {
            return Err(MaskError::Unsupported);
        }
        Ok(())
    }
}

const WIRED: WiredFaults = WiredFaults {
    line: 20,
    trigger: tairix_kernel_irq::Trigger::Edge,
    place: 1,
};

fn wired(model: &'static ModelUnit<'static>, tree: &'static Tree) -> Translation {
    Translation::started(
        vec![Unit {
            node: UNIT_NODE,
            unit: model,
            reserved: Vec::new(),
            faults: FaultSignal::Wired(WIRED),
            family: DmaUnitFamily::Vtd,
            counts: FaultCounts::default(),
        }],
        vec![REGISTERS],
        tree,
        audit_sink(),
        None,
    )
    .0
}

/// A unit whose node names a wired fault line is served on it, edge-triggered
/// as its node says, with no message the port need allocate.
#[test]
fn a_unit_naming_a_wired_fault_line_is_served_there() {
    let (model, tree) = rig!(Behaviour::Correct);
    let controller: &'static Triggering = Box::leak(Box::new(Triggering {
        edge: true,
        asked: SpinLock::new(Vec::new()),
    }));
    let (table, _, admitted) = serving_through(wired(model, tree), None, controller);
    assert_eq!(admitted, 1);
    assert_eq!(
        model.routed(),
        Some(tairix_kernel_iommu_api::FaultRoute::Wired { place: 1 })
    );
    assert_eq!(table.owner_of_line(WIRED.line), Some(FAULT_OWNER));
    assert_eq!(
        controller.asked.lock().as_slice(),
        [(WIRED.line, WIRED.trigger)]
    );
}

/// A wired line its controller cannot trigger as the node says would lose its
/// pulses, so the unit's faults stay unrouted and the line unbound.
#[test]
fn a_wired_fault_line_its_controller_cannot_trigger_is_unrouted() {
    let (model, tree) = rig!(Behaviour::Correct);
    let controller: &'static Triggering = Box::leak(Box::new(Triggering {
        edge: false,
        asked: SpinLock::new(Vec::new()),
    }));
    let (table, sink, admitted) = serving_through(wired(model, tree), None, controller);
    assert_eq!(admitted, 0);
    assert_eq!(model.routed(), None);
    assert_eq!(table.owner_of_line(WIRED.line), None);
    let unit = recorded(sink, AuditEvent::DmaTranslationUnit);
    assert_eq!(field(&unit[0], "reason"), Some("untriggerable"));
}

/// Has nothing to give any line.
struct Unactivating;

impl IrqController for Unactivating {
    fn mask(&self, _line: u32) -> Result<(), MaskError> {
        Ok(())
    }

    fn activate(&self, _line: u32) -> Result<(), tairix_kernel_irq::ActivationError> {
        Err(tairix_kernel_irq::ActivationError::Exhausted)
    }
}

/// A wired line its controller has nothing to give would never deliver, so
/// the unit's faults stay unrouted and the line unbound.
#[test]
fn a_wired_fault_line_its_controller_cannot_activate_is_unrouted() {
    static UNACTIVATING: Unactivating = Unactivating;
    let (model, tree) = rig!(Behaviour::Correct);
    let (table, sink, admitted) = serving_through(wired(model, tree), None, &UNACTIVATING);
    assert_eq!(admitted, 0);
    assert_eq!(model.routed(), None);
    assert_eq!(table.owner_of_line(WIRED.line), None);
    let unit = recorded(sink, AuditEvent::DmaTranslationUnit);
    assert_eq!(field(&unit[0], "reason"), Some("unactivated"));
}

/// A virtio-iommu function raises its faults on the line its node names, its
/// MSI-X table not consulted; else through its MSI-X entry; else nowhere the
/// kernel hears.
#[test]
fn a_virtio_function_raises_its_faults_where_it_can_be_heard() {
    let pci = DmaUnitFamily::VirtioPci;
    let unread = || -> Result<u16, IommuError> { panic!("read with a line named") };
    assert_eq!(
        fault_signal(pci, Some(WIRED), unread),
        Ok(FaultSignal::Wired(WIRED))
    );
    assert_eq!(fault_signal(pci, None, || Ok(1)), Ok(FaultSignal::Message));
    assert_eq!(fault_signal(pci, None, || Ok(0)), Ok(FaultSignal::Unheard));
    assert_eq!(
        fault_signal(pci, None, || Err(IommuError::Hardware)),
        Err(IommuError::Hardware)
    );
}

/// A virtio-mmio slot has its one line and no message to send, so a slot
/// whose node names no line is heard by nothing; every other family raises
/// a message instead.
#[test]
fn a_slot_naming_no_line_is_unheard_where_other_units_send_messages() {
    let unread = || -> Result<u16, IommuError> { panic!("no MSI-X table to read") };
    assert_eq!(
        fault_signal(DmaUnitFamily::VirtioMmio, None, unread),
        Ok(FaultSignal::Unheard)
    );
    assert_eq!(
        fault_signal(DmaUnitFamily::VirtioMmio, Some(WIRED), unread),
        Ok(FaultSignal::Wired(WIRED))
    );
    for family in [
        DmaUnitFamily::Vtd,
        DmaUnitFamily::AmdVi,
        DmaUnitFamily::Smmuv3,
        DmaUnitFamily::Riscv,
    ] {
        assert_eq!(fault_signal(family, None, unread), Ok(FaultSignal::Message));
    }
}

/// A unit nothing hears — no line the platform describes, no message it can
/// raise — has no line bound and no message allocated, and the reason is
/// recorded.
#[test]
fn a_unit_nothing_hears_has_its_faults_unrouted() {
    let (model, tree) = rig!(Behaviour::Correct);
    let controller: &'static Triggering = Box::leak(Box::new(Triggering {
        edge: true,
        asked: SpinLock::new(Vec::new()),
    }));
    let translation = Translation::started(
        vec![Unit {
            node: UNIT_NODE,
            unit: model,
            reserved: Vec::new(),
            faults: FaultSignal::Unheard,
            family: DmaUnitFamily::Vtd,
            counts: FaultCounts::default(),
        }],
        vec![REGISTERS],
        tree,
        audit_sink(),
        None,
    )
    .0;
    let (_, sink, admitted) = serving_through(translation, None, controller);
    assert_eq!(admitted, 0);
    assert_eq!(model.routed(), None);
    assert!(controller.asked.lock().is_empty());
    let unit = recorded(sink, AuditEvent::DmaTranslationUnit);
    assert_eq!(field(&unit[0], "reason"), Some("no_line"));
}

/// A wired line another owner holds is not the kernel's to retrigger: the
/// unit's faults stay unrouted and the holder's line is left as it was.
#[test]
fn a_wired_fault_line_another_owner_holds_keeps_its_trigger() {
    let (model, tree) = rig!(Behaviour::Correct);
    let controller: &'static Triggering = Box::leak(Box::new(Triggering {
        edge: true,
        asked: SpinLock::new(Vec::new()),
    }));
    let translation: &'static Translation = Box::leak(Box::new(wired(model, tree)));
    let table: &'static IrqTable = Box::leak(Box::new(IrqTable::new(31)));
    let holder = tairix_kernel_sec::ProcessId(0x99);
    table.bind_exclusive(WIRED.line, holder).unwrap();
    let sink = audit_sink();
    let env = FaultEnv {
        table,
        controller,
        msi: None,
        audit: sink,
        clock: &NoClock,
    };
    translation.serve_faults(&env, |_body| Some(0x77));
    assert!(controller.asked.lock().is_empty(), "no trigger changed");
    assert_eq!(table.owner_of_line(WIRED.line), Some(holder));
    let unit = recorded(sink, AuditEvent::DmaTranslationUnit);
    assert_eq!(field(&unit[0], "reason"), Some("unbound"));
}

/// The line a unit's node names for its faults is the interrupt at the place
/// the node states, with that interrupt's trigger.
#[test]
fn a_unit_s_fault_line_is_the_interrupt_at_the_place_its_node_states() {
    let mut node = HwNode::new(UNIT_NODE, HW_NODE_ROOT_ID, HwDeviceClass::Iommu);
    node.push_resource(HwResource::irq_at(106, 0)).unwrap();
    node.push_resource(HwResource::edge_irq_at(109, 3)).unwrap();
    assert_eq!(WiredFaults::of(&node), None, "no place stated");
    node.push_resource(HwResource::property(HwProperty::FaultInterrupt, 3))
        .unwrap();
    assert_eq!(
        WiredFaults::of(&node),
        Some(WiredFaults {
            line: 109,
            trigger: tairix_kernel_irq::Trigger::Edge,
            place: 3,
        })
    );
    let mut stray = HwNode::new(UNIT_NODE, HW_NODE_ROOT_ID, HwDeviceClass::Iommu);
    stray.push_resource(HwResource::irq_at(106, 0)).unwrap();
    stray
        .push_resource(HwResource::property(HwProperty::FaultInterrupt, 1))
        .unwrap();
    assert_eq!(WiredFaults::of(&stray), None, "a place with no line");
}

#[test]
fn a_port_with_no_kernel_vector_leaves_every_unit_s_faults_unrouted() {
    let (model, tree) = rig!(Behaviour::Correct);
    let (_, sink, admitted) = serving(started(model, tree, Vec::new()), None);
    assert_eq!(admitted, 0);
    let unit = recorded(sink, AuditEvent::DmaTranslationUnit);
    assert_eq!(field(&unit[0], "reason"), Some("no_vector"));
}

/// A free the unit could not confirm is recorded, once per owner however
/// often the driver retries it.
#[test]
fn an_unconfirmed_free_is_audited_once_per_owner() {
    let (model, tree) = rig!(Behaviour::UnconfirmedSync);
    tree.add(&device_node(DEVICE, STREAM));
    let sink = audit_sink();
    let (translation, _) = Translation::started(
        vec![unit(model, Vec::new())],
        vec![REGISTERS],
        tree,
        sink,
        None,
    );
    let iova = translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    for _ in 0..3 {
        assert_eq!(
            translation.unmap(DEVICE, 1, iova),
            Err(DmaError::Unconfirmed)
        );
    }
    let unconfirmed = recorded(sink, AuditEvent::DmaTranslationUnconfirmed);
    assert_eq!(unconfirmed.len(), 1);
    assert_eq!(field(&unconfirmed[0], "node"), Some("7"));
    assert_eq!(field(&unconfirmed[0], "generation"), Some("1"));
}

/// A firmware window on `stream`.
fn firmware_window(stream: u32) -> IommuReservedWindow {
    IommuReservedWindow::new(stream, 0x7B80_0000, 0x10_0000, ReservedAccess::ReadWrite).unwrap()
}

#[test]
fn an_owner_s_function_masters_once_its_domain_is_attached() {
    let (model, tree) = rig!(Behaviour::Correct);
    let port = Port::new(model, Vec::new());
    let audit = audit_sink();
    let translation = started_with(model, tree, Vec::new(), Some(port), audit);
    assert!(port.calls().is_empty(), "nothing masters before an owner");
    with_log_level(Level::Info, || {
        translation
            .map(DEVICE, 1, &[block(0x8000_0000)], 0)
            .unwrap();
        translation
            .map(DEVICE, 1, &[block(0x9000_0000)], 0)
            .unwrap();
    });
    let calls = port.calls();
    assert_eq!(calls.len(), 1, "granted once, at the owner's first carve");
    assert_eq!((&calls[0].0, calls[0].1), (&streams(STREAM), true));
    assert!(calls[0].2.is_some(), "its domain held the stream first");
    let granted = recorded(audit, AuditEvent::DmaBusMaster);
    assert_eq!(granted.len(), 1);
    assert_eq!(field(&granted[0], "node"), Some("7"));
    assert_eq!(field(&granted[0], "master"), Some("on"));
    assert_eq!(field(&granted[0], "outcome"), Some("applied"));
}

#[test]
fn an_owner_s_function_stops_mastering_before_its_domain_is_destroyed() {
    let (model, tree) = rig!(Behaviour::Correct);
    let port = Port::new(model, Vec::new());
    let audit = audit_sink();
    let translation = started_with(model, tree, Vec::new(), Some(port), audit);
    with_log_level(Level::Info, || {
        translation
            .map(DEVICE, 1, &[block(0x8000_0000)], 0)
            .unwrap();
        assert!(translation.revoke(DEVICE, 1));
    });
    let calls = port.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!((&calls[1].0, calls[1].1), (&streams(STREAM), false));
    assert_eq!(
        calls[1].2, calls[0].2,
        "stopped while its own domain still held the stream"
    );
    assert_eq!(model.attached(STREAM), None, "then blocked");
    let records = recorded(audit, AuditEvent::DmaBusMaster);
    assert_eq!(field(&records[1], "master"), Some("off"));
}

#[test]
fn a_predecessor_stops_mastering_before_its_successor_masters() {
    let (model, tree) = rig!(Behaviour::Correct);
    let port = Port::new(model, Vec::new());
    let translation = started_with(model, tree, Vec::new(), Some(port), audit_sink());
    translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    translation
        .map(DEVICE, 2, &[block(0x9000_0000)], 0)
        .unwrap();
    let calls = port.calls();
    let order: Vec<bool> = calls.iter().map(|call| call.1).collect();
    assert_eq!(order, [true, false, true]);
    assert_eq!(
        calls[1].2, calls[0].2,
        "the predecessor stopped in its domain"
    );
    assert_ne!(calls[2].2, calls[0].2, "the successor masters in its own");
    let generations: Vec<u64> = calls.iter().map(|call| call.3).collect();
    assert_eq!(generations, [1, 1, 2], "each change names its owner");
}

#[test]
fn a_removed_node_s_function_stops_mastering() {
    let (model, tree) = rig!(Behaviour::Correct);
    let port = Port::new(model, Vec::new());
    let translation = started_with(model, tree, Vec::new(), Some(port), audit_sink());
    translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    tree.drop_node(DEVICE);
    assert!(translation.forget(DEVICE));
    let calls = port.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!((&calls[1].0, calls[1].1), (&streams(STREAM), false));
}

#[test]
fn an_unconfirmed_end_stops_mastering_and_no_successor_masters_again() {
    let (model, tree) = rig!(Behaviour::UnconfirmedBlock);
    let port = Port::new(model, Vec::new());
    let audit = audit_sink();
    let translation = started_with(model, tree, Vec::new(), Some(port), audit);
    translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    assert!(!translation.revoke(DEVICE, 1));
    assert_eq!(
        translation.map(DEVICE, 2, &[block(0x9000_0000)], 0),
        Err(DmaError::Translation)
    );
    let order: Vec<bool> = port.calls().iter().map(|call| call.1).collect();
    assert_eq!(order, [true, false]);
    let unconfirmed = recorded(audit, AuditEvent::DmaTranslationUnconfirmed);
    assert_eq!(unconfirmed.len(), 1, "the end is audited once");
    assert_eq!(field(&unconfirmed[0], "generation"), Some("1"));
}

#[test]
fn a_failed_adoption_never_masters() {
    // Adopting the stream gives up its firmware domain first; a block the
    // unit cannot confirm fails the adoption.
    let (model, tree) = rig!(Behaviour::UnconfirmedBlock);
    let port = Port::new(model, Vec::new());
    let translation = started_with(
        model,
        tree,
        vec![firmware_window(STREAM)],
        Some(port),
        audit_sink(),
    );
    assert!(translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .is_err());
    assert!(port.calls().is_empty());
}

#[test]
fn a_stream_firmware_keeps_a_window_for_keeps_mastering_past_its_owner() {
    let (model, tree) = rig!(Behaviour::Correct);
    let port = Port::new(model, Vec::new());
    let translation = started_with(
        model,
        tree,
        vec![firmware_window(STREAM)],
        Some(port),
        audit_sink(),
    );
    translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    assert!(translation.revoke(DEVICE, 1));
    let order: Vec<bool> = port.calls().iter().map(|call| call.1).collect();
    assert_eq!(order, [true], "firmware masters it again");
}

#[test]
fn a_node_that_leaves_as_its_first_carve_is_made_is_never_attached() {
    let (model, tree) = rig!(Behaviour::Correct);
    let port = Port::new(model, Vec::new());
    let translation = started_with(model, tree, Vec::new(), Some(port), audit_sink());
    tree.vanish
        .store(true, core::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        translation.map(DEVICE, 1, &[block(0x8000_0000)], 0),
        Err(DmaError::DeviceGone)
    );
    assert_eq!(model.attached(STREAM), None);
    assert!(port.calls().is_empty());
}

#[test]
fn take_over_counts_the_functions_still_mastering_without_a_firmware_window() {
    let (model, tree) = rig!(Behaviour::Correct);
    tree.add(&device_node(DEVICE, STREAM));
    let port = Port::new(model, vec![STREAM, 0x0018]);
    let audit = audit_sink();
    let (_translation, outcomes) = Translation::started(
        vec![unit(model, vec![firmware_window(0x0018)])],
        vec![REGISTERS],
        tree,
        audit,
        Some(Mastering::new(port, audit)),
    );
    assert_eq!(
        outcomes,
        [(
            UNIT_NODE,
            UnitOutcome::Translating(
                Quiesced {
                    stopped: 1,
                    refused: 0,
                },
                tairix_kernel_iommu_api::Tables::Walked(tairix_kernel_iommu_api::Stage::Second),
            )
        )]
    );
}

#[test]
fn a_refused_unit_s_functions_are_all_stopped_and_master_nothing() {
    let (model, tree) = rig!(Behaviour::Correct);
    tree.add(&device_node(DEVICE, STREAM));
    let port = Port::new(model, vec![STREAM, 0x0018]);
    let audit = audit_sink();
    let mut outcomes = Vec::new();
    let translation = Translation::start(
        Vec::new(),
        vec![Stranded {
            node: UNIT_NODE,
            family: DmaUnitFamily::Unmatched,
            refusal: Refusal::Unmatched,
        }],
        vec![REGISTERS],
        tree,
        audit,
        Some(Mastering::new(port, audit)),
        &mut |node, outcome| outcomes.push((node, outcome)),
    );
    assert_eq!(
        outcomes,
        [(
            UNIT_NODE,
            UnitOutcome::Stranded(
                Refusal::Unmatched,
                Quiesced {
                    stopped: 2,
                    refused: 0,
                }
            )
        )],
        "no window is kept for anything behind a unit that translates nothing"
    );
    assert_eq!(
        translation.dma_path(DEVICE),
        DmaPath::Stranded {
            unit: Some(UNIT_NODE)
        }
    );
    assert!(translation.strands());
}

#[test]
fn a_unit_that_will_not_enable_stops_what_its_firmware_windows_kept() {
    let (model, tree) = rig!(Behaviour::RefusesEnable);
    tree.add(&device_node(DEVICE, STREAM));
    let port = Port::new(model, vec![STREAM, 0x0018]);
    let audit = audit_sink();
    let (translation, outcomes) = Translation::started(
        vec![unit(model, vec![firmware_window(0x0018)])],
        vec![REGISTERS],
        tree,
        audit,
        Some(Mastering::new(port, audit)),
    );
    assert_eq!(
        outcomes,
        [(
            UNIT_NODE,
            UnitOutcome::Stranded(
                Refusal::Unit(IommuError::Hardware),
                Quiesced {
                    stopped: 3,
                    refused: 0,
                }
            )
        )],
        "the one stopped at take-over, then both again with no window kept"
    );
    assert_eq!(translation.units(), 0);
    assert_eq!(
        translation.dma_path(DEVICE),
        DmaPath::Stranded {
            unit: Some(UNIT_NODE)
        }
    );
}

const SIBLING: u32 = 8;
const SIBLING_STREAM: u32 = STREAM + 1;
const ALIAS: u32 = 0x0200;

/// `node`, given `alias` as a further stream its DMA arrives as.
fn aliased(mut node: HwNode, alias: u32) -> HwNode {
    node.push_resource(HwResource::iommu_alias(
        IommuStreams::new(UNIT_NODE, alias, 1).unwrap(),
    ))
    .unwrap();
    node
}

#[test]
fn an_alias_is_translated_through_its_owner_s_domain() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    tree.drop_node(DEVICE);
    tree.add(&aliased(device_node(DEVICE, STREAM), ALIAS));
    let iova = translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    assert_eq!(model.access(ALIAS, iova, true), Some(0x8000_0000));
    assert_eq!(model.attached(ALIAS), model.attached(STREAM));
    assert!(translation.revoke(DEVICE, 1));
    assert_eq!(model.attached(ALIAS), None, "blocked with the rest");
}

#[test]
fn a_stream_named_twice_is_attached_once() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    tree.drop_node(DEVICE);
    tree.add(&aliased(device_node(DEVICE, STREAM), STREAM));
    let iova = translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    assert_eq!(model.access(STREAM, iova, false), Some(0x8000_0000));
}

#[test]
fn a_group_takes_one_owner_at_a_time() {
    let (model, tree) = rig!(Behaviour::Correct);
    let audit = audit_sink();
    let translation = started_with(model, tree, Vec::new(), None, audit);
    tree.add(&grouped(SIBLING, SIBLING_STREAM, STREAM));
    translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    with_log_level(Level::Info, || {
        for _ in 0..3 {
            assert_eq!(
                translation.map(SIBLING, 2, &[block(0x9000_0000)], 0),
                Err(DmaError::GroupBusy)
            );
        }
    });
    assert_eq!(model.attached(SIBLING_STREAM), None, "nothing attached");
    let refused = recorded(audit, AuditEvent::DmaGroupRefused);
    assert_eq!(refused.len(), 1, "a driver retrying cannot flood the log");
    for (key, value) in [
        ("node", "8"),
        ("generation", "2"),
        ("group", "16"),
        ("holder", "7"),
    ] {
        assert_eq!(field(&refused[0], key), Some(value), "{key}");
    }

    assert!(translation.revoke(DEVICE, 1));
    let iova = translation
        .map(SIBLING, 2, &[block(0x9000_0000)], 0)
        .unwrap();
    assert_eq!(model.access(SIBLING_STREAM, iova, false), Some(0x9000_0000));
    assert_eq!(
        model.attached(STREAM),
        None,
        "a member the owner was not loaded for stays blocked"
    );
    assert_eq!(
        translation.map(DEVICE, 3, &[block(0xA000_0000)], 0),
        Err(DmaError::GroupBusy),
        "the first node's next driver waits its turn"
    );
}

#[test]
fn a_forgotten_owner_frees_its_group_for_another_node() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    tree.add(&grouped(SIBLING, SIBLING_STREAM, STREAM));
    translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    tree.drop_node(DEVICE);
    assert!(translation.forget(DEVICE));
    translation
        .map(SIBLING, 2, &[block(0x9000_0000)], 0)
        .unwrap();
}

#[test]
fn the_kernel_s_device_keeps_its_group_from_every_driver() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    tree.add(&grouped(SIBLING, SIBLING_STREAM, STREAM));
    translation
        .map(DEVICE, KERNEL_OWNER, &[block(0x8000_0000)], 0)
        .unwrap();
    assert_eq!(
        translation.map(SIBLING, 1, &[block(0x9000_0000)], 0),
        Err(DmaError::KernelOwned)
    );
}

#[test]
fn an_unconfirmed_end_keeps_its_group_from_every_other_node() {
    let (model, tree) = rig!(Behaviour::UnconfirmedBlock);
    let translation = started(model, tree, Vec::new());
    tree.add(&grouped(SIBLING, SIBLING_STREAM, STREAM));
    translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    assert!(!translation.revoke(DEVICE, 1));
    assert_eq!(
        translation.map(SIBLING, 2, &[block(0x9000_0000)], 0),
        Err(DmaError::Translation)
    );
}

#[test]
fn a_node_without_exactly_one_group_on_its_unit_carves_nothing() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    let mut ungrouped = HwNode::new(20, HW_NODE_ROOT_ID, HwDeviceClass::Storage);
    ungrouped
        .push_resource(HwResource::iommu_stream(
            IommuStreams::new(UNIT_NODE, 0x30, 1).unwrap(),
        ))
        .unwrap();
    let mut twice = grouped(21, 0x31, 0x31);
    twice
        .push_resource(HwResource::iommu_group_member(IommuGroup::new(
            UNIT_NODE, 0x32,
        )))
        .unwrap();
    let mut astray = grouped(22, 0x33, 0x33);
    astray
        .push_resource(HwResource::iommu_alias(
            IommuStreams::new(UNIT_NODE + 1, 0x34, 1).unwrap(),
        ))
        .unwrap();
    for node in [&ungrouped, &twice, &astray] {
        tree.add(node);
        assert_eq!(
            translation.dma_path(node.id()),
            DmaPath::Translated {
                output_limit: model.profile().reach.output_limit()
            },
            "behind the unit all the same"
        );
        assert_eq!(
            translation.map(node.id(), 1, &[block(0x8000_0000)], 0),
            Err(DmaError::Translation)
        );
    }
    assert_eq!(model.domains(), 0);
}

#[test]
fn a_fault_on_an_alias_is_laid_against_the_owner_that_attached_it() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    tree.drop_node(DEVICE);
    tree.add(&aliased(device_node(DEVICE, STREAM), ALIAS));
    translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    let sink = audit_sink();
    let mut budget = FaultBudget::new(FAULT_LIMITS, 0).unwrap();
    assert_eq!(model.access(ALIAS, PAGE, true), None);
    translation.drain_pass(0, &mut budget, sink, &NoClock);
    tree.drop_node(DEVICE);
    assert!(translation.forget(DEVICE));
    assert_eq!(model.access(ALIAS, PAGE, true), None);
    translation.drain_pass(0, &mut budget, sink, &NoClock);
    let faults = recorded(sink, AuditEvent::DmaTranslationFault);
    assert_eq!(field(&faults[0], "node"), Some("7"));
    assert_eq!(field(&faults[1], "node"), None, "forgotten with its node");
}

#[test]
fn an_owner_masters_its_own_functions_never_through_an_alias() {
    let (model, tree) = rig!(Behaviour::Correct);
    let port = Port::new(model, Vec::new());
    let translation = started_with(model, tree, Vec::new(), Some(port), audit_sink());
    tree.drop_node(DEVICE);
    tree.add(&aliased(device_node(DEVICE, STREAM), ALIAS));
    translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    assert!(translation.revoke(DEVICE, 1));
    let named: Vec<Named> = port.calls().into_iter().map(|call| call.0).collect();
    assert_eq!(named, [streams(STREAM), streams(STREAM)]);
}

/// A parent and the child it published for one device share its function.
/// The child's driver, admitted later, owns the group first; when it ends, the
/// parent's driver takes the group, and its grant must outrank the child's
/// withdrawal though its generation is the older.
#[test]
fn owners_change_a_shared_function_in_the_order_they_began() {
    let (model, tree) = rig!(Behaviour::Correct);
    let port = Port::new(model, Vec::new());
    let translation = started_with(model, tree, Vec::new(), Some(port), audit_sink());
    tree.add(&device_node(SIBLING, STREAM));
    translation
        .map(SIBLING, 7, &[block(0x8000_0000)], 0)
        .unwrap();
    assert!(translation.revoke(SIBLING, 7));
    translation
        .map(DEVICE, 5, &[block(0x9000_0000)], 0)
        .unwrap();
    let calls = port.calls();
    let order: Vec<(bool, u64)> = calls.iter().map(|call| (call.1, call.3)).collect();
    assert_eq!(order.len(), 3);
    assert_eq!((order[0].0, order[1].0, order[2].0), (true, false, true));
    assert_eq!(
        order[0].1, order[1].1,
        "the child's on and off are one owner's"
    );
    assert!(
        order[2].1 > order[1].1,
        "the parent began after the child ended"
    );
}

/// Owners published together hold room for every stream each will attach,
/// so laying their streams at their nodes' doors allocates nothing, whatever
/// order their adoptions end in.
#[test]
fn owners_published_together_attach_their_streams_without_allocating() {
    let mut owners = Owners {
        nodes: HashMap::with_hasher(BuildFastHash::new()),
        groups: HashMap::with_hasher(BuildFastHash::new()),
        streams: HashMap::with_hasher(BuildFastHash::new()),
        awaited: 0,
    };
    let owner = |node: u32, first: u32| {
        Arc::new(Owner {
            generation: 1,
            node,
            identity: Identity {
                unit: 0,
                group: node,
                requester: ArrayVec::new(),
                aliases: ArrayVec::new(),
                doorbells: ArrayVec::new(),
            },
            streams: (first..first + 8).collect(),
            epoch: 0,
            state: SpinLock::new(OwnerState::Adopting),
            unconfirmed: AtomicBool::new(false),
            refused: AtomicU64::new(NONE_REFUSED),
        })
    };
    let (first, second, failed) = (owner(1, 0), owner(2, 100), owner(3, 200));
    for published in [&first, &second, &failed] {
        assert_eq!(owners.publish(published, None, None), Ok(true));
    }
    owners.restore(&failed, None, None);
    let room = owners.streams.capacity();
    owners.attached(&second);
    owners.attached(&first);
    assert_eq!(owners.streams.capacity(), room, "an attach allocated");
    assert_eq!((owners.streams.len(), owners.awaited), (16, 0));
}

/// A holder whose adoption failed holds its group for no one: before its
/// records are put back a sibling's owner finds the group free, and the
/// kernel's own and an unconfirmed holder never do.
#[test]
fn a_holder_whose_adoption_failed_leaves_its_group_free() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    let holder = |generation: u64, state: OwnerState| Owner {
        generation,
        node: DEVICE,
        identity: Identity {
            unit: 0,
            group: STREAM,
            requester: ArrayVec::new(),
            aliases: ArrayVec::new(),
            doorbells: ArrayVec::new(),
        },
        streams: Vec::new(),
        epoch: 0,
        state: SpinLock::new(state),
        unconfirmed: AtomicBool::new(false),
        refused: AtomicU64::new(NONE_REFUSED),
    };
    let failed = holder(1, OwnerState::Unadopted(DmaError::Translation));
    assert_eq!(translation.free(&failed, SIBLING, 2), Ok(()));
    let unconfirmed = holder(1, OwnerState::Revoked { confirmed: false });
    assert_eq!(
        translation.free(&unconfirmed, SIBLING, 2),
        Err(DmaError::Translation)
    );
    let kernel = holder(KERNEL_OWNER, OwnerState::Adopting);
    assert_eq!(
        translation.free(&kernel, SIBLING, 2),
        Err(DmaError::KernelOwned)
    );
}

/// Records, for each event written, whether `holder`'s lock was held then.
struct HeldWitness {
    holder: SpinLock<Option<Arc<Owner>>>,
    held: SpinLock<Vec<bool>>,
}

impl Sink for HeldWitness {
    fn write_event(&self, _event: &tairix_log::Event<'_>) {
        if let Some(holder) = self.holder.lock().as_ref() {
            self.held.lock().push(holder.state.is_locked());
        }
    }
}

/// A group's live holder refuses a sibling's owner, and the refusal is
/// audited once the holder's lock is let go, so it never holds up the
/// holder's own maps and unmaps.
#[test]
fn a_refused_group_is_audited_with_its_holder_let_go() {
    let (model, tree) = rig!(Behaviour::Correct);
    let witness: &'static HeldWitness = Box::leak(Box::new(HeldWitness {
        holder: SpinLock::new(None),
        held: SpinLock::new(Vec::new()),
    }));
    tree.add(&device_node(DEVICE, STREAM));
    let (translation, _) = Translation::started(
        vec![unit(model, Vec::new())],
        vec![REGISTERS],
        tree,
        witness,
        None,
    );
    let live = Arc::new(Owner {
        generation: 1,
        node: DEVICE,
        identity: Identity {
            unit: 0,
            group: STREAM,
            requester: ArrayVec::new(),
            aliases: ArrayVec::new(),
            doorbells: ArrayVec::new(),
        },
        streams: Vec::new(),
        epoch: 0,
        state: SpinLock::new(OwnerState::Adopting),
        unconfirmed: AtomicBool::new(false),
        refused: AtomicU64::new(NONE_REFUSED),
    });
    *witness.holder.lock() = Some(Arc::clone(&live));
    assert_eq!(
        translation.free(&live, SIBLING, 2),
        Err(DmaError::GroupBusy)
    );
    assert_eq!(*witness.held.lock(), [false]);
}

/// Remapping is built on every unit before an entry is made, its entries
/// deliver once it is enabled, and only then is the machine said to remap.
#[test]
fn interrupts_are_remapped_through_the_unit_once_every_entry_is_made() {
    use tairix_kernel_iommu_api::conformance::InterruptProbe;

    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, vec![]);
    let target = InterruptTarget {
        vector: 0x51,
        destination: 0,
        level: false,
    };
    assert_eq!(
        translation.remap(UNIT_NODE, InterruptSource::Requester(0x10), target),
        Err(RemapError::UnknownUnit),
        "nothing is remapped before the tables are built"
    );
    assert!(translation.extended());
    translation.prepare(false, 64).unwrap();
    assert_eq!(
        translation.remap(UNIT_NODE + 1, InterruptSource::Requester(0x10), target),
        Err(RemapError::UnknownUnit)
    );
    let entry = translation
        .remap(UNIT_NODE, InterruptSource::Requester(0x10), target)
        .unwrap();
    assert!(!model.remapping());
    translation.enable().unwrap();
    assert!(model.remapping());
    let remapped = entry.remapped;
    assert_eq!(
        model.interrupt(0x10, remapped.address, remapped.data),
        Some(target)
    );
    translation.release(entry);
    assert_eq!(
        model.interrupt(0x10, remapped.address, remapped.data),
        None,
        "a released entry raises nothing"
    );
}

/// A unit that cannot turn remapping on turns it back off at every unit
/// that could, so no device is left refused its compatibility interrupts.
#[test]
fn a_machine_remaps_whole_or_not_at_all() {
    use tairix_kernel_iommu_api::conformance::InterruptProbe;

    let (willing, tree) = rig!(Behaviour::Correct);
    let (refusing, _) = rig!(Behaviour::RefusesRemapping);
    let (translation, _) = Translation::started(
        vec![
            unit(willing, Vec::new()),
            Unit {
                node: UNIT_NODE + 1,
                unit: refusing,
                reserved: Vec::new(),
                faults: FaultSignal::Message,
                family: DmaUnitFamily::Vtd,
                counts: FaultCounts::default(),
            },
        ],
        vec![REGISTERS],
        tree,
        audit_sink(),
        None,
    );
    translation.prepare(false, 64).unwrap();
    assert_eq!(
        translation.enable(),
        Err(RemapError::Unit(IommuError::Hardware))
    );
    assert!(!willing.remapping() && !refusing.remapping());
    assert!(
        willing.interrupt(0x10, 0xFEE0_0000, 0x41).is_some(),
        "the unit that had turned remapping on refuses compatibility interrupts"
    );
}

#[test]
fn a_machine_with_no_translating_unit_does_not_remap() {
    let (_, tree) = rig!(Behaviour::Correct);
    let (translation, _) = Translation::started(vec![], vec![], tree, audit_sink(), None);
    assert!(!translation.extended(), "no unit to remap with");
    assert_eq!(translation.prepare(false, 64), Err(RemapError::Unsupported));
    assert_eq!(translation.enable(), Err(RemapError::UnknownUnit));
}

/// A unit the kernel discovered but does not drive passes its devices'
/// forged messages, so the machine keeps compatibility delivery even though
/// the unit it does drive could remap.
#[test]
fn a_stranded_unit_keeps_the_whole_machine_unremapped() {
    let (model, tree) = rig!(Behaviour::Correct);
    tree.add(&device_node(DEVICE, STREAM));
    let refused = vec![Stranded {
        node: UNIT_NODE + 1,
        family: DmaUnitFamily::Unmatched,
        refusal: Refusal::Unmatched,
    }];
    let mut outcomes = Vec::new();
    let translation = Translation::start(
        vec![unit(model, Vec::new())],
        refused,
        vec![REGISTERS],
        tree,
        audit_sink(),
        None,
        &mut |node, outcome| outcomes.push((node, outcome)),
    );
    assert_eq!(
        outcomes,
        [
            (
                UNIT_NODE + 1,
                UnitOutcome::Stranded(Refusal::Unmatched, Quiesced::default())
            ),
            (
                UNIT_NODE,
                UnitOutcome::Translating(
                    Quiesced::default(),
                    tairix_kernel_iommu_api::Tables::Walked(tairix_kernel_iommu_api::Stage::Second),
                )
            ),
        ]
    );
    assert!(translation.strands());
    assert!(!translation.extended());
    assert_eq!(translation.prepare(false, 64), Err(RemapError::Unsupported));
    assert!(!model.remapping());
    assert_eq!(
        translation.dma_path(DEVICE),
        DmaPath::Translated {
            output_limit: model.profile().reach.output_limit()
        }
    );
}

/// What a batch's release was handed, carve by carve.
struct Released(std::sync::Mutex<Vec<Vec<FrameBlock>>>);

impl Released {
    const fn new() -> Self {
        Self(std::sync::Mutex::new(Vec::new()))
    }

    fn release(&self) -> impl Fn(&[FrameBlock]) + Sync + '_ {
        |blocks| self.0.lock().unwrap().push(blocks.to_vec())
    }

    fn carves(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

/// A rig of its own on every call, for a test that loops over several.
fn fresh_rig() -> (&'static ModelUnit<'static>, &'static Tree) {
    let frames: &'static tairix_kernel_iommu_api::hostmem::HostFrames = Box::leak(Box::new(
        tairix_kernel_iommu_api::hostmem::HostFrames::new(0x1_0000_0000),
    ));
    (
        Box::leak(Box::new(ModelUnit::new(frames, Behaviour::Correct))),
        Box::leak(Box::new(Tree::new())),
    )
}

fn chunks(phys: u64) -> Chunks {
    let mut blocks = Chunks::new();
    blocks.try_push(block(phys)).unwrap();
    blocks
}

/// Map a page carve for `node`'s owner at `phys` and free it into the batch,
/// answering its IOVA.
fn deferred_carve(translation: &Translation, node: u32, phys: u64) -> u64 {
    let iova = translation.map(node, 1, &[block(phys)], 0).unwrap();
    let mut blocks = chunks(phys);
    assert_eq!(translation.defer_free(node, 1, iova, &mut blocks), Ok(true));
    assert!(blocks.is_empty(), "the batch took the blocks");
    iova
}

/// A carve its owner frees leaves the tables at once, keeps its IOVA, and its
/// frames wait for the batch's window before one invalidation confirms them
/// all gone.
#[test]
fn a_freed_carve_waits_for_its_batch_and_is_released_once_confirmed() {
    let _ = crate::test_boot::claim_scheduler();
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    tree.add(&device_node(DEVICE, STREAM));
    translation.open_batch(u64::MAX);
    let released = Released::new();
    let carves: Vec<u64> = (0..5)
        .map(|i| deferred_carve(&translation, DEVICE, 0x8000_0000 + i * PAGE))
        .collect();
    for &iova in &carves {
        assert_eq!(model.access(STREAM, iova, true), None, "out of the tables");
    }
    let later = translation
        .map(DEVICE, 1, &[block(0x9000_0000)], 0)
        .unwrap();
    assert!(
        !carves.contains(&later),
        "no IOVA is reused before it is confirmed gone"
    );
    let now = crate::waitq::wait_now_ns().unwrap();
    let syncs = model.syncs();
    assert_eq!(
        translation.flush_due(now, &released.release()),
        Some(now + BATCH_WINDOW_NS)
    );
    assert_eq!(
        released.carves(),
        0,
        "nothing is freed before its window closes"
    );
    assert_eq!(
        translation.flush_due(now + BATCH_WINDOW_NS, &released.release()),
        None
    );
    assert_eq!(released.carves(), 5);
    assert_eq!(model.syncs(), syncs + 1, "one invalidation for the batch");
}

/// A full batch is due at once, and a carve the batch has no room for is left
/// mapped, for the caller to confirm alone.
#[test]
fn a_full_batch_is_due_at_once_and_a_carve_past_it_is_declined() {
    let _ = crate::test_boot::claim_scheduler();
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    tree.add(&device_node(DEVICE, STREAM));
    translation.open_batch(u64::MAX);
    for i in 0..BATCH_CARVES as u64 {
        deferred_carve(&translation, DEVICE, 0x8000_0000 + i * PAGE);
    }
    let past = translation
        .map(DEVICE, 1, &[block(0x9000_0000)], 0)
        .unwrap();
    let mut blocks = chunks(0x9000_0000);
    assert_eq!(
        translation.defer_free(DEVICE, 1, past, &mut blocks),
        Ok(false)
    );
    assert_eq!(blocks.len(), 1, "a declined carve keeps its blocks");
    assert!(
        model.access(STREAM, past, true).is_some(),
        "and its mapping"
    );
    assert_eq!(translation.unmap(DEVICE, 1, past), Ok(()));
    let released = Released::new();
    let now = crate::waitq::wait_now_ns().unwrap();
    assert_eq!(translation.flush_due(now, &released.release()), None);
    assert_eq!(released.carves(), BATCH_CARVES);
}

/// A batch holds back no more memory than its budget, and none at all before
/// a flusher serves it.
#[test]
fn a_carve_past_the_budget_or_before_a_flusher_serves_is_declined() {
    let _ = crate::test_boot::claim_scheduler();
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    tree.add(&device_node(DEVICE, STREAM));
    let iova = translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    let mut blocks = chunks(0x8000_0000);
    assert_eq!(
        translation.defer_free(DEVICE, 1, iova, &mut blocks),
        Ok(false)
    );
    translation.open_batch(PAGE - 1);
    assert_eq!(
        translation.defer_free(DEVICE, 1, iova, &mut blocks),
        Ok(false)
    );
    assert!(
        model.access(STREAM, iova, true).is_some(),
        "a declined carve is still mapped"
    );
    assert_eq!(Translation::batch_budget(1 << 30), 4 << 20);
}

/// A carve the batch could not take, because its owner no longer maps it,
/// gives back the room it was lent.
#[test]
fn a_carve_the_batch_cannot_take_returns_its_room() {
    let _ = crate::test_boot::claim_scheduler();
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    tree.add(&device_node(DEVICE, STREAM));
    tree.add(&device_node(DEVICE + 1, STREAM + 1));
    translation.open_batch(u64::MAX);
    let mut blocks = chunks(0x8000_0000);
    let iova = translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    assert_eq!(
        translation.defer_free(DEVICE, 1, iova + PAGE, &mut blocks),
        Ok(false)
    );
    assert!(translation.revoke(DEVICE, 1));
    assert_eq!(
        translation.defer_free(DEVICE, 1, iova, &mut blocks),
        Ok(false)
    );
    for i in 0..BATCH_CARVES as u64 {
        deferred_carve(&translation, DEVICE + 1, 0x8800_0000 + i * PAGE);
    }
}

/// A carve whose owner's end was confirmed is released with the batch; one
/// whose owner's end, or whose batch, the unit could not confirm is kept.
#[test]
fn only_carves_an_invalidation_confirmed_gone_are_released() {
    for (behaviour, revoke, released_carves) in [
        (Behaviour::Correct, true, 1),
        (Behaviour::UnconfirmedBlock, true, 0),
        (Behaviour::UnconfirmedSync, false, 0),
    ] {
        let _ = crate::test_boot::claim_scheduler();
        let (model, tree) = fresh_rig();
        let sink = audit_sink();
        let translation = started_with(model, tree, Vec::new(), None, sink);
        tree.add(&device_node(DEVICE, STREAM));
        translation.open_batch(u64::MAX);
        deferred_carve(&translation, DEVICE, 0x8000_0000);
        model.behave(behaviour);
        if revoke {
            translation.revoke(DEVICE, 1);
        }
        let released = Released::new();
        assert_eq!(translation.flush_due(u64::MAX, &released.release()), None);
        assert_eq!(released.carves(), released_carves, "{behaviour:?}");
        let unconfirmed = recorded(sink, AuditEvent::DmaTranslationUnconfirmed);
        assert_eq!(
            unconfirmed.len(),
            usize::from(released_carves == 0),
            "{behaviour:?}"
        );
    }
}

/// Ends a service body once its scripted parks are spent.
struct Ended;

/// Parks by running the next of its steps.
struct Script<'a>(std::collections::VecDeque<Box<dyn FnMut() + 'a>>);

impl crate::kthread::YieldHandle for Script<'_> {
    fn yield_now(&mut self) {}

    fn park(&mut self) {
        match self.0.pop_front() {
            Some(mut step) => step(),
            None => std::panic::panic_any(Ended),
        }
    }
}

/// The flusher serves only once it has parked, sleeps until the batch's
/// window closes, and confirms the batch then.
#[test]
fn the_flusher_parks_until_a_batch_is_due_and_confirms_it() {
    let (_, task) = crate::test_boot::claim_scheduler();
    let (model, tree) = rig!(Behaviour::Correct);
    let translation: &'static Translation = Box::leak(Box::new(started(model, tree, Vec::new())));
    tree.add(&device_node(DEVICE, STREAM));
    let released: &'static Released = Box::leak(Box::new(Released::new()));
    let release: &'static Release<'static> = Box::leak(Box::new(released.release()));
    let mut bodies = Vec::new();
    translation.serve_frees(u64::MAX, release, |body| {
        bodies.push(body);
        Some(0x77)
    });
    let mut body = bodies.pop().expect("a flusher admitted");
    let iova = translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    let mut blocks = chunks(0x8000_0000);
    assert_eq!(
        translation.defer_free(DEVICE, 1, iova, &mut blocks),
        Ok(false),
        "nothing waits before the flusher has parked"
    );
    assert_eq!(translation.unmap(DEVICE, 1, iova), Ok(()));
    let opened = crate::waitq::wait_now_ns().unwrap();
    let syncs = model.syncs();
    let steps: Vec<Box<dyn FnMut()>> = vec![
        Box::new(|| {
            for i in 0..3 {
                deferred_carve(translation, DEVICE, 0x8100_0000 + i * PAGE);
            }
        }),
        Box::new(|| {
            assert_eq!(released.carves(), 0, "the window is still open");
            assert_eq!(
                crate::waitq::DEFERRED_FREE_WAITQ.earliest_deadline(),
                Some(opened + BATCH_WINDOW_NS),
                "parked until it closes"
            );
            crate::test_boot::advance_clock(BATCH_WINDOW_NS);
        }),
        Box::new(|| {
            assert_eq!(released.carves(), 3);
            assert_eq!(model.syncs(), syncs + 1);
            assert_eq!(crate::waitq::DEFERRED_FREE_WAITQ.earliest_deadline(), None);
        }),
    ];
    let mut script = Script(steps.into());
    let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&mut script)));
    crate::waitq::DEFERRED_FREE_WAITQ.deregister(task);
    assert!(
        ended.is_err_and(|ended| ended.is::<Ended>()),
        "every step ran"
    );
}

/// Serve frees on a fresh rig, admitting the flusher or not, and answer the
/// translation, its audit and the flusher's body if it was admitted.
fn serving_frees(
    admitted: bool,
) -> (
    &'static Translation,
    &'static TestSink,
    Option<crate::kthread::KernelServiceBody>,
) {
    let (model, tree) = fresh_rig();
    let sink = audit_sink();
    let translation: &'static Translation =
        Box::leak(Box::new(started_with(model, tree, Vec::new(), None, sink)));
    tree.add(&device_node(DEVICE, STREAM));
    let release: &'static Release<'static> = &|_: &[FrameBlock]| {};
    let mut bodies = Vec::new();
    translation.serve_frees(u64::MAX, release, |body| {
        bodies.push(body);
        admitted.then_some(0x77)
    });
    (translation, sink, bodies.pop().filter(|_| admitted))
}

/// Answer whether a free on `translation` was batched, a clock installed so
/// only the batch itself can decline it.
fn batched(translation: &Translation) -> bool {
    let _ = crate::test_boot::claim_scheduler();
    let iova = translation
        .map(DEVICE, 1, &[block(0x8000_0000)], 0)
        .unwrap();
    let mut blocks = chunks(0x8000_0000);
    translation
        .defer_free(DEVICE, 1, iova, &mut blocks)
        .unwrap()
}

fn unbatched_cause(sink: &TestSink) -> Option<alloc::string::String> {
    let unbatched = recorded(sink, AuditEvent::DmaFreesUnbatched);
    assert!(unbatched.len() <= 1, "audited once");
    unbatched
        .first()
        .and_then(|event| field(event, "cause").map(alloc::string::String::from))
}

/// A flusher the scheduler refuses is audited, and every free is then
/// confirmed alone.
#[test]
fn a_flusher_the_scheduler_refuses_is_audited_and_defers_nothing() {
    let (translation, sink, _) = serving_frees(false);
    assert_eq!(unbatched_cause(sink).as_deref(), Some("not_admitted"));
    assert!(!batched(translation));
}

/// A flusher that cannot prove its park — here no wait hook is installed for
/// its thread — is audited and ends, and every free is then confirmed alone.
#[test]
fn a_flusher_that_cannot_park_is_audited_and_defers_nothing() {
    let (translation, sink, body) = serving_frees(true);
    let mut body = body.expect("a flusher admitted");
    body(&mut Spins);
    assert_eq!(unbatched_cause(sink).as_deref(), Some("cannot_park"));
    assert!(!batched(translation));
}

/// The units of a malformed table are audited with the family it describes
/// and whether their functions were withheld or published unconfined.
#[test]
fn a_malformed_table_s_units_are_audited_with_what_was_done() {
    for (unconfined, outcome) in [(false, "withheld"), (true, "unconfined")] {
        let sink = audit_sink();
        let units = MalformedUnits {
            family: DmaUnitFamily::AmdVi,
            unconfined,
        };
        units.audit(sink);
        let audited = recorded(sink, AuditEvent::DmaUnitsMalformed);
        assert_eq!(audited.len(), 1);
        assert_eq!(field(&audited[0], "family"), Some("amd-vi"));
        assert_eq!(field(&audited[0], "outcome"), Some(outcome));
    }
}
