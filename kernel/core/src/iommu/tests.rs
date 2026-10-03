extern crate std;

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::blkio::FaultDomainState;
use tairix_abi::{Errno, HwDeviceClass, HwResource, IommuGroup, MsiAllocation, HW_NODE_ROOT_ID};
use tairix_kernel_iommu_api::conformance::TranslationProbe;
use tairix_kernel_iommu_api::model::{Behaviour, ModelUnit};
use tairix_kernel_iommu_api::{Access, DomainId, Fault, FaultBudget, FaultLimits, UnitProfile};
use tairix_kernel_irq::{IrqController, IrqTable, MaskError};
use tairix_kernel_mem::{DmaBlock, Frame, PhysAddr};

use super::*;
use crate::audit::AuditEvent;
use crate::devres::MsiAllocFacility;
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
    }
}

fn block(phys: u64) -> DmaBlock {
    DmaBlock {
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
    let (translation, outcomes) = Translation::start(
        vec![unit(model, reserved)],
        vec![REGISTERS],
        tree,
        audit,
        mastering,
    );
    assert_eq!(outcomes, [(UNIT_NODE, Ok(Quiesced::default()))]);
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
    assert!(translation.translates(DEVICE));
    tree.add(&HwNode::new(8, HW_NODE_ROOT_ID, HwDeviceClass::Network));
    assert!(!translation.translates(8), "a node naming no stream");
    assert!(!translation.translates(9), "a node the tree does not hold");
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
    let iova = translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
    assert_eq!(model.access(STREAM, iova + 8, true), Some(0x8000_0008));
    assert_eq!(model.access(0x0018, iova, true), None, "another stream");
    assert_ne!(
        iova, 0x8000_0000,
        "the device is handed an IOVA, not the frame"
    );
}

#[test]
fn revocation_blocks_the_stream_and_leaves_every_carve_unreachable() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    let iova = translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
    assert!(translation.revoke(DEVICE, 1));
    assert_eq!(model.access(STREAM, iova, true), None);
    assert_eq!(model.attached(STREAM), None);
    assert_eq!(
        translation.unmap(DEVICE, 1, iova, block(0x8000_0000)),
        Ok(())
    );
    assert_eq!(model.domains(), 0);
}

#[test]
fn a_successor_gets_a_fresh_domain_and_its_predecessor_stays_unreachable() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    let first = translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
    translation.revoke(DEVICE, 1);
    let second = translation.map(DEVICE, 2, block(0x9000_0000), 0).unwrap();
    assert_eq!(model.access(STREAM, second, false), Some(0x9000_0000));
    assert_eq!(
        translation.unmap(DEVICE, 1, first, block(0x8000_0000)),
        Ok(())
    );
    assert_eq!(
        translation.map(DEVICE, 1, block(0x8000_0000), 0),
        Err(DmaError::DeviceGone),
        "a revoked generation carves nothing more"
    );
}

#[test]
fn an_owner_never_revoked_is_retired_before_its_successor_carves() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    let first = translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
    translation.map(DEVICE, 2, block(0x9000_0000), 0).unwrap();
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
    let iova = translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
    assert!(model.access(STREAM, iova, false).is_some());
    assert_eq!(
        translation.unmap(DEVICE, 1, iova, block(0x8000_0000)),
        Ok(())
    );
    assert_eq!(model.access(STREAM, iova, false), None);
    assert_eq!(
        translation.unmap(DEVICE, 1, iova, block(0x8000_0000)),
        Ok(())
    );
}

#[test]
fn an_unconfirmed_revocation_keeps_its_generation_unconfirmed_for_good() {
    let (model, tree) = rig!(Behaviour::UnconfirmedBlock);
    let translation = started(model, tree, Vec::new());
    let iova = translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
    assert!(!translation.revoke(DEVICE, 1));
    assert_eq!(
        translation.unmap(DEVICE, 1, iova, block(0x8000_0000)),
        Err(DmaError::Unconfirmed)
    );
}

#[test]
fn a_revoked_generation_carves_nothing_more() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
    translation.revoke(DEVICE, 1);
    assert_eq!(
        translation.map(DEVICE, 1, block(0x9000_0000), 0),
        Err(DmaError::DeviceGone)
    );
    assert_eq!(model.attached(STREAM), None, "no domain was made for it");
}

