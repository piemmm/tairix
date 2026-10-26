//! Unit tests for the catalogue: what it asks, how it merges, and how it
//! bounds what one peer may list.

use super::*;
use alloc::format;

use tairix_abi::discovery_ipc::{Answer, Change, Membership};

use crate::test_book::{wire, Book};

fn link(name: &[u8]) -> [u8; IF_NAME_LEN] {
    let mut out = [0u8; IF_NAME_LEN];
    out[..name.len()].copy_from_slice(name);
    out
}

fn v4(last: u8) -> IpAddr {
    IpAddr::V4(core::net::Ipv4Addr::new(192, 168, 1, last))
}

fn on<'a>(interface: &[u8], request: u32, answer: Answer<'a>) -> Entry<'a> {
    Entry::Answer {
        request,
        interface: link(interface),
        change: Change::Added,
        ttl: 120,
        answer,
    }
}

const PACE: u64 = 5_000_000_000;

/// Advertise `label` on `eth0`, resolved to `host` and looked up at `address`.
fn advertise(catalogue: &mut Catalogue, book: &mut Book, label: &str, host: &str, address: IpAddr) {
    catalogue.apply(&on(
        b"eth0",
        book.id("browse smb"),
        Answer::Instance {
            label: label.as_bytes(),
        },
    ));
    catalogue.reconcile(book, 0);
    let target = wire(host);
    catalogue.apply(&on(
        b"eth0",
        book.id(&format!("resolve {label}")),
        Answer::Service {
            priority: 0,
            weight: 0,
            port: 445,
            target: &target,
        },
    ));
    catalogue.reconcile(book, 0);
    if let Some(&id) = book
        .open
        .iter()
        .find(|(_, asked)| asked.as_str() == format!("host {host}"))
        .map(|(id, _)| id)
    {
        catalogue.apply(&on(b"eth0", id, Answer::Address { address }));
    }
}

fn computer<'a>(
    interface: &[u8],
    request: u32,
    key: u64,
    address: IpAddr,
    name: &'a str,
) -> Entry<'a> {
    on(
        interface,
        request,
        Answer::Computer {
            key,
            address,
            name,
            membership: Membership::Workgroup("HOME"),
        },
    )
}

#[test]
fn the_catalogue_browses_the_storage_types_and_the_computers() {
    let mut book = Book::default();
    let mut catalogue = Catalogue::new(PACE);
    catalogue.reconcile(&mut book, 0);
    assert_eq!(book.asks(), ["browse smb", "computers"]);
    assert_eq!(
        catalogue.listing(),
        Listing::default(),
        "nothing heard, nothing listed"
    );
}

#[test]
fn an_instance_and_a_computer_at_one_address_on_one_link_are_one_endpoint() {
    let mut book = Book::default();
    let mut catalogue = Catalogue::new(PACE);
    catalogue.reconcile(&mut book, 0);
    advertise(&mut catalogue, &mut book, "Study NAS", "nas.local", v4(9));
    catalogue.apply(&computer(b"eth0", book.id("computers"), 1, v4(9), "NAS"));
    catalogue.apply(&computer(b"wlan0", book.id("computers"), 2, v4(9), "NAS"));
    let listing = catalogue.listing();
    let [elsewhere, merged] = listing.endpoints.as_slice() else {
        panic!("two endpoints: {listing:?}");
    };
    assert_eq!(
        (
            elsewhere.name.as_slice(),
            elsewhere.found,
            elsewhere.port,
            elsewhere.interface
        ),
        (
            &b"NAS"[..],
            Found::Announced,
            SMB_PORT,
            Some(link(b"wlan0"))
        ),
        "the same address on another link is another endpoint"
    );
    assert_eq!(
        (merged.name.as_slice(), merged.found, merged.host.as_deref()),
        (
            &b"Study NAS"[..],
            Found::Both,
            Some(wire("nas.local").as_slice())
        )
    );
    assert_eq!(
        merged.membership,
        OwnedMembership::Workgroup(String::from("HOME"))
    );
    assert_eq!(merged.addresses, [v4(9)]);
}

#[test]
fn one_source_lists_a_bounded_few_and_the_rest_are_counted() {
    let mut book = Book::default();
    let mut catalogue = Catalogue::new(PACE);
    catalogue.reconcile(&mut book, 0);
    for n in 0..PER_SOURCE + 2 {
        advertise(
            &mut catalogue,
            &mut book,
            &format!("share{n}"),
            "flood.local",
            v4(66),
        );
    }
    catalogue.apply(&computer(b"eth0", book.id("computers"), 9, v4(10), "QUIET"));
    let listing = catalogue.listing();
    let from_flood = listing
        .endpoints
        .iter()
        .filter(|endpoint| endpoint.addresses == [v4(66)])
        .count();
    assert_eq!(from_flood, PER_SOURCE);
    assert_eq!(
        listing.omitted,
        [(link(b"eth0"), Source::Address(v4(66)), 2)]
    );
    assert!(
        listing
            .endpoints
            .iter()
            .any(|endpoint| endpoint.name == b"QUIET"),
        "a neighbour is not crowded out"
    );
}

#[test]
fn a_host_the_user_names_is_listed_until_it_is_forgotten() {
    let mut catalogue = Catalogue::new(PACE);
    assert!(catalogue.name("files.example.com"));
    assert!(
        catalogue.name("files.example.com"),
        "naming it again changes nothing"
    );
    let listing = catalogue.listing();
    let [named] = listing.endpoints.as_slice() else {
        panic!("one endpoint: {listing:?}");
    };
    assert_eq!(
        (
            named.name.as_slice(),
            named.found,
            named.interface,
            named.port
        ),
        (&b"files.example.com"[..], Found::Named, None, SMB_PORT)
    );
    catalogue.forget("files.example.com");
    assert!(catalogue.listing().endpoints.is_empty());
    for n in 0..MAX_NAMED {
        assert!(catalogue.name(&format!("host{n}")));
    }
    assert!(
        !catalogue.name("one too many"),
        "the user names a bounded few"
    );
}

#[test]
fn a_computer_that_goes_leaves_the_catalogue() {
    let mut book = Book::default();
    let mut catalogue = Catalogue::new(PACE);
    catalogue.reconcile(&mut book, 0);
    let id = book.id("computers");
    catalogue.apply(&computer(b"eth0", id, 4, v4(4), "PC"));
    assert_eq!(catalogue.listing().endpoints.len(), 1);
    catalogue.apply(&Entry::Flush {
        request: id,
        interface: link(b"eth0"),
    });
    assert!(
        catalogue.listing().endpoints.is_empty(),
        "a flushed link takes its computers"
    );
}
