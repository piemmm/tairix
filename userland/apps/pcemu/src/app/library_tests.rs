use alloc::vec::Vec;

use super::*;
use crate::dos::setup::default_disk;
use crate::machine::Template;
use crate::storage::hdc::Recording;
use tairix_machines::{Found, Library, LibraryError};

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
        display: Tube::default(),
    }
}

fn add(
    library: &mut Library<Machine>,
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

fn two_machines() -> Library<Machine> {
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

#[test]
fn a_machine_reads_back_as_written_every_part_with_it() {
    let mut library = two_machines();
    let mut changed = library.machine(1).unwrap().clone();
    changed.clock_offset = -3_600;
    changed.display = Tube {
        scanlines: 450,
        phosphor: 1000,
        ..Tube::default()
    };
    library.replace(changed).unwrap();
    let record = library.record().unwrap();
    library.written(record.generation);
    assert_eq!(Library::<Machine>::decode(&record.bytes), Ok(library));
}

#[test]
fn a_disk_needs_a_controller_and_a_tube_no_effect_past_full() {
    let mut library = Library::new();
    let id = library.mint().unwrap();
    let config = Template::IbmPc.config();
    assert_eq!(
        library.add(machine(id, "PC", config, [Some(disk()), None])),
        Err(LibraryError::Invalid(Invalid::NoController)),
        "a PC has no fixed disk controller"
    );
    let mut glaring = machine(id, "PC", config, [None, None]);
    glaring.display.glow = tairix_crt::FULL + 1;
    assert_eq!(
        library.add(glaring),
        Err(LibraryError::Invalid(Invalid::Display))
    );
    library
        .add(machine(id, "PC", config, [None, None]))
        .unwrap();
}

#[test]
fn the_store_holds_every_machine_with_both_disks_and_its_state() {
    assert_eq!(Machine::MAX_MACHINES, 20);
    let blobs = 2 + Machine::MAX_MACHINES * 3;
    assert!(blobs <= tairix_abi::appdata_ipc::APPDATA_BLOB_MAX_COUNT);
}

#[test]
fn a_disk_blob_is_owned_only_while_its_machine_has_that_disk() {
    let library = two_machines();
    let record = library.record().unwrap();
    let mut copies: [Option<&[u8]>; 2] = [None, None];
    let slot = usize::from(record.generation % 2 == 1);
    copies[slot] = Some(&record.bytes);
    let names: Vec<(String, u64)> = [
        record.blob,
        "disk-1-0",
        "disk-1-1",
        "disk-1-2",
        "disk-2-0",
        "state-2",
    ]
    .iter()
    .map(|name| (String::from(*name), 512))
    .collect();
    let loaded = Library::<Machine>::load(&names, copies);
    assert_eq!(loaded.found, Found::Read);
    assert_eq!(disk_blob(1, 0), "disk-1-0");
    assert_eq!(
        loaded.orphans,
        ["disk-1-1", "disk-2-0"],
        "a drive the machine has no disk in; a third drive is no blob of the library's"
    );
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

#[test]
fn a_machine_is_described_by_its_parts() {
    let library = two_machines();
    assert_eq!(
        describe(&library.machines()[0]),
        "8088 at 4.77 MHz \u{b7} 640 KB \u{b7} CGA \u{b7} 1 hard disk \u{b7} AdLib"
    );
    let mut machine = library.machines()[1].clone();
    machine.disks = [None, None];
    assert!(
        !describe(&machine).contains("hard disk"),
        "{}",
        describe(&machine)
    );
    machine.config.coprocessor = true;
    assert!(
        describe(&machine).contains("MHz + 8087 \u{b7}"),
        "{}",
        describe(&machine)
    );
}
