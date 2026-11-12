//! A sparse disk holds only what has data, and a region reaches no further
//! than its own sectors.

use super::*;

#[test]
fn a_sparse_disk_keeps_only_the_sectors_with_data() {
    let mut disk = SparseDisk::new(1_000_000);
    let mut out = [0xFF; SECTOR];
    disk.read(999_999, &mut out).unwrap();
    assert_eq!(out, [0; SECTOR], "never written reads as zeros");
    disk.write(5, &[0x5A; SECTOR]).unwrap();
    disk.write(7, &[0; SECTOR]).unwrap();
    assert_eq!(disk.held(), 1, "zeros cost nothing");
    disk.read(5, &mut out).unwrap();
    assert_eq!(out, [0x5A; SECTOR]);
    disk.write(5, &[0; SECTOR]).unwrap();
    assert_eq!(disk.held(), 0, "a sector written back to zeros is let go");
    disk.write(9, &[1; SECTOR]).unwrap();
    disk.write(2, &[2; SECTOR]).unwrap();
    let held: alloc::vec::Vec<u64> = disk.sectors_held().map(|(sector, _)| sector).collect();
    assert_eq!(held, [2, 9], "in order");
    assert_eq!(disk.read(1_000_000, &mut out), Err(MediaError::OutOfRange));
    assert_eq!(
        disk.write(u64::MAX, &[1; SECTOR]),
        Err(MediaError::OutOfRange)
    );
    let mut locked = disk.protected();
    assert_eq!(
        locked.write(0, &[1; SECTOR]),
        Err(MediaError::WriteProtected)
    );
    assert!(locked.read_only());
}

#[test]
fn a_region_is_its_own_medium_within_the_one_it_lies_on() {
    let mut disk = SparseDisk::new(100);
    let mut region = Region {
        device: &mut disk,
        first: 17,
        sectors: 50,
    };
    region.write(0, &[3; SECTOR]).unwrap();
    region.write(49, &[4; SECTOR]).unwrap();
    assert_eq!(
        region.write(50, &[5; SECTOR]),
        Err(MediaError::OutOfRange),
        "past its end, though the disk goes on"
    );
    assert_eq!(region.sectors(), 50);
    let mut out = [0; SECTOR];
    disk.read(17, &mut out).unwrap();
    assert_eq!(out, [3; SECTOR]);
    disk.read(66, &mut out).unwrap();
    assert_eq!(out, [4; SECTOR]);
    let mut far = Region {
        device: &mut disk,
        first: u64::MAX,
        sectors: 10,
    };
    assert_eq!(
        far.read(1, &mut out),
        Err(MediaError::OutOfRange),
        "no wrapping past the end of the numbers"
    );
}
