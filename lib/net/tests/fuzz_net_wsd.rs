//! Deterministic fuzz harness for WS-Discovery and the HTTP exchange its
//! metadata rides.
//!
//! A WS-Discovery datagram is unsolicited and unauthenticated, and a metadata
//! response is whatever a peer chose to send back, so the invariants are the
//! ones that keep a hostile segment from crashing, wedging or steering this
//! host:
//!
//! 1. The envelope decoder, the metadata decoder and the HTTP response reader
//!    never panic, on any bytes or on valid messages mutated, and a body the
//!    reader returns is within its bound.
//! 2. Driving the engine with arbitrary datagrams from arbitrary senders,
//!    fetch answers and times never panics, keeps at most two fetches in
//!    flight, and answers `next_deadline` exactly while it browses.
//! 3. The engine fetches only from an address that sent it a datagram, names
//!    in a fetch's `Host` field only a name spelled in a datagram that address
//!    sent, and every datagram it builds reads back as a query.
//! 4. A computer is reported gone only after it was found, and only once.
//!
//! Runs the fixed smoke sweep under plain `cargo test`; keeps drawing from
//! the same seeded stream until `TAIRIX_FUZZ_BUDGET_SECS` elapses under
//! `cargo xtask fuzz`.

use std::collections::{BTreeMap, BTreeSet};

use tairix_abi::time::Duration64;
use tairix_fuzzseed::Prng;
use tairix_hash::HashSeed;
use tairix_net::http::read_response;
use tairix_net::wsd::{decode, decode_metadata, Change, Message, Output, WsdEngine, MAX_METADATA};
use tairix_net::{IpAddr, Ipv4Addr};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 400;

const SEED: HashSeed = HashSeed::from_words(3, 5);

/// The host names a URL is drawn with, before any mutation.
const NAMES: [&str; 2] = ["OFFICE", "nas.example"];

fn sender(rng: &mut Prng) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(
        10,
        0,
        0,
        u8::try_from(rng.below(6)).unwrap_or(0),
    ))
}

/// A valid message of a kind drawn from `rng`, from a small pool of
/// endpoints, ids and addresses so they collide as a real segment's do.
fn message(rng: &mut Prng, relates_to: &str) -> String {
    let endpoint = format!("urn:uuid:{}", rng.below(8));
    let id = format!("urn:uuid:m{}", rng.below(64));
    let sequence = format!(
        r#"<wsd:AppSequence InstanceId="{}" MessageNumber="{}"/>"#,
        rng.below(3),
        rng.below(5)
    );
    let host = match rng.below(3) {
        0 => String::from(*rng.pick(&NAMES)),
        _ => format!("10.0.0.{}", rng.below(6)),
    };
    let xaddrs = format!("http://{host}:5357/p{}", rng.below(3));
    let target = format!(
        "<wsa:EndpointReference><wsa:Address>{endpoint}</wsa:Address></wsa:EndpointReference>\
         <wsd:Types>wsdp:Device pub:Computer</wsd:Types><wsd:XAddrs>{xaddrs}</wsd:XAddrs>\
         <wsd:MetadataVersion>{}</wsd:MetadataVersion>",
        rng.below(3)
    );
    let (action, extra, body) = match rng.below(4) {
        0 => ("Hello", String::new(), format!("<wsd:Hello>{target}</wsd:Hello>")),
        1 => (
            "ProbeMatches",
            format!("<wsa:RelatesTo>{relates_to}</wsa:RelatesTo>"),
            format!("<wsd:ProbeMatches><wsd:ProbeMatch>{target}</wsd:ProbeMatch></wsd:ProbeMatches>"),
        ),
        2 => (
            "ResolveMatches",
            format!("<wsa:RelatesTo>{relates_to}</wsa:RelatesTo>"),
            format!("<wsd:ResolveMatches><wsd:ResolveMatch>{target}</wsd:ResolveMatch></wsd:ResolveMatches>"),
        ),
        _ => (
            "Bye",
            String::new(),
            format!(
                "<wsd:Bye><wsa:EndpointReference><wsa:Address>{endpoint}</wsa:Address>\
                 </wsa:EndpointReference></wsd:Bye>"
            ),
        ),
    };
    format!(
        r#"<soap:Envelope xmlns:soap="http://www.w3.org/2003/05/soap-envelope" xmlns:wsa="http://schemas.xmlsoap.org/ws/2004/08/addressing" xmlns:wsd="http://schemas.xmlsoap.org/ws/2005/04/discovery" xmlns:wsdp="http://schemas.xmlsoap.org/ws/2006/02/devprof" xmlns:pub="http://schemas.microsoft.com/windows/pub/2005/07"><soap:Header><wsa:Action>http://schemas.xmlsoap.org/ws/2005/04/discovery/{action}</wsa:Action><wsa:MessageID>{id}</wsa:MessageID>{extra}{sequence}</soap:Header><soap:Body>{body}</soap:Body></soap:Envelope>"#
    )
}

