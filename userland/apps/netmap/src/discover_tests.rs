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
    /// Asks refused, by the start of their description.
    refusals: Vec<(&'static str, Errno)>,
    /// The most requests it holds; a start past them is refused.
    limit: Option<usize>,
    /// Starts asked of it, refused or not.
    attempts: usize,
    starts: usize,
    /// Every call made of it, in order.
    calls: Vec<String>,
}

impl Directory for Book {
    fn start(&mut self, query: &Query<'_>) -> Result<u32, DiscoveryError> {
        self.attempts += 1;
        let what = describe(query);
        if let Some(&(_, errno)) = self
            .refusals
            .iter()
            .find(|(refused, _)| what.starts_with(refused))
        {
            return Err(DiscoveryError::Refused(errno));
        }
        if self.limit.is_some_and(|limit| self.open.len() >= limit) {
            return Err(DiscoveryError::Refused(Errno::LimitExceeded));
        }
        self.next += 1;
        self.starts += 1;
        self.calls.push(format!("start {what}"));
        self.open.insert(self.next, what);
        Ok(self.next)
    }

    fn stop(&mut self, request: u32) -> Result<(), DiscoveryError> {
        if let Some(what) = self.open.remove(&request) {
            self.calls.push(format!("stop {what}"));
        }
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
    book.calls.clear();
    discovery.reconcile(&mut book, &BTreeSet::new());
    let again = book.id("browse ipp");
    assert_ne!(again, browse, "the browse is started afresh");
    assert!(!book.open.contains_key(&browse));
    assert_eq!(
        book.calls,
        ["start browse ipp", "stop browse ipp"],
        "only the lost request is redone, and the old one stops last, so the \
         question they share is never left unasked"
    );
}

#[test]
fn a_lost_request_at_the_accounts_bound_makes_room_for_its_replacement() {
    let mut book = Book::default();
    let mut discovery = printer(&mut book);
    let browse = book.id("browse ipp");
    discovery.apply(&Entry::Lost { request: browse });
    book.limit = Some(book.open.len());
    discovery.reconcile(&mut book, &BTreeSet::new());
    assert_ne!(book.id("browse ipp"), browse, "replaced");
    let stopped = book.calls.iter().position(|call| call == "stop browse ipp");
    let started = book
        .calls
        .iter()
        .rposition(|call| call == "start browse ipp");
    assert!(stopped < started, "the old request made the room first");
    assert_eq!(discovery.stalled(), None);
}

#[test]
fn a_request_lost_again_as_soon_as_it_is_replaced_waits_a_pass() {
    let mut book = Book::default();
    let mut discovery = printer(&mut book);
    let browse = book.id("browse ipp");
    discovery.apply(&Entry::Lost { request: browse });
    discovery.reconcile(&mut book, &BTreeSet::new());
    let replaced = book.id("browse ipp");
    assert_ne!(replaced, browse);
    assert!(!discovery.deferred());
    discovery.apply(&Entry::Lost { request: replaced });
    discovery.reconcile(&mut book, &BTreeSet::new());
    assert_eq!(book.id("browse ipp"), replaced, "held back a pass");
    assert!(discovery.deferred(), "and the pass says so");
    discovery.reconcile(&mut book, &BTreeSet::new());
    assert_ne!(
        book.id("browse ipp"),
        replaced,
        "asked again the pass after"
    );
    assert!(!discovery.deferred());
}

#[test]
fn a_refused_type_list_is_told_apart_from_a_refused_type() {
    let mut book = Book::default();
    book.refusals.push(("types", Errno::PermissionDenied));
    let mut discovery = Discovery::new();
    discovery.reconcile(&mut book, &BTreeSet::new());
    assert!(discovery.locked_out());
}

#[test]
fn a_host_outside_local_is_not_looked_up_on_the_link() {
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
    let label = "Office Printer".as_bytes();
    discovery.apply(&entry(
        book.id("browse ipp"),
        Change::Added,
        Answer::Instance { label },
    ));
    discovery.reconcile(&mut book, &none);
    let target = wire("printer.example.com");
    discovery.apply(&entry(
        book.id("resolve Office Printer"),
        Change::Added,
        Answer::Service {
            priority: 0,
            weight: 0,
            port: 631,
            target: &target,
        },
    ));
    discovery.reconcile(&mut book, &none);
    assert_eq!(
        book.asks(),
        ["types", "browse ipp", "resolve Office Printer"],
        "a unicast name is no question for the link"
    );
    assert_eq!(discovery.services()[0].host, "printer.example.com");
    assert_eq!(discovery.refused(), None);
    assert_eq!(discovery.stalled(), None);
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

    // Once the segment fits the bound again, the view is whole again.
    discovery.apply(&entry(
        types,
        Change::Retired,
        Answer::Type {
            service: ServiceTypeField {
                name: names[0].as_bytes(),
                transport: Transport::Tcp,
            },
        },
    ));
    discovery.reconcile(&mut book, &BTreeSet::new());
    assert_eq!(book.open.len(), 1 + MAX_TYPES);
    assert!(!discovery.truncated());
}

fn link_local(last: u16) -> IpAddr {
    IpAddr::V6(core::net::Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, last))
}

