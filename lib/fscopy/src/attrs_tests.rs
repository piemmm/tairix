use alloc::collections::BTreeMap;
use alloc::string::ToString;
use core::cell::RefCell;

use super::*;

/// Paths' attributes, with the refusal a destination answers a key with.
#[derive(Default)]
struct Store {
    attrs: RefCell<BTreeMap<(String, String), Vec<u8>>>,
    refusals: BTreeMap<String, Errno>,
    listing: Option<Errno>,
    reading: Option<Errno>,
}

impl Store {
    fn with(entries: &[(&str, &str, &[u8])]) -> Self {
        let store = Self::default();
        for (path, key, value) in entries {
            store
                .attrs
                .borrow_mut()
                .insert((path.to_string(), key.to_string()), value.to_vec());
        }
        store
    }

    fn get(&self, path: &str, key: &str) -> Option<Vec<u8>> {
        self.attrs
            .borrow()
            .get(&(path.to_string(), key.to_string()))
            .cloned()
    }
}

impl AttributeStore for Store {
    fn attr_keys(&self, path: &str) -> Result<Vec<String>, Errno> {
        if let Some(err) = self.listing {
            return Err(err);
        }
        Ok(self
            .attrs
            .borrow()
            .keys()
            .filter(|(owner, _)| owner == path)
            .map(|(_, key)| key.clone())
            .collect())
    }

    fn attr_value(&self, path: &str, key: &str) -> Result<Vec<u8>, Errno> {
        if let Some(err) = self.reading {
            return Err(err);
        }
        self.get(path, key).ok_or(Errno::NoData)
    }

    fn attr_set(&self, path: &str, key: &str, value: &[u8]) -> Result<(), Errno> {
        if let Some(err) = self.refusals.get(key) {
            return Err(*err);
        }
        self.attrs
            .borrow_mut()
            .insert((path.to_string(), key.to_string()), value.to_vec());
        Ok(())
    }
}

fn riscos_file() -> Store {
    Store::with(&[
        ("/a", "acorn.filetype", b"fff"),
        ("/a", "acorn.attr", b"RW/r"),
        ("/a", "mac.type", b"TEXT"),
    ])
}

#[test]
fn every_attribute_the_destination_keeps_is_carried() {
    let store = riscos_file();
    assert_eq!(
        copy_attributes(&store, "/a", "/b", Attributes::Carry),
        Ok(Vec::new())
    );
    assert_eq!(
        store.get("/b", "acorn.filetype").as_deref(),
        Some(&b"fff"[..])
    );
    assert_eq!(store.get("/b", "mac.type").as_deref(), Some(&b"TEXT"[..]));
}

#[test]
fn what_the_destination_refuses_is_left_with_why_or_fails_the_copy_that_requires_it() {
    let mut store = riscos_file();
    store
        .refusals
        .insert(String::from("mac.type"), Errno::NotSupported);
    store
        .refusals
        .insert(String::from("acorn.attr"), Errno::OutOfRange);
    let left = copy_attributes(&store, "/a", "/b", Attributes::Carry).expect("carried");
    assert_eq!(
        left,
        [
            (String::from("acorn.attr"), Refusal::NotRepresentable),
            (String::from("mac.type"), Refusal::Unsupported)
        ]
    );
    assert_eq!(
        store.get("/b", "acorn.filetype").as_deref(),
        Some(&b"fff"[..]),
        "the rest still carried"
    );
    assert_eq!(
        copy_attributes(&store, "/a", "/c", Attributes::Require),
        Err(AttrError::Write {
            key: String::from("acorn.attr"),
            error: Errno::OutOfRange
        })
    );
    // A failure that is no attribute's fails even a carrying copy.
    store
        .refusals
        .insert(String::from("acorn.filetype"), Errno::DeviceFault);
    assert!(matches!(
        copy_attributes(&store, "/a", "/d", Attributes::Carry),
        Err(AttrError::Write {
            error: Errno::DeviceFault,
            ..
        })
    ));
}

#[test]
fn a_skipping_copy_carries_nothing_and_a_source_volume_with_none_has_none() {
    let store = riscos_file();
    assert_eq!(
        copy_attributes(&store, "/a", "/b", Attributes::Skip),
        Ok(Vec::new())
    );
    assert_eq!(store.get("/b", "acorn.filetype"), None);
    let mut plain = Store {
        listing: Some(Errno::NotSupported),
        ..Store::default()
    };
    assert_eq!(
        copy_attributes(&plain, "/a", "/b", Attributes::Require),
        Ok(Vec::new())
    );
    plain.listing = Some(Errno::DeviceFault);
    assert_eq!(
        copy_attributes(&plain, "/a", "/b", Attributes::Carry),
        Err(AttrError::Read(Errno::DeviceFault))
    );
}

