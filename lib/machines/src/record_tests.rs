use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::*;

/// A machine of a toy application: a size byte, and a part it may own at
/// each of two indices.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Toy {
    id: u32,
    name: String,
    size: u8,
    parts: [bool; 2],
}

/// Why a toy may not be kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TooBig;

impl Kept for Toy {
    type Invalid = TooBig;
    const MAGIC: u32 = u32::from_le_bytes(*b"TOYS");
    const VERSION: u16 = 1;
    const MAX_MACHINES: usize = 6;
    const MAX_RECORD: usize = 4096;
    const PARTS: &'static [(&'static str, usize)] = &[("disk", 2)];

    fn id(&self) -> u32 {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn put(&self, w: &mut Writer) {
        w.u8(self.size);
        w.u8(u8::from(self.parts[0]) | u8::from(self.parts[1]) << 1);
    }

    fn get(id: u32, name: String, r: &mut Reader<'_>) -> Result<Option<Self>, WireError> {
        let size = r.u8()?;
        let parts = r.u8()?;
        if parts > 3 {
            return Ok(None);
        }
        Ok(Some(Self {
            id,
            name,
            size,
            parts: [parts & 1 != 0, parts & 2 != 0],
        }))
    }

    fn invalid(&self) -> Option<TooBig> {
        (self.size > 100).then_some(TooBig)
    }

    fn holds(&self, word: &str, index: usize) -> bool {
        word == "disk" && self.parts.get(index).copied().unwrap_or(false)
    }
}

type Toys = Library<Toy>;

fn toy(id: u32, name: &str, parts: [bool; 2]) -> Toy {
    Toy {
        id,
        name: String::from(name),
        size: 1,
        parts,
    }
}

fn add(library: &mut Toys, name: &str, parts: [bool; 2]) -> u32 {
    let id = library.mint().unwrap();
    library.add(toy(id, name, parts)).unwrap();
    id
}

fn two_machines() -> Toys {
    let mut library = Toys::new();
    add(&mut library, "First", [true, false]);
    add(&mut library, "Second", [false, false]);
    library
}

/// The record as it stands, said written.
fn commit(library: &mut Toys) -> Record {
    let record = library.record().unwrap();
    library.written(record.generation);
    record
}

fn listing(names: &[&str]) -> Vec<(String, u64)> {
    names
        .iter()
        .map(|name| (String::from(*name), 512))
        .collect()
}

#[test]
fn a_record_reads_back_as_written() {
    let mut library = two_machines();
    let mut changed = library.machine(1).unwrap().clone();
    changed.size = 42;
    library.replace(changed).unwrap();
    let record = commit(&mut library);
    assert_eq!(record.blob, RECORD_BLOBS[1], "the first generation is odd");
    assert_eq!(Toys::decode(&record.bytes), Ok(library.clone()));
    assert_eq!(
        commit(&mut library).blob,
        RECORD_BLOBS[0],
        "and the copies alternate"
    );
}

#[test]
fn a_record_not_said_written_is_written_again_over_the_same_blob() {
    let mut library = two_machines();
    let whole = commit(&mut library);
    let failed = library.record().unwrap();
    assert_ne!(failed.blob, whole.blob);
    library.remove(2);
    let again = library.record().unwrap();
    assert_eq!(again.blob, failed.blob, "never over the copy still whole");
    assert_eq!(again.generation, failed.generation);
}

#[test]
fn the_newer_whole_copy_is_read_and_a_torn_one_passed_over() {
    let mut library = two_machines();
    let older = commit(&mut library);
    library.remove(2);
    let newer = commit(&mut library);
    let mut copies: [Option<&[u8]>; 2] = [None, None];
    copies[slot_of(older.generation)] = Some(&older.bytes);
    copies[slot_of(newer.generation)] = Some(&newer.bytes);
    let (read, any) = Toys::newest(copies);
    assert!(any);
    assert_eq!(read.machines().len(), 1, "the newer copy");
    let torn = &newer.bytes[..newer.bytes.len() / 2];
    copies[slot_of(newer.generation)] = Some(torn);
    let (read, _) = Toys::newest(copies);
    assert_eq!(
        read.machines().len(),
        2,
        "a torn write leaves the older copy"
    );
    copies[slot_of(older.generation)] = None;
    assert_eq!(
        Toys::newest(copies),
        (Toys::new(), false),
        "neither checks out"
    );
}

#[test]
fn a_copy_read_from_the_other_blob_is_not_a_copy() {
    let mut library = two_machines();
    let record = commit(&mut library);
    let mut copies: [Option<&[u8]>; 2] = [None, None];
    copies[1 - slot_of(record.generation)] = Some(&record.bytes);
    assert!(!Toys::newest(copies).1);
}

