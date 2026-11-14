use alloc::vec;
use alloc::vec::Vec;

use super::*;

#[test]
fn a_short_rom_is_mirrored_across_its_sixteen_kilobytes() {
    let mut image = vec![0; 0x2000];
    image[0] = 0x42;
    let mirrored = rom_image(&image).expect("8 KiB is a ROM size");
    assert_eq!(mirrored.len(), ROM_SIZE);
    assert_eq!(mirrored[0x2000], 0x42);
    assert_eq!(rom_image(&[0; 0x3000]), Err(RomError::BadSize));
}

#[test]
fn a_monitor_keeps_the_newest_accesses_and_only_while_asked() {
    let access = |cycle| IoAccess {
        cycle,
        addr: 0xFE00,
        value: 0,
        write: false,
    };
    let mut monitor = Monitor::default();
    monitor.record(access(1));
    assert_eq!(monitor.take().count(), 0, "off");
    monitor.set(true);
    for cycle in 0..IO_TRACE_CAPACITY as u64 + 3 {
        monitor.record(access(cycle));
    }
    assert_eq!(
        monitor.fresh().take().count(),
        0,
        "a fresh one holds nothing"
    );
    let kept: Vec<u64> = monitor.take().map(|access| access.cycle).collect();
    assert_eq!(kept.len(), IO_TRACE_CAPACITY);
    assert_eq!(kept.first(), Some(&3), "the oldest dropped");
}
