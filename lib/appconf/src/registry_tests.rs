use alloc::string::String;
use core::fmt::Write as _;

use super::{overwrite, Keys, Live, Registry, MOST_KEYS};
use crate::{as_u32, Document};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Key {
    Size,
    Mode,
    Detail,
    Note,
}

/// A size held to a bound, a detail read only once the mode is custom, and a
/// note a store says is absent by not holding it.
#[derive(Clone, Debug, PartialEq)]
struct Fixture {
    size: u32,
    custom: Option<u32>,
    note: Option<u32>,
}

impl Default for Fixture {
    fn default() -> Self {
        Self {
            size: 10,
            custom: None,
            note: None,
        }
    }
}

impl Registry for Fixture {
    type Key = Key;
    const KEYS: &'static [Key] = &[Key::Size, Key::Mode, Key::Detail, Key::Note];

    fn name(key: Key) -> &'static str {
        match key {
            Key::Size => "size",
            Key::Mode => "mode",
            Key::Detail => "detail",
            Key::Note => "note",
        }
    }

    fn read(&mut self, key: Key, text: &str) -> bool {
        match key {
            Key::Size => match as_u32(text) {
                Ok(size) if (1..=500).contains(&size) => self.size = size,
                _ => return false,
            },
            Key::Mode => match text {
                "plain" => self.custom = None,
                "custom" => self.custom = Some(1),
                _ => return false,
            },
            Key::Detail => match (self.custom, as_u32(text)) {
                (None, _) => {}
                (Some(_), Ok(detail)) => self.custom = Some(detail),
                (Some(_), Err(_)) => return false,
            },
            Key::Note => match as_u32(text) {
                Ok(note) => self.note = Some(note),
                Err(_) => return false,
            },
        }
        true
    }

    fn spell(&self, key: Key, out: &mut String) -> bool {
        match key {
            Key::Size => write!(out, "{}", self.size).is_ok(),
            Key::Mode => {
                out.push_str(if self.custom.is_some() {
                    "custom"
                } else {
                    "plain"
                });
                true
            }
            Key::Detail => self
                .custom
                .is_some_and(|detail| write!(out, "{detail}").is_ok()),
            Key::Note => self.note.is_some_and(|note| write!(out, "{note}").is_ok()),
        }
    }

    fn normalise(&mut self) {
        self.size = self.size.min(99);
    }
}

impl Live for Fixture {
    fn take(&mut self, other: &Self, key: Key) -> bool {
        match key {
            Key::Size => overwrite(&mut self.size, other.size),
            Key::Mode | Key::Detail => overwrite(&mut self.custom, other.custom),
            Key::Note => overwrite(&mut self.note, other.note),
        }
    }
}

fn parsed(text: &str) -> Document {
    Document::parse(text).expect("a small document")
}

#[test]
fn a_store_holding_nothing_reads_as_the_default() {
    assert_eq!(
        Fixture::load(&Document::new()),
        (Fixture::default(), alloc::vec![])
    );
}

#[test]
fn each_key_is_read_and_a_refused_value_costs_only_itself() {
    let (record, refused) = Fixture::load(&parsed("size = 400\nnote = many\n"));
    assert_eq!(record.size, 99, "read, then held to its bound");
    assert_eq!(record.note, None, "refused and left at its default");
    assert_eq!(refused, [Key::Note]);
}

#[test]
fn a_key_is_read_in_the_light_of_the_one_before_it() {
    let (plain, refused) = Fixture::load(&parsed("mode = plain\ndetail = 7\n"));
    assert_eq!(plain.custom, None, "a plain mode has no detail to read");
    assert!(refused.is_empty());
    let (custom, _) = Fixture::load(&parsed("detail = 7\nmode = custom\n"));
    assert_eq!(
        custom.custom,
        Some(7),
        "read in key order, whatever the document's"
    );
}

#[test]
fn a_document_spells_the_keys_asked_for_and_leaves_an_absent_one_out() {
    let record = Fixture {
        size: 42,
        custom: Some(3),
        note: None,
    };
    let every = record.document_of(Fixture::KEYS);
    assert_eq!(every.get("size"), Some("42"));
    assert_eq!(every.get("detail"), Some("3"));
    assert_eq!(every.get("note"), None);
    assert_eq!(Fixture::load(&every), (record.clone(), alloc::vec![]));
    let one = record.document_of(&[Key::Mode]);
    assert_eq!((one.get("mode"), one.get("size")), (Some("custom"), None));
}

#[test]
fn an_edit_is_told_apart_setting_by_setting() {
    let was = Fixture::default();
    let mut now = was.clone();
    now.size = 30;
    now.note = Some(2);
    let touched = was.differing(&now);
    assert_eq!(touched, Keys::of(Key::Size).union(Keys::of(Key::Note)));
    let mut copy = was.clone();
    assert_eq!(
        copy.set_from(&now, Keys::of(Key::Size)),
        Keys::of(Key::Size)
    );
    assert_eq!((copy.size, copy.note), (30, None), "only what was named");
    assert!(
        copy.set_from(&now, Keys::of(Key::Size)).is_empty(),
        "already taken"
    );
    assert_eq!(copy.set_from(&now, Keys::ALL), Keys::of(Key::Note));
    assert_eq!(copy, now);
}

#[test]
fn a_set_holds_what_it_is_given() {
    let both = Keys::<Fixture>::of(Key::Mode).union(Keys::of(Key::Note));
    assert!(both.contains(Key::Mode) && both.contains(Key::Note) && !both.contains(Key::Size));
    assert!(Keys::<Fixture>::EMPTY.is_empty() && !both.is_empty());
    assert!(Fixture::KEYS
        .iter()
        .all(|&key| Keys::<Fixture>::ALL.contains(key)));
}

/// A registry of exactly as many keys as a set holds.
#[derive(Clone, Default, PartialEq)]
struct Widest;

const WIDEST: [usize; MOST_KEYS] = {
    let mut keys = [0; MOST_KEYS];
    let mut index = 0;
    while index < MOST_KEYS {
        keys[index] = index;
        index += 1;
    }
    keys
};

impl Registry for Widest {
    type Key = usize;
    const KEYS: &'static [usize] = &WIDEST;

    fn name(_: usize) -> &'static str {
        "key"
    }

    fn read(&mut self, _: usize, _: &str) -> bool {
        true
    }

    fn spell(&self, _: usize, _: &mut String) -> bool {
        false
    }
}

#[test]
fn the_widest_registry_fills_its_set() {
    assert!(WIDEST.iter().all(|&key| Keys::<Widest>::ALL.contains(key)));
    assert_eq!(Keys::<Widest>::ALL, Keys::of(63).union(Keys::<Widest>::ALL));
}