#[test]
fn every_flipped_bit_is_caught() {
    let library = two_machines();
    let bytes = library.record().unwrap().bytes;
    for at in 0..bytes.len() * 8 {
        let mut flipped = bytes.clone();
        flipped[at / 8] ^= 1 << (at % 8);
        assert_eq!(
            Toys::decode(&flipped),
            Err(LibraryError::Malformed),
            "bit {at}"
        );
    }
}

#[test]
fn a_record_larger_than_the_application_allows_is_refused_unread() {
    let mut bytes = two_machines().record().unwrap().bytes;
    bytes.resize(Toy::MAX_RECORD + 1, 0);
    assert_eq!(Toys::decode(&bytes), Err(LibraryError::Malformed));
}

#[test]
fn names_numbers_and_room_are_held_to_their_bounds() {
    let mut library = Toys::new();
    let id = library.mint().unwrap();
    for bad in ["", &"x".repeat(MAX_NAME + 1), "a\u{7}b"] {
        assert_eq!(
            library.add(toy(id, bad, [false; 2])),
            Err(LibraryError::Name)
        );
    }
    let big = Toy {
        size: 101,
        ..toy(id, "Big", [false; 2])
    };
    assert_eq!(
        library.add(big),
        Err(LibraryError::Invalid(TooBig)),
        "the application's own rule"
    );
    assert_eq!(
        library.add(toy(id + 1, "Toy", [false; 2])),
        Err(LibraryError::NoMachine),
        "not minted"
    );
    library
        .add(toy(id, &"é".repeat(MAX_NAME / 2), [false; 2]))
        .unwrap();
    assert_eq!(
        library.add(toy(id, "Toy", [false; 2])),
        Err(LibraryError::NoMachine),
        "held"
    );
    library.remove(id);
    let second = library.mint().unwrap();
    assert!(second > id, "a number is never used again");
    while library.machines().len() < Toy::MAX_MACHINES {
        add(&mut library, "Toy", [false; 2]);
    }
    assert_eq!(library.mint(), Err(LibraryError::Full));
    assert_eq!(
        library.add(toy(second, "Toy", [false; 2])),
        Err(LibraryError::Full)
    );
    let mut nameless = library.machines()[0].clone();
    nameless.name.clear();
    assert_eq!(library.replace(nameless), Err(LibraryError::Name));
    let mut stranger = library.machines()[0].clone();
    stranger.id = 999;
    assert_eq!(library.replace(stranger), Err(LibraryError::NoMachine));
}

#[test]
fn a_record_with_numbers_out_of_order_or_fields_no_machine_holds_is_refused() {
    let mut library = two_machines();
    library.next_id = 2;
    assert_eq!(
        Toys::decode(&library.record().unwrap().bytes),
        Err(LibraryError::Malformed),
        "a machine numbered at or past the next number"
    );
    let mut twice = two_machines();
    twice.machines[1].id = 1;
    assert_eq!(
        Toys::decode(&twice.record().unwrap().bytes),
        Err(LibraryError::Malformed),
        "a number held twice"
    );
    let mut big = two_machines();
    big.machines[0].size = 200;
    assert_eq!(
        Toys::decode(&big.record().unwrap().bytes),
        Err(LibraryError::Malformed),
        "a machine the application refuses"
    );
}

#[test]
fn the_blobs_are_named_by_machine_and_only_those_names_are_owned() {
    assert_eq!(part_blob("disk", 1, 0), "disk-1-0");
    assert_eq!(state_blob(12), "state-12");
    assert_eq!(owner::<Toy>("disk-12-1"), Some((12, Part::Own("disk", 1))));
    assert_eq!(owner::<Toy>("state-3"), Some((3, Part::State)));
    for stranger in [
        "disk-1-2",
        "disk-01-0",
        "disk-+1-0",
        "disk-1-00",
        "state-",
        "state-1x",
        "library-a",
        "disk-1",
        "tape-1-0",
        "rom.basic",
    ] {
        assert_eq!(owner::<Toy>(stranger), None, "{stranger}");
    }
}

#[test]
fn a_first_run_finds_nothing_and_sweeps_what_a_cut_short_creation_left() {
    let loaded = Toys::load(&listing(&["disk-1-0", "rom.os"]), [None, None]);
    assert_eq!(loaded.found, Found::Fresh);
    assert!(loaded.library.machines().is_empty());
    assert_eq!(
        loaded.orphans,
        ["disk-1-0"],
        "a blob no machine names is left"
    );
}

