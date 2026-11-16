//! Direct access and the block, memory and user commands on the true
//! drive, this tree's KERNAL and BASIC driving it: each screen and disk is
//! the one the original DOS leaves with the same disk in the same drive.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::disk::{closed, offset};
use super::files::{check as check_screens, on_commands, screen_of, Scene, Screen};
use super::writing::{check as check_disks, commands, commands_with, Scene as Written};
use crate::media::c1541::d64;
use crate::media::c1541::layout::{bam, kind, DIRECTORY_TRACK};

/// The commands' disk with tracks' entries in the BAM recorded wholly
/// used, as a `B-A` or `B-F` writes the BAM for the tracks it touched.
fn touched(tracks: &[u8]) -> Vec<u8> {
    let mut image = commands().image();
    for &track in tracks {
        let at =
            offset(DIRECTORY_TRACK, 0) + usize::from(bam::TRACK_ENTRY_BYTES) * usize::from(track);
        image[at..at + usize::from(bam::TRACK_ENTRY_BYTES)].fill(0);
    }
    image
}

/// The commands' disk with `bytes` from offset `at` of a block, the BAM
/// as it was.
fn with_bytes((track, sector): (u8, u8), at: usize, bytes: &[u8]) -> Vec<u8> {
    let mut image = commands().image();
    let start = offset(track, sector) + at;
    assert!(at + bytes.len() <= d64::SECTOR_BYTES, "within the block");
    image[start..start + bytes.len()].copy_from_slice(bytes);
    image
}

