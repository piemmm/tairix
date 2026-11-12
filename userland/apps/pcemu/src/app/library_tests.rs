use super::*;
use crate::dos::setup::default_disk;
use crate::machine::Template;
use crate::storage::hdc::Recording;

fn disk() -> StoredDisk {
    let made = default_disk(Recording::Rll, 7).unwrap();
    StoredDisk {
        kind: made.kind,
        format: made.low_level,
    }
}

fn machine(id: u32, name: &str, config: BoardConfig, disks: [Option<StoredDisk>; 2]) -> Machine {
    Machine {
        id,
        name: String::from(name),
        config,
        disks,
        clock_offset: 0,
        display: tairix_crt::Tube::default(),
    }
}

fn add(
    library: &mut Library,
    name: &str,
    template: Template,
    disks: [Option<StoredDisk>; 2],
) -> u32 {
    let id = library.mint().unwrap();
    library
        .add(machine(id, name, template.config(), disks))
        .unwrap();
    id
}

fn two_machines() -> Library {
    let mut library = Library::new();
    add(&mut library, "XT", Template::IbmXt, [Some(disk()), None]);
    add(
        &mut library,
        "Workstation",
        Template::Workstation186,
        [None, None],
    );
    library
}

/// The record as it stands, said written.
fn commit(library: &mut Library) -> Record {
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
    changed.clock_offset = -3_600;
    changed.display = tairix_crt::Tube {
        scanlines: 450,
        phosphor: 1000,
        ..tairix_crt::Tube::default()
    };
    library.replace(changed).unwrap();
    let record = commit(&mut library);
    assert_eq!(record.blob, RECORD_BLOBS[1], "the first generation is odd");
    assert_eq!(Library::decode(&record.bytes), Ok(library.clone()));
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
    let (read, any) = Library::newest(copies);
    assert!(any);
    assert_eq!(read.machines().len(), 1, "the newer copy");
    let torn = &newer.bytes[..newer.bytes.len() / 2];
    copies[slot_of(newer.generation)] = Some(torn);
    let (read, _) = Library::newest(copies);
    assert_eq!(
        read.machines().len(),
        2,
        "a torn write leaves the older copy"
    );
    copies[slot_of(older.generation)] = None;
    assert_eq!(
        Library::newest(copies),
        (Library::new(), false),
        "neither checks out"
    );
}

#[test]
fn a_copy_read_from_the_other_blob_is_not_a_copy() {
    let mut library = two_machines();
    let record = commit(&mut library);
    let mut copies: [Option<&[u8]>; 2] = [None, None];
    copies[1 - slot_of(record.generation)] = Some(&record.bytes);
    assert!(!Library::newest(copies).1);
}

#[test]
fn every_flipped_bit_is_caught() {
    let library = two_machines();
    let bytes = library.record().unwrap().bytes;
    for at in 0..bytes.len() * 8 {
        let mut flipped = bytes.clone();
        flipped[at / 8] ^= 1 << (at % 8);
        assert_eq!(
            Library::decode(&flipped),
            Err(LibraryError::Malformed),
            "bit {at}"
        );
    }
}

#[test]
fn names_numbers_and_room_are_held_to_their_bounds() {
    let mut library = Library::new();
    let config = Template::IbmPc.config();
    let id = library.mint().unwrap();
    for bad in ["", &"x".repeat(MAX_NAME + 1), "a\u{7}b"] {
        assert_eq!(
            library.add(machine(id, bad, config, [None, None])),
            Err(LibraryError::Name)
        );
    }
    assert_eq!(
        library.add(machine(id, "PC", config, [Some(disk()), None])),
        Err(LibraryError::NoController),
        "a PC has no fixed disk controller"
    );
    assert_eq!(
        library.add(machine(id + 1, "PC", config, [None, None])),
        Err(LibraryError::NoMachine),
        "not minted"
    );
    library
        .add(machine(id, &"é".repeat(MAX_NAME / 2), config, [None, None]))
        .unwrap();
    assert_eq!(
        library.add(machine(id, "PC", config, [None, None])),
        Err(LibraryError::NoMachine),
        "held"
    );
    library.remove(id);
    let second = library.mint().unwrap();
    assert!(second > id, "a number is never used again");
    while library.machines().len() < MAX_MACHINES {
        add(&mut library, "PC", Template::IbmPc, [None, None]);
    }
    assert_eq!(library.mint(), Err(LibraryError::Full));
    assert_eq!(
        library.add(machine(second, "PC", config, [None, None])),
        Err(LibraryError::Full)
    );
    let mut nameless = library.machines()[0].clone();
    nameless.name.clear();
    assert_eq!(library.replace(nameless), Err(LibraryError::Name));
    let mut stranger = library.machines()[0].clone();
    stranger.id = 999;
    assert_eq!(library.replace(stranger), Err(LibraryError::NoMachine));
    let mut glaring = library.machines()[0].clone();
    glaring.display.glow = tairix_crt::FULL + 1;
    assert_eq!(library.replace(glaring), Err(LibraryError::Display));
}

