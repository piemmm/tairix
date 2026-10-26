//! Unit tests for WS-Discovery: the grammar of what a peer names, the codec
//! against messages shaped as Windows and `wsdd` send them and as a QNAP NAS
//! sent them, and the engine's rounds, matching, naming, bounds and expiry.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::time::Duration64;
use tairix_hash::HashSeed;

use super::vectors::{
    probe_id, qnap_probe_matches, QNAP_ENDPOINT, QNAP_METADATA, QNAP_NAME, QNAP_PATH, QNAP_PORT,
    QNAP_WORKGROUP,
};
use super::{
    decode, decode_metadata, write_metadata_request, write_probe, write_resolve, AppSequence,
    Change, Computer, Destination, Emit, Endpoint, Membership, Message, MetadataPath, Output, Uuid,
    WsdEngine, WsdError, XAddr,
};
use crate::addr::{IpAddr, Ipv4Addr, Ipv6Addr};
use crate::http::{Authority, Host, HostName, HttpError};

const SEED: HashSeed = HashSeed::from_words(7, 9);
const HOST: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20));
const OTHER: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 21));
const ENDPOINT: &str = "urn:uuid:1f2e3d4c-5b6a-4978-8695-a4b3c2d1e0f9";

fn secs(secs: u64) -> Duration64 {
    Duration64::from_nanos(secs * 1_000_000_000)
}

/// An envelope as `wsdd` writes one, with `body` inside and `header_extra`
/// after its message id.
fn envelope(action: &str, id: &str, header_extra: &str, body: &str) -> String {
    alloc::format!(
        concat!(
            r#"<?xml version="1.0" encoding="utf-8"?>"#,
            r#"<soap:Envelope xmlns:soap="http://www.w3.org/2003/05/soap-envelope""#,
            r#" xmlns:wsa="http://schemas.xmlsoap.org/ws/2004/08/addressing""#,
            r#" xmlns:wsd="http://schemas.xmlsoap.org/ws/2005/04/discovery""#,
            r#" xmlns:wsdp="http://schemas.xmlsoap.org/ws/2006/02/devprof""#,
            r#" xmlns:pub="http://schemas.microsoft.com/windows/pub/2005/07">"#,
            "<soap:Header><wsa:To>urn:schemas-xmlsoap-org:ws:2005:04:discovery</wsa:To>",
            "<wsa:Action>http://schemas.xmlsoap.org/ws/2005/04/discovery/{}</wsa:Action>",
            "<wsa:MessageID>{}</wsa:MessageID>{}</soap:Header>",
            "<soap:Body>{}</soap:Body></soap:Envelope>",
        ),
        action,
        id,
        header_extra,
        body
    )
}

fn sequence(instance: u32, number: u32) -> String {
    alloc::format!(
        r#"<wsd:AppSequence InstanceId="{instance}" SequenceId="urn:uuid:aaaa" MessageNumber="{number}"/>"#
    )
}

/// What a computer target says of itself: its endpoint, types, addresses
/// and metadata version.
fn target_xml(endpoint: &str, xaddrs: &str, version: u32) -> String {
    alloc::format!(
        concat!(
            "<wsa:EndpointReference><wsa:Address>{}</wsa:Address></wsa:EndpointReference>",
            "<wsd:Types>wsdp:Device pub:Computer</wsd:Types>",
            "<wsd:XAddrs>{}</wsd:XAddrs>",
            "<wsd:MetadataVersion>{}</wsd:MetadataVersion>",
        ),
        endpoint,
        xaddrs,
        version
    )
}

fn hello(id: &str, endpoint: &str, xaddrs: &str, version: u32, number: u32) -> String {
    envelope(
        "Hello",
        id,
        &sequence(1, number),
        &alloc::format!(
            "<wsd:Hello>{}</wsd:Hello>",
            target_xml(endpoint, xaddrs, version)
        ),
    )
}

fn probe_matches(id: &str, relates_to: Uuid, targets: &[String]) -> String {
    let matches: String = targets
        .iter()
        .map(|target| ["<wsd:ProbeMatch>", target, "</wsd:ProbeMatch>"].concat())
        .collect();
    envelope(
        "ProbeMatches",
        id,
        &alloc::format!(
            "<wsa:RelatesTo>{relates_to}</wsa:RelatesTo>{}",
            sequence(1, 2)
        ),
        &alloc::format!("<wsd:ProbeMatches>{matches}</wsd:ProbeMatches>"),
    )
}

fn bye(id: &str, endpoint: &str, instance: u32, number: u32) -> String {
    envelope(
        "Bye",
        id,
        &sequence(instance, number),
        &alloc::format!(
            "<wsd:Bye><wsa:EndpointReference><wsa:Address>{endpoint}</wsa:Address>\
             </wsa:EndpointReference></wsd:Bye>"
        ),
    )
}

/// A metadata response as `wsdd` serves one, naming `computer`.
fn metadata_response(computer: &str) -> Vec<u8> {
    metadata("pub:Computer", computer)
}

/// A metadata response whose host has `types` and names `computer`.
fn metadata(types: &str, computer: &str) -> Vec<u8> {
    let body = alloc::format!(
        concat!(
            r#"<?xml version="1.0" encoding="utf-8"?>"#,
            r#"<soap:Envelope xmlns:soap="http://www.w3.org/2003/05/soap-envelope""#,
            r#" xmlns:wsa="http://schemas.xmlsoap.org/ws/2004/08/addressing""#,
            r#" xmlns:wsx="http://schemas.xmlsoap.org/ws/2004/09/mex""#,
            r#" xmlns:wsdp="http://schemas.xmlsoap.org/ws/2006/02/devprof""#,
            r#" xmlns:pub="http://schemas.microsoft.com/windows/pub/2005/07">"#,
            "<soap:Header><wsa:To>http://schemas.xmlsoap.org/ws/2004/08/addressing/role/anonymous</wsa:To>",
            "<wsa:Action>http://schemas.xmlsoap.org/ws/2004/09/transfer/GetResponse</wsa:Action>",
            "<wsa:MessageID>urn:uuid:0b0b</wsa:MessageID></soap:Header><soap:Body><wsx:Metadata>",
            r#"<wsx:MetadataSection Dialect="http://schemas.xmlsoap.org/ws/2006/02/devprof/ThisDevice">"#,
            "<wsdp:ThisDevice><wsdp:FriendlyName>WSD Device</wsdp:FriendlyName></wsdp:ThisDevice>",
            "</wsx:MetadataSection>",
            r#"<wsx:MetadataSection Dialect="http://schemas.xmlsoap.org/ws/2006/02/devprof/Relationship">"#,
            r#"<wsdp:Relationship Type="http://schemas.xmlsoap.org/ws/2006/02/devprof/host">"#,
            "<wsdp:Host><wsa:EndpointReference><wsa:Address>{}</wsa:Address></wsa:EndpointReference>",
            "<wsdp:Types>{}</wsdp:Types><pub:Computer>{}</pub:Computer></wsdp:Host>",
            "</wsdp:Relationship></wsx:MetadataSection></wsx:Metadata></soap:Body></soap:Envelope>",
        ),
        ENDPOINT, types, computer
    );
    let mut response = alloc::format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/soap+xml\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body.as_bytes());
    response
}

