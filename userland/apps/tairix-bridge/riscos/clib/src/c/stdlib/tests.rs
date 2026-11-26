use ::alloc::vec::Vec;

use super::*;
use crate::c::statics::at;
use crate::c::testing::{Client, CODE, FREE, STATICS};

const S: u32 = FREE;
const END: u32 = FREE + 0x80;
const ARRAY: u32 = FREE + 0x100;
const LOG: u32 = FREE + 0x2000;
const CURSOR: u32 = FREE + 0x1FFC;

/// A comparison logging the pair it is given, then the difference of the
/// first words: `LDR R12, cursor; LDR R2, [R12]; STMIA R2!, {R0, R1};
/// STR R2, [R12]; LDR R2, [R0]; LDR R3, [R1]; SUB R0, R2, R3; MOV PC, R14`.
const COMPARE: [u32; 9] = [
    0xE59F_C018,
    0xE59C_2000,
    0xE8A2_0003,
    0xE58C_2000,
    0xE590_2000,
    0xE591_3000,
    0xE042_0003,
    0xE1A0_F00E,
    CURSOR,
];

/// Genuine's recorded sorts: the keys, the tags in the order it left them,
/// and how many comparisons it made.
const SORTED: [(&[u32], &[u32], usize); 35] = [
    (&[3, 1, 2], &[1, 2, 0], 3),
    (&[5, 4, 3, 2, 1], &[4, 3, 2, 1, 0], 10),
    (&[7, 7, 7, 7, 7], &[0, 1, 2, 3, 4], 4),
    (&[3, 1, 4, 1, 5, 9, 2, 6], &[1, 3, 6, 0, 2, 4, 7, 5], 14),
    (
        &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
        &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
        9,
    ),
    (
        &[3, 2, 1, 1, 3, 4, 5, 2, 4, 2, 3, 5],
        &[2, 3, 1, 7, 9, 0, 4, 10, 5, 8, 6, 11],
        37,
    ),
    (
        &[
            97, 73, 1, 99, 31, 84, 2, 56, 19, 19, 92, 40, 21, 32, 83, 88, 79, 74, 7, 15,
        ],
        &[
            2, 6, 18, 19, 9, 8, 12, 4, 13, 11, 7, 1, 17, 16, 14, 5, 15, 10, 0, 3,
        ],
        83,
    ),
    (&[9], &[0], 0),
    (&[], &[], 0),
    (&[2, 1], &[1, 0], 1),
    (
        &[
            0, 3, 2, 1, 0, 2, 3, 1, 0, 0, 0, 1, 1, 0, 2, 2, 1, 0, 1, 3, 0, 2, 3, 1, 0, 0, 3, 0, 3,
            3,
        ],
        &[
            0, 20, 13, 10, 8, 9, 27, 25, 24, 17, 4, 3, 12, 7, 23, 16, 11, 18, 21, 15, 2, 5, 14, 22,
            6, 19, 28, 26, 1, 29,
        ],
        115,
    ),
    (
        &[16, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1],
        &[15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0],
        44,
    ),
    (
        &[1, 2, 3, 4, 5, 6, 7, 8, 0],
        &[8, 0, 1, 2, 3, 4, 5, 6, 7],
        15,
    ),
    (
        &[1, 2, 1, 1, 3, 2, 2, 4, 2, 3, 3],
        &[0, 2, 3, 8, 6, 5, 1, 4, 9, 10, 7],
        28,
    ),
    (
        &[1, 1, 0, 3, 1, 3, 2, 1, 2, 4, 2],
        &[2, 0, 1, 4, 7, 8, 10, 6, 3, 5, 9],
        29,
    ),
    (
        &[2, 0, 4, 0, 3, 1, 1, 0, 2, 3, 3],
        &[1, 3, 7, 5, 6, 8, 0, 4, 9, 10, 2],
        29,
    ),
    (
        &[2, 0, 0, 0, 1, 3, 0, 2, 2, 1, 1],
        &[1, 2, 3, 6, 10, 4, 9, 8, 0, 7, 5],
        28,
    ),
    (
        &[3, 4, 4, 1, 3, 1, 2, 3, 1, 0, 2],
        &[9, 5, 8, 3, 6, 10, 7, 4, 0, 2, 1],
        32,
    ),
    (
        &[3, 0, 3, 3, 1, 2, 0, 1, 0, 3, 1],
        &[1, 8, 6, 10, 7, 4, 5, 3, 2, 9, 0],
        27,
    ),
    (
        &[1, 4, 2, 4, 0, 1, 3, 2, 1, 1, 2],
        &[4, 0, 8, 9, 5, 2, 7, 10, 6, 1, 3],
        28,
    ),
    (
        &[1, 0, 1, 3, 0, 2, 4, 3, 4, 0, 1],
        &[1, 9, 4, 0, 10, 2, 5, 7, 3, 6, 8],
        32,
    ),
    (
        &[2, 3, 2, 3, 3, 1, 0, 2, 4, 1, 1],
        &[6, 5, 9, 10, 2, 7, 0, 4, 1, 3, 8],
        31,
    ),
    (
        &[1, 3, 2, 4, 3, 2, 3, 0, 1, 4, 2],
        &[7, 0, 8, 5, 2, 10, 4, 6, 1, 9, 3],
        33,
    ),
    (
        &[2, 0, 1, 1, 3, 1, 0, 1, 4, 4, 2],
        &[1, 6, 5, 2, 3, 7, 0, 10, 4, 8, 9],
        29,
    ),
    (
        &[2, 3, 2, 0, 1, 2, 0, 3, 4, 1, 1],
        &[6, 3, 10, 9, 4, 0, 2, 5, 1, 7, 8],
        31,
    ),
    (
        &[1, 3, 2, 2, 2, 1, 1, 4, 1, 1, 1],
        &[0, 8, 6, 9, 5, 10, 3, 2, 4, 1, 7],
        30,
    ),
    (
        &[
            16, 20, 5, 13, 2, 1, 16, 19, 11, 6, 12, 9, 9, 4, 8, 12, 19, 7, 3, 8, 19, 18, 13, 9, 12,
            15, 7, 3, 16, 15, 19, 14, 7, 15, 5, 15, 9, 2, 7, 15, 15, 18, 12, 1, 19, 17, 0, 20, 6,
            13,
        ],
        &[
            46, 5, 43, 4, 37, 27, 18, 13, 2, 34, 9, 48, 32, 17, 38, 26, 19, 14, 12, 23, 11, 36, 8,
            10, 42, 15, 24, 49, 3, 22, 31, 39, 35, 40, 25, 29, 33, 6, 0, 28, 45, 21, 41, 44, 16,
            20, 30, 7, 1, 47,
        ],
        303,
    ),
    (
        &[
            822, 510, 330, 940, 375, 593, 176, 437, 888, 16, 227, 169, 893, 630, 526, 410, 152, 27,
            670, 604, 543, 384, 27, 239, 843, 869, 277, 636, 24, 570, 784, 538, 744, 252, 395, 743,
            190, 45, 539, 335, 600, 522, 706, 188, 501, 65, 581, 550, 738, 41, 194, 378, 184, 283,
            496, 89, 772, 674, 822, 957, 873, 876, 449, 17, 730, 317, 108, 311, 398, 825, 68, 557,
            855, 913, 326, 685, 449, 770, 735, 381, 524, 813, 183, 87, 73, 379, 398, 359, 164, 884,
            468, 177, 270, 162, 936, 91, 66, 863, 14, 322,
        ],
        &[
            98, 9, 63, 28, 22, 17, 49, 37, 45, 96, 70, 84, 83, 55, 95, 66, 16, 93, 88, 11, 6, 91,
            82, 52, 43, 36, 50, 10, 23, 33, 92, 26, 53, 67, 65, 99, 74, 2, 39, 87, 4, 51, 85, 79,
            21, 34, 68, 86, 15, 7, 76, 62, 90, 54, 44, 1, 41, 80, 14, 31, 38, 20, 47, 71, 29, 46,
            5, 40, 19, 13, 27, 18, 57, 75, 42, 64, 78, 48, 35, 32, 77, 56, 30, 81, 58, 0, 69, 24,
            72, 97, 25, 60, 61, 89, 8, 12, 73, 94, 3, 59,
        ],
        717,
    ),
    (
        &[
            2, 2, 2, 0, 1, 0, 3, 0, 0, 1, 2, 2, 2, 1, 3, 3, 3, 2, 2, 1, 1, 0, 2, 0, 1, 2, 2, 2, 2,
            2, 2, 2, 0, 0, 2, 2, 1, 3, 0, 2, 1, 2, 3, 3, 3, 1, 0, 1, 0, 2, 2, 0, 1, 3, 3, 0, 3, 0,
            0, 3,
        ],
        &[
            46, 38, 23, 21, 58, 48, 51, 8, 33, 57, 55, 5, 3, 7, 32, 36, 24, 40, 19, 20, 45, 4, 47,
            13, 9, 52, 34, 35, 39, 50, 49, 41, 0, 30, 31, 17, 2, 12, 18, 22, 11, 29, 25, 28, 1, 26,
            27, 10, 54, 53, 6, 56, 16, 15, 44, 43, 42, 37, 14, 59,
        ],
        296,
    ),
    (
        &[
            35, 36, 50, 20, 21, 44, 22, 38, 31, 37, 29, 4, 5, 17, 30, 44, 42, 4, 3, 46, 44, 19, 41,
            36, 43, 28, 18, 45, 24, 42, 22, 1, 29, 22, 10, 39, 7, 31, 3, 13, 49, 18, 8, 47, 15, 25,
            25, 31, 5, 10, 28, 25, 35, 17, 8, 27, 35, 17, 45, 26, 22, 43, 24, 14, 9, 5, 11, 9, 14,
            42,
        ],
        &[
            31, 18, 38, 11, 17, 12, 48, 65, 36, 42, 54, 67, 64, 34, 49, 66, 39, 68, 63, 44, 13, 57,
            53, 41, 26, 21, 3, 4, 6, 33, 30, 60, 62, 28, 46, 51, 45, 59, 55, 50, 25, 32, 10, 14,
            47, 8, 37, 0, 56, 52, 23, 1, 9, 7, 35, 22, 16, 69, 29, 24, 61, 5, 15, 20, 58, 27, 19,
            43, 40, 2,
        ],
        499,
    ),
    (
        &[
            38, 32, 12, 44, 17, 28, 32, 34, 30, 32, 15, 44, 33, 16, 35, 12, 28, 8, 26, 7, 25, 28,
            20, 4, 42, 15, 27, 4, 13, 42, 19, 50, 7, 49, 9, 45, 41, 42, 23, 9, 16, 8, 29, 14, 47,
            6, 25, 31, 10, 42, 14, 10, 45, 27,
        ],
        &[
            27, 23, 45, 19, 32, 17, 41, 34, 39, 48, 51, 2, 15, 28, 50, 43, 25, 10, 13, 40, 4, 30,
            22, 38, 46, 20, 18, 53, 26, 5, 21, 16, 42, 8, 47, 6, 1, 9, 12, 7, 14, 0, 36, 37, 29,
            49, 24, 3, 11, 35, 52, 44, 33, 31,
        ],
        298,
    ),
    (
        &[
            22, 1, 16, 2, 0, 1, 46, 32, 35, 12, 32, 30, 15, 28, 6, 42, 41, 27, 42, 31, 34, 25, 32,
            19, 44, 13, 14, 21, 12, 45, 46, 40, 8, 25, 22, 3, 8, 0, 4,
        ],
        &[
            4, 37, 1, 5, 3, 35, 38, 14, 36, 32, 28, 9, 25, 26, 12, 2, 23, 27, 34, 0, 33, 21, 17,
            13, 11, 19, 7, 22, 10, 20, 8, 31, 16, 15, 18, 24, 29, 30, 6,
        ],
        207,
    ),
    (
        &[
            14, 47, 41, 29, 31, 24, 4, 30, 43, 18, 49, 2, 39, 40, 41, 12, 4, 38, 9, 21, 16, 41, 47,
            44, 19, 39, 36, 8, 0, 30, 3, 31, 17, 43, 6,
        ],
        &[
            28, 11, 30, 16, 6, 34, 27, 18, 15, 0, 20, 32, 9, 24, 19, 5, 3, 29, 7, 31, 4, 26, 17,
            12, 25, 13, 21, 2, 14, 33, 8, 23, 1, 22, 10,
        ],
        185,
    ),
    (
        &[
            7, 41, 10, 38, 47, 41, 17, 18, 34, 48, 14, 16, 0, 26, 23, 23, 35, 4, 48, 36, 43, 17,
            31, 27, 34, 32, 28, 4, 3, 22, 4, 43, 9, 34, 3, 31, 42, 16, 14, 42, 3, 21, 1, 39, 44,
            21, 17, 38, 32, 12, 6, 6, 22, 18, 4, 34, 32, 7, 29, 48, 15, 23, 17, 3, 46, 38, 15, 4,
            43, 44, 41, 13, 24,
        ],
        &[
            12, 42, 28, 34, 63, 40, 27, 54, 67, 30, 17, 51, 50, 57, 0, 32, 2, 49, 71, 38, 10, 66,
            60, 11, 37, 46, 62, 21, 6, 7, 53, 41, 45, 52, 29, 61, 15, 14, 72, 13, 23, 26, 58, 35,
            22, 56, 25, 48, 8, 55, 24, 33, 16, 19, 3, 65, 47, 43, 1, 70, 5, 39, 36, 31, 20, 68, 44,
            69, 64, 4, 9, 59, 18,
        ],
        440,
    ),
    (
        &[
            23, 17, 48, 26, 32, 39, 43, 47, 27, 38, 13, 47, 5, 30, 33, 0, 21, 1, 34, 39, 25, 44, 8,
            35, 30, 49, 23, 49, 19, 6, 29, 1, 28, 17, 17, 47, 16, 11, 29, 39, 50, 3, 6, 43, 16, 49,
            46, 30, 34, 26, 49, 18, 13, 30, 35, 32, 17, 4, 37, 12, 35, 14, 1, 11, 20, 11, 46, 18,
            33, 25, 45, 34,
        ],
        &[
            15, 62, 17, 31, 41, 57, 12, 42, 29, 22, 65, 37, 63, 59, 52, 10, 61, 44, 36, 56, 1, 34,
            33, 51, 67, 28, 64, 16, 0, 26, 20, 69, 49, 3, 8, 32, 30, 38, 13, 53, 24, 47, 4, 55, 68,
            14, 48, 71, 18, 23, 54, 60, 58, 9, 5, 39, 19, 6, 43, 21, 70, 66, 46, 35, 7, 11, 2, 45,
            25, 27, 50, 40,
        ],
        403,
    ),
];
/// Keys sorted, and the pairs of their places compared.
type Trace = (&'static [u32], &'static [(u32, u32)]);