/// `#` channels opened.
const DIRECT: [Screen; 13] = [
    (
        "hash",
        "1 OPEN15,8,15:OPEN2,8,2,\"#\":GET#2,A$:PRINTASC(A$+CHR$(0))|2 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#\":GET#2,A$:PRI",
            "NTASC(A$+CHR$(0))",
            "2 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 3",
            " 0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "hashn",
        "1 OPEN15,8,15:FORI=0TO5:OPEN2,8,2,\"#\"+MID$(STR$(I),2):GET#2,A$|2 INPUT#15,A,B$,C,D:PRINTI;ASC(A$+CHR$(0));A;B$;C;D:CLOSE2:NEXT|RUN|~~",
        &[
            "1 OPEN15,8,15:FORI=0TO5:OPEN2,8,2,\"#\"+MI",
            "D$(STR$(I),2):GET#2,A$",
            "2 INPUT#15,A,B$,C,D:PRINTI;ASC(A$+CHR$(0",
            "));A;B$;C;D:CLOSE2:NEXT",
            "RUN",
            " 0  0  0 OK 0  0",
            " 1  1  0 OK 0  0",
            " 2  2  0 OK 0  0",
            " 3  3  0 OK 0  0",
            " 4  199  70 NO CHANNEL 0  0",
            " 5  199  70 NO CHANNEL 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "many",
        "1 OPEN15,8,15:FORI=2TO6:OPENI,8,I,\"#\":GET#I,A$:INPUT#15,A,B$|2 PRINTI;ASC(A$+CHR$(0));A;B$:NEXT|RUN|~~",
        &[
            "1 OPEN15,8,15:FORI=2TO6:OPENI,8,I,\"#\":GE",
            "T#I,A$:INPUT#15,A,B$",
            "2 PRINTI;ASC(A$+CHR$(0));A;B$:NEXT",
            "RUN",
            " 2  3  0 OK",
            " 3  2  0 OK",
            " 4  1  0 OK",
            " 5  0  0 OK",
            " 6  199  70 NO CHANNEL",
            "",
            "READY.",
        ],
    ),
    (
        "first",
        "1 OPEN15,8,15:PRINT#15,\"M-W\"CHR$(0)CHR$(6)CHR$(4)\"ABCD\":OPEN2,8,2,\"#3\"|2 FORI=1TO5:GET#2,A$:PRINTASC(A$+CHR$(0));:NEXT|RUN|~~",
        &[
            "1 OPEN15,8,15:PRINT#15,\"M-W\"CHR$(0)CHR$(",
            "6)CHR$(4)\"ABCD\":OPEN2,8,2,\"#3\"",
            "2 FORI=1TO5:GET#2,A$:PRINTASC(A$+CHR$(0)",
            ");:NEXT",
            "RUN",
            " 3  67  68  0  0",
            "READY.",
        ],
    ),
    (
        "hashfile",
        "1 OPEN15,8,15:OPEN2,8,2,\"#\":OPEN3,8,3,\"DATA\":OPEN4,8,4,\"DOT\"|2 OPEN5,8,5,\"ONE\":INPUT#15,A,B$:PRINTA;B$|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#\":OPEN3,8,3,\"D",
            "ATA\":OPEN4,8,4,\"DOT\"",
            "2 OPEN5,8,5,\"ONE\":INPUT#15,A,B$:PRINTA;B",
            "$",
            "RUN",
            " 70 NO CHANNEL",
            "",
            "READY.",
        ],
    ),
    (
        "hash2file",
        "1 OPEN15,8,15:OPEN2,8,2,\"#\":OPEN3,8,3,\"#\":OPEN4,8,4,\"DOT\"|2 INPUT#15,A,B$:PRINTA;B$:OPEN5,8,5,\"ONE\":INPUT#15,A,B$:PRINTA;B$|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#\":OPEN3,8,3,\"#",
            "\":OPEN4,8,4,\"DOT\"",
            "2 INPUT#15,A,B$:PRINTA;B$:OPEN5,8,5,\"ONE",
            "\":INPUT#15,A,B$:PRINTA;B$",
            "RUN",
            " 0 OK",
            " 70 NO CHANNEL",
            "",
            "READY.",
        ],
    ),
    (
        "hashinit",
        "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"I0\":GET#2,A$|2 PRINTASC(A$+CHR$(0));ST:INPUT#15,A,B$:PRINTA;B$|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"I0",
            "\":GET#2,A$",
            "2 PRINTASC(A$+CHR$(0));ST:INPUT#15,A,B$:",
            "PRINTA;B$",
            "RUN",
            " 199  66",
            " 0 OK",
            "",
            "READY.",
        ],
    ),
    (
        "none",
        "1 OPEN15,8,15|2 FORI=2TO6:OPENI,8,I,\"#\":GET#I,A$:PRINTASC(A$+CHR$(0));:NEXT|RUN|~~",
        &[
            "1 OPEN15,8,15",
            "2 FORI=2TO6:OPENI,8,I,\"#\":GET#I,A$:PRINT",
            "ASC(A$+CHR$(0));:NEXT",
            "RUN",
            " 3  2  1  0  199",
            "READY.",
        ],
    ),
    (
        "hsp",
        "1 OPEN15,8,15:OPEN2,8,2,\"# 2\"|2 GET#2,A$:PRINTASC(A$+CHR$(0)):INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"# 2\"",
            "2 GET#2,A$:PRINTASC(A$+CHR$(0)):INPUT#15",
            ",A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 2",
            " 0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "hx",
        "1 OPEN15,8,15:OPEN2,8,2,\"#X\"|2 GET#2,A$:PRINTASC(A$+CHR$(0)):INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#X\"",
            "2 GET#2,A$:PRINTASC(A$+CHR$(0)):INPUT#15",
            ",A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 0",
            " 0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "h03",
        "1 OPEN15,8,15:OPEN2,8,2,\"#03\"|2 GET#2,A$:PRINTASC(A$+CHR$(0)):INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#03\"",
            "2 GET#2,A$:PRINTASC(A$+CHR$(0)):INPUT#15",
            ",A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 3",
            " 0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "h12",
        "1 OPEN15,8,15:OPEN2,8,2,\"#12\"|2 GET#2,A$:PRINTASC(A$+CHR$(0)):INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#12\"",
            "2 GET#2,A$:PRINTASC(A$+CHR$(0)):INPUT#15",
            ",A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 199",
            " 70 NO CHANNEL 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "hbusy",
        "1 OPEN15,8,15:OPEN3,8,3,\"#2\":OPEN2,8,2,\"#2\"|2 GET#2,A$:PRINTASC(A$+CHR$(0)):INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN3,8,3,\"#2\":OPEN2,8,2,\"",
            "#2\"",
            "2 GET#2,A$:PRINTASC(A$+CHR$(0)):INPUT#15",
            ",A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 199",
            " 70 NO CHANNEL 0  0",
            "",
            "READY.",
        ],
    ),
];