#[test]
fn a_read_library_sweeps_only_blobs_no_machine_owns() {
    let mut library = two_machines();
    let record = commit(&mut library);
    let mut copies: [Option<&[u8]>; 2] = [None, None];
    copies[slot_of(record.generation)] = Some(&record.bytes);
    let names = listing(&[
        record.blob,
        "disk-1-0",
        "state-1",
        "disk-1-1",
        "state-2",
        "disk-2-0",
        "disk-9-0",
        "notes",
    ]);
    let loaded = Toys::load(&names, copies);
    assert_eq!(loaded.found, Found::Read);
    assert_eq!(loaded.library, library);
    assert_eq!(
        loaded.orphans,
        ["disk-1-1", "disk-2-0", "disk-9-0"],
        "a part the machine does not have, and a machine the record does not hold"
    );
}

#[test]
fn an_unreadable_library_sweeps_nothing_and_never_mints_their_numbers() {
    let names = listing(&["library-a", "disk-7-0", "state-12"]);
    let mut loaded = Toys::load(&names, [Some(b"torn"), None]);
    assert_eq!(loaded.found, Found::Unreadable);
    assert!(loaded.orphans.is_empty(), "nothing is deleted on a guess");
    assert_eq!(loaded.library.mint(), Ok(13));
}

#[test]
fn a_new_machine_is_named_apart_from_the_rest_and_a_copy_says_so() {
    let mut library = Toys::new();
    assert_eq!(library.fresh_name("Toy"), "Toy");
    add(&mut library, "Toy", [false; 2]);
    assert_eq!(library.fresh_name("Toy"), "Toy 2");
    assert_eq!(library.copy_name("Toy"), "Toy copy");
    add(&mut library, "Toy copy", [false; 2]);
    assert_eq!(library.copy_name("Toy"), "Toy copy 2");
    let long = "é".repeat(MAX_NAME / 2);
    let copy = library.copy_name(&long);
    assert!(copy.len() <= MAX_NAME && copy.ends_with(" copy"), "{copy}");
}

#[test]
fn the_store_holds_as_many_machines_as_their_blobs_allow() {
    let room = APPDATA_BLOB_MAX_COUNT - RECORD_BLOBS.len();
    assert_eq!(store_holds(&[]), room, "each owning only its state");
    assert_eq!(store_holds(&[("disk", 2)]), room / 3);
    assert_eq!(store_holds(&[("disk", 2), ("tape", 1)]), room / 4);
}

#[test]
fn a_tube_reads_back_as_written_and_one_past_full_is_not_within() {
    let tube = Tube {
        scanlines: 1,
        glow: 2,
        fuzz: 3,
        phosphor: 4,
        wobble: FULL,
    };
    let mut w = Writer::try_with_capacity(16).unwrap();
    put_tube(&mut w, tube);
    let bytes = w.finish();
    let mut r = Reader::new(&bytes);
    assert_eq!(get_tube(&mut r).unwrap(), tube);
    assert!(r.is_exhausted());
    assert!(get_tube(&mut Reader::new(&bytes[..bytes.len() - 1])).is_err());
    assert!(tube_within(tube));
    for past in [
        Tube {
            scanlines: FULL + 1,
            ..tube
        },
        Tube {
            glow: FULL + 1,
            ..tube
        },
        Tube {
            fuzz: FULL + 1,
            ..tube
        },
        Tube {
            phosphor: FULL + 1,
            ..tube
        },
        Tube {
            wobble: FULL + 1,
            ..tube
        },
    ] {
        assert!(!tube_within(past), "{past:?}");
    }
}

/// One write at a time: a change made while one is under way is written once
/// it lands, and a deleted machine's blobs wait on a write that succeeds.
#[test]
fn a_record_write_waits_its_turn_and_removals_wait_on_success() {
    let mut writes = Writes::default();
    writes.doom(4);
    assert!(writes.begin(), "the first write begins");
    writes.doom(9);
    assert!(!writes.begin(), "a second waits for the first");
    assert_eq!(
        writes.landed(true),
        Landed {
            removals: vec![4],
            newly_failed: false,
            again: true,
        },
        "the first write's removals go, and the change made meanwhile is written"
    );
    assert!(writes.begin());
    assert_eq!(
        writes.landed(false),
        Landed {
            removals: Vec::new(),
            newly_failed: true,
            again: false,
        },
        "a failed write removes nothing"
    );
    assert!(writes.failed(), "and is to be tried again");
    assert!(writes.begin());
    assert!(
        !writes.landed(false).newly_failed,
        "the user is told of a failure once"
    );
    assert!(writes.begin());
    assert_eq!(
        writes.landed(true).removals,
        vec![9],
        "what waited on the failures goes once one succeeds"
    );
    assert!(!writes.failed());
    assert!(writes.begin());
    assert!(
        writes.landed(false).newly_failed,
        "a later failure is told again"
    );
}