#[test]
fn a_service_at_its_bounds_is_asked_again_on_the_next_pass() {
    let mut book = Book {
        limit: Some(2),
        ..Book::default()
    };
    let mut discovery = Discovery::new();
    let named = BTreeSet::from([link_local(1), link_local(2), link_local(3)]);
    discovery.reconcile(&mut book, &named);
    assert_eq!(book.open.len(), 2);
    assert_eq!(
        discovery.stalled(),
        Some(&DiscoveryError::Refused(Errno::LimitExceeded))
    );
    assert_eq!(book.attempts, 3, "the pass ends at the first refusal");
    assert_eq!(discovery.refused(), None, "nothing is given up");
    book.limit = None;
    discovery.reconcile(&mut book, &named);
    assert_eq!(book.open.len(), 4, "the rest are asked once there is room");
    assert_eq!(discovery.stalled(), None);
}

#[test]
fn an_ask_this_program_may_not_make_is_not_made_again_and_the_rest_are() {
    let mut book = Book {
        refusals: alloc::vec![("types", Errno::PermissionDenied)],
        ..Book::default()
    };
    let mut discovery = Discovery::new();
    let named = BTreeSet::from([link_local(1)]);
    discovery.reconcile(&mut book, &named);
    assert_eq!(book.asks(), ["reverse fe80::1"]);
    assert_eq!(
        discovery.refused(),
        Some(&DiscoveryError::Refused(Errno::PermissionDenied))
    );
    book.refusals.clear();
    discovery.reconcile(&mut book, &named);
    assert_eq!(
        book.asks(),
        ["reverse fe80::1"],
        "a refusal for authority is not asked, nor audited, again"
    );
}

#[test]
fn an_ask_the_service_will_not_answer_is_skipped_and_the_rest_are_made() {
    let mut book = Book {
        refusals: alloc::vec![("reverse fe80::1", Errno::OutOfRange)],
        ..Book::default()
    };
    let mut discovery = Discovery::new();
    let named = BTreeSet::from([link_local(1), link_local(2)]);
    discovery.reconcile(&mut book, &named);
    assert_eq!(book.asks(), ["types", "reverse fe80::2"]);
    assert_eq!(discovery.refused(), None);
    assert_eq!(discovery.stalled(), None);
    let attempts = book.attempts;
    discovery.reconcile(&mut book, &named);
    assert_eq!(book.attempts, attempts, "the refused ask is not made again");
}

#[test]
fn a_map_of_a_busy_segment_keeps_hundreds_of_requests_in_one_session() {
    let mut book = Book::default();
    let mut discovery = Discovery::new();
    let named: BTreeSet<IpAddr> = (1..=MAX_REVERSES)
        .map(|last| link_local(u16::try_from(last).expect("small")))
        .collect();
    discovery.reconcile(&mut book, &named);
    assert_eq!(book.open.len(), 1 + MAX_REVERSES);
    assert_eq!(discovery.stalled(), None);
    assert!(!discovery.truncated());
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
