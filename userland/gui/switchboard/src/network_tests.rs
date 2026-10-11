//! The network report from the interface-state reading.

use tairix_abi::net_ipc::{NetInterfaceStateRecord, IF_NAME_LEN, NET_IF_MAX_ADDRS};

use alloc::vec::Vec;

use super::report;

fn name(text: &str) -> [u8; IF_NAME_LEN] {
    let mut bytes = [0u8; IF_NAME_LEN];
    bytes[..text.len()].copy_from_slice(text.as_bytes());
    bytes
}

fn state(text: &str, link_up: bool) -> NetInterfaceStateRecord {
    NetInterfaceStateRecord {
        name: name(text),
        link_up,
        addr_count: 0,
        addrs: [NetInterfaceStateRecord::EMPTY_ADDR; NET_IF_MAX_ADDRS],
    }
}

#[test]
fn a_missing_reading_is_unmeasured_never_empty() {
    assert!(!report(None).is_measured());
    assert!(report(Some(&[])).is_measured());
}

#[test]
fn the_report_names_each_interface_in_the_stacks_order() {
    let states = [state("bond0", false), state("eth0", true)];
    let made = report(Some(&states));
    assert!(made.is_measured());
    let named: Vec<_> = made.interfaces().map(|i| i.name).collect();
    assert_eq!(named, [name("bond0"), name("eth0")]);
}
