//! The interface's shared parts: a listed entry's layouts and its record,
//! the catalogue information in registers, and the five-byte stamp.

use alloc::vec::Vec;

use super::{kind, stamp, Info, Layout};
use crate::memory::layout::RISC_OS_5;
use crate::memory::Bits;
use crate::os::Regs;
use crate::testkit::{with_application, DATA};

/// A text file of `&123` bytes stamped `&45_6789_0ABC`, its owner's to read
/// and write.
const TEXT: Info = Info {
    kind: kind::FILE,
    load: 0xFFFF_FF45,
    exec: 0x6789_0ABC,
    length: 0x123,
    attributes: 0x03,
};

#[test]
fn an_entry_takes_its_name_and_terminator_a_record_s_padded_to_a_word() {
    assert_eq!(Layout::Names.size(b"abc"), 4);
    assert_eq!(Layout::Info.size(b"abc"), 24);
    assert_eq!(Layout::Info.size(b"abcd"), 28);
    assert_eq!(Layout::FullInfo.size(b"ab"), 32);
    assert_eq!(Layout::FullInfo.size(b"abc"), 36);
}

#[test]
fn an_entry_s_name_follows_its_record_and_a_full_one_s_internal_name_and_stamp() {
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    s.write(DATA, &[0xEE; 40]);
    s.kernel
        .os()
        .write_listed(DATA, Layout::Names, &TEXT, 7, b"Name")
        .expect("written");
    assert_eq!(s.read(DATA, 6), b"Name\0\xEE", "a name alone");
    s.kernel
        .os()
        .write_listed(DATA, Layout::Info, &TEXT, 7, b"Name")
        .expect("written");
    let record: Vec<u32> = (0..5).map(|i| s.peek(DATA + 4 * i)).collect();
    assert_eq!(record, [0xFFFF_FF45, 0x6789_0ABC, 0x123, 0x03, kind::FILE]);
    assert_eq!(s.read(DATA + 20, 5), b"Name\0");
    assert_eq!(s.kernel.os().read_listed(DATA).expect("read"), TEXT);
    s.kernel
        .os()
        .write_listed(DATA, Layout::FullInfo, &TEXT, 7, b"Name")
        .expect("written");
    assert_eq!(s.peek(DATA + 20), 7, "its internal name");
    assert_eq!(s.read(DATA + 24, 5), [0xBC, 0x0A, 0x89, 0x67, 0x45]);
    assert_eq!(s.read(DATA + 29, 5), b"Name\0");
    assert_eq!(s.kernel.os().read_listed(DATA).expect("read"), TEXT);
}

#[test]
fn information_answers_in_r0_and_r2_to_r5_and_is_read_back_from_them() {
    let mut regs = Regs::default();
    regs.r[1] = 0x1234;
    TEXT.answer(&mut regs);
    assert_eq!(
        regs.r[..6],
        [kind::FILE, 0x1234, 0xFFFF_FF45, 0x6789_0ABC, 0x123, 0x03]
    );
    assert_eq!(Info::answered(&regs), TEXT);
}

#[test]
fn a_stamp_is_the_execution_address_beneath_the_load_address_s_low_byte() {
    assert_eq!(stamp::of(0xFFFF_FD45, 0x6789_0ABC), 0x45_6789_0ABC);
    assert_eq!(TEXT.stamp(), 0x45_6789_0ABC);
    assert!(stamp::of(0xFFFF_FD46, 0) > stamp::of(0xFFFF_FD45, u32::MAX));
    assert!(stamp::of(0xFFFF_FD45, 0x6789_0ABD) > stamp::of(0xFFFF_FD45, 0x6789_0ABC));
}