#[test]
fn a_minted_uuid_is_version_four_and_reads_back_in_any_case() {
    let id = Uuid::from_random([0xff; 16]);
    let text = alloc::format!("{id}");
    assert_eq!(text, "urn:uuid:ffffffff-ffff-4fff-bfff-ffffffffffff");
    assert_eq!(Uuid::parse_urn(&text), Some(id));
    assert_eq!(
        Uuid::parse_urn(&text.to_uppercase().replace("URN:UUID:", "urn:uuid:")),
        Some(id)
    );
    for bad in [
        "urn:uuid:ffffffff-ffff-4fff-bfff-fffffffffff",
        "urn:uuid:ffffffff_ffff-4fff-bfff-ffffffffffff",
        "urn:uuid:gfffffff-ffff-4fff-bfff-ffffffffffff",
        "uuid:ffffffff-ffff-4fff-bfff-ffffffffffff",
    ] {
        assert_eq!(Uuid::parse_urn(bad), None, "{bad}");
    }
}

#[test]
fn an_endpoint_holds_only_what_xml_need_not_escape() {
    assert_eq!(
        Endpoint::new(ENDPOINT).map(|e| String::from(e.as_str())),
        Some(String::from(ENDPOINT))
    );
    for bad in [
        "",
        "urn:a&b",
        "urn:a'b",
        "urn:a<b",
        "urn:a b",
        "urn:caf\u{e9}",
    ] {
        assert_eq!(Endpoint::new(bad), None, "{bad:?}");
    }
    assert!(Endpoint::new(&"a".repeat(255)).is_some());
    assert!(Endpoint::new(&"a".repeat(256)).is_none());
}

#[test]
fn a_metadata_path_can_neither_end_a_request_line_nor_change_resource() {
    assert_eq!(
        MetadataPath::new("/a/b?c=d").map(|p| String::from(p.as_str())),
        Some(String::from("/a/b?c=d"))
    );
    for bad in [
        "",
        "relative",
        "/a b",
        "/x#frag",
        "/a\r\nHost: x",
        "/caf\u{e9}",
    ] {
        assert_eq!(MetadataPath::new(bad), None, "{bad:?}");
    }
    assert!(MetadataPath::new(&alloc::format!("/{}", "a".repeat(254))).is_some());
    assert!(MetadataPath::new(&alloc::format!("/{}", "a".repeat(255))).is_none());
}

#[test]
fn an_xaddr_names_its_host_by_address_or_by_name() {
    let v4 = XAddr::parse("http://192.168.1.20:5357/1f2e-abc").expect("v4");
    assert_eq!(
        (&v4.host, v4.port, v4.path.as_str()),
        (&Host::Addr(HOST), 5357, "/1f2e-abc")
    );
    let default = XAddr::parse("HTTP://10.0.0.1/").expect("port 80");
    assert_eq!(default.port, 80);
    let v6 = XAddr::parse("http://[fe80::1%254]:5357/x").expect("v6 with zone");
    assert_eq!(
        v6.host,
        Host::Addr(IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)))
    );
    let named = XAddr::parse("http://OFFICE:8080/wsd/x").expect("a name");
    assert_eq!(
        (&named.host, named.port),
        (&Host::Name(HostName::new("OFFICE").expect("a name")), 8080)
    );
    let dotted = XAddr::parse("http://server.local/x").expect("a dotted name");
    assert!(matches!(dotted.host, Host::Name(ref n) if n.as_str() == "server.local"));
    let digit = XAddr::parse("http://OFFI0E:8080/x").expect("a digit inside a label");
    assert!(
        matches!(digit.host, Host::Name(ref n) if n.as_str() == "OFFI0E"),
        "a name, as `fuzz_net_wsd`'s mutator spelled one"
    );
    for bad in [
        "https://192.168.1.20/x",
        "http://192.168.001.20/x",
        "http://192.168.1.20:0/x",
        "http://192.168.1.20:65536/x",
        "http://192.168.1.20",
        "http://192.168.1.20/x#frag",
        "http://192.168.1.20/a b",
        "http://[fe80::1:5357/x",
        "http://1.2.3/x",
        "http://user@OFFICE/x",
        "http://OFF%49CE/x",
        "http://OFFICE:/x",
        "http:///x",
    ] {
        assert_eq!(XAddr::parse(bad), None, "{bad}");
    }
}

#[test]
fn a_computer_value_names_its_workgroup_or_domain() {
    let computer = Computer::parse("FILESERVER/Workgroup:WORKGROUP").expect("workgroup");
    assert_eq!(computer.name.as_str(), "FILESERVER");
    assert_eq!(
        computer.membership,
        Membership::Workgroup("WORKGROUP".try_into().expect("fits"))
    );
    let joined = Computer::parse("ws1/Domain:corp.example").expect("domain");
    assert_eq!(
        joined.membership,
        Membership::Domain("corp.example".try_into().expect("fits"))
    );
    assert_eq!(
        Computer::parse("alone").map(|c| c.membership),
        Some(Membership::Unstated)
    );
    assert_eq!(
        Computer::parse("x/NotJoined").map(|c| c.membership),
        Some(Membership::Unstated)
    );
    for bad in ["", "/Workgroup:WG", "x/Workgroup:", "x/Domain:"] {
        assert_eq!(Computer::parse(bad), None, "{bad}");
    }
    assert_eq!(Computer::parse(&"n".repeat(64)), None);
}

#[test]
fn an_app_sequence_orders_restarts_and_numbers() {
    let at = |instance, sequence, number| AppSequence {
        instance,
        sequence,
        number,
    };
    assert!(at(1, 5, 3).follows(at(1, 5, 2)));
    assert!(!at(1, 5, 2).follows(at(1, 5, 2)), "a replay");
    assert!(!at(1, 5, 1).follows(at(1, 5, 2)), "stale");
    assert!(at(2, 5, 1).follows(at(1, 5, 9)), "a restart");
    assert!(!at(1, 5, 9).follows(at(2, 5, 1)), "an earlier life");
    assert!(at(1, 6, 1).follows(at(1, 5, 9)), "another sequence");
}

