//! CMOS: the defaults and checksum, and the SWIs that read and write it.

use super::*;

#[test]
fn a_reset_leaves_the_defaults_and_a_good_checksum() {
    let mut cmos = Cmos::new(Locale {
        time_zone: -20,
        dst: true,
    });
    assert_eq!(cmos.read(FILE_LANG), HOSTFS);
    assert_eq!(
        cmos.read(TIME_ZONE),
        (-20i8).cast_unsigned(),
        "the locale's time zone"
    );
    assert_eq!(cmos.read(ALARM_AND_TIME) & DST, DST, "and its summer time");
    let winter = Cmos::new(Locale {
        time_zone: 0,
        dst: false,
    });
    assert_eq!(winter.read(ALARM_AND_TIME) & DST, 0);
    assert_eq!(cmos.read(KEY_DELAY), 32);
    assert_eq!(cmos.read(0xCA), 0, "an absent module's setting is zero");
    assert_eq!(cmos.read(CHECKSUM), cmos.checksum());
    assert!(cmos.write(KEY_REPEAT, 9));
    assert_eq!(cmos.read(CHECKSUM), cmos.checksum(), "kept by a write");
    assert!(!cmos.write(STATION, 5), "the station is protected");
    assert!(cmos.write(0xF4, 5), "past the checksum, as any other");
    assert_eq!(
        cmos.read(CHECKSUM),
        cmos.checksum(),
        "which it does not cover"
    );
    assert_eq!(cmos.reset_value(0x0C), Some(32));
    assert_eq!(cmos.reset_value(0x07), Some(0));
    assert_eq!(
        cmos.reset_value(u32::from(TIME_ZONE)),
        Some((-20i8).cast_unsigned())
    );
    assert_eq!(cmos.reset_value(0), None);
    assert_eq!(cmos.reset_value(0xEF), None);
}

#[test]
fn nv_memory_reads_reset_values_and_takes_no_passphrase() {
    use crate::memory::layout::RISC_OS_5;
    use crate::memory::Bits;
    use crate::testkit::{boot, DATA};
    const NV_MEMORY: u32 = 0x2_0076;
    let mut s = boot(&RISC_OS_5);
    s.application(Bits::Bit32, 0x1_0000);
    let mut r = [0; 13];
    r[..2].copy_from_slice(&[6, u32::from(KEY_DELAY)]);
    assert_eq!(s.user_swi(NV_MEMORY, r).0.r[2], 32);
    r[1] = u32::from(STATION);
    assert_eq!(s.user_swi(NV_MEMORY, r).0.r[2], u32::MAX, "kept by a reset");
    s.poke(DATA, &[u32::from(b'x')]);
    r[..3].copy_from_slice(&[7, DATA, 5]);
    let (regs, _) = s.user_swi(NV_MEMORY, r);
    assert!(regs.flag(tairix_riscos_arm::psr::V));
    assert_eq!(s.peek(regs.r[0]), 0x411);
    assert_eq!(s.kernel.os().world.cmos.read(STATION), 0);
}