/// Blocks read into channels and pointed into.
const READS: [Screen; 6] = [
    (
        "u1",
        "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"U1 2 0 18 0\":INPUT#15,A,B$,C,D|2 PRINTA;B$;C;D:FORI=0TO7:GET#2,A$:PRINTASC(A$+CHR$(0));:NEXT|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"U1",
            " 2 0 18 0\":INPUT#15,A,B$,C,D",
            "2 PRINTA;B$;C;D:FORI=0TO7:GET#2,A$:PRINT",
            "ASC(A$+CHR$(0));:NEXT",
            "RUN",
            " 0 OK 0  0",
            " 18  1  65  0  12  0  254  31",
            "READY.",
        ],
    ),
    (
        "br",
        "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"B-R 2 0 18 0\":INPUT#15,A,B$,C,D|2 PRINTA;B$;C;D:FORI=0TO7:GET#2,A$:PRINTASC(A$+CHR$(0));:NEXT|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"B-",
            "R 2 0 18 0\":INPUT#15,A,B$,C,D",
            "2 PRINTA;B$;C;D:FORI=0TO7:GET#2,A$:PRINT",
            "ASC(A$+CHR$(0));:NEXT",
            "RUN",
            " 0 OK 0  0",
            " 1  65  0  12  0  254  31  21",
            "READY.",
        ],
    ),
    (
        "bp",
        "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"U1:2,0,18,0\":PRINT#15,\"B-P 2 144\"|2 FORI=0TO7:GET#2,A$:PRINTASC(A$+CHR$(0));:NEXT:INPUT#15,A,B$:PRINTA;B$|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"U1",
            ":2,0,18,0\":PRINT#15,\"B-P 2 144\"",
            "2 FORI=0TO7:GET#2,A$:PRINTASC(A$+CHR$(0)",
            ");:NEXT:INPUT#15,A,B$:PRINTA;B$",
            "RUN",
            " 67  79  77  77  65  78  68  83  0 OK",
            "",
            "READY.",
        ],
    ),
    (
        "brend",
        "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"B-R 2 0 18 0\"|2 FORI=1TO20:GET#2,A$:PRINTASC(A$+CHR$(0));ST;:NEXT|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"B-",
            "R 2 0 18 0\"",
            "2 FORI=1TO20:GET#2,A$:PRINTASC(A$+CHR$(0",
            "));ST;:NEXT",
            "RUN",
            " 1  0  65  0  0  0  12  0  0  0  254  0",
            " 31  0  21  0  255  0  255  0  31  0  21",
            "  0  255  0  255  0  31  0  21  0  255",
            "0  255  64  1  0  65  0",
            "READY.",
        ],
    ),
    (
        "u1end",
        "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"U1 2 0 18 0\":PRINT#15,\"B-P 2 253\"|2 FORI=1TO5:GET#2,A$:PRINTASC(A$+CHR$(0));ST;:NEXT|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"U1",
            " 2 0 18 0\":PRINT#15,\"B-P 2 253\"",
            "2 FORI=1TO5:GET#2,A$:PRINTASC(A$+CHR$(0)",
            ");ST;:NEXT",
            "RUN",
            " 0  0  0  0  0  64  1  0  65  0",
            "READY.",
        ],
    ),
    (
        "u1file",
        "1 OPEN15,8,15:OPEN2,8,2,\"DATA\":PRINT#15,\"U1 2 0 18 0\":GET#2,A$:PRINTASC(A$)|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"DATA\":PRINT#15,",
            "\"U1 2 0 18 0\":GET#2,A$:PRINTASC(A$)",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 18",
            " 0 OK 0  0",
            "",
            "READY.",
        ],
    ),
];

