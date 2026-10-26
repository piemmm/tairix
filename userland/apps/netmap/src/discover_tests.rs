//! Unit tests for how the map draws what the segment advertises.

use super::*;

fn eth0() -> InterfaceName {
    let mut name = [0u8; tairix_abi::net_ipc::IF_NAME_LEN];
    name[..4].copy_from_slice(b"eth0");
    name
}

/// A resolved printer whose instance is `label`.
fn printer(label: &[u8]) -> Service {
    Service {
        instance: label.to_vec(),
        name: b"ipp".to_vec(),
        transport: Transport::Tcp,
        interface: eth0(),
        port: 631,
        host: b"\x07printer\x05local\x00".to_vec(),
        addresses: Vec::from([IpAddr::V4(core::net::Ipv4Addr::new(192, 168, 1, 20))]),
        text: Vec::from([b"\x09txtvers=1\x00\x0crp=ipp/print".to_vec()]),
    }
}

#[test]
fn a_service_is_drawn_with_its_type_host_and_strings() {
    let [service] = advertised(Vec::from([printer(b"Office Printer")]))
        .try_into()
        .expect("one service");
    assert_eq!(service.instance, "Office Printer");
    assert_eq!(service.service, "_ipp._tcp");
    assert_eq!(service.host, "printer.local");
    assert_eq!(
        service.text,
        ["txtvers=1", "rp=ipp/print"],
        "an empty string is no string"
    );
    let named = names(BTreeMap::from([(
        service.addresses[0],
        &b"\x03nas\x05local\x00"[..],
    )]));
    assert_eq!(
        named.get(&service.addresses[0]).map(String::as_str),
        Some("nas.local")
    );
}

#[test]
fn a_peer_authored_label_cannot_hide_anything() {
    let drawn = advertised(Vec::from([printer("Print\u{200b}er".as_bytes())]));
    assert_eq!(drawn[0].instance, "Print\\226\\128\\139er");
}
