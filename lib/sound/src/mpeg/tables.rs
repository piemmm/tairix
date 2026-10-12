//! The standard's tables: bit rates and sample rates, Layer II's
//! allocation, the synthesis window, and Layer III's scalefactor bands
//! (ISO/IEC 11172-3 Annex B, ISO/IEC 13818-3). MPEG-2.5 is the 13818-3
//! framework at half its rates, and borrows its bands.

/// Kbit/s by layer for MPEG-1, at bitrate indices 1 to 14; index 0 is free
/// format and 15 is forbidden.
pub(super) const MPEG1_BITRATES: [[u16; 15]; 3] = [
    [
        0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
    ],
    [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
    ],
    [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ],
];

/// Kbit/s at the lower sample rates: Layer I, then Layers II and III.
pub(super) const LSF_BITRATES: [[u16; 15]; 2] = [
    [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256,
    ],
    [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
];

/// Sample rates by version — MPEG-1, MPEG-2, MPEG-2.5 — and rate index.
pub(super) const SAMPLE_RATES: [[u32; 3]; 3] = [
    [44_100, 48_000, 32_000],
    [22_050, 24_000, 16_000],
    [11_025, 12_000, 8_000],
];

/// How one Layer II allocation quantises: levels, bits a code takes, and
/// whether three samples share one code.
#[derive(Copy, Clone)]
pub(super) struct QuantClass {
    pub(super) steps: u32,
    pub(super) bits: u8,
    pub(super) grouped: bool,
}

const fn class(steps: u32, bits: u8, grouped: bool) -> QuantClass {
    QuantClass {
        steps,
        bits,
        grouped,
    }
}

/// Layer II's quantisation classes (Table 3-B.4); a grouped class codes
/// three samples in `bits` bits.
pub(super) const QUANT_CLASSES: [QuantClass; 17] = [
    class(3, 5, true),
    class(5, 7, true),
    class(7, 3, false),
    class(9, 10, true),
    class(15, 4, false),
    class(31, 5, false),
    class(63, 6, false),
    class(127, 7, false),
    class(255, 8, false),
    class(511, 9, false),
    class(1023, 10, false),
    class(2047, 11, false),
    class(4095, 12, false),
    class(8191, 13, false),
    class(16383, 14, false),
    class(32767, 15, false),
    class(65535, 16, false),
];

/// One subband's allocation: bits of its allocation field, and the class
/// each non-zero allocation selects.
pub(super) struct AllocRow {
    pub(super) nbal: u8,
    pub(super) classes: &'static [u8],
}

const ROWS: [AllocRow; 8] = [
    AllocRow {
        nbal: 4,
        classes: &[0, 2, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
    },
    AllocRow {
        nbal: 4,
        classes: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 16],
    },
    AllocRow {
        nbal: 3,
        classes: &[0, 1, 2, 3, 4, 5, 16],
    },
    AllocRow {
        nbal: 2,
        classes: &[0, 1, 16],
    },
    AllocRow {
        nbal: 4,
        classes: &[0, 1, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    },
    AllocRow {
        nbal: 3,
        classes: &[0, 1, 3, 4, 5, 6, 7],
    },
    AllocRow {
        nbal: 4,
        classes: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14],
    },
    AllocRow {
        nbal: 2,
        classes: &[0, 1, 3],
    },
];

/// Layer II's allocation tables (Tables 3-B.2a to 3-B.2d, and 13818-3's for
/// the lower rates), each a row per subband up to its limit.
pub(super) const ALLOC_TABLES: [&[u8]; 5] = [
    &[
        0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 3, 3, 3,
    ],
    &[
        0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 3, 3, 3, 3, 3, 3,
    ],
    &[4, 4, 5, 5, 5, 5, 5, 5],
    &[4, 4, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5],
    &[
        6, 6, 6, 6, 5, 5, 5, 5, 5, 5, 5, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
    ],
];

/// The allocation row for subband `subband` of table `table`.
pub(super) const fn alloc_row(table: usize, subband: usize) -> &'static AllocRow {
    &ROWS[ALLOC_TABLES[table][subband] as usize]
}