/// The numbers block commands take.
const NUMBERS: [Screen; 14] = [
    (
        "bread",
        "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"B-READ 2 0 18 0\"|2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0));:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"B-",
            "READ 2 0 18 0\"",
            "2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0)",
            ");:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 3  0  0  30 SYNTAX ERROR 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "u1colsp",
        "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"U1:2 0 18 0\"|2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0));:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"U1",
            ":2 0 18 0\"",
            "2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0)",
            ");:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 18  1  65  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "u1nosep",
        "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"U12 0 18 0\"|2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0));:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"U1",
            "2 0 18 0\"",
            "2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0)",
            ");:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 3  0  0  70 NO CHANNEL 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "u1comma",
        "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"U1,2,0,18,0\"|2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0));:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"U1",
            ",2,0,18,0\"",
            "2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0)",
            ");:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 18  1  65  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "ua",
        "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"UA 2 0 18 0\"|2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0));:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"UA",
            " 2 0 18 0\"",
            "2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0)",
            ");:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 18  1  65  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "hsp read",
        "1 OPEN15,8,15:OPEN2,8,2,\"# 2\":PRINT#15,\"U1 2 0 18 0\"|2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0));:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"# 2\":PRINT#15,\"",
            "U1 2 0 18 0\"",
            "2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0)",
            ");:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 18  1  65  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "h03 read",
        "1 OPEN15,8,15:OPEN2,8,2,\"#03\":PRINT#15,\"U1 2 0 18 0\"|2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0));:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#03\":PRINT#15,\"",
            "U1 2 0 18 0\"",
            "2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0)",
            ");:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 18  1  65  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "hx read",
        "1 OPEN15,8,15:OPEN2,8,2,\"#X\":PRINT#15,\"U1 2 0 18 0\"|2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0));:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#X\":PRINT#15,\"U",
            "1 2 0 18 0\"",
            "2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0)",
            ");:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 18  1  65  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "bpnone",
        "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"U1 2 0 18 0\":PRINT#15,\"B-P 2\"|2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0));:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"U1",
            " 2 0 18 0\":PRINT#15,\"B-P 2\"",
            "2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0)",
            ");:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 18  1  65  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "br2",
        "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"B-R2 0 18 0\"|2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0));:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"B-",
            "R2 0 18 0\"",
            "2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0)",
            ");:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 1  65  0  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "brx",
        "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"B-RX 2 0 18 0\"|2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0));:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"B-",
            "RX 2 0 18 0\"",
            "2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0)",
            ");:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 3  0  0  30 SYNTAX ERROR 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "u1x",
        "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"U1X 2 0 18 0\"|2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0));:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"U1",
            "X 2 0 18 0\"",
            "2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0)",
            ");:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 18  1  65  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "brcol",
        "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"B-R:2 0 18 0\"|2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0));:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"B-",
            "R:2 0 18 0\"",
            "2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0)",
            ");:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 1  65  0  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "brsp2",
        "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"B-R  2  0  18  0\"|2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0));:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"B-",
            "R  2  0  18  0\"",
            "2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0)",
            ");:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 1  65  0  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
];

/// Block commands refused.
const REFUSALS: [Screen; 9] = [
    (
        "u1nochan",
        "1 OPEN15,8,15:PRINT#15,\"U1 5 0 18 0\"|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:PRINT#15,\"U1 5 0 18 0\"",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 70 NO CHANNEL 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "u1bad",
        "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"U1 2 0 36 0\"|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"U1",
            " 2 0 36 0\"",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 66 ILLEGAL TRACK OR SECTOR 36  0",
            "",
            "READY.",
        ],
    ),
    (
        "u1bads",
        "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"U1 2 0 18 19\"|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"U1",
            " 2 0 18 19\"",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 66 ILLEGAL TRACK OR SECTOR 18  19",
            "",
            "READY.",
        ],
    ),
    (
        "u1short",
        "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"U1 2 0\"|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"U1",
            " 2 0\"",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 66 ILLEGAL TRACK OR SECTOR 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "bpnochan",
        "1 OPEN15,8,15:PRINT#15,\"B-P 5 0\"|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:PRINT#15,\"B-P 5 0\"",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 70 NO CHANNEL 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "bx",
        "1 OPEN15,8,15:PRINT#15,\"B-X 5 0\"|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:PRINT#15,\"B-X 5 0\"",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 31 SYNTAX ERROR 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "d1then",
        "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"U1 2 1 18 0\":PRINT#15,\"U1 2 0 18 0\"|2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0));:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"U1",
            " 2 1 18 0\":PRINT#15,\"U1 2 0 18 0\"",
            "2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0)",
            ");:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 18  1  65  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "illthen",
        "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"U1 2 0 36 0\"|2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0));:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"U1",
            " 2 0 36 0\"",
            "2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0)",
            ");:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 3  0  0  66 ILLEGAL TRACK OR SECTOR 36",
            " 0",
            "",
            "READY.",
        ],
    ),
    (
        "d1ba",
        "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"B-A 1 18 0\"|2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0));:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"B-",
            "A 1 18 0\"",
            "2 FORI=1TO3:GET#2,A$:PRINTASC(A$+CHR$(0)",
            ");:NEXT:INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 3  0  0  74 DRIVE NOT READY 18  0",
            "",
            "READY.",
        ],
    ),
];