/// Three of them compared pair by pair, as genuine compared them.
const TRACED: [Trace; 3] = [
    (
        &[3, 2, 1, 1, 3, 4, 5, 2, 4, 2, 3, 5],
        &[
            (0, 6),
            (6, 11),
            (1, 10),
            (2, 10),
            (3, 10),
            (4, 10),
            (5, 10),
            (6, 10),
            (7, 10),
            (8, 10),
            (9, 10),
            (10, 10),
            (9, 10),
            (0, 1),
            (1, 2),
            (0, 2),
            (2, 3),
            (1, 3),
            (0, 3),
            (3, 4),
            (4, 5),
            (5, 6),
            (4, 6),
            (6, 7),
            (5, 7),
            (4, 7),
            (3, 7),
            (2, 7),
            (7, 8),
            (8, 9),
            (7, 9),
            (6, 9),
            (5, 9),
            (4, 9),
            (3, 9),
            (9, 10),
            (10, 11),
        ],
    ),
    (
        &[
            97, 73, 1, 99, 31, 84, 2, 56, 19, 19, 92, 40, 21, 32, 83, 88, 79, 74, 7, 15,
        ],
        &[
            (0, 10),
            (10, 19),
            (0, 19),
            (1, 18),
            (2, 18),
            (3, 18),
            (17, 18),
            (4, 18),
            (5, 18),
            (6, 18),
            (7, 18),
            (8, 18),
            (9, 18),
            (10, 18),
            (11, 18),
            (12, 18),
            (13, 18),
            (14, 18),
            (15, 18),
            (16, 18),
            (17, 18),
            (16, 18),
            (0, 8),
            (8, 15),
            (1, 14),
            (13, 14),
            (12, 14),
            (11, 14),
            (10, 14),
            (2, 14),
            (3, 14),
            (9, 14),
            (4, 14),
            (8, 14),
            (7, 14),
            (6, 14),
            (5, 14),
            (5, 14),
            (4, 14),
            (5, 10),
            (10, 15),
            (6, 14),
            (7, 14),
            (8, 14),
            (13, 14),
            (9, 14),
            (12, 14),
            (10, 14),
            (11, 14),
            (11, 14),
            (10, 14),
            (0, 1),
            (1, 2),
            (0, 2),
            (2, 3),
            (3, 4),
            (2, 4),
            (1, 4),
            (0, 4),
            (4, 5),
            (5, 6),
            (6, 7),
            (7, 8),
            (6, 8),
            (8, 9),
            (7, 9),
            (6, 9),
            (5, 9),
            (9, 10),
            (8, 10),
            (10, 11),
            (11, 12),
            (12, 13),
            (13, 14),
            (14, 15),
            (15, 16),
            (14, 16),
            (13, 16),
            (12, 16),
            (16, 17),
            (17, 18),
            (18, 19),
            (17, 19),
        ],
    ),
    (
        &[16, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1],
        &[
            (0, 8),
            (8, 15),
            (0, 15),
            (1, 14),
            (13, 14),
            (2, 14),
            (12, 14),
            (3, 14),
            (11, 14),
            (4, 14),
            (10, 14),
            (5, 14),
            (9, 14),
            (6, 14),
            (8, 14),
            (7, 14),
            (7, 14),
            (6, 14),
            (0, 1),
            (1, 2),
            (2, 3),
            (3, 4),
            (4, 5),
            (5, 6),
            (4, 6),
            (3, 6),
            (2, 6),
            (1, 6),
            (0, 6),
            (6, 7),
            (7, 8),
            (8, 9),
            (9, 10),
            (10, 11),
            (11, 12),
            (12, 13),
            (13, 14),
            (12, 14),
            (11, 14),
            (10, 14),
            (9, 14),
            (8, 14),
            (7, 14),
            (14, 15),
        ],
    ),
];

