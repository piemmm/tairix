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