#[test]
fn a_hello_is_read_by_namespace_whatever_the_prefixes() {
    let text = hello(
        "urn:uuid:01",
        ENDPOINT,
        "http://192.168.1.20:5357/abc http://[fe80::2]:5357/abc",
        3,
        7,
    );
    let envelope = decode(text.as_bytes(), SEED).expect("a hello");
    let Message::Hello(target) = envelope.message else {
        panic!("not a hello: {:?}", envelope.message);
    };
    assert_eq!(target.endpoint.as_str(), ENDPOINT);
    assert!(target.computer);
    assert_eq!(target.xaddrs.len(), 2);
    assert_eq!(target.metadata_version, 3);
    assert_eq!(
        envelope.app_sequence.map(|s| (s.instance, s.number)),
        Some((1, 7))
    );
    // The same message with other prefixes, a default namespace, and the
    // computer type spelled with its own prefix.
    let renamed = concat!(
        r#"<e:Envelope xmlns:e="http://www.w3.org/2003/05/soap-envelope""#,
        r#" xmlns:a="http://schemas.xmlsoap.org/ws/2004/08/addressing">"#,
        "<e:Header><a:Action>http://schemas.xmlsoap.org/ws/2005/04/discovery/Hello</a:Action>",
        "<a:MessageID>urn:uuid:02</a:MessageID></e:Header>",
        r#"<e:Body><Hello xmlns="http://schemas.xmlsoap.org/ws/2005/04/discovery""#,
        r#" xmlns:m="http://schemas.microsoft.com/windows/pub/2005/07">"#,
        "<a:EndpointReference><a:Address>urn:uuid:x</a:Address></a:EndpointReference>",
        "<Types>m:Computer</Types><MetadataVersion>1</MetadataVersion></Hello></e:Body></e:Envelope>",
    );
    let envelope = decode(renamed.as_bytes(), SEED).expect("renamed");
    assert!(matches!(envelope.message, Message::Hello(ref t) if t.computer && t.xaddrs.is_empty()));
}

#[test]
fn what_a_message_says_twice_or_not_at_all_is_refused() {
    let doubled = hello("urn:uuid:01", ENDPOINT, "", 1, 1).replace(
        "</wsa:Address>",
        &alloc::format!("</wsa:Address><wsa:Address>{ENDPOINT}</wsa:Address>"),
    );
    assert_eq!(
        decode(doubled.as_bytes(), SEED).err(),
        Some(WsdError::Malformed)
    );
    let unnumbered = hello("urn:uuid:01", ENDPOINT, "", 1, 1).replace(
        "<wsd:MetadataVersion>1</wsd:MetadataVersion>",
        "<wsd:MetadataVersion>-1</wsd:MetadataVersion>",
    );
    assert_eq!(
        decode(unnumbered.as_bytes(), SEED).err(),
        Some(WsdError::Malformed)
    );
    let anonymous = hello("urn:uuid:01", ENDPOINT, "", 1, 1)
        .replace("<wsa:MessageID>urn:uuid:01</wsa:MessageID>", "");
    assert_eq!(
        decode(anonymous.as_bytes(), SEED).err(),
        Some(WsdError::Malformed)
    );
    let unknown = envelope("Gossip", "urn:uuid:01", "", "<wsd:Gossip/>");
    assert_eq!(
        decode(unknown.as_bytes(), SEED).err(),
        Some(WsdError::Unsupported)
    );
    assert_eq!(decode(b"\xff\xfe<", SEED).err(), Some(WsdError::Malformed));
    assert_eq!(decode(b"<a/>", SEED).err(), Some(WsdError::Malformed));
    // A type spelled with an unbound prefix names nothing.
    let unbound = hello("urn:uuid:01", ENDPOINT, "", 1, 1).replace("pub:Computer", "zz:Computer");
    assert!(
        matches!(decode(unbound.as_bytes(), SEED).map(|e| e.message), Ok(Message::Hello(t)) if !t.computer)
    );
}

#[test]
fn matches_byes_and_queries_are_told_apart() {
    let asked = Uuid::from_random([1; 16]);
    let text = probe_matches("urn:uuid:03", asked, &[target_xml(ENDPOINT, "", 2)]);
    let envelope = decode(text.as_bytes(), SEED).expect("matches");
    assert_eq!(envelope.relates_to, Some(asked));
    assert!(matches!(envelope.message, Message::ProbeMatches(ref t) if t.len() == 1));
    let gone = decode(bye("urn:uuid:04", ENDPOINT, 1, 3).as_bytes(), SEED).expect("bye");
    assert!(matches!(gone.message, Message::Bye { ref endpoint } if endpoint.as_str() == ENDPOINT));
    let mut probe = [0u8; 2048];
    let len = write_probe(&mut probe, asked).expect("fits");
    assert_eq!(
        decode(&probe[..len], SEED).map(|e| e.message),
        Ok(Message::Query)
    );
    assert_ne!(
        decode(text.as_bytes(), SEED).map(|e| e.message_id),
        decode(text.replace("urn:uuid:03", "urn:uuid:05").as_bytes(), SEED).map(|e| e.message_id)
    );
}

#[test]
fn what_this_host_sends_says_exactly_what_it_must() {
    let id = Uuid::from_random([2; 16]);
    let mut out = [0u8; 2048];
    let len = write_probe(&mut out, id).expect("fits");
    let probe = core::str::from_utf8(&out[..len]).expect("UTF-8");
    assert!(
        probe.contains("<wsd:Types>wsdp:Device</wsd:Types>"),
        "wsdd's spelling"
    );
    assert!(probe.contains(&alloc::format!("<wsa:MessageID>{id}</wsa:MessageID>")));
    let endpoint = Endpoint::new(ENDPOINT).expect("an endpoint");
    let len = write_resolve(&mut out, id, &endpoint).expect("fits");
    let resolve = core::str::from_utf8(&out[..len]).expect("UTF-8");
    assert!(resolve.contains(&alloc::format!("<wsa:Address>{ENDPOINT}</wsa:Address>")));
    let host = Host::Addr(HOST);
    let at = Authority {
        host: &host,
        port: 5357,
    };
    let path = MetadataPath::new("/abc").expect("a path");
    let len = write_metadata_request(&mut out, at, &path, &endpoint, id).expect("fits");
    let request = core::str::from_utf8(&out[..len]).expect("UTF-8");
    assert!(request.starts_with("POST /abc HTTP/1.1\r\nHost: 192.168.1.20:5357\r\n"));
    assert!(request.contains("Connection: close\r\n"));
    assert!(request.contains(&alloc::format!("<wsa:To>{ENDPOINT}</wsa:To>")));
    assert!(request.contains("transfer/Get</wsa:Action>"));
    assert_eq!(write_probe(&mut out[..100], id), Err(WsdError::TooLarge));
    assert_eq!(
        write_metadata_request(&mut out[..200], at, &path, &endpoint, id),
        Err(WsdError::TooLarge)
    );
}