fn client() -> Client {
    let mut client = Client::new();
    client.put(STATICS + at::CTYPE, &crate::c::ctype::C_LOCALE);
    let words: Vec<u8> = COMPARE.iter().flat_map(|w| w.to_le_bytes()).collect();
    client.put(CODE, &words);
    client.running();
    client
}

fn number(client: &mut Client, entry: Code, text: &[u8], base: u32) -> (i32, u32, u32) {
    client.put(S, &[text, b"\0"].concat());
    client.put(STATICS + at::ERRNO, &[0; 4]);
    let value = client.answer(entry, &[S, END, base]).cast_signed();
    (
        value,
        client.word(END) - S,
        client.word(STATICS + at::ERRNO),
    )
}

type Code = tairix_riscos_kernel::module::Code;

#[test]
fn strtol_reads_a_signed_number_saturating_with_erange() {
    let mut client = client();
    assert_eq!(number(&mut client, strtol, b" \t-123x", 10), (-123, 6, 0));
    assert_eq!(
        number(&mut client, strtol, b"99999999999", 10),
        (i32::MAX, 11, ERANGE)
    );
    assert_eq!(
        number(&mut client, strtol, b"-99999999999", 10),
        (i32::MIN, 12, ERANGE)
    );
    assert_eq!(
        number(&mut client, strtol, b"2147483648", 10),
        (i32::MAX, 10, ERANGE)
    );
    assert_eq!(
        number(&mut client, strtol, b"-2147483648", 10),
        (i32::MIN, 11, 0)
    );
    assert_eq!(number(&mut client, strtol, b"0x1F", 0), (31, 4, 0));
    assert_eq!(number(&mut client, strtol, b"017", 0), (15, 3, 0));
    assert_eq!(
        number(&mut client, strtol, b"0xz", 16),
        (0, 0, 0),
        "a 0x commits to hex: no digit after it, no number"
    );
    assert_eq!(number(&mut client, strtol, b"0xz", 0), (0, 0, 0));
    assert_eq!(
        number(&mut client, strtol, b"00x1", 0),
        (0, 2, 0),
        "octal noughts, then a stop"
    );
    assert_eq!(number(&mut client, strtol, b"zz", 36), (36 * 35 + 35, 2, 0));
    assert_eq!(
        number(&mut client, strtol, b"z9", 37),
        (35 * 37 + 9, 2, 0),
        "any base above one"
    );
    assert_eq!(
        number(&mut client, strtol, b"z9", 100),
        (35 * 100 + 9, 2, 0)
    );
    assert_eq!(
        number(&mut client, strtol, b"- 5", 10),
        (-5, 3, 0),
        "white space after the sign too"
    );
    assert_eq!(number(&mut client, strtol, b"-\t5", 10), (-5, 3, 0));
    assert_eq!(
        number(&mut client, strtol, b"  +", 10),
        (0, 0, 0),
        "no digits: the start"
    );
    assert_eq!(
        number(&mut client, strtol, b"12", 1),
        (0, 0, 0),
        "no such base"
    );
    assert_eq!(
        number(&mut client, strtol, b"12", u32::MAX),
        (0, 0, 0),
        "nor a negative one"
    );
}

