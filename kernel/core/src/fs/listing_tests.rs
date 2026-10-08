use alloc::boxed::Box;

use super::*;

#[test]
fn a_resume_name_holds_the_last_name_set_and_erases_a_longer_one() {
    let mut name = ResumeName::default();
    assert_eq!(name.get(), b"");
    name.set(b"longer-name").expect("fits");
    name.set(b"short").expect("fits");
    assert_eq!(name.get(), b"short");
    assert!(
        name.bytes[5..11].iter().all(|byte| *byte == 0),
        "the tail of the longer name is erased"
    );
    assert_eq!(name.clone().get(), b"short");
}

#[test]
fn a_resume_name_refuses_one_longer_than_a_component() {
    let mut name = ResumeName::default();
    let long = [b'n'; MAX_COMPONENT_LEN + 1];
    assert_eq!(name.set(&long), Err(VfsError::Io));
    assert_eq!(name.get(), b"", "a refused name changes nothing");
    name.set(&long[..MAX_COMPONENT_LEN])
        .expect("a whole component fits");
}

#[test]
fn a_listing_walks_entries_then_mounts_then_ends() {
    let mut listing = Listing::default();
    let at = listing.entries().expect("entries come first");
    at.cursor = 7;
    at.after.set(b"seven").expect("fits");
    assert!(listing.mounts().is_none());

    listing.finish_entries();
    assert!(listing.entries().is_none());
    assert_eq!(listing.mounts().expect("mounts follow").get(), b"");
    listing.finish();
    assert!(listing.is_done());
    assert!(listing.entries().is_none() && listing.mounts().is_none());

    listing.restart();
    assert!(!listing.is_done());
    assert_eq!(listing.entries().expect("from the start").cursor, 0);
}

#[test]
fn a_listing_is_fixed_to_the_volume_its_first_batch_read() {
    let mut listing = Listing::default();
    assert_eq!(listing.on_volume([1; 16]), Ok(()));
    assert_eq!(listing.on_volume([1; 16]), Ok(()));
    assert_eq!(listing.on_volume([2; 16]), Err(VfsError::Stale));
    listing.entries().expect("entries").dir = Some(42);
    listing.finish_entries();
    listing.restart();
    assert_eq!(listing.on_volume([2; 16]), Ok(()), "a restart unbinds");
    assert_eq!(listing.entries().expect("entries").dir, None);
}

#[test]
fn the_mount_phase_keeps_the_directory_the_entries_were_read_from() {
    let mut listing = Listing::default();
    listing.entries().expect("entries").dir = Some(42);
    listing.finish_entries();
    listing.finish();
    assert!(listing.is_done());
    assert_eq!(listing.bound_dir(), Some(42));
}

/// A listing bound to `node` on driver 1 through `registry`.
fn bound(registry: &'static ListingRegistry, node: u64) -> Listing {
    let mut listing = Listing::default();
    listing.entries().expect("entries").dir = Some(node);
    listing.hold(registry, 1).expect("binds");
    listing
}

#[test]
fn removing_a_directory_stales_every_listing_bound_to_it_and_no_other() {
    let registry: &'static ListingRegistry = Box::leak(Box::new(ListingRegistry::new()));
    let mut first = bound(registry, 7);
    let mut copy = first.clone();
    let mut other = bound(registry, 8);
    let mut elsewhere = Listing::default();
    elsewhere.entries().expect("entries").dir = Some(7);
    elsewhere
        .hold(registry, 2)
        .expect("another driver's node 7");

    registry.removed(1, 7);
    assert_eq!(first.hold(registry, 1), Err(VfsError::Stale));
    assert_eq!(copy.hold(registry, 1), Err(VfsError::Stale));
    assert_eq!(other.hold(registry, 1), Ok(()));
    assert_eq!(elsewhere.hold(registry, 2), Ok(()));

    // A directory made under the removed number binds afresh.
    first.restart();
    first.entries().expect("entries").dir = Some(7);
    first.hold(registry, 1).expect("binds the successor");
    assert_eq!(first.hold(registry, 1), Ok(()));
}

#[test]
fn a_record_lives_exactly_as_long_as_a_listing_holds_it() {
    let registry: &'static ListingRegistry = Box::leak(Box::new(ListingRegistry::new()));
    registry.removed(1, 3);
    assert_eq!(registry.len(), 0, "a removal records nothing unheld");
    let listing = bound(registry, 3);
    let copy = listing.clone();
    assert_eq!(registry.len(), 1);
    drop(listing);
    assert_eq!(registry.len(), 1, "the copy still holds it");
    drop(copy);
    assert_eq!(registry.len(), 0);

    // An unbound listing holds nothing until a batch fixes its directory.
    let mut unbound = Listing::default();
    unbound.hold(registry, 1).expect("nothing to bind");
    assert_eq!(registry.len(), 0);
}