#[test]
fn metadata_names_the_computer_its_host_relationship_names() {
    let computer = decode_metadata(&metadata_response("FILESERVER/Workgroup:HOME")).expect("named");
    assert_eq!(computer.name.as_str(), "FILESERVER");
    assert_eq!(
        computer.membership,
        Membership::Workgroup("HOME".try_into().expect("fits"))
    );
    let device = metadata("wsdp:Device", "FILESERVER/Workgroup:HOME");
    assert_eq!(
        decode_metadata(&device).err(),
        Some(WsdError::Malformed),
        "not a computer"
    );
    assert_eq!(
        decode_metadata(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n").err(),
        Some(WsdError::Http(HttpError::Status(404)))
    );
}

#[test]
fn a_qnaps_answer_and_metadata_read_as_it_sent_them() {
    let asked = Uuid::from_random([3; 16]);
    let text = qnap_probe_matches(Uuid::from_random([4; 16]), asked, 2_801_352_027);
    let envelope = decode(text.as_bytes(), SEED).expect("its answer");
    assert_eq!(envelope.relates_to, Some(asked));
    assert_eq!(
        envelope.app_sequence,
        Some(AppSequence {
            instance: 2_801_352_027,
            sequence: 0,
            number: 1
        }),
        "numbered with no sequence id"
    );
    let Message::ProbeMatches(targets) = envelope.message else {
        panic!("not matches: {:?}", envelope.message);
    };
    let [target] = targets.as_slice() else {
        panic!("one target: {targets:?}");
    };
    assert_eq!(
        target.endpoint.as_str(),
        QNAP_ENDPOINT,
        "a bare UUID, its line break trimmed"
    );
    assert!(target.computer);
    assert_eq!(target.metadata_version, 1);
    let [xaddr] = target.xaddrs.as_slice() else {
        panic!("one URL: {:?}", target.xaddrs);
    };
    let name = Host::Name(HostName::new(QNAP_NAME).expect("its name"));
    assert_eq!(
        (&xaddr.host, xaddr.port, xaddr.path.as_str()),
        (&name, QNAP_PORT, QNAP_PATH)
    );
    let computer = decode_metadata(QNAP_METADATA).expect("its chunked metadata");
    assert_eq!(computer.name.as_str(), QNAP_NAME);
    assert_eq!(
        computer.membership,
        Membership::Workgroup(QNAP_WORKGROUP.try_into().expect("fits"))
    );
}

/// An engine browsing from `now` whose first probe has gone, with that
/// probe's id.
fn browsing(now: Duration64) -> (WsdEngine, Uuid) {
    let mut engine = WsdEngine::new(SEED);
    let mut counter = 0u32;
    let mut rng = || {
        counter = counter.wrapping_add(0x9e37_79b9);
        counter
    };
    engine.browse(now, &mut rng);
    let mut out = [0u8; 2048];
    let Some(Output::Transmit(Emit {
        len,
        to: Destination::Group,
    })) = engine.poll(now, &mut rng, &mut out, &mut |_| {}, &mut || true)
    else {
        panic!("browsing starts with a probe");
    };
    (engine, probe_id(&out[..len]).expect("the probe's id"))
}

fn rng() -> impl FnMut() -> u32 {
    let mut counter = 7u32;
    move || {
        counter = counter.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        counter
    }
}

/// What a poll asked for, copied out of the engine so it can be driven on.
#[derive(Debug, PartialEq)]
enum Polled {
    Transmit(Emit),
    Fetch(Fetched),
}

/// A fetch the engine asked for, copied out.
#[derive(Debug, PartialEq)]
struct Fetched {
    id: u32,
    addr: IpAddr,
    port: u16,
    path: String,
    endpoint: String,
    name: Option<String>,
}

impl From<Output<'_>> for Polled {
    fn from(output: Output<'_>) -> Self {
        match output {
            Output::Transmit(emit) => Self::Transmit(emit),
            Output::Fetch(fetch) => Self::Fetch(Fetched {
                id: fetch.id,
                addr: fetch.addr,
                port: fetch.port,
                path: String::from(fetch.path.as_str()),
                endpoint: String::from(fetch.endpoint.as_str()),
                name: fetch.name.map(|name| String::from(name.as_str())),
            }),
        }
    }
}

/// Poll `engine` at `now` past any probe repeat, returning its first output
/// that is not a probe and the changes it reported.
fn poll_past_probes(engine: &mut WsdEngine, now: Duration64) -> (Option<Polled>, Vec<Change>) {
    let mut changes = Vec::new();
    let mut out = [0u8; 2048];
    let mut rng = rng();
    for _ in 0..8 {
        let polled = engine
            .poll(
                now,
                &mut rng,
                &mut out,
                &mut |change| changes.push(change),
                &mut || true,
            )
            .map(Polled::from);
        match polled {
            Some(Polled::Transmit(Emit { len, .. })) if probe_id(&out[..len]).is_some() => {}
            other => return (other, changes),
        }
    }
    (None, changes)
}

#[test]
fn an_idle_engine_hears_and_sends_nothing() {
    let mut engine = WsdEngine::new(SEED);
    let mut changes = Vec::new();
    let text = hello(
        "urn:uuid:01",
        ENDPOINT,
        "http://192.168.1.20:5357/abc",
        1,
        1,
    );
    engine.on_datagram(secs(1), text.as_bytes(), HOST, &mut |c| changes.push(c));
    let mut out = [0u8; 2048];
    assert_eq!(
        engine.poll(
            secs(1),
            &mut rng(),
            &mut out,
            &mut |c| changes.push(c),
            &mut || true
        ),
        None
    );
    assert_eq!(engine.next_deadline(Some(Duration64::ZERO)), None);
    assert!(changes.is_empty());
}