#[test]
fn an_unconfirmed_end_takes_no_successor() {
    let (model, tree) = rig!(Behaviour::UnconfirmedBlock);
    let translation = started(model, tree, Vec::new());
    let iova = translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
    translation.revoke(DEVICE, 1);
    assert_eq!(
        translation.map(DEVICE, 2, block(0x9000_0000), 0),
        Err(DmaError::Translation),
        "refused before it maps, so its block is free to go back"
    );
    assert_eq!(
        translation.unmap(DEVICE, 1, iova, block(0x8000_0000)),
        Err(DmaError::Unconfirmed),
        "the refused successor did not displace the record"
    );
}

#[test]
fn the_kernel_s_own_device_takes_no_driver() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    let iova = translation
        .map(DEVICE, KERNEL_OWNER, block(0x8000_0000), 0)
        .unwrap();
    assert_eq!(
        translation.map(DEVICE, 1, block(0x9000_0000), 0),
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
    let iova = translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
    tree.drop_node(DEVICE);
    assert!(translation.forget(DEVICE));
    assert_eq!(model.access(STREAM, iova, true), None);
    assert_eq!(
        translation.unmap(DEVICE, 1, iova, block(0x8000_0000)),
        Ok(())
    );
    assert_eq!(
        translation.map(DEVICE, 1, block(0x9000_0000), 0),
        Err(DmaError::DeviceGone)
    );
    assert_eq!(translation.reserve(DEVICE), Err(DmaError::DeviceGone));
}

