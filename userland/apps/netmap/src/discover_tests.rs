use super::*;
use alloc::string::ToString;

use tairix_abi::discovery_ipc::{Answer, Change};
use tairix_abi::Errno;

/// A directory that hands out fresh ids and records what each open request
/// asks, in the words the tests match on.
#[derive(Default)]
struct Book {
    next: u32,
    open: BTreeMap<u32, String>,
    refuse: bool,
    starts: usize,
}

impl Directory for Book {
    fn start(&mut self, query: &Query<'_>) -> Result<u32, DiscoveryError> {
        if self.refuse {
            return Err(DiscoveryError::Refused(Errno::PermissionDenied));
        }
        self.next += 1;
        self.starts += 1;
        self.open.insert(self.next, describe(query));
        Ok(self.next)
    }

    fn stop(&mut self, request: u32) -> Result<(), DiscoveryError> {
        self.open.remove(&request);
        Ok(())
    }
}

impl Book {
    fn id(&self, what: &str) -> u32 {
        *self
            .open
            .iter()
            .find(|(_, asked)| asked.as_str() == what)
            .unwrap_or_else(|| panic!("{what} is not open: {:?}", self.open))
            .0
    }

    fn asks(&self) -> Vec<&str> {
        self.open.values().map(String::as_str).collect()
    }
}

fn describe(query: &Query<'_>) -> String {
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).to_string();
    match query {
        Query::Types => String::from("types"),
        Query::Browse { service } => format!("browse {}", text(service.name)),
        Query::Resolve { instance, .. } => format!("resolve {}", text(instance)),
        Query::Host { name, .. } => format!("host {}", WireName(name)),
        Query::Reverse { address } => format!("reverse {address}"),
    }
}

fn eth0() -> InterfaceName {
    let mut name = [0u8; tairix_abi::net_ipc::IF_NAME_LEN];
    name[..4].copy_from_slice(b"eth0");
    name
}

fn entry(request: u32, change: Change, answer: Answer<'_>) -> Entry<'_> {
    Entry::Answer {
        request,
        interface: eth0(),
        change,
        ttl: 120,
        answer,
    }
}

fn wire(name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for label in name.split('.') {
        out.push(u8::try_from(label.len()).expect("short"));
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out
}

fn ipp() -> ServiceTypeField<'static> {
    ServiceTypeField {
        name: b"ipp",
        transport: Transport::Tcp,
    }
}

/// Drive a discovery to one resolved, looked-up printer.
fn printer(book: &mut Book) -> Discovery {
    let mut discovery = Discovery::new();
    let none = BTreeSet::new();
    discovery.reconcile(book, &none);
    discovery.apply(&entry(
        book.id("types"),
        Change::Added,
        Answer::Type { service: ipp() },
    ));
    discovery.reconcile(book, &none);
    let label = "Office Printer".as_bytes();
    discovery.apply(&entry(
        book.id("browse ipp"),
        Change::Added,
        Answer::Instance { label },
    ));
    discovery.reconcile(book, &none);
    let resolve = book.id("resolve Office Printer");
    let target = wire("printer.local");
    discovery.apply(&entry(
        resolve,
        Change::Added,
        Answer::Service {
            priority: 0,
            weight: 0,
            port: 631,
            target: &target,
        },
    ));
    let txt = b"\x09txtvers=1\x0crp=ipp/print";
    discovery.apply(&entry(resolve, Change::Added, Answer::Text { octets: txt }));
    discovery.reconcile(book, &none);
    discovery.apply(&entry(
        book.id("host printer.local"),
        Change::Added,
        Answer::Address {
            address: IpAddr::V4(core::net::Ipv4Addr::new(192, 168, 1, 20)),
        },
    ));
    discovery.reconcile(book, &none);
    discovery
}

#[test]
fn the_walk_follows_types_to_instances_to_their_hosts() {
    let mut book = Book::default();
    let discovery = printer(&mut book);
    assert_eq!(
        book.asks(),
        [
            "types",
            "browse ipp",
            "resolve Office Printer",
            "host printer.local"
        ]
    );
    let services = discovery.services();
    assert_eq!(services.len(), 1);
    let service = &services[0];
    assert_eq!(service.instance, "Office Printer");
    assert_eq!(service.service, "_ipp._tcp");
    assert_eq!(service.port, 631);
    assert_eq!(service.host, "printer.local");
    assert_eq!(service.interface, eth0());
    assert_eq!(
        service.addresses,
        [IpAddr::V4(core::net::Ipv4Addr::new(192, 168, 1, 20))]
    );
    assert_eq!(service.text, ["txtvers=1", "rp=ipp/print"]);
    assert_eq!(
        discovery
            .names()
            .get(&service.addresses[0])
            .map(String::as_str),
        Some("printer.local"),
        "the host lookup names the address it found"
    );
}

#[test]
fn a_retired_instance_takes_its_resolve_and_host_with_it() {
    let mut book = Book::default();
    let mut discovery = printer(&mut book);
    let label = "Office Printer".as_bytes();
    discovery.apply(&entry(
        book.id("browse ipp"),
        Change::Retired,
        Answer::Instance { label },
    ));
    discovery.reconcile(&mut book, &BTreeSet::new());
    assert_eq!(book.asks(), ["types", "browse ipp"]);
    assert!(discovery.services().is_empty());
    assert!(discovery.names().is_empty());
}

#[test]
fn a_flushed_type_list_stops_every_browse_below_it() {
    let mut book = Book::default();
    let mut discovery = printer(&mut book);
    discovery.apply(&Entry::Flush {
        request: book.id("types"),
        interface: eth0(),
    });
    discovery.reconcile(&mut book, &BTreeSet::new());
    assert_eq!(book.asks(), ["types"], "the walk unwinds from the top");
    assert!(discovery.services().is_empty());
}

#[test]
fn a_request_whose_updates_were_lost_is_asked_again() {
    let mut book = Book::default();
    let mut discovery = printer(&mut book);
    let browse = book.id("browse ipp");
    discovery.apply(&Entry::Lost { request: browse });
    discovery.reconcile(&mut book, &BTreeSet::new());
    let again = book.id("browse ipp");
    assert_ne!(again, browse, "the browse is started afresh");
    assert!(!book.open.contains_key(&browse));
    assert!(
        book.asks().contains(&"types"),
        "only the lost request is redone"
    );
}

#[test]
fn only_link_local_addresses_are_asked_of_the_segment() {
    let mut book = Book::default();
    let mut discovery = Discovery::new();
    let link = IpAddr::V6(core::net::Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0x22));
    let routable = IpAddr::V4(core::net::Ipv4Addr::new(192, 0, 2, 9));
    discovery.reconcile(&mut book, &BTreeSet::from([link, routable]));
    assert_eq!(book.asks(), ["types", "reverse fe80::22"]);
    let name = wire("nas.local");
    discovery.apply(&entry(
        book.id("reverse fe80::22"),
        Change::Added,
        Answer::Pointer { target: &name },
    ));
    assert_eq!(
        discovery.names().get(&link).map(String::as_str),
        Some("nas.local")
    );
    discovery.reconcile(&mut book, &BTreeSet::new());
    assert_eq!(
        book.asks(),
        ["types"],
        "an address no longer shown is not asked about"
    );
    assert!(discovery.names().is_empty());
}