#[test]
fn a_probe_round_repeats_then_waits_for_the_next() {
    let (mut engine, id) = browsing(secs(10));
    let mut out = [0u8; 2048];
    let mut sent = Vec::new();
    let mut rng = rng();
    for _ in 0..4 {
        let now = engine
            .next_deadline(Some(Duration64::ZERO))
            .expect("a repeat is due");
        if let Some(Output::Transmit(Emit { len, .. })) =
            engine.poll(now, &mut rng, &mut out, &mut |_| {}, &mut || true)
        {
            let text = String::from(core::str::from_utf8(&out[..len]).expect("UTF-8"));
            sent.push((now, text.contains(&alloc::format!("{id}"))));
        }
    }
    assert_eq!(sent.len(), 4);
    assert!(
        sent[..3].iter().all(|&(_, same)| same),
        "three repeats of the first probe"
    );
    assert!(!sent[3].1, "a fresh round with a fresh id");
    assert!(sent[2].0 < secs(12), "the repeats come within two seconds");
    assert!(sent[3].0 >= secs(70), "the next round a minute on");
}

#[test]
fn a_computer_is_reported_once_its_metadata_is_read() {
    let (mut engine, _) = browsing(secs(1));
    let mut changes = Vec::new();
    let text = hello(
        "urn:uuid:01",
        ENDPOINT,
        "http://10.9.9.9:5357/x http://192.168.1.20:5357/abc",
        2,
        1,
    );
    engine.on_datagram(secs(2), text.as_bytes(), HOST, &mut |c| changes.push(c));
    let (output, _) = poll_past_probes(&mut engine, secs(2));
    let Some(Polled::Fetch(fetch)) = output else {
        panic!("a fetch is due: {output:?}");
    };
    assert_eq!(
        (fetch.addr, fetch.port, fetch.path.as_str()),
        (HOST, 5357, "/abc"),
        "the URL it sent from"
    );
    assert_eq!(fetch.endpoint.as_str(), ENDPOINT);
    engine.on_fetched(
        secs(3),
        fetch.id,
        &metadata_response("FILESERVER/Workgroup:HOME"),
        &mut |c| changes.push(c),
    );
    let [Change::Found { addr, computer, .. }] = changes.as_slice() else {
        panic!("one computer found: {changes:?}");
    };
    assert_eq!((*addr, computer.name.as_str()), (HOST, "FILESERVER"));
    // The same announcement again, and a fresh one with the same version,
    // ask for nothing more.
    engine.on_datagram(secs(4), text.as_bytes(), HOST, &mut |c| changes.push(c));
    let again = hello(
        "urn:uuid:06",
        ENDPOINT,
        "http://192.168.1.20:5357/abc",
        2,
        2,
    );
    engine.on_datagram(secs(4), again.as_bytes(), HOST, &mut |c| changes.push(c));
    assert_eq!(poll_past_probes(&mut engine, secs(4)).0, None);
    assert_eq!(changes.len(), 1);
}

#[test]
fn only_answers_to_this_hosts_own_probes_are_believed() {
    let (mut engine, probe) = browsing(secs(1));
    let target = target_xml(ENDPOINT, "http://192.168.1.20:5357/abc", 1);
    let stranger = probe_matches(
        "urn:uuid:07",
        Uuid::from_random([9; 16]),
        core::slice::from_ref(&target),
    );
    engine.on_datagram(secs(2), stranger.as_bytes(), HOST, &mut |_| {});
    assert_eq!(
        poll_past_probes(&mut engine, secs(2)).0,
        None,
        "an unasked answer"
    );
    let late = probe_matches("urn:uuid:08", probe, core::slice::from_ref(&target));
    engine.on_datagram(secs(30), late.as_bytes(), HOST, &mut |_| {});
    assert_eq!(
        poll_past_probes(&mut engine, secs(30)).0,
        None,
        "an answer too late"
    );
    let answer = probe_matches("urn:uuid:09", probe, &[target]);
    engine.on_datagram(secs(3), answer.as_bytes(), HOST, &mut |_| {});
    assert!(matches!(
        poll_past_probes(&mut engine, secs(3)).0,
        Some(Polled::Fetch(_))
    ));
}

#[test]
fn a_target_naming_no_address_it_sent_from_is_resolved() {
    let (mut engine, _) = browsing(secs(1));
    let text = hello("urn:uuid:01", ENDPOINT, "http://10.9.9.9:5357/x", 1, 1);
    engine.on_datagram(secs(2), text.as_bytes(), HOST, &mut |_| {});
    let mut out = [0u8; 2048];
    let mut rng = rng();
    let mut resolves = 0;
    let mut now = secs(2);
    for _ in 0..12 {
        if let Some(Output::Transmit(Emit { len, .. })) =
            engine.poll(now, &mut rng, &mut out, &mut |_| {}, &mut || true)
        {
            let text = core::str::from_utf8(&out[..len]).expect("UTF-8");
            if text.contains("wsd:Resolve>") {
                resolves += 1;
                assert!(text.contains(ENDPOINT));
            }
        }
        now = engine
            .next_deadline(Some(Duration64::ZERO))
            .map_or(now, |due| due.max(now));
    }
    assert_eq!(resolves, 3, "three tries, then the target is dropped");
}

#[test]
fn a_bye_retires_a_computer_unless_it_is_stale() {
    let (mut engine, _) = browsing(secs(1));
    let mut changes = Vec::new();
    let text = hello(
        "urn:uuid:01",
        ENDPOINT,
        "http://192.168.1.20:5357/abc",
        1,
        5,
    );
    engine.on_datagram(secs(2), text.as_bytes(), HOST, &mut |c| changes.push(c));
    let Some(Polled::Fetch(fetch)) = poll_past_probes(&mut engine, secs(2)).0 else {
        panic!("a fetch is due");
    };
    engine.on_fetched(
        secs(2),
        fetch.id,
        &metadata_response("PC/Workgroup:HOME"),
        &mut |c| changes.push(c),
    );
    engine.on_datagram(
        secs(3),
        bye("urn:uuid:10", ENDPOINT, 1, 4).as_bytes(),
        HOST,
        &mut |c| changes.push(c),
    );
    assert_eq!(changes.len(), 1, "a stale goodbye is ignored");
    engine.on_datagram(
        secs(3),
        bye("urn:uuid:11", ENDPOINT, 1, 6).as_bytes(),
        HOST,
        &mut |c| changes.push(c),
    );
    assert!(matches!(changes.last(), Some(Change::Gone { .. })));
}

