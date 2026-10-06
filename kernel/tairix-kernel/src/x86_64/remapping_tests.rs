use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::msix::MsiMessage;
use tairix_abi::IommuStreams;
use tairix_kernel_core::iommu::{
    InterruptRouting, InterruptSource, InterruptTarget, RemapError, Translation, Unit,
};
use tairix_kernel_iommu_api::conformance::InterruptProbe;
use tairix_kernel_iommu_api::model::{Behaviour, ModelUnit};

use super::*;
use crate::test_support::NullSink;

const UNIT: u32 = 0x800A_0000;
const IOAPIC_SID: u16 = 0xF0F8;
const DEVICE: u16 = 0x0018;

macro_rules! model {
    ($behaviour:expr) => {{
        static FRAMES: tairix_kernel_iommu_api::hostmem::HostFrames =
            tairix_kernel_iommu_api::hostmem::HostFrames::new(0x1_0000_0000);
        static MODEL: ModelUnit<'static> = ModelUnit::new(&FRAMES, $behaviour);
        &MODEL
    }};
}

static AUDIT: NullSink = NullSink;

/// The facility over `model`, the one unit at node [`UNIT`].
fn facility(model: &'static ModelUnit<'static>) -> Translation {
    Translation::start(
        vec![Unit {
            node: UNIT,
            unit: model,
            reserved: Vec::new(),
            faults: None,
        }],
        &[],
        Vec::new(),
        &tairix_kernel_core::NullHwTreeSource,
        &AUDIT,
        None,
        &mut |_, _| {},
    )
}

fn pin(gsi: u32, vector: u8, level: bool) -> ProgrammedPin {
    ProgrammedPin {
        gsi,
        ioapic: 2,
        vector,
        destination: 0,
        level,
    }
}

const IOAPIC: IoApicSource = IoApicSource {
    id: 2,
    unit: UNIT,
    requester: IOAPIC_SID,
};

fn vector(vector: u8) -> MsiVector {
    MsiVector {
        vector,
        line: 4096 + u32::from(vector),
        destination: 0,
    }
}

/// A function the probe published as `node`, translated by [`UNIT`] where
/// `interrupts` says how its messages arrive.
fn function(address: u64, translated: bool, interrupts: Option<InterruptSource>) -> Published {
    Published {
        segment: 0,
        address,
        stream: translated.then(|| IommuStreams::new(UNIT, u32::from(DEVICE), 1).unwrap()),
        interrupts,
    }
}

fn route(node: u32, at: u8, published: Option<Published>) -> (PendingRoute, Option<Published>) {
    (
        PendingRoute {
            node,
            vector: vector(at),
        },
        published,
    )
}

/// Every pin and every function a unit sees is given an entry that delivers
/// its own vector, at its own trigger, to it alone; a function no unit sees
/// keeps its compatibility route, and one whose requester ids are unknown is
/// counted unrouted.
#[test]
fn every_source_a_unit_sees_raises_only_its_own_entry() {
    let model = model!(Behaviour::Correct);
    let translation = facility(model);
    let pins = [pin(1, 0x31, false), pin(20, 0x44, true)];
    let functions = [
        route(
            7,
            0x60,
            Some(function(
                0x1800,
                true,
                Some(InterruptSource::Requester(DEVICE)),
            )),
        ),
        route(8, 0x61, Some(function(0x2000, false, None))),
        route(9, 0x62, Some(function(0x2800, true, None))),
        route(10, 0x63, None),
    ];
    let (routing, plan) = plan(&translation, &pins, &[IOAPIC], &functions, false, 207);
    assert_eq!(routing, InterruptRouting::Unrouted(1));
    assert!(model.remapping());
    assert_eq!(plan.pins.len(), 2);
    assert_eq!(plan.routes.len(), 1);

    let (published, message, _) = plan.routes[0];
    assert_eq!(published.address, 0x1800);
    assert_eq!(
        model.interrupt(DEVICE, message.address, message.data),
        Some(InterruptTarget {
            vector: 0x60,
            destination: 0,
            level: false
        })
    );
    assert_eq!(
        model.interrupt(IOAPIC_SID, message.address, message.data),
        None,
        "the I/O APIC raised a device's entry"
    );
    assert_eq!(
        model.interrupt(DEVICE, 0xFEE0_0000, 0x60),
        None,
        "a compatibility message passed with remapping on"
    );
}

