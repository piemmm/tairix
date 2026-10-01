extern crate std;

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::blkio::FaultDomainState;
use tairix_abi::{Errno, HwDeviceClass, HwResource, HW_NODE_ROOT_ID};
use tairix_kernel_iommu_api::conformance::TranslationProbe;
use tairix_kernel_iommu_api::model::{Behaviour, ModelUnit};
use tairix_kernel_mem::{DmaBlock, Frame, PhysAddr};

use super::*;
use crate::hwtree::HwNodeLiveness;

/// A tree holding the nodes a test put in it.
struct Tree(SpinLock<Vec<HwNode>>);

impl Tree {
    const fn new() -> Self {
        Self(SpinLock::new(Vec::new()))
    }

    fn add(&self, node: &HwNode) {
        self.0.lock().push(*node);
    }

    fn drop_node(&self, id: u32) {
        self.0.lock().retain(|node| node.id() != id);
    }
}

impl HwNodeLiveness for Tree {
    fn is_live(&self, node_id: u32) -> bool {
        self.0.lock().iter().any(|node| node.id() == node_id)
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

    fn set_health(&self, _node_id: u32, _health: FaultDomainState) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    fn node(&self, node_id: u32) -> Result<Option<HwNode>, Errno> {
        Ok(self
            .0
            .lock()
            .iter()
            .find(|node| node.id() == node_id)
            .copied())
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

fn device_node(id: u32, stream: u32) -> HwNode {
    let mut node = HwNode::new(id, HW_NODE_ROOT_ID, HwDeviceClass::Storage);
    node.push_resource(HwResource::iommu_stream(
        IommuStreams::new(UNIT_NODE, stream, 1).unwrap(),
    ))
    .unwrap();
    node
}

fn unit(model: &'static ModelUnit<'static>, reserved: Vec<IommuReservedWindow>) -> Unit {
    Unit {
        node: UNIT_NODE,
        unit: model,
        registers: 0xFED9_0000..0xFED9_1000,
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
    tree.add(&device_node(DEVICE, STREAM));
    let (translation, outcomes) = Translation::start(vec![unit(model, reserved)], tree);
    assert_eq!(outcomes, [(UNIT_NODE, Ok(()))]);
    translation
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
    assert_eq!(translation.forget(DEVICE), None);
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
    assert_eq!(translation.forget(DEVICE), Some(1));
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
    let window = IommuReservedWindow::new(STREAM, 0x7B80_0000, 0x10_0000).unwrap();
    let mut pair = HwNode::new(8, HW_NODE_ROOT_ID, HwDeviceClass::Storage);
    pair.push_resource(HwResource::iommu_stream(
        IommuStreams::new(UNIT_NODE, STREAM, 2).unwrap(),
    ))
    .unwrap();
    tree.add(&pair);
    let translation = started(model, tree, vec![window]);
    let mut other = HwNode::new(9, HW_NODE_ROOT_ID, HwDeviceClass::Storage);
    other
        .push_resource(HwResource::iommu_stream(
            IommuStreams::new(UNIT_NODE, shared, 1).unwrap(),
        ))
        .unwrap();
    tree.add(&other);
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
    let window = IommuReservedWindow::new(STREAM, 0x7B80_0000, 0x10_0000).unwrap();
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

#[test]
fn a_unit_that_will_not_enable_is_dropped_and_translates_nothing() {
    let (model, tree) = rig!(Behaviour::RefusesEnable);
    tree.add(&device_node(DEVICE, STREAM));
    let (translation, outcomes) = Translation::start(vec![unit(model, Vec::new())], tree);
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