#[test]
fn a_failed_fetch_is_tried_again_later_and_less_often() {
    let (mut engine, _) = browsing(secs(1));
    let text = hello(
        "urn:uuid:01",
        ENDPOINT,
        "http://192.168.1.20:5357/abc",
        1,
        1,
    );
    engine.on_datagram(secs(2), text.as_bytes(), HOST, &mut |_| {});
    let Some(Polled::Fetch(first)) = poll_past_probes(&mut engine, secs(2)).0 else {
        panic!("a fetch is due");
    };
    engine.on_fetch_failed(secs(2), first.id);
    assert_eq!(
        poll_past_probes(&mut engine, secs(20)).0,
        None,
        "not before the backoff"
    );
    let Some(Polled::Fetch(second)) = poll_past_probes(&mut engine, secs(32)).0 else {
        panic!("tried again after thirty seconds");
    };
    assert_ne!(first.id, second.id);
    engine.on_fetched(
        secs(32),
        second.id,
        b"HTTP/1.1 500 Oops\r\n\r\n",
        &mut |_| {},
    );
    // Answering every round keeps it alive while its fetch backs off.
    let keep = hello(
        "urn:uuid:02",
        ENDPOINT,
        "http://192.168.1.20:5357/abc",
        1,
        2,
    );
    engine.on_datagram(secs(80), keep.as_bytes(), HOST, &mut |_| {});
    assert_eq!(
        poll_past_probes(&mut engine, secs(80)).0,
        None,
        "a minute's backoff now"
    );
    assert!(matches!(
        poll_past_probes(&mut engine, secs(93)).0,
        Some(Polled::Fetch(_))
    ));
}

#[test]
fn a_computer_that_stops_answering_expires() {
    let (mut engine, _) = browsing(secs(1));
    let mut changes = Vec::new();
    let text = hello(
        "urn:uuid:01",
        ENDPOINT,
        "http://192.168.1.20:5357/abc",
        1,
        1,
    );
    engine.on_datagram(secs(2), text.as_bytes(), HOST, &mut |c| changes.push(c));
    let Some(Polled::Fetch(fetch)) = poll_past_probes(&mut engine, secs(2)).0 else {
        panic!("a fetch is due");
    };
    engine.on_fetched(
        secs(2),
        fetch.id,
        &metadata_response("PC/Workgroup:HOME"),
        &mut |c| changes.push(c),
    );
    assert!(engine
        .next_deadline(Some(Duration64::ZERO))
        .is_some_and(|due| due <= secs(153)));
    let (_, expired) = poll_past_probes(&mut engine, secs(153));
    assert!(
        matches!(expired.as_slice(), [Change::Gone { .. }]),
        "{expired:?}"
    );
}

#[test]
fn one_sender_holds_at_most_four_targets_and_two_fetches_fly() {
    let (mut engine, _) = browsing(secs(1));
    for i in 0..6 {
        let endpoint = alloc::format!("urn:uuid:{i}");
        let id = alloc::format!("urn:uuid:id{i}");
        let text = hello(&id, &endpoint, "http://192.168.1.20:5357/abc", 1, 1);
        engine.on_datagram(secs(2), text.as_bytes(), HOST, &mut |_| {});
    }
    let other = hello(
        "urn:uuid:other",
        "urn:uuid:o",
        "http://192.168.1.21:5357/abc",
        1,
        1,
    );
    engine.on_datagram(secs(2), other.as_bytes(), OTHER, &mut |_| {});
    let mut fetches = Vec::new();
    while let (Some(Polled::Fetch(fetch)), _) = poll_past_probes(&mut engine, secs(2)) {
        fetches.push(fetch);
    }
    assert_eq!(fetches.len(), 2, "two fly at once");
    for fetch in fetches.drain(..) {
        engine.on_fetch_failed(secs(2), fetch.id);
    }
    while let (Some(Polled::Fetch(fetch)), _) = poll_past_probes(&mut engine, secs(2)) {
        engine.on_fetch_failed(secs(2), fetch.id);
        fetches.push(fetch);
    }
    let from_host = fetches.iter().filter(|f| f.addr == HOST).count() + 2;
    assert!(from_host <= 4, "{from_host} targets tracked for one sender");
    assert!(
        fetches.iter().any(|f| f.addr == OTHER),
        "another sender is not crowded out"
    );
}

#[test]
fn stopping_forgets_everything_and_late_answers_are_ignored() {
    let (mut engine, _) = browsing(secs(1));
    let text = hello(
        "urn:uuid:01",
        ENDPOINT,
        "http://192.168.1.20:5357/abc",
        1,
        1,
    );
    engine.on_datagram(secs(2), text.as_bytes(), HOST, &mut |_| {});
    let Some(Polled::Fetch(fetch)) = poll_past_probes(&mut engine, secs(2)).0 else {
        panic!("a fetch is due");
    };
    engine.stop();
    assert!(!engine.is_browsing());
    let mut changes = Vec::new();
    engine.on_fetched(
        secs(3),
        fetch.id,
        &metadata_response("PC/Workgroup:HOME"),
        &mut |c| changes.push(c),
    );
    assert!(changes.is_empty());
    assert_eq!(engine.next_deadline(Some(Duration64::ZERO)), None);
}

/// Fetch and settle `ENDPOINT` at `HOST`, named `computer`, reporting what
/// the engine said.
fn name_it(
    engine: &mut WsdEngine,
    now: Duration64,
    id: &str,
    version: u32,
    computer: &str,
) -> Vec<Change> {
    let mut changes = Vec::new();
    let text = hello(
        id,
        ENDPOINT,
        "http://192.168.1.20:5357/abc",
        version,
        version + 1,
    );
    engine.on_datagram(now, text.as_bytes(), HOST, &mut |c| changes.push(c));
    let Some(Polled::Fetch(fetch)) = poll_past_probes(engine, now).0 else {
        panic!("a fetch is due");
    };
    engine.on_fetched(now, fetch.id, &metadata_response(computer), &mut |c| {
        changes.push(c);
    });
    changes
}

#[test]
fn a_renamed_computer_goes_and_is_found_again_and_an_unchanged_one_does_not() {
    let (mut engine, _) = browsing(secs(1));
    let first = name_it(&mut engine, secs(2), "urn:uuid:01", 1, "PC/Workgroup:HOME");
    assert!(matches!(first.as_slice(), [Change::Found { .. }]));
    let renamed = name_it(
        &mut engine,
        secs(3),
        "urn:uuid:02",
        2,
        "STUDY/Workgroup:HOME",
    );
    let [Change::Gone {
        key: gone,
        computer: was,
        ..
    }, Change::Found { key, computer, .. }] = renamed.as_slice()
    else {
        panic!("a revision: {renamed:?}");
    };
    assert_eq!(gone, key, "under one key");
    assert_eq!(
        (was.name.as_str(), computer.name.as_str()),
        ("PC", "STUDY"),
        "what it was, and is"
    );
    let same = name_it(
        &mut engine,
        secs(4),
        "urn:uuid:03",
        3,
        "STUDY/Workgroup:HOME",
    );
    assert!(same.is_empty(), "nothing changed: {same:?}");
    let listed: Vec<_> = engine
        .computers()
        .map(|(_, addr, c)| (addr, String::from(c.name.as_str())))
        .collect();
    assert_eq!(listed, [(HOST, String::from("STUDY"))]);
}

