//! Genuine's `time.h` answers as the probes recorded them on RISC OS 5.30.

/// A zone in quarter hours, whether daylight saving is configured, then
/// each time and its `struct tm`.
pub(super) type Configured = (i8, bool, [(u32, [i32; 9]); 8]);
/// A format, the `struct tm`, the answer, the text.
pub(super) type Formatted = (&'static [u8], &'static [i32; 9], u32, &'static [u8]);

/// Genuine's `gmtime` of each time: the time, then its `struct tm`.
pub(super) const GMTIME: [(u32, [i32; 9]); 16] = [
    (0x0000_0000, [0, 0, 0, 1, 0, 70, 4, 0, 0]),
    (0x0001_517F, [59, 59, 23, 1, 0, 70, 4, 0, 0]),
    (0x38BB_0C00, [0, 0, 0, 29, 1, 100, 2, 59, 0]),
    (0xFFFF_FFFF, [0, 0, 0, 1, 0, 0, 0, 0, 0]),
    (0x7FFF_FFFF, [7, 14, 3, 19, 0, 138, 2, 18, 0]),
    (0x3B9A_CA00, [40, 46, 1, 9, 8, 101, 0, 251, 0]),
    (0x3956_F480, [20, 13, 6, 26, 5, 100, 1, 177, 0]),
    (0x0000_0001, [1, 0, 0, 1, 0, 70, 4, 0, 0]),
    (0x0000_0000, [0, 0, 0, 1, 0, 70, 4, 0, 0]),
    (0x3956_F480, [20, 13, 6, 26, 5, 100, 1, 177, 0]),
    (0x386D_EC40, [0, 0, 12, 1, 0, 100, 6, 0, 0]),
    (0xFFFF_FFFE, [14, 28, 6, 6, 1, 206, 0, 36, 0]),
    (0xFFFE_AE80, [16, 28, 6, 5, 1, 206, 6, 35, 0]),
    (0x8000_0000, [8, 14, 3, 19, 0, 138, 2, 18, 0]),
    (0xFFFF_FF00, [0, 24, 6, 6, 1, 206, 0, 36, 0]),
    (0xEE6B_2800, [40, 6, 7, 2, 9, 196, 2, 275, 0]),
];

/// Genuine's `localtime` in each configuration: the zone in quarter hours,
/// whether daylight saving is configured, then each time and its
/// `struct tm`.
pub(super) const LOCALTIME: [Configured; 4] = [
    (
        0,
        false,
        [
            (0x0000_0000, [0, 0, 0, 1, 0, 70, 4, 0, 0]),
            (0x3956_F480, [20, 13, 6, 26, 5, 100, 1, 177, 0]),
            (0x386D_EC40, [0, 0, 12, 1, 0, 100, 6, 0, 0]),
            (0xFFFF_FFFE, [14, 28, 6, 6, 1, 206, 0, 36, 0]),
            (0xFFFE_AE80, [16, 28, 6, 5, 1, 206, 6, 35, 0]),
            (0x8000_0000, [8, 14, 3, 19, 0, 138, 2, 18, 0]),
            (0xFFFF_FF00, [0, 24, 6, 6, 1, 206, 0, 36, 0]),
            (0xEE6B_2800, [40, 6, 7, 2, 9, 196, 2, 275, 0]),
        ],
    ),
    (
        0,
        true,
        [
            (0x0000_0000, [0, 0, 1, 1, 0, 70, 4, 0, 32768]),
            (0x3956_F480, [20, 13, 7, 26, 5, 100, 1, 177, 32768]),
            (0x386D_EC40, [0, 0, 13, 1, 0, 100, 6, 0, 32768]),
            (0xFFFF_FFFE, [58, 59, 0, 1, 0, 70, 4, 0, 32768]),
            (0xFFFE_AE80, [16, 28, 7, 5, 1, 206, 6, 35, 32768]),
            (0x8000_0000, [8, 14, 4, 19, 0, 138, 2, 18, 32768]),
            (0xFFFF_FF00, [44, 55, 0, 1, 0, 70, 4, 0, 32768]),
            (0xEE6B_2800, [40, 6, 8, 2, 9, 196, 2, 275, 32768]),
        ],
    ),
    (
        20,
        false,
        [
            (0x0000_0000, [0, 0, 5, 1, 0, 70, 4, 0, 0]),
            (0x3956_F480, [20, 13, 11, 26, 5, 100, 1, 177, 0]),
            (0x386D_EC40, [0, 0, 17, 1, 0, 100, 6, 0, 0]),
            (0xFFFF_FFFE, [58, 59, 4, 1, 0, 70, 4, 0, 0]),
            (0xFFFE_AE80, [16, 28, 11, 5, 1, 206, 6, 35, 0]),
            (0x8000_0000, [8, 14, 8, 19, 0, 138, 2, 18, 0]),
            (0xFFFF_FF00, [44, 55, 4, 1, 0, 70, 4, 0, 0]),
            (0xEE6B_2800, [40, 6, 12, 2, 9, 196, 2, 275, 0]),
        ],
    ),
    (
        -12,
        true,
        [
            (0x0000_0000, [16, 28, 4, 6, 1, 206, 0, 36, 32768]),
            (0x3956_F480, [20, 13, 4, 26, 5, 100, 1, 177, 32768]),
            (0x386D_EC40, [0, 0, 10, 1, 0, 100, 6, 0, 32768]),
            (0xFFFF_FFFE, [14, 28, 4, 6, 1, 206, 0, 36, 32768]),
            (0xFFFE_AE80, [16, 28, 4, 5, 1, 206, 6, 35, 32768]),
            (0x8000_0000, [8, 14, 1, 19, 0, 138, 2, 18, 32768]),
            (0xFFFF_FF00, [0, 24, 4, 6, 1, 206, 0, 36, 32768]),
            (0xEE6B_2800, [40, 6, 5, 2, 9, 196, 2, 275, 32768]),
        ],
    ),
];

/// Genuine's `ctime` of each time, the zone UTC.
pub(super) const CTIME: [(u32, &[u8]); 8] = [
    (0x0000_0000, b"Thu Jan  1 00:00:00 1970\n"),
    (0x0001_517F, b"Thu Jan  1 23:59:59 1970\n"),
    (0x38BB_0C00, b"Tue Feb 29 00:00:00 2000\n"),
    (0xFFFF_FFFF, b"Sun Jan  1 00:00:00 1900\n"),
    (0x7FFF_FFFF, b"Tue Jan 19 03:14:07 2038\n"),
    (0x3B9A_CA00, b"Sun Sep  9 01:46:40 2001\n"),
    (0x3956_F480, b"Mon Jun 26 06:13:20 2000\n"),
    (0x0000_0001, b"Thu Jan  1 00:00:01 1970\n"),
];

/// A `struct tm` and genuine's `asctime` of it.
pub(super) const ASCTIME: ([i32; 9], &[u8]) = (
    [52, 3, 1, 16, 8, 73, 0, 258, 0],
    b"Sun Sep 16 01:03:52 1973\n",
);

/// Genuine's `mktime`: the `struct tm` given, the answer, and the
/// `struct tm` left.
pub(super) const MKTIME: [([i32; 9], u32, [i32; 9]); 7] = [
    (
        [0, 0, 12, 1, 0, 100, 0, 0, -1],
        0x386D_EC40,
        [0, 0, 12, 1, 0, 100, 6, 0, -1],
    ),
    (
        [75, 70, 25, 0, 13, 99, 0, 0, 0],
        0x3896_40C3,
        [15, 11, 2, 1, 1, 100, 2, 31, 0],
    ),
    (
        [0, 0, 12, 1, 6, 100, 0, 0, -1],
        0x395D_DD40,
        [0, 0, 12, 1, 6, 100, 6, 182, -1],
    ),
    (
        [0, 0, 0, 1, 0, 70, 0, 0, 0],
        0x0000_0000,
        [0, 0, 0, 1, 0, 70, 4, 0, 0],
    ),
    (
        [0, 0, 12, 1, 6, 100, 0, 0, -1],
        0x395D_DD40,
        [0, 0, 12, 1, 6, 100, 6, 182, -1],
    ),
    (
        [0, 0, 12, 1, 6, 100, 0, 0, 0],
        0x395D_DD40,
        [0, 0, 12, 1, 6, 100, 6, 182, 0],
    ),
    (
        [0, 0, 12, 1, 6, 100, 0, 0, 1],
        0x395D_CF30,
        [0, 0, 12, 1, 6, 100, 6, 182, 1],
    ),
];

/// The `struct tm`s genuine's `strftime` was recorded with.
pub(super) const TM_A: [i32; 9] = [9, 5, 13, 29, 1, 100, 2, 59, 0];
pub(super) const TM_B: [i32; 9] = [0, 0, 0, 1, 0, 70, 4, 0, 32768];

/// Genuine's `strftime` into a 64-byte buffer, in the C locale and then
/// the UK's: the format, the `struct tm`, the answer, the text.
pub(super) const STRFTIME_C: [Formatted; 52] = [
    (b"%a", &TM_A, 3, b"Tue"),
    (b"%A", &TM_A, 7, b"Tuesday"),
    (b"%b", &TM_A, 3, b"Feb"),
    (b"%B", &TM_A, 8, b"February"),
    (b"%c", &TM_A, 24, b"Tue Feb 29 13:05:09 2000"),
    (b"%d", &TM_A, 2, b"29"),
    (b"%H", &TM_A, 2, b"13"),
    (b"%I", &TM_A, 2, b"01"),
    (b"%j", &TM_A, 3, b"060"),
    (b"%m", &TM_A, 2, b"02"),
    (b"%M", &TM_A, 2, b"05"),
    (b"%p", &TM_A, 2, b"PM"),
    (b"%S", &TM_A, 2, b"09"),
    (b"%U", &TM_A, 2, b"09"),
    (b"%w", &TM_A, 1, b"2"),
    (b"%W", &TM_A, 2, b"09"),
    (b"%x", &TM_A, 8, b"02/29/00"),
    (b"%X", &TM_A, 8, b"13:05:09"),
    (b"%y", &TM_A, 2, b"00"),
    (b"%Y", &TM_A, 4, b"2000"),
    (b"%Z", &TM_A, 0, b""),
    (b"%%", &TM_A, 1, b"%"),
    (b"%q", &TM_A, 2, b"%q"),
    (b"x%dy", &TM_A, 4, b"x29y"),
    (b"%E", &TM_A, 1, b"%"),
    (b"", &TM_A, 0, b""),
    (b"%a", &TM_B, 3, b"Thu"),
    (b"%A", &TM_B, 8, b"Thursday"),
    (b"%b", &TM_B, 3, b"Jan"),
    (b"%B", &TM_B, 7, b"January"),
    (b"%c", &TM_B, 24, b"Thu Jan  1 00:00:00 1970"),
    (b"%d", &TM_B, 2, b"01"),
    (b"%H", &TM_B, 2, b"00"),
    (b"%I", &TM_B, 2, b"12"),
    (b"%j", &TM_B, 3, b"001"),
    (b"%m", &TM_B, 2, b"01"),
    (b"%M", &TM_B, 2, b"00"),
    (b"%p", &TM_B, 2, b"AM"),
    (b"%S", &TM_B, 2, b"00"),
    (b"%U", &TM_B, 2, b"00"),
    (b"%w", &TM_B, 1, b"4"),
    (b"%W", &TM_B, 2, b"00"),
    (b"%x", &TM_B, 8, b"01/01/70"),
    (b"%X", &TM_B, 8, b"00:00:00"),
    (b"%y", &TM_B, 2, b"70"),
    (b"%Y", &TM_B, 4, b"1970"),
    (b"%Z", &TM_B, 0, b""),
    (b"%%", &TM_B, 1, b"%"),
    (b"%q", &TM_B, 2, b"%q"),
    (b"x%dy", &TM_B, 4, b"x01y"),
    (b"%E", &TM_B, 1, b"%"),
    (b"", &TM_B, 0, b""),
];
pub(super) const STRFTIME_UK: [Formatted; 26] = [
    (b"%a", &TM_A, 3, b"Tue"),
    (b"%A", &TM_A, 7, b"Tuesday"),
    (b"%b", &TM_A, 3, b"Feb"),
    (b"%B", &TM_A, 8, b"February"),
    (b"%c", &TM_A, 20, b"13:05:09 29-Feb-2000"),
    (b"%d", &TM_A, 2, b"29"),
    (b"%H", &TM_A, 2, b"13"),
    (b"%I", &TM_A, 2, b"01"),
    (b"%j", &TM_A, 3, b"060"),
    (b"%m", &TM_A, 2, b"02"),
    (b"%M", &TM_A, 2, b"05"),
    (b"%p", &TM_A, 2, b"pm"),
    (b"%S", &TM_A, 2, b"09"),
    (b"%U", &TM_A, 2, b"09"),
    (b"%w", &TM_A, 1, b"2"),
    (b"%W", &TM_A, 2, b"09"),
    (b"%x", &TM_A, 11, b"29-Feb-2000"),
    (b"%X", &TM_A, 8, b"13:05:09"),
    (b"%y", &TM_A, 2, b"00"),
    (b"%Y", &TM_A, 4, b"2000"),
    (b"%Z", &TM_A, 3, b"GMT"),
    (b"%%", &TM_A, 1, b"%"),
    (b"%q", &TM_A, 2, b"%q"),
    (b"x%dy", &TM_A, 4, b"x29y"),
    (b"%E", &TM_A, 1, b"%"),
    (b"", &TM_A, 0, b""),
];

/// Genuine's `strftime("%A")` of `TM_A` into a smaller buffer: its size,
/// the answer, and the buffer's bytes to one past its size.
pub(super) const STRFTIME_SHORT: [(u32, u32, &[u8]); 2] =
    [(5, 0, b"Tuesd\0"), (8, 7, b"Tuesday\0\0")];