/// A metadata response naming a computer drawn from `rng`.
fn metadata(rng: &mut Prng) -> Vec<u8> {
    let body = format!(
        r#"<soap:Envelope xmlns:soap="http://www.w3.org/2003/05/soap-envelope" xmlns:wsx="http://schemas.xmlsoap.org/ws/2004/09/mex" xmlns:wsdp="http://schemas.xmlsoap.org/ws/2006/02/devprof" xmlns:pub="http://schemas.microsoft.com/windows/pub/2005/07"><soap:Body><wsx:Metadata><wsx:MetadataSection Dialect="http://schemas.xmlsoap.org/ws/2006/02/devprof/Relationship"><wsdp:Relationship Type="http://schemas.xmlsoap.org/ws/2006/02/devprof/host"><wsdp:Host><wsdp:Types>pub:Computer</wsdp:Types><pub:Computer>PC{}/Workgroup:WG</pub:Computer></wsdp:Host></wsdp:Relationship></wsx:MetadataSection></wsx:Metadata></soap:Body></soap:Envelope>"#,
        rng.below(4)
    );
    let mut response =
        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
    response.extend_from_slice(body.as_bytes());
    response
}

/// `bytes` with a few of them flipped, forced to markup, or cut short.
fn mutate(rng: &mut Prng, mut bytes: Vec<u8>) -> Vec<u8> {
    for _ in 0..rng.below(4) {
        if bytes.is_empty() {
            break;
        }
        let at = rng.below(bytes.len());
        bytes[at] = *rng.pick(&[
            b'<', b'>', b'/', b':', b'"', b'&', b'\r', b'\n', 0, 0xff, b'0',
        ]);
    }
    if rng.below(4) == 0 {
        let len = rng.below(bytes.len() + 1);
        bytes.truncate(len);
    }
    bytes
}

fn exercise_parsers(rng: &mut Prng) {
    let mut noise = vec![0u8; rng.below(512)];
    rng.fill(&mut noise);
    let valid = message(rng, "urn:uuid:00000000-0000-4000-8000-000000000000").into_bytes();
    for bytes in [noise.clone(), mutate(rng, valid)] {
        let _ = decode(&bytes, SEED);
    }
    let response = metadata(rng);
    let response = mutate(rng, response);
    let _ = decode_metadata(&response);
    for bytes in [&response, &noise] {
        if let Ok(body) = read_response(bytes, MAX_METADATA) {
            assert!(body.len() <= MAX_METADATA);
        }
    }
}

/// What the engine has said it is doing, held against what it does.
#[derive(Default)]
struct Ledger {
    /// Every datagram each sender has sent.
    sent: BTreeMap<IpAddr, Vec<Vec<u8>>>,
    in_flight: Vec<u32>,
    found: BTreeSet<u64>,
    probe_ids: Vec<String>,
    reports: usize,
}

impl Ledger {
    fn change(&mut self, change: &Change) {
        match *change {
            Change::Found { key, .. } => {
                assert!(self.found.insert(key), "found twice without going");
                self.reports += 1;
            }
            Change::Gone { key, .. } => {
                assert!(self.found.remove(&key), "gone before found, or twice");
            }
        }
    }
}