/// The memory and user commands.
const MEMORY: [Screen; 10] = [
    (
        "mr",
        "1 OPEN15,8,15:PRINT#15,\"M-R\"CHR$(0)CHR$(2)CHR$(4):FORI=1TO6|2 GET#15,A$:PRINTASC(A$+CHR$(0));ST;:NEXT|RUN|~~",
        &[
            "1 OPEN15,8,15:PRINT#15,\"M-R\"CHR$(0)CHR$(",
            "2)CHR$(4):FORI=1TO6",
            "2 GET#15,A$:PRINTASC(A$+CHR$(0));ST;:NEX",
            "T",
            "RUN",
            " 77  0  45  0  82  0  0  0  13  64  48",
            "0",
            "READY.",
        ],
    ),
    (
        "mr1",
        "1 OPEN15,8,15:PRINT#15,\"M-R\"CHR$(0)CHR$(2):FORI=1TO3|2 GET#15,A$:PRINTASC(A$+CHR$(0));ST;:NEXT|RUN|~~",
        &[
            "1 OPEN15,8,15:PRINT#15,\"M-R\"CHR$(0)CHR$(",
            "2):FORI=1TO3",
            "2 GET#15,A$:PRINTASC(A$+CHR$(0));ST;:NEX",
            "T",
            "RUN",
            " 77  0  13  64  48  0",
            "READY.",
        ],
    ),
    (
        "mw",
        "1 OPEN15,8,15:PRINT#15,\"M-W\"CHR$(0)CHR$(5)CHR$(2)\"XY\"|2 PRINT#15,\"M-R\"CHR$(0)CHR$(5)CHR$(2):GET#15,A$,B$:PRINTA$B$|RUN|~~",
        &[
            "1 OPEN15,8,15:PRINT#15,\"M-W\"CHR$(0)CHR$(",
            "5)CHR$(2)\"XY\"",
            "2 PRINT#15,\"M-R\"CHR$(0)CHR$(5)CHR$(2):GE",
            "T#15,A$,B$:PRINTA$B$",
            "RUN",
            "XY",
            "",
            "READY.",
        ],
    ),
    (
        "mr0",
        "1 OPEN15,8,15:PRINT#15,\"M-R\"CHR$(0)CHR$(2)CHR$(0):FORI=1TO3|2 GET#15,A$:PRINTASC(A$+CHR$(0));ST;:NEXT|RUN|~~",
        &[
            "1 OPEN15,8,15:PRINT#15,\"M-R\"CHR$(0)CHR$(",
            "2)CHR$(0):FORI=1TO3",
            "2 GET#15,A$:PRINTASC(A$+CHR$(0));ST;:NEX",
            "T",
            "RUN",
            " 77  0  45  0  82  0",
            "READY.",
        ],
    ),
    (
        "me2",
        "1 OPEN15,8,15:C$=CHR$(169)+\"Q\"+CHR$(141)+CHR$(16)+CHR$(5)+CHR$(96)|2 PRINT#15,\"M-W\"CHR$(0)CHR$(5)CHR$(6)C$:PRINT#15,\"M-E\"CHR$(0)CHR$(5)|3 PRINT#15,\"M-R\"CHR$(16)CHR$(5):GET#15,A$:PRINTA$|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:C$=CHR$(169)+\"Q\"+CHR$(141)",
            "+CHR$(16)+CHR$(5)+CHR$(96)",
            "2 PRINT#15,\"M-W\"CHR$(0)CHR$(5)CHR$(6)C$:",
            "PRINT#15,\"M-E\"CHR$(0)CHR$(5)",
            "3 PRINT#15,\"M-R\"CHR$(16)CHR$(5):GET#15,A",
            "$:PRINTA$",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            "Q",
        ],
    ),
    (
        "u3",
        "1 OPEN15,8,15:C$=CHR$(169)+\"Q\"+CHR$(141)+CHR$(16)+CHR$(5)+CHR$(96)|2 PRINT#15,\"M-W\"CHR$(0)CHR$(5)CHR$(6)C$:PRINT#15,\"U3\"|3 PRINT#15,\"M-R\"CHR$(16)CHR$(5):GET#15,A$:PRINTA$|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:C$=CHR$(169)+\"Q\"+CHR$(141)",
            "+CHR$(16)+CHR$(5)+CHR$(96)",
            "2 PRINT#15,\"M-W\"CHR$(0)CHR$(5)CHR$(6)C$:",
            "PRINT#15,\"U3\"",
            "3 PRINT#15,\"M-R\"CHR$(16)CHR$(5):GET#15,A",
            "$:PRINTA$",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            "Q",
        ],
    ),
    (
        "be",
        "1 OPEN15,8,15:C$=CHR$(169)+\"Q\"+CHR$(141)+CHR$(16)+CHR$(5)+CHR$(96)|2 OPEN2,8,2,\"#\":PRINT#15,\"B-P 2 0\":PRINT#2,C$;:PRINT#15,\"U2 2 0 17 0\"|3 PRINT#15,\"B-E 2 0 17 0\":PRINT#15,\"M-R\"CHR$(16)CHR$(5):GET#15,A$:PRINTA$|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:C$=CHR$(169)+\"Q\"+CHR$(141)",
            "+CHR$(16)+CHR$(5)+CHR$(96)",
            "2 OPEN2,8,2,\"#\":PRINT#15,\"B-P 2 0\":PRINT",
            "#2,C$;:PRINT#15,\"U2 2 0 17 0\"",
            "3 PRINT#15,\"B-E 2 0 17 0\":PRINT#15,\"M-R\"",
            "CHR$(16)CHR$(5):GET#15,A$:PRINTA$",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            "Q",
        ],
    ),
    (
        "ui",
        "1 OPEN15,8,15:PRINT#15,\"UI\"|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:PRINT#15,\"UI\"",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 73 OPENMICRO DOS 1541 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "uiplus",
        "1 OPEN15,8,15:PRINT#15,\"UI+\"|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:PRINT#15,\"UI+\"",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "u9",
        "1 OPEN15,8,15:PRINT#15,\"U9\"|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:PRINT#15,\"U9\"",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 73 OPENMICRO DOS 1541 0  0",
            "",
            "READY.",
        ],
    ),
];