#[test]
fn a_key_gone_since_the_listing_is_passed_over_but_a_source_gone_fails_the_copy() {
    struct Vanishing(Store);
    impl AttributeStore for Vanishing {
        fn attr_keys(&self, path: &str) -> Result<Vec<String>, Errno> {
            let mut keys = self.0.attr_keys(path)?;
            keys.insert(0, String::from("user.gone"));
            Ok(keys)
        }
        fn attr_value(&self, path: &str, key: &str) -> Result<Vec<u8>, Errno> {
            self.0.attr_value(path, key)
        }
        fn attr_set(&self, path: &str, key: &str, value: &[u8]) -> Result<(), Errno> {
            self.0.attr_set(path, key, value)
        }
    }
    let mut store = Vanishing(riscos_file());
    assert_eq!(
        copy_attributes(&store, "/a", "/b", Attributes::Require),
        Ok(Vec::new())
    );
    assert_eq!(
        store.0.get("/b", "acorn.filetype").as_deref(),
        Some(&b"fff"[..])
    );
    store.0.reading = Some(Errno::NotFound);
    assert_eq!(
        copy_attributes(&store, "/a", "/c", Attributes::Carry),
        Err(AttrError::Read(Errno::NotFound))
    );
}

#[test]
fn a_run_of_copies_reports_what_it_left_once() {
    let mut omissions = Omissions::default();
    assert_eq!(omissions.report("cp"), None);
    omissions.note(&[]);
    for _ in 0..3 {
        omissions.note(&[(String::from("acorn.filetype"), Refusal::Unsupported)]);
    }
    omissions.note(&[(String::from("mac.type"), Refusal::Unsupported)]);
    let report = omissions.report("cp").expect("a report");
    assert_eq!(
        report.summary,
        "extended attributes not kept on 4 files (the destination cannot store them): \
         acorn.filetype, mac.type"
    );
    let record = core::str::from_utf8(&report.record)
        .expect("json")
        .to_string();
    assert!(record.contains("\"kind\":\"omission\""), "{record}");
    assert!(
        record.contains("\"code\":\"fs.attributes_not_kept\""),
        "{record}"
    );
    assert!(record.contains("\"files\":4"), "{record}");
    assert!(record.contains("\"keys_exhaustive\":true"), "{record}");
    assert_eq!(record.matches('\n').count(), 1, "one record");
}

#[test]
fn a_report_names_a_bounded_set_of_keys() {
    let mut omissions = Omissions::default();
    for index in 0..(REPORT_KEYS + 3) {
        omissions.note(&[(alloc::format!("user.k{index:02}"), Refusal::NoRoom)]);
    }
    let warning = omissions.report("mv").expect("a report").summary;
    assert!(warning.ends_with(", and others"), "{warning}");
    assert_eq!(warning.matches("user.k").count(), REPORT_KEYS);
}

#[test]
fn runs_merged_report_as_one_and_keep_the_key_bound() {
    let mut first = Omissions::default();
    first.note(&[(String::from("acorn.filetype"), Refusal::Unsupported)]);
    let mut second = Omissions::default();
    for index in 0..REPORT_KEYS {
        second.note(&[(alloc::format!("user.k{index}"), Refusal::NoRoom)]);
    }
    let mut whole = Omissions::default();
    whole.merge(&first);
    whole.merge(&second);
    whole.merge(&Omissions::default());
    let warning = whole.report("fstree").expect("a report").summary;
    assert!(
        warning.contains(&alloc::format!("on {} files", REPORT_KEYS + 1)),
        "{warning}"
    );
    assert!(
        warning.contains("cannot store them; the destination has no room"),
        "{warning}"
    );
    assert!(warning.ends_with(", and others"), "{warning}");
    assert_eq!(warning.matches(", ").count(), REPORT_KEYS, "{warning}");
}

#[test]
fn a_report_frames_keys_of_any_spelling_whole() {
    let mut omissions = Omissions::default();
    let hostile = String::from(
        "user.\u{1}\u{1}\u{1}\u{1}\u{1}\u{1}\u{1}\u{1}\u{1}\u{1}\u{1}\u{1}\u{1}\u{1}\u{1}\u{1}",
    );
    omissions.note(&[(hostile, Refusal::Unsupported)]);
    let record = omissions.report("cp").expect("a report").record;
    assert!(
        record.ends_with(b"}\n"),
        "{:?}",
        core::str::from_utf8(&record)
    );
}

#[test]
fn a_key_carrying_control_bytes_is_never_shown_as_them() {
    let mut omissions = Omissions::default();
    omissions.note(&[(
        String::from("user.\u{1b}]0;owned\u{7}"),
        Refusal::Unsupported,
    )]);
    let report = omissions.report("cp").expect("a report");
    assert!(
        report.summary.ends_with("user.\\x1b]0;owned\\x07"),
        "{}",
        report.summary
    );
    assert!(!report.summary.chars().any(char::is_control));
    let record = core::str::from_utf8(&report.record).expect("json");
    assert!(
        record.contains(r"user.\\x1b]0;owned\\x07."),
        "the message shown: {record}"
    );
}