#[test]
fn an_empty_text_at_the_end_of_memory_is_read_no_further_than_its_end() {
    let mut client = client();
    let last = crate::c::testing::SLOT_END - 1;
    client.put(last, &[0]);
    client.put(STATICS + at::ERRNO, &[0; 4]);
    for base in [0, 16] {
        assert_eq!(client.answer(strtol, &[last, END, base]), 0, "base {base}");
        assert_eq!(client.word(END), last, "nothing read: the start");
    }
}

#[test]
fn strtoul_negates_with_a_minus_and_saturates_at_the_top() {
    let mut client = client();
    assert_eq!(number(&mut client, strtoul, b"-1", 10), (-1, 2, 0));
    assert_eq!(number(&mut client, strtoul, b"4294967295", 10), (-1, 10, 0));
    assert_eq!(
        number(&mut client, strtoul, b"99999999999", 10),
        (-1, 11, ERANGE)
    );
    client.put(S, b" -123x\0");
    assert_eq!(client.answer(atoi, &[S]).cast_signed(), -123);
}

#[test]
fn rand_follows_genuine_s_sequences_for_its_seeds() {
    let mut client = client();
    let statics = Statics::of_base(STATICS);
    seed(&mut client.session.kernel.os(), statics, 1).expect("seeded");
    let first: Vec<u32> = (0..8).map(|_| client.answer(rand, &[])).collect();
    assert_eq!(
        first,
        [
            0x5647_A0B0,
            0x7180_21CB,
            0x5F94_7B49,
            0x7DA0_D868,
            0x58E6_AD07,
            0x6A62_5345,
            0x3D1B_1BC8,
            0x5DDF_4F7F
        ]
    );
    client.call(srand, &[2]);
    let two: Vec<u32> = (0..4).map(|_| client.answer(rand, &[])).collect();
    assert_eq!(two, [0x7459_E7A1, 0x5E01_ACA3, 0x15D5_6338, 0x4268_1796]);
    client.call(srand, &[0x1234_5678]);
    let other: Vec<u32> = (0..4).map(|_| client.answer(rand, &[])).collect();
    assert_eq!(other, [0x7EC7_5C62, 0x4DA5_7BEA, 0x26AF_CE47, 0x2D6C_B19E]);
    assert_eq!(client.word(STATICS + at::RANDOM_SEED), 0x1234_5678 + 0x1234);
}

