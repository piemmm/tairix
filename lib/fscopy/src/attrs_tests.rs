use alloc::collections::BTreeMap;
use alloc::string::ToString;
use core::cell::RefCell;

use tairix_abi::FileStat;

use super::*;

/// One held node's attributes, with the refusal it answers a key with.
#[derive(Default)]
struct Held {
    attrs: RefCell<BTreeMap<String, Vec<u8>>>,
    refusals: BTreeMap<String, Errno>,
    listing: Option<Errno>,
    reading: Option<Errno>,
    /// A key its listing names that it no longer carries.
    ghost: Option<&'static str>,
}

impl Held {
    fn with(entries: &[(&str, &[u8])]) -> Self {
        let held = Self::default();
        for (key, value) in entries {
            held.attrs
                .borrow_mut()
                .insert(key.to_string(), value.to_vec());
        }
        held
    }

    fn refusing(refusals: &[(&str, Errno)]) -> Self {
        Self {
            refusals: refusals
                .iter()
                .map(|(key, errno)| (key.to_string(), *errno))
                .collect(),
            ..Self::default()
        }
    }

    fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.attrs.borrow().get(key).cloned()
    }
}

impl Node for Held {
    fn stat(&self) -> Result<FileStat, Errno> {
        Err(Errno::NotSupported)
    }

    fn set_mode(&self, _: u32) -> Result<(), Errno> {
        Err(Errno::NotSupported)
    }

    fn set_owner(&self, _: u32, _: u32) -> Result<(), Errno> {
        Err(Errno::NotSupported)
    }

    fn attr_keys(&self) -> Result<Vec<String>, Errno> {
        if let Some(err) = self.listing {
            return Err(err);
        }
        let mut keys: Vec<String> = self.ghost.iter().map(ToString::to_string).collect();
        keys.extend(self.attrs.borrow().keys().cloned());
        Ok(keys)
    }

    fn attr_value(&self, key: &str) -> Result<Vec<u8>, Errno> {
        if let Some(err) = self.reading {
            return Err(err);
        }
        self.get(key).ok_or(Errno::NoData)
    }

    fn attr_set(&self, key: &str, value: &[u8]) -> Result<(), Errno> {
        if let Some(err) = self.refusals.get(key) {
            return Err(*err);
        }
        self.attrs
            .borrow_mut()
            .insert(key.to_string(), value.to_vec());
        Ok(())
    }

    fn attr_remove(&self, key: &str) -> Result<(), Errno> {
        self.attrs
            .borrow_mut()
            .remove(key)
            .map(drop)
            .ok_or(Errno::NoData)
    }

    fn sync(&self) -> Result<(), Errno> {
        Ok(())
    }
}

fn riscos_file() -> Held {
    Held::with(&[
        ("acorn.filetype", b"fff"),
        ("acorn.attr", b"RW/r"),
        ("mac.type", b"TEXT"),
    ])
}

#[test]
fn every_attribute_the_destination_keeps_is_carried() {
    let destination = Held::default();
    assert_eq!(
        copy_attributes(&riscos_file(), &destination, Attributes::Carry),
        Ok(Vec::new())
    );
    assert_eq!(
        destination.get("acorn.filetype").as_deref(),
        Some(&b"fff"[..])
    );
    assert_eq!(destination.get("mac.type").as_deref(), Some(&b"TEXT"[..]));
}

#[test]
fn what_the_destination_refuses_is_left_with_why_or_fails_the_copy_that_requires_it() {
    let refusals = [
        ("mac.type", Errno::NotSupported),
        ("acorn.attr", Errno::OutOfRange),
    ];
    let destination = Held::refusing(&refusals);
    let left = copy_attributes(&riscos_file(), &destination, Attributes::Carry).expect("carried");
    assert_eq!(
        left,
        [
            (String::from("acorn.attr"), Refusal::NotRepresentable),
            (String::from("mac.type"), Refusal::Unsupported)
        ]
    );
    assert_eq!(
        destination.get("acorn.filetype").as_deref(),
        Some(&b"fff"[..]),
        "the rest still carried"
    );
    assert_eq!(
        copy_attributes(
            &riscos_file(),
            &Held::refusing(&refusals),
            Attributes::Require
        ),
        Err(AttrError::Write {
            key: String::from("acorn.attr"),
            error: Errno::OutOfRange
        })
    );
    // A failure that is no attribute's fails even a carrying copy.
    let faulty = Held::refusing(&[("acorn.filetype", Errno::DeviceFault)]);
    assert!(matches!(
        copy_attributes(&riscos_file(), &faulty, Attributes::Carry),
        Err(AttrError::Write {
            error: Errno::DeviceFault,
            ..
        })
    ));
}

#[test]
fn a_skipping_copy_carries_nothing_and_a_source_volume_with_none_has_none() {
    let destination = Held::default();
    assert_eq!(
        copy_attributes(&riscos_file(), &destination, Attributes::Skip),
        Ok(Vec::new())
    );
    assert_eq!(destination.get("acorn.filetype"), None);
    let mut plain = Held {
        listing: Some(Errno::NotSupported),
        ..Held::default()
    };
    assert_eq!(
        copy_attributes(&plain, &destination, Attributes::Require),
        Ok(Vec::new())
    );
    plain.listing = Some(Errno::DeviceFault);
    assert_eq!(
        copy_attributes(&plain, &destination, Attributes::Carry),
        Err(AttrError::Read(Errno::DeviceFault))
    );
}

#[test]
fn a_key_gone_since_the_listing_is_passed_over_but_a_source_gone_fails_the_copy() {
    let mut source = riscos_file();
    source.ghost = Some("user.gone");
    let destination = Held::default();
    assert_eq!(
        copy_attributes(&source, &destination, Attributes::Require),
        Ok(Vec::new())
    );
    assert_eq!(
        destination.get("acorn.filetype").as_deref(),
        Some(&b"fff"[..])
    );
    source.reading = Some(Errno::NotFound);
    assert_eq!(
        copy_attributes(&source, &Held::default(), Attributes::Carry),
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
        report.summary.ends_with("'user.'$'\\033'']0;owned'$'\\a'"),
        "{}",
        report.summary
    );
    assert!(!report.summary.chars().any(char::is_control));
    let record = core::str::from_utf8(&report.record).expect("json");
    assert!(
        record.contains(r"'user.'$'\\033'']0;owned'$'\\a'."),
        "the message shown: {record}"
    );
}

/// The keys are listed with `, ` between them, so a key holding a comma is
/// quoted and cannot pass for two.
#[test]
fn a_key_holding_a_comma_cannot_pass_for_two() {
    let mut omissions = Omissions::default();
    omissions.note(&[
        (String::from("user.a, user.b"), Refusal::Unsupported),
        (String::from("user.c,"), Refusal::Unsupported),
        (String::from("user.d"), Refusal::Unsupported),
    ]);
    let report = omissions.report("cp").expect("a report");
    assert!(
        report
            .summary
            .ends_with(": 'user.a, user.b', 'user.c,', user.d"),
        "{}",
        report.summary
    );
}