#[test]
fn direct_channels_take_their_buffers_as_the_originals_do() {
    on_commands(&DIRECT);
}

/// A request's own buffers given back once it is done: a scratch's, before
/// the direct channels after it take them.
#[test]
fn a_command_gives_back_the_buffers_it_held() {
    check_screens(vec![Scene {
        name: "afterscratch",
        disk: commands().image(),
        second: None,
        keys: "1 OPEN15,8,15,\"S0:NOPE\"|2 FORI=2TO6:OPENI,8,I,\"#\":GET#I,A$:PRINTASC(A$+CHR$(0));:NEXT|RUN|~~",
        screen: &[
            "1 OPEN15,8,15,\"S0:NOPE\"",
            "2 FORI=2TO6:OPENI,8,I,\"#\":GET#I,A$:PRINT",
            "ASC(A$+CHR$(0));:NEXT",
            "RUN",
            " 3  2  1  0  199",
            "READY.",
        ],
    }]);
}

#[test]
fn blocks_read_into_a_channel_as_the_original_reads_them() {
    on_commands(&READS);
}

#[test]
fn block_commands_take_their_numbers_as_the_original_takes_them() {
    on_commands(&NUMBERS);
}

#[test]
fn block_commands_are_refused_as_the_original_refuses_them() {
    on_commands(&REFUSALS);
}