/// An I/O APIC no unit's scope names keeps the whole machine unremapped:
/// its pins' messages would be refused with nothing to raise instead.
#[test]
fn an_io_apic_no_unit_names_keeps_the_machine_unremapped() {
    let model = model!(Behaviour::Correct);
    let translation = facility(model);
    let stray = ProgrammedPin {
        ioapic: 9,
        ..pin(30, 0x50, true)
    };
    let (routing, plan) = plan(
        &translation,
        &[pin(1, 0x31, false), stray],
        &[IOAPIC],
        &[],
        false,
        207,
    );
    assert_eq!(routing, InterruptRouting::Unremapped);
    assert_eq!(plan, Plan::default());
    assert!(!model.remapping());
    assert!(model.interrupt(DEVICE, 0xFEE0_0000, 0x41).is_some());
}

/// An I/O APIC named by a unit that is not translating cannot be remapped,
/// so nothing is, and every entry already made is let go.
#[test]
fn an_io_apic_behind_a_unit_that_does_not_translate_keeps_the_machine_unremapped() {
    let model = model!(Behaviour::Correct);
    let translation = facility(model);
    let elsewhere = IoApicSource {
        id: 3,
        unit: UNIT + 1,
        requester: 0xF0F0,
    };
    let other = ProgrammedPin {
        ioapic: 3,
        ..pin(24, 0x50, true)
    };
    let (routing, _) = plan(
        &translation,
        &[pin(1, 0x31, false), other],
        &[IOAPIC, elsewhere],
        &[],
        false,
        207,
    );
    assert_eq!(routing, InterruptRouting::Unremapped);
    assert!(!model.remapping());
}

/// A unit that will not turn remapping on leaves every source as it was and
/// takes back every entry made for them.
#[test]
fn a_unit_refusing_to_remap_leaves_every_source_as_it_was() {
    let model = model!(Behaviour::RefusesRemapping);
    let translation = facility(model);
    let functions = [route(
        7,
        0x60,
        Some(function(
            0x1800,
            true,
            Some(InterruptSource::Requester(DEVICE)),
        )),
    )];
    let (routing, plan) = plan(
        &translation,
        &[pin(1, 0x31, false)],
        &[IOAPIC],
        &functions,
        false,
        207,
    );
    assert_eq!(
        routing,
        InterruptRouting::Refused(RemapError::Unit(
            tairix_kernel_iommu_api::IommuError::Hardware
        ))
    );
    assert_eq!(plan, Plan::default());
    assert!(!model.remapping());
}

/// A machine whose units cannot remap at all keeps compatibility delivery.
#[test]
fn a_machine_whose_units_cannot_remap_keeps_compatibility_delivery() {
    let translation = Translation::start(
        Vec::new(),
        &[],
        Vec::new(),
        &tairix_kernel_core::NullHwTreeSource,
        &AUDIT,
        None,
        &mut |_, _| {},
    );
    let (routing, plan) = plan(
        &translation,
        &[pin(1, 0x31, false)],
        &[IOAPIC],
        &[],
        false,
        207,
    );
    assert_eq!(routing, InterruptRouting::Unremapped);
    assert_eq!(plan, Plan::default());
}

/// A message is the unit's entry only where remapping is on and a unit sees
/// the function; otherwise the compatibility message, where there is one.
#[test]
fn a_function_s_message_is_its_entry_only_where_a_remapping_unit_sees_it() {
    let model = model!(Behaviour::Correct);
    let translation = facility(model);
    let message = MsiMessage {
        address: 0xFEE0_0000,
        data: 0x60,
    };
    let compatibility = Some(message);
    let translated = function(0x1800, true, Some(InterruptSource::Requester(DEVICE)));
    assert_eq!(
        message_for(None, vector(0x60), &translated, compatibility),
        Ok((message, None)),
        "remapping off"
    );
    assert_eq!(
        message_for(None, vector(0x60), &translated, None),
        Err(MsiRouteError::Unaddressable)
    );
    translation.prepare(false, 64).unwrap();
    let untranslated = function(0x2000, false, None);
    assert_eq!(
        message_for(
            Some(&translation),
            vector(0x60),
            &untranslated,
            compatibility
        ),
        Ok((message, None)),
        "no unit sees it to refuse it"
    );
    assert_eq!(
        message_for(
            Some(&translation),
            vector(0x60),
            &function(0x2800, true, None),
            compatibility
        ),
        Err(MsiRouteError::Unattributed)
    );
    let (message, entry) =
        message_for(Some(&translation), vector(0x60), &translated, compatibility).unwrap();
    assert!(entry.is_some());
    translation.enable().unwrap();
    assert_eq!(
        model
            .interrupt(DEVICE, message.address, message.data)
            .map(|target| target.vector),
        Some(0x60)
    );
}