#[test]
fn a_record_with_numbers_out_of_order_is_refused() {
    let mut library = two_machines();
    library.next_id = 2;
    assert_eq!(
        Library::decode(&library.record().unwrap().bytes),
        Err(LibraryError::Malformed),
        "a machine numbered at or past the next number"
    );
    let mut twice = two_machines();
    twice.machines[1].id = 1;
    assert_eq!(
        Library::decode(&twice.record().unwrap().bytes),
        Err(LibraryError::Malformed),
        "a number held twice"
    );
}

#[test]
fn the_blobs_are_named_by_machine_and_only_those_names_are_owned() {
    assert_eq!(disk_blob(1, 0), "disk-1-0");
    assert_eq!(state_blob(12), "state-12");
    assert_eq!(owner("disk-12-1"), Some((12, Part::Disk(1))));
    assert_eq!(owner("state-3"), Some((3, Part::State)));
    for stranger in [
        "disk-1-2",
        "disk-01-0",
        "disk-+1-0",
        "state-",
        "state-1x",
        "library-a",
        "disk-1",
    ] {
        assert_eq!(owner(stranger), None, "{stranger}");
    }
}

#[test]
fn a_first_run_finds_nothing_and_sweeps_what_a_cut_short_creation_left() {
    let loaded = Library::load(&listing(&["disk-1-0"]), [None, None]);
    assert_eq!(loaded.found, Found::Fresh);
    assert!(loaded.library.machines().is_empty());
    assert_eq!(loaded.orphans, ["disk-1-0"]);
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
    let loaded = Library::load(&names, copies);
    assert_eq!(loaded.found, Found::Read);
    assert_eq!(loaded.library, library);
    assert_eq!(
        loaded.orphans,
        ["disk-1-1", "disk-2-0", "disk-9-0"],
        "a disk the machine does not have, and a machine the record does not hold"
    );
}

#[test]
fn an_unreadable_library_sweeps_nothing_and_never_mints_their_numbers() {
    let names = listing(&["library-a", "disk-7-0", "state-12"]);
    let mut loaded = Library::load(&names, [Some(b"torn"), None]);
    assert_eq!(loaded.found, Found::Unreadable);
    assert!(loaded.orphans.is_empty(), "nothing is deleted on a guess");
    assert_eq!(loaded.library.mint(), Ok(13));
}

#[test]
fn a_new_machine_is_named_apart_from_the_rest_and_a_copy_says_so() {
    let mut library = Library::new();
    assert_eq!(library.fresh_name("Turbo XT"), "Turbo XT");
    add(&mut library, "Turbo XT", Template::TurboXt, [None, None]);
    assert_eq!(library.fresh_name("Turbo XT"), "Turbo XT 2");
    assert_eq!(library.copy_name("Turbo XT"), "Turbo XT copy");
    add(
        &mut library,
        "Turbo XT copy",
        Template::TurboXt,
        [None, None],
    );
    assert_eq!(library.copy_name("Turbo XT"), "Turbo XT copy 2");
    let long = "é".repeat(MAX_NAME / 2);
    let copy = library.copy_name(&long);
    assert!(copy.len() <= MAX_NAME && copy.ends_with(" copy"), "{copy}");
}

#[test]
fn a_disk_is_as_large_as_its_drive_on_its_controller() {
    let mut library = Library::new();
    let turbo = add(
        &mut library,
        "Turbo",
        Template::TurboXt,
        [Some(disk()), None],
    );
    let machine = library.machine(turbo).unwrap();
    assert_eq!(
        machine.disk_sectors(0),
        Some(615 * 4 * 26),
        "every cylinder of the ST-238R at 26 sectors a track"
    );
    assert_eq!(machine.disk_sectors(1), None);
    let mut mfm = machine.clone();
    mfm.config = Template::IbmXt.config();
    assert_eq!(
        mfm.disk_sectors(0),
        Some(615 * 4 * 17),
        "an MFM controller records 17"
    );
}

#[test]
fn the_clock_offset_is_what_the_guest_set_against_the_host() {
    let mut machine = two_machines().machine(1).unwrap().clone();
    let host = 1_000_000_000;
    let guest = CivilTime::from_unix_secs(host - 86_400);
    machine.clock_offset = clock_offset(&guest, host).unwrap();
    assert_eq!(machine.clock_offset, -86_400);
    assert_eq!(
        machine.clock_at(host + 60),
        CivilTime::from_unix_secs(host - 86_400 + 60)
    );
}