#[test]
fn abs_and_div_wrap_as_genuine_s_do() {
    let mut client = client();
    assert_eq!(client.answer(abs, &[(-5_i32).cast_unsigned()]), 5);
    assert_eq!(
        client.answer(abs, &[i32::MIN.cast_unsigned()]),
        i32::MIN.cast_unsigned()
    );
    let result = FREE + 0x40;
    let quotient = client.answer(div, &[result, (-7_i32).cast_unsigned(), 2]);
    assert_eq!(quotient.cast_signed(), -3, "R0 left the quotient");
    assert_eq!(
        [client.word(result), client.word(result + 4)],
        [(-3_i32).cast_unsigned(), (-1_i32).cast_unsigned()]
    );
}

/// The array of `(key, tag)` pairs written, sorted, and the tags it left
/// and the pairs compared, by index.
fn sort(client: &mut Client, keys: &[u32]) -> (Vec<u32>, Vec<(u32, u32)>) {
    let pairs: Vec<u8> = (0..)
        .zip(keys)
        .flat_map(|(tag, &key)| [key, tag])
        .flat_map(u32::to_le_bytes)
        .collect();
    client.put(ARRAY, &pairs);
    client.put(CURSOR, &LOG.to_le_bytes());
    let count = u32::try_from(keys.len()).expect("small");
    client.call(qsort, &[ARRAY, count, 8, CODE]);
    let tags = (0..count).map(|n| client.word(ARRAY + 8 * n + 4)).collect();
    let logged = (client.word(CURSOR) - LOG) / 8;
    let index = |p: u32| (p - ARRAY) / 8;
    let compared = (0..logged)
        .map(|n| {
            (
                index(client.word(LOG + 8 * n)),
                index(client.word(LOG + 8 * n + 4)),
            )
        })
        .collect();
    (tags, compared)
}

