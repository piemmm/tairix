//! The boot block's defect lists as RISC OS 5 writes them: their check bytes,
//! their order, and what they refuse.

use super::{check_byte, Defects, FIRST_END};
use crate::record::{boot_block_checksum, BOOT_BLOCK_SIZE};
use crate::{Fault, Structure};

#[test]
fn check_bytes_are_those_risc_os_5_wrote() {
    for (words, check) in [
        (&[][..], 0x00),
        (&[0x8_0000][..], 0x08),
        (&[0x1400][..], 0x14),
        (&[0x8_0000, 0xB_0000][..], 0x4B),
        (&[0x8_0000, 0x9_0400][..], 0x4D),
        (&[0x8_0000, 0x9_0400, 0xA_0000][..], 0x60),
        (&[0x8_0000, 0xB_0000, 0xF_FC00][..], 0xA9),
        (&[0x8_0000, 0xB_0000, 0xF_FC00, 0x10_0000][..], 0x5D),
        (
            &[0x8_0000, 0xB_0000, 0xC_0400, 0xF_FC00, 0x10_0000][..],
            0x1B,
        ),
    ] {
        assert_eq!(check_byte(words), check, "{words:X?}");
    }
}

#[test]
fn a_defect_joins_the_first_list_in_order_and_once() {
    let mut defects = Defects::none(false);
    for address in [0x9_0400, 0x8_0000, 0xA_0000] {
        assert_eq!(defects.insert(address), Ok(true));
    }
    assert_eq!(defects.insert(0x8_0000), Ok(false), "listed already");
    assert_eq!(defects.first(), &[0x8_0000, 0x9_0400, 0xA_0000]);
    let mut block = [0xEEu8; BOOT_BLOCK_SIZE];
    defects.write(&mut block);
    assert_eq!(
        &block[..20],
        &[0, 0, 8, 0, 0, 4, 9, 0, 0, 0, 0xA, 0, 0x60, 0, 0, 0x20, 0, 0, 0, 0]
    );
    assert_eq!(block[BOOT_BLOCK_SIZE - 1], boot_block_checksum(&block));
    assert_eq!(Defects::read(&block, false), Ok(defects));
}

#[test]
fn a_big_disc_keeps_a_second_list_after_the_first() {
    let mut defects = Defects::none(true);
    defects.insert(0x1400).expect("listed");
    let mut block = [0u8; BOOT_BLOCK_SIZE];
    defects.write(&mut block);
    assert_eq!(
        &block[..12],
        &[0, 0x14, 0, 0, 0x14, 0, 0, 0x20, 0, 0, 0, 0x40]
    );
    let read = Defects::read(&block, true).expect("read");
    assert_eq!((read.first(), read.second()), (&[0x1400][..], &[][..]));
}

#[test]
fn the_lists_hold_no_more_than_the_words_before_the_disc_record() {
    let mut defects = Defects::none(false);
    for n in 0..111 {
        defects.insert(n * 0x400).expect("room");
    }
    assert_eq!(defects.insert(0x10_0000), Err(Fault::TooManyDefects));
    assert_eq!(defects.insert(FIRST_END), Err(Fault::BadDefect));
    let mut block = [0u8; BOOT_BLOCK_SIZE];
    defects.write(&mut block);
    assert_eq!(
        Defects::read(&block, false).map(|d| d.first().len()),
        Ok(111)
    );
}

#[test]
fn a_list_failing_its_checks_is_broken() {
    let broken = Err(Fault::Broken(Structure::DefectList));
    let mut block = [0u8; BOOT_BLOCK_SIZE];
    assert_eq!(Defects::read(&block, false), broken, "no end");
    block[..8].copy_from_slice(&[0, 0, 8, 0, 9, 0, 0, 0x20]);
    assert_eq!(Defects::read(&block, false), broken, "a wrong check byte");
    block[4] = 8;
    assert!(Defects::read(&block, false).is_ok());
    assert_eq!(Defects::read(&block, true), broken, "no second list");
    block[..4].copy_from_slice(&0x2000_0100u32.to_le_bytes());
    assert_eq!(
        Defects::read(&block, false),
        broken,
        "past the first list's reach"
    );
}
