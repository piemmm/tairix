use alloc::vec::Vec;

use tairix_abi::hwtree::{HwDeviceClass, HW_NODE_ROOT};
use tairix_abi::HwNode;

use super::{BootSettle, Devices, Moves, Pending, SETTLE_BUDGET_NS};
use crate::events;
use crate::testsink::RecordingSink;

#[test]
fn the_boot_settles_once_nothing_is_waited_on_and_only_once() {
    let sink = RecordingSink::new();
    let mut settle = BootSettle::default();
    // Nothing can be matched before the catalogue is in hand.
    assert!(!settle.observe(0, false, [], &sink));
    assert!(!settle.observe(10, true, [Pending::Node(4), Pending::Channels], &sink));
    assert_eq!(settle.deadline(), Some(10 + SETTLE_BUDGET_NS));
    // One settles; the deadline stays the other's, never reset.
    assert!(!settle.observe(20, true, [Pending::Channels], &sink));
    assert_eq!(settle.deadline(), Some(10 + SETTLE_BUDGET_NS));
    assert!(settle.observe(30, true, [], &sink));
    assert_eq!(occurrences(&sink, events::DEVICES_SETTLED), 1);
    assert_eq!(settle.deadline(), None);
    // A later wait does not un-settle the boot, nor announce it again.
    assert!(!settle.observe(40, true, [Pending::Node(9)], &sink));
    assert_eq!(settle.deadline(), None);
    assert_eq!(occurrences(&sink, events::DEVICES_SETTLED), 1);
    assert_eq!(occurrences(&sink, events::SETTLE_LAPSED), 0);
}

#[test]
fn a_driver_that_never_settles_holds_the_boot_back_only_for_its_budget() {
    let sink = RecordingSink::new();
    let mut settle = BootSettle::default();
    assert!(!settle.observe(100, true, [Pending::Node(4)], &sink));
    assert!(!settle.observe(100 + SETTLE_BUDGET_NS - 1, true, [Pending::Node(4)], &sink));
    assert!(settle.observe(100 + SETTLE_BUDGET_NS, true, [Pending::Node(4)], &sink));
    assert_eq!(occurrences(&sink, events::SETTLE_LAPSED), 1);
}

#[test]
fn a_lapsed_wait_is_never_begun_again() {
    let sink = RecordingSink::new();
    let mut settle = BootSettle::default();
    assert!(!settle.observe(0, false, [Pending::Node(4)], &sink));
    // It lapses while the catalogue is still awaited, so the boot is not yet
    // settled, but the node is not given a second budget.
    assert!(!settle.observe(SETTLE_BUDGET_NS, false, [Pending::Node(4)], &sink));
    assert_eq!(settle.deadline(), None);
    assert!(settle.observe(SETTLE_BUDGET_NS + 1, true, [Pending::Node(4)], &sink));
    assert_eq!(occurrences(&sink, events::SETTLE_LAPSED), 1);
}

/// How many records `sink` was handed with `id`.
fn occurrences(sink: &RecordingSink, id: tairix_log::EventId) -> usize {
    sink.ids().into_iter().filter(|&seen| seen == id.0).count()
}

const CONTROLLER: u32 = 2;

fn device(id: u32, parent: u32, address: u32, removable: bool, hot: bool) -> HwNode {
    let mut node = HwNode::new(id, parent, HwDeviceClass::Input);
    node.set_address(address);
    node.set_removable(removable);
    // The kernel's mark on a node published under a settled emitter.
    node.admit_published(hot);
    node
}

fn controller() -> HwNode {
    HwNode::new(CONTROLLER, HW_NODE_ROOT, HwDeviceClass::Bus)
}

fn ids(nodes: &[HwNode]) -> Vec<u32> {
    let mut ids: Vec<u32> = nodes.iter().map(HwNode::id).collect();
    ids.sort_unstable();
    ids
}

/// Step `devices` from snapshot `from` to snapshot `to`.
fn step(devices: &mut Devices, from: &[HwNode], to: &[HwNode]) -> Moves {
    devices.update(to, &ids(from), &ids(to))
}

#[test]
fn a_device_plugged_in_is_one_arrival_however_many_nodes_it_is() {
    let boot = [controller(), device(3, CONTROLLER, 0x11, true, false)];
    let mut devices = Devices::default();
    assert_eq!(devices.update(&boot, &[], &ids(&boot)), Moves::default());

    // A composite device: two interfaces at one address.
    let mut plugged = boot.to_vec();
    plugged.push(device(4, CONTROLLER, 0x21, true, true));
    plugged.push(device(5, CONTROLLER, 0x21, true, true));
    assert_eq!(
        step(&mut devices, &boot, &plugged),
        Moves {
            arrived: 1,
            left: 0
        }
    );
    // Nothing moved since.
    assert_eq!(step(&mut devices, &plugged, &plugged), Moves::default());

    // Pulled, both interfaces at once: one departure. The device found as
    // the bus came up leaves too, and that is a departure like any other.
    let pulled = [controller()];
    assert_eq!(
        step(&mut devices, &plugged, &pulled),
        Moves {
            arrived: 0,
            left: 2
        }
    );
}

#[test]
fn what_is_found_as_a_bus_comes_up_is_not_a_device_plugged_in() {
    let boot = [controller()];
    let mut devices = Devices::default();
    let _ = step(&mut devices, &boot, &boot);
    let found = [controller(), device(3, CONTROLLER, 0x11, true, false)];
    assert_eq!(step(&mut devices, &boot, &found), Moves::default());
}

#[test]
fn only_a_removable_device_moves() {
    let boot = [controller()];
    let mut devices = Devices::default();
    let _ = step(&mut devices, &boot, &boot);
    // An array, a volume or a channel published late is not plugged in.
    let composed = [controller(), device(3, CONTROLLER, 0, false, true)];
    assert_eq!(step(&mut devices, &boot, &composed), Moves::default());
    assert_eq!(step(&mut devices, &composed, &boot), Moves::default());
}

#[test]
fn a_device_that_came_or_went_with_its_parent_did_not_move_on_its_own() {
    let boot = [controller()];
    let mut devices = Devices::default();
    let _ = step(&mut devices, &boot, &boot);
    // A hub plugged in with a device behind it, both published before the
    // manager next looked: the hub's arrival is the one event.
    let hub = device(6, CONTROLLER, 0x31, true, true);
    let behind = device(7, 6, 0x41, true, true);
    let plugged = [controller(), hub, behind];
    assert_eq!(
        step(&mut devices, &boot, &plugged),
        Moves {
            arrived: 1,
            left: 0
        }
    );
    // Pulled together: the hub's departure is the one event.
    assert_eq!(
        step(&mut devices, &plugged, &boot),
        Moves {
            arrived: 0,
            left: 1
        }
    );
}
