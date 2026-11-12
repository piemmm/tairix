//! TAIRiX DOS's master boot record started by the BIOS from a fixed disk
//! with no diskette in its drive: the active partition's system started,
//! and each disk it cannot start said.

use crate::bios::run_until;
use crate::dos::harness::{
    fixed_boot, fixed_image, fixed_volume, screen, settle, typed, PARTITION_START, SECOND,
};
use crate::dos::volume;
use crate::machine::Machine;
use alloc::vec::Vec;
use tairix_fat::stamp::Stamp;
use tairix_partition::mbr::{PARTITION_ENTRY_LEN, PARTITION_TABLE_OFFSET};

/// The fixed disk image with TAIRiX DOS's system on its one partition.
fn system_disk() -> Vec<u8> {
    let shell = crate::dos::shell::image().unwrap();
    let files = [volume::File {
        path: "COMMAND.COM",
        bytes: &shell,
        attributes: 0,
    }];
    let volume = volume::volume(&fixed_volume(0x1989_0610), None, &files, Stamp::NONE).unwrap();
    fixed_image(Some(&volume))
}

/// The machine booting `image`, run until its boot has settled where it
/// stops.
fn started(image: Vec<u8>, bad: &[u32]) -> Machine {
    let mut m = fixed_boot(image, bad);
    let now = m.now();
    run_until(&mut m, now + 20 * SECOND);
    m
}

#[test]
fn the_master_boot_record_starts_the_active_partitions_system() {
    let mut m = fixed_boot(system_disk(), &[]);
    settle(&mut m);
    assert!(screen(&m).contains("Enter new date"), "{}", screen(&m));
    typed(&mut m, "");
    typed(&mut m, "");
    assert!(
        screen(&m).contains("C>"),
        "started from drive C:\n{}",
        screen(&m)
    );
}

#[test]
fn a_disk_the_master_boot_record_cannot_start_is_said_and_stays_said() {
    let entry = |slot: usize| PARTITION_TABLE_OFFSET + slot * PARTITION_ENTRY_LEN;
    let mut second_active = system_disk();
    second_active[entry(1)] = 0x80;
    let mut unsigned = system_disk();
    unsigned[usize::try_from(PARTITION_START).unwrap() * 512 + 510] = 0;
    let mut none_active = system_disk();
    none_active[entry(0)] = 0;
    let first_track_of_the_partition = 1;
    for (image, bad, said) in [
        (second_active, Vec::new(), "Invalid partition table"),
        (unsigned, Vec::new(), "Missing operating system"),
        (
            system_disk(),
            alloc::vec![first_track_of_the_partition],
            "Error loading operating system",
        ),
        (none_active, Vec::new(), "No system disk"),
    ] {
        let mut m = started(image, &bad);
        assert!(screen(&m).contains(said), "{said}:\n{}", screen(&m));
        let now = m.now();
        run_until(&mut m, now + 2 * SECOND);
        assert_eq!(
            screen(&m).matches(said).count(),
            1,
            "said once, and left there:\n{}",
            screen(&m)
        );
    }
}