#[test]
fn every_kind_of_request_is_bounded() {
    let mut book = Book::default();
    let mut discovery = Discovery::new();
    discovery.reconcile(&mut book, &BTreeSet::new());
    let types = book.id("types");
    let names: Vec<String> = (0..=MAX_TYPES).map(|n| format!("t{n}")).collect();
    for name in &names {
        discovery.apply(&entry(
            types,
            Change::Added,
            Answer::Type {
                service: ServiceTypeField {
                    name: name.as_bytes(),
                    transport: Transport::Tcp,
                },
            },
        ));
    }
    discovery.reconcile(&mut book, &BTreeSet::new());
    assert_eq!(book.open.len(), 1 + MAX_TYPES);
    assert!(discovery.truncated());
}

#[test]
fn a_refusal_stops_further_requests_and_is_kept() {
    let mut book = Book {
        refuse: true,
        ..Book::default()
    };
    let mut discovery = Discovery::new();
    discovery.reconcile(&mut book, &BTreeSet::new());
    assert_eq!(
        discovery.refused(),
        Some(&DiscoveryError::Refused(Errno::PermissionDenied))
    );
    book.refuse = false;
    discovery.reconcile(&mut book, &BTreeSet::new());
    assert_eq!(book.starts, 0, "a refused service is not asked again");
}

#[test]
fn a_peer_authored_label_cannot_hide_anything() {
    let mut book = Book::default();
    let mut discovery = Discovery::new();
    let none = BTreeSet::new();
    discovery.reconcile(&mut book, &none);
    discovery.apply(&entry(
        book.id("types"),
        Change::Added,
        Answer::Type { service: ipp() },
    ));
    discovery.reconcile(&mut book, &none);
    let label = "Print\u{200b}er".as_bytes();
    discovery.apply(&entry(
        book.id("browse ipp"),
        Change::Added,
        Answer::Instance { label },
    ));
    discovery.reconcile(&mut book, &none);
    let target = wire("x.local");
    discovery.apply(&entry(
        book.id("resolve Print\u{200b}er"),
        Change::Added,
        Answer::Service {
            priority: 0,
            weight: 0,
            port: 1,
            target: &target,
        },
    ));
    assert_eq!(discovery.services()[0].instance, "Print\\226\\128\\139er");
}

#[test]
fn the_preferred_target_is_the_lowest_priority_then_the_heaviest() {
    let mut book = Book::default();
    let mut discovery = printer(&mut book);
    let resolve = book.id("resolve Office Printer");
    let primary = wire("primary.local");
    let backup = wire("backup.local");
    let target = |priority, weight, target| Answer::Service {
        priority,
        weight,
        port: 631,
        target,
    };
    for (priority, weight, name) in [(5, 90, &backup), (0, 30, &primary), (0, 10, &backup)] {
        discovery.apply(&entry(
            resolve,
            Change::Added,
            target(priority, weight, name),
        ));
    }
    assert_eq!(discovery.services()[0].host, "primary.local");
    discovery.apply(&entry(resolve, Change::Retired, target(0, 30, &primary)));
    assert_eq!(
        discovery.services()[0].host,
        "backup.local",
        "at equal priority the heavier weight wins over the printer's own zero"
    );
}
