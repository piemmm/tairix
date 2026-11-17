//! The RISC OS release whose interface the emulator implements
//! (`plans/RISCOS-EMULATOR.md` decision 2): its version answers are RISC OS
//! 5's, and its descriptive strings name the emulator, so the guest is told
//! where it is.

use tairix_inline::ArrayVec;

/// The release.
pub const RELEASE: &[u8] = b"5.30";
/// `OS_ReadSysInfo` 9's operating system name and description.
pub const OS_NAME: &[u8] = b"RISC OS 5.30";
/// `OS_ReadSysInfo` 9's platform name.
pub const PLATFORM: &[u8] = b"TAIRiX";
/// The five-byte time the ROM was made: its files' stamp, 24 Apr 2024 at
/// 16:29:06.62, as genuine RISC OS 5.30's are stamped.
pub const ROM_STAMP: u64 = 0x5B_56B1_7646;
/// That time as `OS_ReadSysInfo` 9 gives the build date, in the PRM's
/// standard date format.
pub const BUILD_DATE: &[u8] = b"Wed,24 Apr 2024.16:29:06";
/// That time's year, month and day.
pub const DATE: (i64, u32, u32) = (2024, 4, 24);

/// The months' short names, as a date in a message names them.
pub const MONTHS: [&[u8; 3]; 12] = [
    b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov", b"Dec",
];

/// The release's date as a message gives it: `24 Apr 2024`.
#[must_use]
pub fn date_text() -> [u8; 11] {
    let (year, month, day) = DATE;
    let month = MONTHS[usize::try_from(month).unwrap_or(1).clamp(1, 12) - 1];
    let digit = |n: i64, place: i64| b'0' + u8::try_from(n / place % 10).unwrap_or(0);
    let day = i64::from(day);
    [
        digit(day, 10),
        digit(day, 1),
        b' ',
        month[0],
        month[1],
        month[2],
        b' ',
        digit(year, 1000),
        digit(year, 100),
        digit(year, 10),
        digit(year, 1),
    ]
}

/// `OS_Byte` 0's message: the release and the date it was made.
#[must_use]
pub fn os_byte_0() -> ArrayVec<u8, 32> {
    let mut text = ArrayVec::new();
    for part in [&b"RISC OS "[..], RELEASE, b" (", &date_text(), b")"] {
        // The parts are fixed, and fit.
        let _ = text.try_extend_from_slice(part);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dates_are_the_rom_stamp_s() {
        let days = tairix_abi::time::days_from_civil(DATE.0, DATE.1, DATE.2)
            .and_then(|days| u64::try_from(days).ok())
            .expect("a day after 1970");
        let midnight = crate::clock::UNIX_EPOCH + days * crate::clock::CENTISECONDS_PER_DAY;
        let into_day = ROM_STAMP - midnight;
        assert!(
            into_day < crate::clock::CENTISECONDS_PER_DAY,
            "the stamp's own day"
        );
        let seconds = into_day / 100;
        let time = [seconds / 3600, seconds / 60 % 60, seconds % 60];
        assert_eq!(time, [16, 29, 6]);
        assert_eq!(&BUILD_DATE[4..15], &date_text());
        assert_eq!(&BUILD_DATE[16..], b"16:29:06");
    }
}