#[test]
fn qsort_leaves_every_recorded_array_as_genuine_did_in_as_many_comparisons() {
    let mut client = client();
    for (keys, tags, comparisons) in SORTED {
        let (left, compared) = sort(&mut client, keys);
        assert_eq!(left, tags, "{keys:?}");
        assert_eq!(compared.len(), comparisons, "{keys:?}");
    }
}

#[test]
fn qsort_sorts_no_further_a_range_a_comparison_scanned_past() {
    let mut client = client();
    // Less for any element below `limit`, else equal: the scan runs past
    // the array to it. `LDR R2, limit; CMP R0, R2; MVNCC R0, #0;
    // MOVCS R0, #0; MOV PC, R14`.
    let hostile = CODE + 0x100;
    let limit = ARRAY + 8 * 30;
    let code: Vec<u8> = [
        0xE59F_200C,
        0xE150_0002,
        0x33E0_0000,
        0x23A0_0000,
        0xE1A0_F00E,
    ]
    .into_iter()
    .chain([limit])
    .flat_map(u32::to_le_bytes)
    .collect();
    client.put(hostile, &code);
    client.put(ARRAY, &[0; 8 * 40]);
    // Far more than the sort needs; sorting on past the array never ends.
    client.session.allow_words(Some(1000));
    client.call(qsort, &[ARRAY, 20, 8, hostile]);
}