#[test]
fn what_the_send_budget_cannot_pay_for_stays_due() {
    let mut engine = WsdEngine::new(SEED);
    let mut rng = rng();
    engine.browse(secs(1), &mut rng);
    let mut out = [0u8; 2048];
    assert_eq!(
        engine.poll(secs(1), &mut rng, &mut out, &mut |_| {}, &mut || false),
        None,
        "no probe without budget"
    );
    assert_eq!(
        engine.next_deadline(None),
        None,
        "never sendable, never due"
    );
    assert_eq!(
        engine.next_deadline(Some(secs(3))),
        Some(secs(3)),
        "due once the budget allows"
    );
    assert!(matches!(
        engine.poll(secs(3), &mut rng, &mut out, &mut |_| {}, &mut || true),
        Some(Output::Transmit(_))
    ));
}

#[test]
fn a_gone_computer_takes_what_it_was_reported_as() {
    let (mut engine, _) = browsing(secs(1));
    name_it(
        &mut engine,
        secs(2),
        "urn:uuid:01",
        1,
        "PC/Domain:corp.example",
    );
    let mut changes = Vec::new();
    engine.on_datagram(
        secs(3),
        bye("urn:uuid:09", ENDPOINT, 1, 9).as_bytes(),
        HOST,
        &mut |c| changes.push(c),
    );
    let [Change::Gone { addr, computer, .. }] = changes.as_slice() else {
        panic!("gone: {changes:?}");
    };
    assert_eq!((*addr, computer.name.as_str()), (HOST, "PC"));
    assert_eq!(engine.computers().count(), 0);
}

#[test]
fn a_computer_heard_at_two_addresses_stays_at_the_first_until_it_falls_silent() {
    let (mut engine, _) = browsing(secs(1));
    name_it(&mut engine, secs(2), "urn:uuid:01", 1, "PC/Workgroup:HOME");
    let mut changes = Vec::new();
    let v6 = IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 9));
    let other_family = hello("urn:uuid:02", ENDPOINT, "http://[fe80::9]:5357/abc", 1, 3);
    engine.on_datagram(secs(3), other_family.as_bytes(), v6, &mut |c| {
        changes.push(c);
    });
    assert_eq!(
        poll_past_probes(&mut engine, secs(3)).0,
        None,
        "nothing to fetch again"
    );
    assert!(changes.is_empty(), "nor anything to tell");
    let silent_since = hello("urn:uuid:03", ENDPOINT, "http://[fe80::9]:5357/abc", 1, 4);
    engine.on_datagram(secs(160), silent_since.as_bytes(), v6, &mut |c| {
        changes.push(c);
    });
    let Some(Polled::Fetch(fetch)) = poll_past_probes(&mut engine, secs(160)).0 else {
        panic!("fetched where it answers now");
    };
    assert_eq!(fetch.addr, v6);
    engine.on_fetched(
        secs(160),
        fetch.id,
        &metadata_response("PC/Workgroup:HOME"),
        &mut |c| changes.push(c),
    );
    let [Change::Gone { addr: was_at, .. }, Change::Found { addr, .. }] = changes.as_slice() else {
        panic!("moved: {changes:?}");
    };
    assert_eq!((*was_at, *addr), (HOST, v6));
}

#[test]
fn a_fetch_answered_after_its_computer_changed_is_made_again() {
    let (mut engine, _) = browsing(secs(1));
    let text = hello(
        "urn:uuid:01",
        ENDPOINT,
        "http://192.168.1.20:5357/abc",
        1,
        1,
    );
    engine.on_datagram(secs(2), text.as_bytes(), HOST, &mut |_| {});
    let Some(Polled::Fetch(stale)) = poll_past_probes(&mut engine, secs(2)).0 else {
        panic!("a fetch is due");
    };
    let newer = hello(
        "urn:uuid:02",
        ENDPOINT,
        "http://192.168.1.20:5357/abc",
        2,
        2,
    );
    engine.on_datagram(secs(3), newer.as_bytes(), HOST, &mut |_| {});
    let mut changes = Vec::new();
    engine.on_fetched(
        secs(3),
        stale.id,
        &metadata_response("OLD/Workgroup:HOME"),
        &mut |c| changes.push(c),
    );
    assert!(changes.is_empty(), "what the fetch read is stale");
    let Some(Polled::Fetch(fresh)) = poll_past_probes(&mut engine, secs(3)).0 else {
        panic!("fetched again at once");
    };
    engine.on_fetched(
        secs(3),
        fresh.id,
        &metadata_response("NEW/Workgroup:HOME"),
        &mut |c| changes.push(c),
    );
    let [Change::Found { computer, .. }] = changes.as_slice() else {
        panic!("found: {changes:?}");
    };
    assert_eq!(computer.name.as_str(), "NEW");
}

/// The QNAP answering probe `asked` from `HOST` at `now` under `instance`,
/// as its answer numbered `nth`.
fn qnap_answers(
    engine: &mut WsdEngine,
    now: Duration64,
    asked: Uuid,
    instance: u32,
    nth: u8,
    changes: &mut Vec<Change>,
) {
    let text = qnap_probe_matches(Uuid::from_random([nth; 16]), asked, instance);
    engine.on_datagram(now, text.as_bytes(), HOST, &mut |c| changes.push(c));
}

#[test]
fn a_target_naming_its_host_is_fetched_where_its_answer_came_from() {
    let (mut engine, asked) = browsing(secs(1));
    let mut changes = Vec::new();
    qnap_answers(&mut engine, secs(2), asked, 7, 1, &mut changes);
    let Some(Polled::Fetch(fetch)) = poll_past_probes(&mut engine, secs(2)).0 else {
        panic!("its metadata is fetched");
    };
    assert_eq!(
        fetch,
        Fetched {
            id: fetch.id,
            addr: HOST,
            port: QNAP_PORT,
            path: String::from(QNAP_PATH),
            endpoint: String::from(QNAP_ENDPOINT),
            name: Some(String::from(QNAP_NAME)),
        },
        "at the address it answered from, by the name its URL gives"
    );
    engine.on_fetched(secs(3), fetch.id, QNAP_METADATA, &mut |c| changes.push(c));
    let [Change::Found { addr, computer, .. }] = changes.as_slice() else {
        panic!("found: {changes:?}");
    };
    assert_eq!((*addr, computer.name.as_str()), (HOST, QNAP_NAME));
}