/// The synthesis window D (Table 3-B.3), in steps of 2^-16: every
/// coefficient the standard prints is an exact multiple of one.
pub(super) const WINDOW_STEPS: [i32; 512] = [
    0, -1, -1, -1, -1, -1, -1, -2, -2, -2, -2, -3, -3, -4, -4, -5, -5, -6, -7, -7, -8, -9, -10,
    -11, -13, -14, -16, -17, -19, -21, -24, -26, -29, -31, -35, -38, -41, -45, -49, -53, -58, -63,
    -68, -73, -79, -85, -91, -97, -104, -111, -117, -125, -132, -139, -147, -154, -161, -169, -176,
    -183, -190, -196, -202, -208, 213, 218, 222, 225, 227, 228, 228, 227, 224, 221, 215, 208, 200,
    189, 177, 163, 146, 127, 106, 83, 57, 29, -2, -36, -72, -111, -153, -197, -244, -294, -347,
    -401, -459, -519, -581, -645, -711, -779, -848, -919, -991, -1064, -1137, -1210, -1283, -1356,
    -1428, -1498, -1567, -1634, -1698, -1759, -1817, -1870, -1919, -1962, -2001, -2032, -2057,
    -2075, -2085, -2087, -2080, -2063, 2037, 2000, 1952, 1893, 1822, 1739, 1644, 1535, 1414, 1280,
    1131, 970, 794, 605, 402, 185, -45, -288, -545, -814, -1095, -1388, -1692, -2006, -2330, -2663,
    -3004, -3351, -3705, -4063, -4425, -4788, -5153, -5517, -5879, -6237, -6589, -6935, -7271,
    -7597, -7910, -8209, -8491, -8755, -8998, -9219, -9416, -9585, -9727, -9838, -9916, -9959,
    -9966, -9935, -9863, -9750, -9592, -9389, -9139, -8840, -8492, -8092, -7640, -7134, 6574, 5959,
    5288, 4561, 3776, 2935, 2037, 1082, 70, -998, -2122, -3300, -4533, -5818, -7154, -8540, -9975,
    -11455, -12980, -14548, -16155, -17799, -19478, -21189, -22929, -24694, -26482, -28289, -30112,
    -31947, -33791, -35640, -37489, -39336, -41176, -43006, -44821, -46617, -48390, -50137, -51853,
    -53534, -55178, -56778, -58333, -59838, -61289, -62684, -64019, -65290, -66494, -67629, -68692,
    -69679, -70590, -71420, -72169, -72835, -73415, -73908, -74313, -74630, -74856, -74992, 75038,
    74992, 74856, 74630, 74313, 73908, 73415, 72835, 72169, 71420, 70590, 69679, 68692, 67629,
    66494, 65290, 64019, 62684, 61289, 59838, 58333, 56778, 55178, 53534, 51853, 50137, 48390,
    46617, 44821, 43006, 41176, 39336, 37489, 35640, 33791, 31947, 30112, 28289, 26482, 24694,
    22929, 21189, 19478, 17799, 16155, 14548, 12980, 11455, 9975, 8540, 7154, 5818, 4533, 3300,
    2122, 998, -70, -1082, -2037, -2935, -3776, -4561, -5288, -5959, 6574, 7134, 7640, 8092, 8492,
    8840, 9139, 9389, 9592, 9750, 9863, 9935, 9966, 9959, 9916, 9838, 9727, 9585, 9416, 9219, 8998,
    8755, 8491, 8209, 7910, 7597, 7271, 6935, 6589, 6237, 5879, 5517, 5153, 4788, 4425, 4063, 3705,
    3351, 3004, 2663, 2330, 2006, 1692, 1388, 1095, 814, 545, 288, 45, -185, -402, -605, -794,
    -970, -1131, -1280, -1414, -1535, -1644, -1739, -1822, -1893, -1952, -2000, 2037, 2063, 2080,
    2087, 2085, 2075, 2057, 2032, 2001, 1962, 1919, 1870, 1817, 1759, 1698, 1634, 1567, 1498, 1428,
    1356, 1283, 1210, 1137, 1064, 991, 919, 848, 779, 711, 645, 581, 519, 459, 401, 347, 294, 244,
    197, 153, 111, 72, 36, 2, -29, -57, -83, -106, -127, -146, -163, -177, -189, -200, -208, -215,
    -221, -224, -227, -228, -228, -227, -225, -222, -218, 213, 208, 202, 196, 190, 183, 176, 169,
    161, 154, 147, 139, 132, 125, 117, 111, 104, 97, 91, 85, 79, 73, 68, 63, 58, 53, 49, 45, 41,
    38, 35, 31, 29, 26, 24, 21, 19, 17, 16, 14, 13, 11, 10, 9, 8, 7, 7, 6, 5, 5, 4, 4, 3, 3, 2, 2,
    2, 2, 1, 1, 1, 1, 1, 1,
];

/// Layer III's bands at one sample rate, by width: 22 long, 13 short (each
/// for one window), and a mixed block's — its long bands, then its short
/// ones.
pub(super) struct Bands {
    pub(super) long: [u16; 22],
    pub(super) short: [u16; 13],
    /// Long bands a mixed block keeps below its short ones.
    pub(super) mixed_long: u8,
}

/// Short bands a mixed block's short part starts at.
pub(super) const MIXED_SHORT_START: usize = 3;

const fn bands(long: [u16; 22], short: [u16; 13], mixed_long: u8) -> Bands {
    Bands {
        long,
        short,
        mixed_long,
    }
}