#[test]
fn qsort_compares_pair_by_pair_as_genuine_did() {
    let mut client = client();
    for (keys, pairs) in TRACED {
        assert_eq!(sort(&mut client, keys).1, pairs, "{keys:?}");
    }
}

#[test]
fn bsearch_probes_as_genuine_did_and_finds_the_element_it_meets() {
    let mut client = client();
    let keys = [1, 1, 2, 3, 3, 3, 5, 8, 8, 9];
    let words: Vec<u8> = (0..)
        .zip(keys)
        .flat_map(|(tag, key): (u32, u32)| [key, tag])
        .flat_map(u32::to_le_bytes)
        .collect();
    client.put(ARRAY, &words);
    let key = FREE + 0x40;
    let sp = client.session.kernel.os().registers().r[13];
    client.put(sp, &CODE.to_le_bytes());
    for (wanted, probes, found) in [
        (1, &[5, 2, 1][..], Some(1)),
        (3, &[5], Some(5)),
        (0, &[5, 2, 1, 0], None),
        (4, &[5, 8, 7, 6], None),
        (10, &[5, 8, 9], None),
    ] {
        client.put(key, &[wanted, 0].map(u32::to_le_bytes).concat());
        client.put(CURSOR, &LOG.to_le_bytes());
        let answer = client.answer(bsearch, &[key, ARRAY, 10, 8]);
        let logged = (client.word(CURSOR) - LOG) / 8;
        let probed: Vec<u32> = (0..logged)
            .map(|n| (client.word(LOG + 8 * n + 4) - ARRAY) / 8)
            .collect();
        assert_eq!(probed, probes, "key {wanted}");
        assert_eq!(answer, found.map_or(0, |n| ARRAY + 8 * n), "key {wanted}");
    }
}