#[test]
fn a_removed_node_whose_end_is_unconfirmed_stays_recorded() {
    let (model, tree) = rig!(Behaviour::UnconfirmedBlock);
    let translation = started(model, tree, Vec::new());
    let iova = translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
    tree.drop_node(DEVICE);
    assert!(!translation.forget(DEVICE), "the unit could not confirm it");
    assert_eq!(
        translation.unmap(DEVICE, 1, iova, block(0x8000_0000)),
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
    translation.map(9, 1, block(0x9000_0000), 0).unwrap();

    assert_eq!(
        translation.map(8, 1, block(0x8000_0000), 0),
        Err(DmaError::Translation),
        "a stream another owner holds"
    );
    assert_eq!(
        model.access(STREAM, 0x7B80_0040, false),
        Some(0x7B80_0040),
        "the stream the adoption took from firmware has its windows again"
    );
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
    let iova = translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
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

    fn route_faults(&self, address: u64, data: u32) -> Result<(), IommuError> {
        self.model.route_faults(address, data)
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
    let (translation, outcomes) = Translation::start(
        vec![Unit {
            node: UNIT_NODE,
            unit: watching,
            reserved: vec![firmware_window(STREAM)],
        }],
        vec![REGISTERS],
        tree,
        audit_sink(),
        None,
    );
    assert_eq!(outcomes, [(UNIT_NODE, Ok(Quiesced::default()))]);
    translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
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
    let (translation, outcomes) = Translation::start(
        vec![unit(model, Vec::new())],
        vec![REGISTERS],
        tree,
        audit,
        None,
    );
    assert_eq!(outcomes, [(UNIT_NODE, Err(IommuError::Hardware))]);
    assert_eq!(translation.units(), 0);
    assert!(!translation.translates(DEVICE));
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

#[test]
fn a_unit_no_family_drives_is_left_untranslated() {
    let env = UnitEnv {
        mmio: &|_, _| None,
        frames: &NO_FRAMES,
        coherence: None,
        clock: &NoClock,
    };
    let mut other = HwNode::new(UNIT_NODE, HW_NODE_ROOT_ID, HwDeviceClass::Iommu);
    other
        .push_match_key(HwMatchKey::compatible(b"arm,smmu-v3").unwrap())
        .unwrap();
    assert_eq!(take_over(&other, &env).err(), Some(Refusal::Unmatched));

    let mut vtd = HwNode::new(UNIT_NODE, HW_NODE_ROOT_ID, HwDeviceClass::Iommu);
    vtd.push_match_key(HwMatchKey::compatible(tairix_kernel_iommu_vtd::COMPATIBLE).unwrap())
        .unwrap();
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
    };
    let mut vtd = HwNode::new(UNIT_NODE, HW_NODE_ROOT_ID, HwDeviceClass::Iommu);
    vtd.push_match_key(HwMatchKey::compatible(tairix_kernel_iommu_vtd::COMPATIBLE).unwrap())
        .unwrap();
    vtd.push_resource(HwResource::mmio(0xFED9_0000, PAGE))
        .unwrap();
    assert_eq!(
        take_over(&vtd, &env).err(),
        Some(Refusal::Unit(IommuError::OutOfRange))
    );
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
    translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
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
    translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
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
    translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
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

/// An adoption that fails puts the node's record back, so a revoked
/// generation still carves nothing, and leaves the stream's attribution with
/// the owner that holds it.
#[test]
fn a_failed_adoption_puts_the_revoked_predecessor_back() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    // Disagreeing groups on one stream, so only the unit can refuse it.
    tree.add(&grouped(9, STREAM, STREAM + 1));
    translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
    assert!(translation.revoke(DEVICE, 1));
    translation.map(9, 1, block(0x9000_0000), 0).unwrap();
    assert_eq!(
        translation.map(DEVICE, 2, block(0xA000_0000), 0),
        Err(DmaError::Translation),
        "another node's owner holds the stream"
    );
    assert_eq!(
        translation.map(DEVICE, 1, block(0xA000_0000), 0),
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

struct OneVector(MsiAllocation);

impl MsiAllocFacility for OneVector {
    fn allocate(&self) -> Result<MsiAllocation, Errno> {
        Ok(self.0)
    }
}

const LINE: u32 = 9;
const VECTOR: OneVector = OneVector(MsiAllocation::new(0xFEE0_0000, 0x41, LINE));

fn serving(
    translation: Translation,
    msi: &dyn MsiAllocFacility,
) -> (&'static IrqTable, &'static TestSink, usize) {
    let translation: &'static Translation = Box::leak(Box::new(translation));
    let table: &'static IrqTable = Box::leak(Box::new(IrqTable::new(31)));
    let sink = audit_sink();
    let env = FaultEnv {
        table,
        controller: &OK_CONTROLLER,
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

#[test]
fn each_unit_s_fault_interrupt_is_routed_bound_to_the_kernel_and_served() {
    let (model, tree) = rig!(Behaviour::Correct);
    let (table, sink, admitted) = serving(started(model, tree, Vec::new()), &VECTOR);
    assert_eq!(admitted, 1);
    assert_eq!(model.routed(), Some((0xFEE0_0000, 0x41)));
    assert_eq!(table.owner_of_line(LINE), Some(FAULT_OWNER));
    assert!(recorded(sink, AuditEvent::DmaTranslationUnit).is_empty());
}

#[test]
fn a_unit_that_refuses_its_route_is_audited_and_its_line_released() {
    let (model, tree) = rig!(Behaviour::RefusesRoute);
    let (table, sink, admitted) = serving(started(model, tree, Vec::new()), &VECTOR);
    assert_eq!(admitted, 0);
    assert_eq!(table.owner_of_line(LINE), None);
    let unit = recorded(sink, AuditEvent::DmaTranslationUnit);
    assert_eq!(field(&unit[0], "outcome"), Some("faults_unrouted"));
    assert_eq!(field(&unit[0], "reason"), Some("refused"));
}

#[test]
fn a_port_with_no_kernel_vector_leaves_every_unit_s_faults_unrouted() {
    let (model, tree) = rig!(Behaviour::Correct);
    let (_, sink, admitted) = serving(
        started(model, tree, Vec::new()),
        &crate::devres::NULL_MSI_ALLOC_FACILITY,
    );
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
    let (translation, _) = Translation::start(
        vec![unit(model, Vec::new())],
        vec![REGISTERS],
        tree,
        sink,
        None,
    );
    let iova = translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
    for _ in 0..3 {
        assert_eq!(
            translation.unmap(DEVICE, 1, iova, block(0x8000_0000)),
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
    IommuReservedWindow::new(stream, 0x7B80_0000, 0x10_0000).unwrap()
}

#[test]
fn an_owner_s_function_masters_once_its_domain_is_attached() {
    let (model, tree) = rig!(Behaviour::Correct);
    let port = Port::new(model, Vec::new());
    let audit = audit_sink();
    let translation = started_with(model, tree, Vec::new(), Some(port), audit);
    assert!(port.calls().is_empty(), "nothing masters before an owner");
    with_log_level(Level::Info, || {
        translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
        translation.map(DEVICE, 1, block(0x9000_0000), 0).unwrap();
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
        translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
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
    translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
    translation.map(DEVICE, 2, block(0x9000_0000), 0).unwrap();
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
    translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
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
    translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
    assert!(!translation.revoke(DEVICE, 1));
    assert_eq!(
        translation.map(DEVICE, 2, block(0x9000_0000), 0),
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
    assert!(translation.map(DEVICE, 1, block(0x8000_0000), 0).is_err());
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
    translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
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
        translation.map(DEVICE, 1, block(0x8000_0000), 0),
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
    let (_translation, outcomes) = Translation::start(
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
            Ok(Quiesced {
                stopped: 1,
                refused: 0,
            })
        )]
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
    let iova = translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
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
    let iova = translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
    assert_eq!(model.access(STREAM, iova, false), Some(0x8000_0000));
}

#[test]
fn a_group_takes_one_owner_at_a_time() {
    let (model, tree) = rig!(Behaviour::Correct);
    let audit = audit_sink();
    let translation = started_with(model, tree, Vec::new(), None, audit);
    tree.add(&grouped(SIBLING, SIBLING_STREAM, STREAM));
    translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
    with_log_level(Level::Info, || {
        assert_eq!(
            translation.map(SIBLING, 2, block(0x9000_0000), 0),
            Err(DmaError::GroupBusy)
        );
    });
    assert_eq!(model.attached(SIBLING_STREAM), None, "nothing attached");
    let refused = recorded(audit, AuditEvent::DmaGroupRefused);
    assert_eq!(refused.len(), 1);
    for (key, value) in [
        ("node", "8"),
        ("generation", "2"),
        ("group", "16"),
        ("holder", "7"),
    ] {
        assert_eq!(field(&refused[0], key), Some(value), "{key}");
    }

    assert!(translation.revoke(DEVICE, 1));
    let iova = translation.map(SIBLING, 2, block(0x9000_0000), 0).unwrap();
    assert_eq!(model.access(SIBLING_STREAM, iova, false), Some(0x9000_0000));
    assert_eq!(
        model.attached(STREAM),
        None,
        "a member the owner was not loaded for stays blocked"
    );
    assert_eq!(
        translation.map(DEVICE, 3, block(0xA000_0000), 0),
        Err(DmaError::GroupBusy),
        "the first node's next driver waits its turn"
    );
}

#[test]
fn a_forgotten_owner_frees_its_group_for_another_node() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    tree.add(&grouped(SIBLING, SIBLING_STREAM, STREAM));
    translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
    tree.drop_node(DEVICE);
    assert!(translation.forget(DEVICE));
    translation.map(SIBLING, 2, block(0x9000_0000), 0).unwrap();
}

#[test]
fn the_kernel_s_device_keeps_its_group_from_every_driver() {
    let (model, tree) = rig!(Behaviour::Correct);
    let translation = started(model, tree, Vec::new());
    tree.add(&grouped(SIBLING, SIBLING_STREAM, STREAM));
    translation
        .map(DEVICE, KERNEL_OWNER, block(0x8000_0000), 0)
        .unwrap();
    assert_eq!(
        translation.map(SIBLING, 1, block(0x9000_0000), 0),
        Err(DmaError::KernelOwned)
    );
}

#[test]
fn an_unconfirmed_end_keeps_its_group_from_every_other_node() {
    let (model, tree) = rig!(Behaviour::UnconfirmedBlock);
    let translation = started(model, tree, Vec::new());
    tree.add(&grouped(SIBLING, SIBLING_STREAM, STREAM));
    translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
    assert!(!translation.revoke(DEVICE, 1));
    assert_eq!(
        translation.map(SIBLING, 2, block(0x9000_0000), 0),
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
        assert!(
            translation.translates(node.id()),
            "behind the unit all the same"
        );
        assert_eq!(
            translation.map(node.id(), 1, block(0x8000_0000), 0),
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
    translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
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
    translation.map(DEVICE, 1, block(0x8000_0000), 0).unwrap();
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
    translation.map(SIBLING, 7, block(0x8000_0000), 0).unwrap();
    assert!(translation.revoke(SIBLING, 7));
    translation.map(DEVICE, 5, block(0x9000_0000), 0).unwrap();
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