#[test]
fn memory_and_user_commands_reach_the_drive_as_the_originals_do() {
    on_commands(&MEMORY);
}

#[test]
fn blocks_are_written_as_the_originals_are() {
    check_disks(vec![
        Written::new(
            "u2",
            commands().image(),
            String::from("1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#2,\"HELLO\";:PRINT#15,\"U2 2 0 17 0\"|2 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~"),
            " 0 OK 0  0",
            with_bytes((17, 0), 1, b"HELLO"),
        ),
        Written::new(
            "bw",
            commands().image(),
            String::from("1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#2,\"HELLO\";:PRINT#15,\"B-W 2 0 17 1\"|2 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~"),
            " 0 OK 0  0",
            with_bytes((17, 1), 0, b"\x05HELLO"),
        ),
        Written::new(
            "bpw",
            commands().image(),
            String::from("1 OPEN15,8,15:OPEN2,8,2,\"#\":PRINT#15,\"B-P 2 10\":PRINT#2,\"AB\";|2 PRINT#15,\"U2 2 0 17 2\":INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~"),
            " 0 OK 0  0",
            with_bytes((17, 2), 10, b"AB"),
        ),
        Written::new(
            "be",
            commands().image(),
            String::from("1 OPEN15,8,15:C$=CHR$(169)+\"Q\"+CHR$(141)+CHR$(16)+CHR$(5)+CHR$(96)|2 OPEN2,8,2,\"#\":PRINT#15,\"B-P 2 0\":PRINT#2,C$;:PRINT#15,\"U2 2 0 17 0\"|3 PRINT#15,\"B-E 2 0 17 0\":PRINT#15,\"M-R\"CHR$(16)CHR$(5):GET#15,A$:PRINTA$|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~"),
            "Q",
            with_bytes((17, 0), 0, &[0xA9, b'Q', 0x8D, 0x10, 0x05, 0x60]),
        ),
    ]);
}