#[test]
fn getenv_answers_one_buffer_grown_to_the_value_and_nought_for_none() {
    /// `OS_SetVarVal`'s type for a literal string of the length given.
    const LITERAL: u32 = 4;
    let mut client = Client::with_heap();
    client.put(S, b"Probe$Var\0");
    let value = FREE + 0x400;
    let long: Vec<u8> = b"abcdefghijklmnopqrstuvwxyz"
        .iter()
        .copied()
        .cycle()
        .take(1000)
        .collect();
    client.put(value, &[&long[..], b"\0"].concat());
    let mut set = Regs::default();
    set.r[..5].copy_from_slice(&[S, value, 3, 0, LITERAL]);
    client
        .session
        .kernel
        .os()
        .perform(tairix_riscos_kernel::swi::os::SET_VAR_VAL, &mut set)
        .expect("set");
    let first = client.answer(getenv, &[S]);
    assert_eq!(client.string(first), b"abc");
    assert_eq!(client.answer(getenv, &[S]), first, "the same buffer");
    set.r[..5].copy_from_slice(&[S, value, 1000, 0, LITERAL]);
    client
        .session
        .kernel
        .os()
        .perform(tairix_riscos_kernel::swi::os::SET_VAR_VAL, &mut set)
        .expect("set");
    let grown = client.answer(getenv, &[S]);
    assert_eq!(
        client
            .session
            .kernel
            .os()
            .read_zstring(grown, 2000)
            .expect("read"),
        long
    );
    client.put(S, b"NoSuch$Var\0");
    assert_eq!(client.answer(getenv, &[S]), 0);
    assert_eq!(
        client.answer(system, &[0]),
        1,
        "a command line interpreter there is"
    );
}
