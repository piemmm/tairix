//! Unit tests for what the front remembers of a share's nodes for a second.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{NodeId, NodeKind};
use tairix_abi::fsserve::{Answer, Request};
use tairix_abi::netattach::OfferId;
use tairix_abi::netshare_ipc::NetshareRequest;
use tairix_smb::peer::DISK_SHARE;

use super::FRESH_NS;
use crate::front::served::tests::{create, listed, lookup, mounted, waiting, write};
use crate::front::Caller;
use crate::nodes::ROOT;
use crate::testing::{connect, session, Bench, BOB, PASSWORD};

fn size(bench: &mut Bench, offer: OfferId, file: NodeId) -> u64 {
    crate::front::served::tests::ask(bench, offer, &Request::NodeInfo { node: file }, |result| {
        match result {
            Ok(Answer::Info(info)) => info.size,
            other => panic!("not info: {other:?}"),
        }
    })
}

/// Whether `request` is answered without the server, its answers held back.
fn answered_kept(bench: &mut Bench, offer: OfferId, request: &Request<'_>) -> bool {
    bench.world().hold(true);
    let ticket = bench.serve(offer, request);
    let kept = !waiting(bench, offer, ticket);
    bench.world().hold(false);
    bench.run();
    kept
}

#[test]
fn what_the_server_said_of_a_node_stands_for_a_second() {
    let (mut bench, offer) = mounted(false);
    let file = create(&mut bench, offer, b"f", NodeKind::RegularFile);
    assert!(answered_kept(
        &mut bench,
        offer,
        &Request::NodeInfo { node: file }
    ));
    let looked = Request::Lookup {
        dir: ROOT,
        name: b"f",
    };
    assert!(answered_kept(&mut bench, offer, &looked));
    bench.pass(FRESH_NS);
    assert!(!answered_kept(
        &mut bench,
        offer,
        &Request::NodeInfo { node: file }
    ));
    assert!(
        answered_kept(&mut bench, offer, &looked),
        "the answer just read stands"
    );
}

#[test]
fn a_listing_read_whole_answers_the_next_from_its_start_for_a_second() {
    let (mut bench, offer) = mounted(false);
    for name in [b"a", b"b"] {
        create(&mut bench, offer, name, NodeKind::RegularFile);
    }
    let first = listed(&mut bench, offer, ROOT);
    let again = Request::ReadDir {
        dir: ROOT,
        cursor: 0,
        after: b"",
    };
    assert!(answered_kept(&mut bench, offer, &again));
    assert_eq!(listed(&mut bench, offer, ROOT), first);
    bench.pass(FRESH_NS);
    assert!(!answered_kept(&mut bench, offer, &again));
}

#[test]
fn the_front_s_own_changes_forget_what_they_make_stale() {
    let (mut bench, offer) = mounted(false);
    let file = create(&mut bench, offer, b"f", NodeKind::RegularFile);
    let before: Vec<String> = listed(&mut bench, offer, ROOT)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(before, ["f"]);
    create(&mut bench, offer, b"g", NodeKind::RegularFile);
    let mut after: Vec<String> = listed(&mut bench, offer, ROOT)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    after.sort();
    assert_eq!(
        after,
        ["f", "g"],
        "a create forgets its directory's listing"
    );
    assert_eq!(size(&mut bench, offer, file), 0);
    write(&mut bench, offer, file, 0, b"grown");
    assert_eq!(
        size(&mut bench, offer, file),
        5,
        "a write forgets the file's size"
    );
    let removed = Request::Remove {
        dir: ROOT,
        name: b"g",
    };
    crate::front::served::tests::answered(&mut bench, offer, &removed).expect("removed");
    let names: Vec<String> = listed(&mut bench, offer, ROOT)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(names, ["f"]);
    assert_eq!(
        lookup(&mut bench, offer, ROOT, b"g"),
        Err(tairix_abi::DriverError::NotFound)
    );
}

#[test]
fn another_writer_s_change_is_seen_within_the_second() {
    let (mut bench, offer) = mounted(false);
    let file = create(&mut bench, offer, b"shared", NodeKind::RegularFile);
    assert_eq!(size(&mut bench, offer, file), 0);
    let bob = Caller {
        uid: BOB,
        session: Some(session(2)),
    };
    let ticket = bench.call(
        bob,
        &NetshareRequest::Mount {
            connect: connect(PASSWORD),
            share: DISK_SHARE,
            read_only: false,
        },
    );
    let (_, theirs) = bench.mounted(ticket);
    let seen = lookup(&mut bench, theirs, ROOT, b"shared").expect("on the share");
    write(&mut bench, theirs, seen, 0, b"bob's");
    assert_eq!(size(&mut bench, offer, file), 0, "within the second");
    bench.pass(FRESH_NS);
    assert_eq!(size(&mut bench, offer, file), 5, "past it");
}