#[test]
fn blocks_are_allocated_and_freed_as_the_originals_are() {
    check_disks(vec![
        Written::new(
            "ba",
            commands().image(),
            String::from("1 OPEN15,8,15,\"I0\":PRINT#15,\"B-A 0 1 0\":INPUT#15,A,B$,C,D:PRINTA;B$;C;D|2 PRINT#15,\"B-A 0 17 5\":INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~"),
            " 0 OK 0  0",
            touched(&[1, 17]),
        ),
        Written::new(
            "baf",
            commands().image(),
            String::from("1 OPEN15,8,15,\"I0\":PRINT#15,\"B-A 0 17 5\":PRINT#15,\"B-F 0 1 8\"|2 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~"),
            " 0 OK 0  0",
            touched(&[1, 17]),
        ),
        Written::new(
            "ba18",
            commands().image(),
            String::from("1 OPEN15,8,15,\"I0\":PRINT#15,\"B-A 0 18 5\":INPUT#15,A,B$,C,D:PRINTA;B$;C;D|2 PRINT#15,\"B-A 0 36 0\":INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~"),
            " 66 ILLEGAL TRACK OR SECTOR 36  0",
            touched(&[18]),
        ),
        Written::new(
            "bafull",
            commands().image(),
            String::from("1 OPEN15,8,15,\"I0\":FORI=0TO20:PRINT#15,\"B-A 0 17\"I:NEXT|2 PRINT#15,\"B-A 0 17 0\":INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~"),
            " 65 NO BLOCK 18  2",
            touched(&[17, 18]),
        ),
        Written::new(
            "bafail",
            commands().image(),
            String::from("1 OPEN15,8,15,\"I0\":PRINT#15,\"B-A 0 1 0\"|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~"),
            " 65 NO BLOCK 1  9",
            touched(&[1]),
        ),
        Written::new(
            "bffree",
            commands().image(),
            String::from("1 OPEN15,8,15,\"I0\":PRINT#15,\"B-F 0 17 5\"|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~"),
            " 0 OK 0  0",
            touched(&[17]),
        ),
        Written::new(
            "bawrap",
            commands().image(),
            String::from("1 OPEN15,8,15,\"I0\":FORI=0TO16:PRINT#15,\"B-A 0 35\"I:NEXT:PRINT#15,\"B-A 0 35 3\"|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~"),
            " 65 NO BLOCK 0  0",
            touched(&[35]),
        ),
        Written::new(
            "bafree",
            commands().image(),
            String::from("1 OPEN15,8,15:PRINT#15,\"B-A 0 17 5\"|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~"),
            " 29 DISK ID MISMATCH 18  0",
            commands().image(),
        ),
        Written::new(
            "baw",
            commands().image(),
            String::from("1 OPEN15,8,15,\"I0\":PRINT#15,\"B-A 0 17 5\":OPEN2,8,2,\"NEW,S,W\":CLOSE2|2 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~"),
            " 0 OK 0  0",
            commands_with(|disk| {
                disk.take((17, 5));
                let (first, blocks) = disk.chain_along(&[(17, 0)], b"\r");
                disk.entry(closed(kind::SEQ), b"NEW", first, blocks);
            }),
        ),
    ]);
}

/// The original's restart misses an ATN the controller raises before it
/// finishes; this DOS answers it, as its `UI` does.
#[test]
fn a_restart_answers_the_controller_straight_after() {
    let status = [
        "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
        "RUN",
        " 73 OPENMICRO DOS 1541 0  0",
        "",
        "READY.",
    ];
    let uj = [&["1 OPEN15,8,15:PRINT#15,\"UJ\""][..], &status].concat();
    let colon = [&["1 OPEN15,8,15:PRINT#15,\"U:\""][..], &status].concat();
    for (name, keys, screen) in [
        (
            "UJ",
            "1 OPEN15,8,15:PRINT#15,\"UJ\"|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
            uj,
        ),
        (
            "U:",
            "1 OPEN15,8,15:PRINT#15,\"U:\"|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
            colon,
        ),
    ] {
        assert_eq!(screen_of(&commands().image(), None, keys), screen, "{name}");
    }
}