/// Whether `name` is spelled, byte for byte, in one of `datagrams`.
fn spelled_in(datagrams: &[Vec<u8>], name: &str) -> bool {
    datagrams.iter().any(|bytes| {
        bytes
            .windows(name.len())
            .any(|window| window == name.as_bytes())
    })
}

/// Drive one engine through a sweep, returning how many computers it found,
/// so the harness can show it reaches the end of the naming path.
fn exercise_engine(rng: &mut Prng) -> usize {
    let mut engine = WsdEngine::new(SEED);
    let mut ledger = Ledger::default();
    let mut now = 1_000_000_000u64;
    let mut draw = {
        let mut state = rng.next_u32() | 1;
        move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        }
    };
    let mut out = [0u8; 4096];
    engine.browse(Duration64::from_nanos(now), &mut draw);
    for _ in 0..64 {
        now += u64::try_from(rng.below(40_000)).unwrap_or(0) * 1_000_000;
        let at = Duration64::from_nanos(now);
        match rng.below(5) {
            0 | 1 => {
                let from = sender(rng);
                let relates = ledger
                    .probe_ids
                    .last()
                    .cloned()
                    .unwrap_or_else(|| String::from("urn:uuid:x"));
                let mut bytes = message(rng, &relates).into_bytes();
                if rng.below(3) == 0 {
                    bytes = mutate(rng, bytes);
                }
                engine.on_datagram(at, &bytes, from, &mut |c| ledger.change(&c));
                ledger.sent.entry(from).or_default().push(bytes);
            }
            2 => {
                if let Some(id) = ledger.in_flight.pop() {
                    let response = metadata(rng);
                    let response = if rng.below(2) == 0 {
                        response
                    } else {
                        mutate(rng, response)
                    };
                    engine.on_fetched(at, id, &response, &mut |c| ledger.change(&c));
                }
            }
            3 => {
                if let Some(id) = ledger.in_flight.pop() {
                    engine.on_fetch_failed(at, id);
                }
            }
            _ => {
                let mut changes = Vec::new();
                let budget = rng.below(4) != 0;
                match engine.poll(
                    at,
                    &mut draw,
                    &mut out,
                    &mut |c| changes.push(c),
                    &mut || budget,
                ) {
                    Some(Output::Transmit(emit)) => {
                        let written = decode(&out[..emit.len], SEED).map(|e| e.message);
                        assert_eq!(
                            written,
                            Ok(Message::Query),
                            "what the engine builds reads back"
                        );
                        let text = std::str::from_utf8(&out[..emit.len]).unwrap_or("");
                        if let Some(start) = text.find("<wsa:MessageID>") {
                            let start = start + "<wsa:MessageID>".len();
                            let end = text[start..].find('<').map_or(start, |end| start + end);
                            ledger.probe_ids.push(text[start..end].to_owned());
                        }
                    }
                    Some(Output::Fetch(fetch)) => {
                        let sent = ledger.sent.get(&fetch.addr);
                        assert!(sent.is_some(), "a fetch steered elsewhere");
                        assert!(
                            fetch.name.is_none_or(
                                |name| sent.is_some_and(|sent| spelled_in(sent, name.as_str()))
                            ),
                            "a name its sender never spelled: {:?}",
                            fetch.name
                        );
                        ledger.in_flight.push(fetch.id);
                        assert!(ledger.in_flight.len() <= 2, "more than two fetches fly");
                    }
                    None => {}
                }
                for change in &changes {
                    ledger.change(change);
                }
            }
        }
        assert!(
            engine.next_deadline(Some(Duration64::ZERO)).is_some(),
            "a browsing engine has a deadline"
        );
    }
    engine.stop();
    assert_eq!(engine.next_deadline(Some(Duration64::ZERO)), None);
    ledger.reports
}

#[test]
fn hostile_discovery_never_panics_or_steers_the_engine() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_discovery_never_panics_or_steers_the_engine",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut found = 0;
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            exercise_parsers(&mut rng);
            found += exercise_engine(&mut rng);
        }
        assert!(found > 0, "the sweeps never reached a found computer");
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}