/// Bands by sample rate, in [`SAMPLE_RATES`] order. 11.025 and 12 kHz take
/// 22.05 kHz's long bands and 16 kHz's short ones.
pub(super) const BANDS: [[Bands; 3]; 3] = [
    [
        bands(
            [
                4, 4, 4, 4, 4, 4, 6, 6, 8, 8, 10, 12, 16, 20, 24, 28, 34, 42, 50, 54, 76, 158,
            ],
            [4, 4, 4, 4, 6, 8, 10, 12, 14, 18, 22, 30, 56],
            8,
        ),
        bands(
            [
                4, 4, 4, 4, 4, 4, 6, 6, 6, 8, 10, 12, 16, 18, 22, 28, 34, 40, 46, 54, 54, 192,
            ],
            [4, 4, 4, 4, 6, 6, 10, 12, 14, 16, 20, 26, 66],
            8,
        ),
        bands(
            [
                4, 4, 4, 4, 4, 4, 6, 6, 8, 10, 12, 16, 20, 24, 30, 38, 46, 56, 68, 84, 102, 26,
            ],
            [4, 4, 4, 4, 6, 8, 12, 16, 20, 26, 34, 42, 12],
            8,
        ),
    ],
    [
        bands(
            [
                6, 6, 6, 6, 6, 6, 8, 10, 12, 14, 16, 20, 24, 28, 32, 38, 46, 52, 60, 68, 58, 54,
            ],
            [4, 4, 4, 6, 6, 8, 10, 14, 18, 26, 32, 42, 18],
            6,
        ),
        bands(
            [
                6, 6, 6, 6, 6, 6, 8, 10, 12, 14, 16, 18, 22, 26, 32, 38, 46, 54, 62, 70, 76, 36,
            ],
            [4, 4, 4, 6, 8, 10, 12, 14, 18, 24, 32, 44, 12],
            6,
        ),
        bands(
            [
                6, 6, 6, 6, 6, 6, 8, 10, 12, 14, 16, 20, 24, 28, 32, 38, 46, 52, 60, 68, 58, 54,
            ],
            [4, 4, 4, 6, 8, 10, 12, 14, 18, 24, 30, 40, 18],
            6,
        ),
    ],
    [
        bands(
            [
                6, 6, 6, 6, 6, 6, 8, 10, 12, 14, 16, 20, 24, 28, 32, 38, 46, 52, 60, 68, 58, 54,
            ],
            [4, 4, 4, 6, 8, 10, 12, 14, 18, 24, 30, 40, 18],
            6,
        ),
        bands(
            [
                6, 6, 6, 6, 6, 6, 8, 10, 12, 14, 16, 20, 24, 28, 32, 38, 46, 52, 60, 68, 58, 54,
            ],
            [4, 4, 4, 6, 8, 10, 12, 14, 18, 24, 30, 40, 18],
            6,
        ),
        bands(
            [
                12, 12, 12, 12, 12, 12, 16, 20, 24, 28, 32, 40, 48, 56, 64, 76, 90, 2, 2, 2, 2, 2,
            ],
            [8, 8, 8, 12, 16, 20, 24, 28, 36, 2, 2, 2, 26],
            6,
        ),
    ],
];

/// Layer III's pre-emphasis, added to the long scalefactors when a granule
/// asks for it.
pub(super) const PRETAB: [u8; 22] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 3, 2, 0,
];

/// MPEG-1's scalefactor widths by `scalefac_compress`: the first eleven
/// long bands' (or the first six short), then the rest.
pub(super) const SLEN: [[u8; 16]; 2] = [
    [0, 0, 0, 0, 3, 1, 1, 1, 2, 2, 2, 3, 3, 3, 4, 4],
    [0, 1, 2, 3, 0, 1, 2, 3, 1, 2, 3, 1, 2, 3, 2, 3],
];

/// The lower rates' scalefactor partitions (13818-3 `nr_of_sfb_block`): by
/// the partition `scalefac_compress` picks, then long, short and mixed, the
/// scalefactors in each of four parts.
pub(super) const LSF_PARTITIONS: [[[u8; 4]; 3]; 6] = [
    [[6, 5, 5, 5], [9, 9, 9, 9], [6, 9, 9, 9]],
    [[6, 5, 7, 3], [9, 9, 12, 6], [6, 9, 12, 6]],
    [[11, 10, 0, 0], [18, 18, 0, 0], [15, 18, 0, 0]],
    [[7, 7, 7, 0], [12, 12, 12, 0], [6, 15, 12, 0]],
    [[6, 6, 6, 3], [12, 9, 9, 6], [6, 12, 9, 6]],
    [[8, 8, 5, 0], [15, 12, 9, 0], [6, 18, 9, 0]],
];