#[test]
fn under_a_directory_lease_a_listing_and_its_entries_stand_until_the_lease_breaks() {
    use crate::front::served::tests::mounted_in;
    use crate::testing::World;
    use tairix_smb::negotiate::Dialect;
    use tairix_smb::peer::PeerConfig;

    let world = World::serving(PeerConfig {
        directory_leasing: true,
        ..PeerConfig::new(Dialect::Smb311)
    });
    let (mut bench, offer) = mounted_in(world, false);
    let file = create(&mut bench, offer, b"a", NodeKind::RegularFile);
    let first: Vec<String> = listed(&mut bench, offer, ROOT)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(first, ["a"]);
    bench.pass(3 * FRESH_NS);
    let again = Request::ReadDir {
        dir: ROOT,
        cursor: 0,
        after: b"",
    };
    assert!(
        answered_kept(&mut bench, offer, &again),
        "the lease keeps the listing"
    );
    assert!(
        answered_kept(&mut bench, offer, &Request::NodeInfo { node: file }),
        "and what it said of its entries"
    );

    let bob = Caller {
        uid: BOB,
        session: Some(session(2)),
    };
    let ticket = bench.call(
        bob,
        &NetshareRequest::Mount {
            connect: connect(PASSWORD),
            share: DISK_SHARE,
            read_only: false,
        },
    );
    let (_, theirs) = bench.mounted(ticket);
    create(&mut bench, theirs, b"b", NodeKind::RegularFile);
    assert!(
        !answered_kept(&mut bench, offer, &again),
        "the break ends it"
    );
    let mut names: Vec<String> = listed(&mut bench, offer, ROOT)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    names.sort();
    assert_eq!(names, ["a", "b"]);
}

/// A mount on a server that leases directories.
fn leasing() -> (Bench, OfferId) {
    use crate::front::served::tests::mounted_in;
    use crate::testing::World;
    use tairix_smb::negotiate::Dialect;
    use tairix_smb::peer::PeerConfig;

    mounted_in(
        World::serving(PeerConfig {
            directory_leasing: true,
            ..PeerConfig::new(Dialect::Smb311)
        }),
        false,
    )
}

/// The request that lists `dir` from its start.
const fn listing(dir: NodeId) -> Request<'static> {
    Request::ReadDir {
        dir,
        cursor: 0,
        after: b"",
    }
}

#[test]
fn a_directory_lease_is_let_go_once_it_goes_unused_long_enough() {
    use crate::front::served::DIR_LEASE_IDLE_NS;

    let (mut bench, offer) = leasing();
    create(&mut bench, offer, b"a", NodeKind::RegularFile);
    listed(&mut bench, offer, ROOT);
    let opens = bench.world().server_opens();
    bench.pass(DIR_LEASE_IDLE_NS - FRESH_NS);
    assert!(
        answered_kept(&mut bench, offer, &listing(ROOT)),
        "used within its time"
    );
    bench.pass(DIR_LEASE_IDLE_NS - FRESH_NS);
    assert!(
        answered_kept(&mut bench, offer, &listing(ROOT)),
        "which each use starts again"
    );
    bench.pass(DIR_LEASE_IDLE_NS - 1);
    assert_eq!(bench.world().server_opens(), opens, "kept to the last");
    bench.pass(1);
    assert_eq!(
        bench.world().server_opens(),
        opens - 1,
        "its open let go on the server the moment its time is up"
    );
    assert_eq!(
        bench.front.next_deadline(),
        None,
        "and nothing left to wake for"
    );
    assert!(!answered_kept(&mut bench, offer, &listing(ROOT)));
}

#[test]
fn past_the_most_directories_leased_the_least_recently_used_lease_goes() {
    use crate::front::served::DIR_LEASES_MAX;

    let (mut bench, offer) = leasing();
    let dirs: Vec<NodeId> = (0..=DIR_LEASES_MAX)
        .map(|at| {
            let name = alloc::format!("d{at}");
            create(&mut bench, offer, name.as_bytes(), NodeKind::Directory)
        })
        .collect();
    for &dir in &dirs[..DIR_LEASES_MAX] {
        bench.pass(1);
        listed(&mut bench, offer, dir);
    }
    bench.pass(FRESH_NS);
    assert!(
        answered_kept(&mut bench, offer, &listing(dirs[0])),
        "the first leased, used again"
    );
    listed(&mut bench, offer, dirs[DIR_LEASES_MAX]);
    bench.pass(FRESH_NS);
    assert!(
        answered_kept(&mut bench, offer, &listing(dirs[0])),
        "kept, being the most recently used"
    );
    for &dir in &dirs[2..] {
        assert!(answered_kept(&mut bench, offer, &listing(dir)));
    }
    // Last, since listing it again leases it again.
    assert!(
        !answered_kept(&mut bench, offer, &listing(dirs[1])),
        "the least recently used let go"
    );
}