#[test]
fn a_url_naming_the_sender_comes_first_and_one_naming_another_address_never() {
    let fetched = |xaddrs: &str| {
        let (mut engine, _) = browsing(secs(1));
        let text = hello("urn:uuid:01", ENDPOINT, xaddrs, 1, 1);
        engine.on_datagram(secs(2), text.as_bytes(), HOST, &mut |_| {});
        match poll_past_probes(&mut engine, secs(2)).0 {
            Some(Polled::Fetch(fetch)) => (fetch.addr, fetch.port, fetch.path, fetch.name),
            other => panic!("a fetch is due: {other:?}"),
        }
    };
    assert_eq!(
        fetched("http://10.9.9.9:5357/other http://NAS:8080/named http://192.168.1.20:5357/own"),
        (HOST, 5357, String::from("/own"), None)
    );
    assert_eq!(
        fetched("http://10.9.9.9:5357/other http://NAS:8080/named"),
        (
            HOST,
            8080,
            String::from("/named"),
            Some(String::from("NAS"))
        ),
        "a named URL is fetched at the sender, never at the other address"
    );
}

#[test]
fn a_target_respelling_its_hosts_case_is_not_fetched_again() {
    let (mut engine, _) = browsing(secs(1));
    let mut changes = Vec::new();
    let text = hello("urn:uuid:01", ENDPOINT, "http://NAS:8080/x", 1, 1);
    engine.on_datagram(secs(2), text.as_bytes(), HOST, &mut |c| changes.push(c));
    let Some(Polled::Fetch(fetch)) = poll_past_probes(&mut engine, secs(2)).0 else {
        panic!("its metadata is fetched");
    };
    engine.on_fetched(
        secs(2),
        fetch.id,
        &metadata_response("NAS/Workgroup:HOME"),
        &mut |c| changes.push(c),
    );
    let respelled = hello("urn:uuid:02", ENDPOINT, "http://nas:8080/x", 1, 2);
    engine.on_datagram(secs(3), respelled.as_bytes(), HOST, &mut |c| {
        changes.push(c);
    });
    assert_eq!(
        poll_past_probes(&mut engine, secs(3)).0,
        None,
        "the same URL: a host compares without case"
    );
    assert!(matches!(changes.as_slice(), [Change::Found { .. }]));
}

#[test]
fn a_target_numbering_every_answer_afresh_is_still_heard() {
    let (mut engine, first) = browsing(secs(1));
    let mut changes = Vec::new();
    qnap_answers(&mut engine, secs(2), first, 3_000_000_000, 1, &mut changes);
    let Some(Polled::Fetch(fetch)) = poll_past_probes(&mut engine, secs(2)).0 else {
        panic!("its metadata is fetched");
    };
    engine.on_fetched(secs(2), fetch.id, QNAP_METADATA, &mut |c| changes.push(c));
    // Each round's answer carries a lower instance than the last, as a
    // responder drawing a fresh one per answer soon does.
    let mut instances = [2_000_000_000, 1_000_000_000, 5].into_iter();
    let (mut out, mut rng) = ([0u8; 2048], rng());
    let (mut now, mut answered, mut nth) = (secs(2), first, 2);
    for _ in 0..256 {
        if now >= secs(300) {
            break;
        }
        now = engine
            .next_deadline(Some(Duration64::ZERO))
            .expect("a browsing engine is due")
            .max(now);
        let polled = engine.poll(
            now,
            &mut rng,
            &mut out,
            &mut |c| changes.push(c),
            &mut || true,
        );
        let Some(Output::Transmit(Emit { len, .. })) = polled else {
            continue;
        };
        if let Some(asked) = probe_id(&out[..len]).filter(|&asked| asked != answered) {
            if let Some(instance) = instances.next() {
                qnap_answers(&mut engine, now, asked, instance, nth, &mut changes);
                (answered, nth) = (asked, nth + 1);
            }
        }
    }
    assert!(now >= secs(300), "the rounds ran past two expiries");
    assert_eq!(instances.next(), None, "every round was answered");
    assert!(
        matches!(changes.as_slice(), [Change::Found { .. }]),
        "found once and never gone: {changes:?}"
    );
    assert_eq!(engine.computers().count(), 1);
}

#[test]
fn an_answer_is_current_however_numbered_and_an_announcement_held_to_its_order() {
    let (mut engine, asked) = browsing(secs(1));
    name_it(&mut engine, secs(2), "urn:uuid:01", 1, "PC/Workgroup:HOME");
    // `name_it` announced at number 2; the answer is numbered 2 as well.
    let changed = probe_matches(
        "urn:uuid:02",
        asked,
        &[target_xml(ENDPOINT, "http://192.168.1.20:5357/abc", 2)],
    );
    engine.on_datagram(secs(3), changed.as_bytes(), HOST, &mut |_| {});
    let Some(Polled::Fetch(fetch)) = poll_past_probes(&mut engine, secs(3)).0 else {
        panic!("the answer's new version is fetched");
    };
    engine.on_fetch_failed(secs(3), fetch.id);
    let stale = hello(
        "urn:uuid:03",
        ENDPOINT,
        "http://192.168.1.20:5357/abc",
        3,
        1,
    );
    engine.on_datagram(secs(4), stale.as_bytes(), HOST, &mut |_| {});
    let Some(Polled::Fetch(retried)) = poll_past_probes(&mut engine, secs(40)).0 else {
        panic!("the failed fetch is retried after its backoff");
    };
    engine.on_fetched(
        secs(40),
        retried.id,
        &metadata_response("PC/Workgroup:HOME"),
        &mut |_| {},
    );
    assert_eq!(
        poll_past_probes(&mut engine, secs(40)).0,
        None,
        "settled at the answer's version: the stale announcement's was never believed"
    );
    let fresh = hello(
        "urn:uuid:04",
        ENDPOINT,
        "http://192.168.1.20:5357/abc",
        3,
        3,
    );
    engine.on_datagram(secs(41), fresh.as_bytes(), HOST, &mut |_| {});
    assert!(
        matches!(
            poll_past_probes(&mut engine, secs(41)).0,
            Some(Polled::Fetch(_))
        ),
        "an announcement in order is believed"
    );
}
