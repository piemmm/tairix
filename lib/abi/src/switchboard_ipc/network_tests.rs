//! The network report's round trip and its fail-closed decode.

use crate::net_ipc::{
    NetAddrFamily, NetAddrState, NetIfAddr, NetInterfaceStateRecord, IF_NAME_LEN, NET_IF_MAX_ADDRS,
};
use crate::Errno;

use super::{
    is_network_report, NetworkReport, NETWORK_REPORT_INTERFACES_MAX, NETWORK_REPORT_MAGIC,
};

fn interface(name: &str, link_up: bool) -> NetInterfaceStateRecord {
    let mut bytes = [0u8; IF_NAME_LEN];
    bytes[..name.len()].copy_from_slice(name.as_bytes());
    let mut addrs = [NetInterfaceStateRecord::EMPTY_ADDR; NET_IF_MAX_ADDRS];
    addrs[0] = NetIfAddr {
        family: NetAddrFamily::V4,
        prefix: 24,
        state: NetAddrState::Preferred,
        addr: [10, 0, 2, 15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    };
    NetInterfaceStateRecord {
        name: bytes,
        link_up,
        addr_count: 1,
        addrs,
    }
}

#[test]
fn a_measured_report_round_trips_and_counts_what_it_omits() {
    const NAMES: [&str; NETWORK_REPORT_INTERFACES_MAX + 2] = [
        "eth0", "eth1", "eth2", "eth3", "eth4", "eth5", "eth6", "eth7", "eth8", "eth9",
    ];
    let many = NAMES.map(|name| interface(name, name.ends_with(['0', '2', '4', '6', '8'])));
    let report = NetworkReport::measured(&many);
    assert!(report.is_measured());
    assert_eq!(report.interfaces().count(), NETWORK_REPORT_INTERFACES_MAX);
    assert_eq!(report.omitted(), 2);
    let bytes = report.to_le_bytes();
    assert!(is_network_report(&bytes));
    assert_eq!(NetworkReport::from_bytes(&bytes), Ok(report));
}

#[test]
fn an_unmeasured_report_round_trips_naming_nothing() {
    let bytes = NetworkReport::UNMEASURED.to_le_bytes();
    let decoded = NetworkReport::from_bytes(&bytes).expect("decodes");
    assert!(!decoded.is_measured());
    assert_eq!(decoded.interfaces().count(), 0);
}

#[test]
fn a_malformed_report_is_refused_whole() {
    let good = NetworkReport::measured(&[interface("eth0", true)]).to_le_bytes();
    let with = |edit: &dyn Fn(&mut [u8])| {
        let mut bytes = good;
        edit(&mut bytes);
        NetworkReport::from_bytes(&bytes)
    };
    assert_eq!(
        NetworkReport::from_bytes(&good[..good.len() - 1]),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(with(&|b| b[0] ^= 1), Err(Errno::BadMagic));
    assert_eq!(with(&|b| b[4] = 9), Err(Errno::AbiVersionUnsupported));
    assert_eq!(
        with(&|b| b[6] = 0b11),
        Err(Errno::OutOfRange),
        "a reserved flag"
    );
    assert_eq!(
        with(&|b| b[10] = 1),
        Err(Errno::OutOfRange),
        "a reserved byte"
    );
    assert_eq!(
        with(&|b| b[7] = 9),
        Err(Errno::LengthOutOfRange),
        "more than it holds"
    );
    assert_eq!(
        with(&|b| b[6] = 0),
        Err(Errno::OutOfRange),
        "unmeasured naming one"
    );
    assert_eq!(
        with(&|b| b[8] = 1),
        Err(Errno::OutOfRange),
        "interfaces omitted while fewer than it holds are named"
    );
    let unmeasured_omitting = {
        let mut bytes = NetworkReport::UNMEASURED.to_le_bytes();
        bytes[8] = 1;
        bytes
    };
    assert_eq!(
        NetworkReport::from_bytes(&unmeasured_omitting),
        Err(Errno::OutOfRange),
        "an unmeasured report omitting any"
    );
    assert_eq!(
        with(&|b| b[12 + 16] = 2),
        Err(Errno::OutOfRange),
        "an interface record its own decode refuses"
    );
    let second = 12 + NetInterfaceStateRecord::WIRE_LEN;
    assert_eq!(
        with(&|b| b[second] = 1),
        Err(Errno::OutOfRange),
        "a slot past the count"
    );
    assert_eq!(
        u32::from_le_bytes([good[0], good[1], good[2], good[3]]),
        NETWORK_REPORT_MAGIC
    );
}
