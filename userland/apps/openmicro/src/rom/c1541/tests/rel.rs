//! Relative files and `P` on the true drive, this tree's KERNAL and BASIC
//! driving it: each screen and disk is the one the original DOS leaves with
//! the same disk in the same drive, but for the departures the plan records.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::disk::{offset, Disk, Relative};
use super::files::{on_commands, screen_of, Screen};
use super::writing::{check as check_disks, commands, commands_with, image_of, Scene};
use super::{played, surface, with_drive};
use crate::machine::c64::drive::{bus, Unit, VIA1};
use crate::machine::c64::keyboard::Key;
use crate::machine::via::reg;
use crate::media::c1541::layout::{bam, entry, kind, side, DIRECTORY_TRACK};
use crate::rom::c64::testing::{press, run_for, screen_rows, type_text, JIFFY};

/// Records written and read back.
const WRITES: [Screen; 11] = [
    (
        "create",
        "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"REL,L,\"+CHR$(10):PRINT#15,P$CHR$(5)Z$O$|3 INPUT#15,A,B$,C,D:PRINTA;B$;C;D:PRINT#2,\"HELLO\":CLOSE2|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0)",
            ":O$=CHR$(1)",
            "2 OPEN2,8,2,\"REL,L,\"+CHR$(10):PRINT#15,P",
            "$CHR$(5)Z$O$",
            "3 INPUT#15,A,B$,C,D:PRINTA;B$;C;D:PRINT#",
            "2,\"HELLO\":CLOSE2",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 0 OK 0  0",
            " 0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "readback",
        "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"REL,L,\"+CHR$(10):PRINT#15,P$CHR$(5)Z$O$:PRINT#2,\"HELLO\"|3 CLOSE2:OPEN2,8,2,\"REL\":PRINT#15,P$CHR$(5)Z$O$:INPUT#2,A$:PRINTA$|4 PRINT#15,P$CHR$(3)Z$O$:GET#2,A$:PRINTASC(A$+Z$);ST|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0)",
            ":O$=CHR$(1)",
            "2 OPEN2,8,2,\"REL,L,\"+CHR$(10):PRINT#15,P",
            "$CHR$(5)Z$O$:PRINT#2,\"HELLO\"",
            "3 CLOSE2:OPEN2,8,2,\"REL\":PRINT#15,P$CHR$",
            "(5)Z$O$:INPUT#2,A$:PRINTA$",
            "4 PRINT#15,P$CHR$(3)Z$O$:GET#2,A$:PRINTA",
            "SC(A$+Z$);ST",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            "HELLO",
            " 255  64",
            " 0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "overflow",
        "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"REL,L,\"+CHR$(5):PRINT#15,P$O$Z$O$:PRINT#2,\"ABCDEFGH\"|3 INPUT#15,A,B$,C,D:PRINTA;B$;C;D:CLOSE2|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0)",
            ":O$=CHR$(1)",
            "2 OPEN2,8,2,\"REL,L,\"+CHR$(5):PRINT#15,P$",
            "O$Z$O$:PRINT#2,\"ABCDEFGH\"",
            "3 INPUT#15,A,B$,C,D:PRINTA;B$;C;D:CLOSE2",
            "",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 51 OVERFLOW IN RECORD 0  0",
            " 0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "pastend",
        "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"REL,L,\"+CHR$(10):PRINT#15,P$CHR$(2)Z$O$:PRINT#2,\"X\"|3 PRINT#15,P$CHR$(9)Z$O$:INPUT#15,A,B$,C,D:PRINTA;B$;C;D:CLOSE2|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0)",
            ":O$=CHR$(1)",
            "2 OPEN2,8,2,\"REL,L,\"+CHR$(10):PRINT#15,P",
            "$CHR$(2)Z$O$:PRINT#2,\"X\"",
            "3 PRINT#15,P$CHR$(9)Z$O$:INPUT#15,A,B$,C",
            ",D:PRINTA;B$;C;D:CLOSE2",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 0 OK 0  0",
            " 0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "span",
        "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"BIG,L,\"+CHR$(100):PRINT#15,P$CHR$(6)Z$O$:PRINT#2,\"SPAN\"|3 CLOSE2|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0)",
            ":O$=CHR$(1)",
            "2 OPEN2,8,2,\"BIG,L,\"+CHR$(100):PRINT#15,",
            "P$CHR$(6)Z$O$:PRINT#2,\"SPAN\"",
            "3 CLOSE2",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 50 RECORD NOT PRESENT 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "twoprints",
        "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$:PRINT#2,\"AB\":PRINT#2,\"CD\"|3 PRINT#15,P$O$Z$O$|4 FORI=1TO12:GET#2,A$:PRINTASC(A$+Z$);ST;:NEXT|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0)",
            ":O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$:PRINT#2,\"AB\":PRINT#2,\"CD\"",
            "3 PRINT#15,P$O$Z$O$",
            "4 FORI=1TO12:GET#2,A$:PRINTASC(A$+Z$);ST",
            ";:NEXT",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 65  0  66  0  13  64  67  0  68  0  13",
            " 64  255  64  255  64  255  64  255  64",
            " 255  64  255  64  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "semicolon",
        "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$:PRINT#2,\"AB\";:PRINT#2,\"CD\"|3 PRINT#15,P$O$Z$O$|4 FORI=1TO12:GET#2,A$:PRINTASC(A$+Z$);ST;:NEXT|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0)",
            ":O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$:PRINT#2,\"AB\";:PRINT#2,\"CD\"",
            "3 PRINT#15,P$O$Z$O$",
            "4 FORI=1TO12:GET#2,A$:PRINTASC(A$+Z$);ST",
            ";:NEXT",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 65  0  66  64  67  0  68  0  13  64  25",
            "5  64  255  64  255  64  255  64  255  6",
            "4  255  64  255  64  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "position",
        "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$:PRINT#2,\"ABCDEFG\"|3 PRINT#15,P$O$Z$CHR$(3):PRINT#2,\"X\":PRINT#15,P$O$Z$O$|4 FORI=1TO12:GET#2,A$:PRINTASC(A$+Z$);ST;:NEXT|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0)",
            ":O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$:PRINT#2,\"ABCDEFG\"",
            "3 PRINT#15,P$O$Z$CHR$(3):PRINT#2,\"X\":PRI",
            "NT#15,P$O$Z$O$",
            "4 FORI=1TO12:GET#2,A$:PRINTASC(A$+Z$);ST",
            ";:NEXT",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 65  0  66  0  88  0  13  64  255  64  2",
            "55  64  255  64  255  64  255  64  255",
            "64  255  64  255  64  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "readpos",
        "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$:PRINT#2,\"ABCDEFG\"|3 PRINT#15,P$O$Z$CHR$(4)|4 FORI=1TO12:GET#2,A$:PRINTASC(A$+Z$);ST;:NEXT|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0)",
            ":O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$:PRINT#2,\"ABCDEFG\"",
            "3 PRINT#15,P$O$Z$CHR$(4)",
            "4 FORI=1TO12:GET#2,A$:PRINTASC(A$+Z$);ST",
            ";:NEXT",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 68  0  69  0  70  0  71  0  13  64  255",
            "  64  255  64  255  64  255  64  255  64",
            "  255  64  255  64  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "zeros",
        "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$:PRINT#2,\"A\"Z$\"B\";|3 PRINT#15,P$O$Z$O$|4 FORI=1TO12:GET#2,A$:PRINTASC(A$+Z$);ST;:NEXT|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0)",
            ":O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$:PRINT#2,\"A\"Z$\"B\";",
            "3 PRINT#15,P$O$Z$O$",
            "4 FORI=1TO12:GET#2,A$:PRINTASC(A$+Z$);ST",
            ";:NEXT",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 65  0  0  0  66  64  255  64  255  64",
            "255  64  255  64  255  64  255  64  255",
            " 64  255  64  255  64  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "rec0",
        "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$Z$Z$O$:PRINT#2,\"Q\"|3 PRINT#15,P$O$Z$O$|4 FORI=1TO12:GET#2,A$:PRINTASC(A$+Z$);ST;:NEXT|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0)",
            ":O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$Z",
            "$Z$O$:PRINT#2,\"Q\"",
            "3 PRINT#15,P$O$Z$O$",
            "4 FORI=1TO12:GET#2,A$:PRINTASC(A$+Z$);ST",
            ";:NEXT",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 81  0  13  64  255  64  255  64  255  6",
            "4  255  64  255  64  255  64  255  64  2",
            "55  64  255  64  255  64  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
];

/// Relative files opened, positioned, listed, scratched and copied.
const OPENS: [Screen; 14] = [
    (
        "readpast",
        "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$:PRINT#2,\"AB\":CLOSE2|3 OPEN2,8,2,\"R\":PRINT#15,P$CHR$(40)Z$O$:INPUT#15,A,B$,C,D:PRINTA;B$;C;D|4 FORI=1TO3:GET#2,A$:PRINTASC(A$+Z$);ST;:NEXT|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0)",
            ":O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$:PRINT#2,\"AB\":CLOSE2",
            "3 OPEN2,8,2,\"R\":PRINT#15,P$CHR$(40)Z$O$:",
            "INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "4 FORI=1TO3:GET#2,A$:PRINTASC(A$+Z$);ST;",
            ":NEXT",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 50 RECORD NOT PRESENT 0  0",
            " 13  64  13  64  13  64  50 RECORD NOT P",
            "RESENT 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "nopos",
        "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$:PRINT#2,\"AB\":CLOSE2|3 OPEN2,8,2,\"R\":PRINT#15,P$O$Z$|4 FORI=1TO3:GET#2,A$:PRINTASC(A$+Z$);ST;:NEXT|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0)",
            ":O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$:PRINT#2,\"AB\":CLOSE2",
            "3 OPEN2,8,2,\"R\":PRINT#15,P$O$Z$",
            "4 FORI=1TO3:GET#2,A$:PRINTASC(A$+Z$);ST;",
            ":NEXT",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 65  0  66  0  13  64  51 OVERFLOW IN RE",
            "CORD 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "norec",
        "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$:PRINT#2,\"AB\":CLOSE2|3 OPEN2,8,2,\"R\":PRINT#15,P$|4 FORI=1TO3:GET#2,A$:PRINTASC(A$+Z$);ST;:NEXT|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0)",
            ":O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$:PRINT#2,\"AB\":CLOSE2",
            "3 OPEN2,8,2,\"R\":PRINT#15,P$",
            "4 FORI=1TO3:GET#2,A$:PRINTASC(A$+Z$);ST;",
            ":NEXT",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 255  64  255  64  255  64  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "pseq",
        "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|3 OPEN2,8,2,\"DATA\":PRINT#15,P$O$Z$O$|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0)",
            ":O$=CHR$(1)",
            "3 OPEN2,8,2,\"DATA\":PRINT#15,P$O$Z$O$",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 64 FILE TYPE MISMATCH 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "pclosed",
        "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|3 PRINT#15,P$O$Z$O$|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0)",
            ":O$=CHR$(1)",
            "3 PRINT#15,P$O$Z$O$",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 70 NO CHANNEL 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "otherlen",
        "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$:PRINT#2,\"AB\":CLOSE2|3 OPEN2,8,2,\"R,L,\"+CHR$(20)|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0)",
            ":O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$:PRINT#2,\"AB\":CLOSE2",
            "3 OPEN2,8,2,\"R,L,\"+CHR$(20)",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 50 RECORD NOT PRESENT 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "samelen",
        "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$:PRINT#2,\"AB\":CLOSE2|3 OPEN2,8,2,\"R,L,\"+CHR$(10)|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0)",
            ":O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$:PRINT#2,\"AB\":CLOSE2",
            "3 OPEN2,8,2,\"R,L,\"+CHR$(10)",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "nolen",
        "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|3 OPEN2,8,2,\"NEWR,L\"|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0)",
            ":O$=CHR$(1)",
            "3 OPEN2,8,2,\"NEWR,L\"",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 62 FILE NOT FOUND 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "len255",
        "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|3 OPEN2,8,2,\"NEWR,L,\"+CHR$(255)|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0)",
            ":O$=CHR$(1)",
            "3 OPEN2,8,2,\"NEWR,L,\"+CHR$(255)",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "seqasrel",
        "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|3 OPEN2,8,2,\"DATA,L,\"+CHR$(10)|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0)",
            ":O$=CHR$(1)",
            "3 OPEN2,8,2,\"DATA,L,\"+CHR$(10)",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 64 FILE TYPE MISMATCH 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "scratchrel",
        "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$:PRINT#2,\"AB\":CLOSE2|3 PRINT#15,\"S0:R\"|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0)",
            ":O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$:PRINT#2,\"AB\":CLOSE2",
            "3 PRINT#15,\"S0:R\"",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 1 FILES SCRATCHED 1  0",
            "",
            "READY.",
        ],
    ),
    (
        "copyrel",
        "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$:PRINT#2,\"AB\":CLOSE2|3 PRINT#15,\"C0:R2=R\"|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0)",
            ":O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$:PRINT#2,\"AB\":CLOSE2",
            "3 PRINT#15,\"C0:R2=R\"",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 71 DIR ERROR 2  2",
            "",
            "READY.",
        ],
    ),
    (
        "seqpast",
        "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$CHR$(25)Z$O$|3 FORI=1TO3:GET#2,A$:PRINTASC(A$+Z$);ST;:NEXT|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~~~~~~~~~~~~~~~~~~~~~~~",
        &[
            "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(",
            "98):Z$=CHR$(0):O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$C",
            "HR$(25)Z$O$",
            "3 FORI=1TO3:GET#2,A$:PRINTASC(A$+Z$);ST;",
            ":NEXT",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 255  64  13  64  13  64  50 RECORD NOT",
            "PRESENT 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "len80",
        "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R8,L,\"+CHR$(80):CLOSE2:OPEN2,8,2,\"R9,L,\"+CHR$(87):CLOSE2|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~~~~~~~~~~~~~~~~~~~~~~~",
        &[
            "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(",
            "98):Z$=CHR$(0):O$=CHR$(1)",
            "2 OPEN2,8,2,\"R8,L,\"+CHR$(80):CLOSE2:OPEN",
            "2,8,2,\"R9,L,\"+CHR$(87):CLOSE2",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 0 OK 0  0",
            "",
            "READY.",
        ],
    ),
];

/// Records read and written at their edges, and a relative file beside a
/// sequential one.
const EDGES: [Screen; 14] = [
    (
        "zeropos",
        "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$|3 PRINT#2,\"AB\":PRINT#15,P$O$Z$CHR$(6)|4 FORI=1TO3:GET#2,A$:PRINTASC(A$+Z$);ST;:NEXT|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~~~",
        &[
            "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(",
            "98):Z$=CHR$(0):O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$",
            "3 PRINT#2,\"AB\":PRINT#15,P$O$Z$CHR$(6)",
            "4 FORI=1TO3:GET#2,A$:PRINTASC(A$+Z$);ST;",
            ":NEXT",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 0  0  0  0  0  0  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "allzero",
        "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$|3 PRINT#2,Z$;:PRINT#15,P$O$Z$O$|4 FORI=1TO3:GET#2,A$:PRINTASC(A$+Z$);ST;:NEXT|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~~~",
        &[
            "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(",
            "98):Z$=CHR$(0):O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$",
            "3 PRINT#2,Z$;:PRINT#15,P$O$Z$O$",
            "4 FORI=1TO3:GET#2,A$:PRINTASC(A$+Z$);ST;",
            ":NEXT",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 0  64  255  64  255  64  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "posend",
        "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$|3 PRINT#2,\"ABCDEFGHI\":PRINT#15,P$O$Z$CHR$(10)|4 FORI=1TO3:GET#2,A$:PRINTASC(A$+Z$);ST;:NEXT|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~~~",
        &[
            "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(",
            "98):Z$=CHR$(0):O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$",
            "3 PRINT#2,\"ABCDEFGHI\":PRINT#15,P$O$Z$CHR",
            "$(10)",
            "4 FORI=1TO3:GET#2,A$:PRINTASC(A$+Z$);ST;",
            ":NEXT",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 13  64  255  64  255  64  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "pos0",
        "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$|3 PRINT#2,\"ABC\":PRINT#15,P$O$Z$Z$|4 FORI=1TO3:GET#2,A$:PRINTASC(A$+Z$);ST;:NEXT|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~~~",
        &[
            "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(",
            "98):Z$=CHR$(0):O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$",
            "3 PRINT#2,\"ABC\":PRINT#15,P$O$Z$Z$",
            "4 FORI=1TO3:GET#2,A$:PRINTASC(A$+Z$);ST;",
            ":NEXT",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 65  0  66  0  67  0  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "pos11",
        "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$|3 PRINT#2,\"ABC\":PRINT#15,P$O$Z$CHR$(11)|4 FORI=1TO3:GET#2,A$:PRINTASC(A$+Z$);ST;:NEXT|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~~~",
        &[
            "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(",
            "98):Z$=CHR$(0):O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$",
            "3 PRINT#2,\"ABC\":PRINT#15,P$O$Z$CHR$(11)",
            "4 FORI=1TO3:GET#2,A$:PRINTASC(A$+Z$);ST;",
            ":NEXT",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 65  0  66  0  67  0  51 OVERFLOW IN REC",
            "ORD 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "writefull",
        "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$|3 PRINT#2,\"ABCDEFGHIJ\";:PRINT#2,\"K\":PRINT#15,P$O$Z$O$|4 FORI=1TO14:GET#2,A$:PRINTASC(A$+Z$);ST;:NEXT|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~~~",
        &[
            "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(",
            "98):Z$=CHR$(0):O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$",
            "3 PRINT#2,\"ABCDEFGHIJ\";:PRINT#2,\"K\":PRIN",
            "T#15,P$O$Z$O$",
            "4 FORI=1TO14:GET#2,A$:PRINTASC(A$+Z$);ST",
            ";:NEXT",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 65  0  66  0  67  0  68  0  69  0  70",
            "0  71  0  72  0  73  0  74  64  75  0  1",
            "3  64  255  64  255  64  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "write11",
        "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$|3 PRINT#2,\"ABCDEFGHIJK\";:PRINT#2,\"L\":PRINT#15,P$O$Z$O$|4 FORI=1TO14:GET#2,A$:PRINTASC(A$+Z$);ST;:NEXT|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~~~",
        &[
            "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(",
            "98):Z$=CHR$(0):O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$",
            "3 PRINT#2,\"ABCDEFGHIJK\";:PRINT#2,\"L\":PRI",
            "NT#15,P$O$Z$O$",
            "4 FORI=1TO14:GET#2,A$:PRINTASC(A$+Z$);ST",
            ";:NEXT",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 65  0  66  0  67  0  68  0  69  0  70",
            "0  71  0  72  0  73  0  74  64  76  0  1",
            "3  64  255  64  255  64  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "relseq",
        "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$|3 OPEN3,8,3,\"NS,S,W\":PRINT#3,\"HI\":CLOSE3:INPUT#15,A,B$:PRINTA;B$|5 OPEN4,8,4,\"DATA\":GET#4,A$:PRINTASC(A$+Z$);ST|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~~~",
        &[
            "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(",
            "98):Z$=CHR$(0):O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$",
            "3 OPEN3,8,3,\"NS,S,W\":PRINT#3,\"HI\":CLOSE3",
            ":INPUT#15,A,B$:PRINTA;B$",
            "5 OPEN4,8,4,\"DATA\":GET#4,A$:PRINTASC(A$+",
            "Z$);ST",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 0 OK",
            " 72  0",
            " 0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "pastgrow",
        "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$|3 PRINT#15,P$CHR$(26)Z$O$:PRINT#2,\"Z\":PRINT#15,P$CHR$(30)Z$O$:PRINT#2,\"Y\"|5 PRINT#15,P$CHR$(27)Z$O$|4 FORI=1TO3:GET#2,A$:PRINTASC(A$+Z$);ST;:NEXT|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~~~",
        &[
            "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(",
            "98):Z$=CHR$(0):O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$",
            "3 PRINT#15,P$CHR$(26)Z$O$:PRINT#2,\"Z\":PR",
            "INT#15,P$CHR$(30)Z$O$:PRINT#2,\"Y\"",
            "5 PRINT#15,P$CHR$(27)Z$O$",
            "4 FORI=1TO3:GET#2,A$:PRINTASC(A$+Z$);ST;",
            ":NEXT",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 255  64  255  64  255  64  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "len0old",
        "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$|3 PRINT#2,\"AB\":CLOSE2:OPEN2,8,2,\"R,L,\"+Z$|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~~~",
        &[
            "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(",
            "98):Z$=CHR$(0):O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$",
            "3 PRINT#2,\"AB\":CLOSE2:OPEN2,8,2,\"R,L,\"+Z",
            "$",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "pdirect",
        "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|3 OPEN5,8,5,\"#\":PRINT#15,\"P\"+CHR$(101)+O$+Z$+O$|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~~~",
        &[
            "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(",
            "98):Z$=CHR$(0):O$=CHR$(1)",
            "3 OPEN5,8,5,\"#\":PRINT#15,\"P\"+CHR$(101)+O",
            "$+Z$+O$",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 64 FILE TYPE MISMATCH 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "pastwrite",
        "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$|3 PRINT#15,P$CHR$(30)Z$O$:GET#2,A$:PRINTASC(A$+Z$);ST:PRINT#2,\"Q\"|5 PRINT#15,P$CHR$(30)Z$O$|4 FORI=1TO3:GET#2,A$:PRINTASC(A$+Z$);ST;:NEXT|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~~~",
        &[
            "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(",
            "98):Z$=CHR$(0):O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$",
            "3 PRINT#15,P$CHR$(30)Z$O$:GET#2,A$:PRINT",
            "ASC(A$+Z$);ST:PRINT#2,\"Q\"",
            "5 PRINT#15,P$CHR$(30)Z$O$",
            "4 FORI=1TO3:GET#2,A$:PRINTASC(A$+Z$);ST;",
            ":NEXT",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 13  64",
            " 255  64  255  64  255  64  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "readwrite",
        "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$|3 PRINT#2,\"ABCDEFG\":PRINT#15,P$O$Z$O$:GET#2,A$,B$:PRINT#2,\"X\"|5 PRINT#15,P$O$Z$O$|4 FORI=1TO14:GET#2,A$:PRINTASC(A$+Z$);ST;:NEXT|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~~~",
        &[
            "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(",
            "98):Z$=CHR$(0):O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$",
            "3 PRINT#2,\"ABCDEFG\":PRINT#15,P$O$Z$O$:GE",
            "T#2,A$,B$:PRINT#2,\"X\"",
            "5 PRINT#15,P$O$Z$O$",
            "4 FORI=1TO14:GET#2,A$:PRINTASC(A$+Z$);ST",
            ";:NEXT",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 255  64  255  64  255  64  255  64  255",
            "  64  255  64  255  64  255  64  255  64",
            "  255  64  255  64  255  64  255  64  25",
            "5  64  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "seqlen",
        "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|3 OPEN2,8,2,\"DATA,S,R\":CLOSE2:OPEN2,8,2,\"DATA,L\"|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~~~",
        &[
            "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(",
            "98):Z$=CHR$(0):O$=CHR$(1)",
            "3 OPEN2,8,2,\"DATA,S,R\":CLOSE2:OPEN2,8,2,",
            "\"DATA,L\"",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 64 FILE TYPE MISMATCH 0  0",
            "",
            "READY.",
        ],
    ),
];

/// Where this DOS departs from the original: no hang for a length of 0 or
/// none after the comma, a read past a record's data that ends at the
/// record's end, other buffers free, an error that keeps its number, and
/// FILE TOO LARGE for a record no disk could hold.
const DEPARTURES: [Screen; 8] = [
    (
        "len0",
        "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|3 OPEN2,8,2,\"NEWR,L,\"+Z$|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~",
        &[
            "1 OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0)",
            ":O$=CHR$(1)",
            "3 OPEN2,8,2,\"NEWR,L,\"+Z$",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 62 FILE NOT FOUND 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "nocomma",
        "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|3 OPEN2,8,2,\"NEWR,L,\"|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~~~",
        &[
            "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(",
            "98):Z$=CHR$(0):O$=CHR$(1)",
            "3 OPEN2,8,2,\"NEWR,L,\"",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 62 FILE NOT FOUND 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "zeropos8",
        "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$|3 PRINT#2,\"AB\":PRINT#15,P$O$Z$CHR$(6)|4 FORI=1TO8:GET#2,A$:PRINTASC(A$+Z$);ST;:NEXT|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~~~",
        &[
            "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(",
            "98):Z$=CHR$(0):O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$",
            "3 PRINT#2,\"AB\":PRINT#15,P$O$Z$CHR$(6)",
            "4 FORI=1TO8:GET#2,A$:PRINTASC(A$+Z$);ST;",
            ":NEXT",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 0  0  0  0  0  0  0  0  0  64  255  64",
            " 255  64  255  64  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "zeropos8c",
        "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$|3 PRINT#2,\"AB\":CLOSE2:OPEN2,8,2,\"R\":PRINT#15,P$O$Z$CHR$(6)|4 FORI=1TO8:GET#2,A$:PRINTASC(A$+Z$);ST;:NEXT|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~~~",
        &[
            "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(",
            "98):Z$=CHR$(0):O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$",
            "3 PRINT#2,\"AB\":CLOSE2:OPEN2,8,2,\"R\":PRIN",
            "T#15,P$O$Z$CHR$(6)",
            "4 FORI=1TO8:GET#2,A$:PRINTASC(A$+Z$);ST;",
            ":NEXT",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 0  0  0  0  0  0  0  0  0  64  255  64",
            " 255  64  255  64  0 OK 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "pastwritec",
        "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$|3 PRINT#15,P$CHR$(30)Z$O$:GET#2,A$:PRINTASC(A$+Z$);ST:PRINT#2,\"Q\"|5 CLOSE2|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~~~",
        &[
            "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(",
            "98):Z$=CHR$(0):O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O",
            "$Z$O$",
            "3 PRINT#15,P$CHR$(30)Z$O$:GET#2,A$:PRINT",
            "ASC(A$+Z$);ST:PRINT#2,\"Q\"",
            "5 CLOSE2",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 13  64",
            " 50 RECORD NOT PRESENT 0  0",
            "",
            "READY.",
        ],
    ),
    (
        "relbufs",
        "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10)|3 FORI=3TO7:OPENI,8,I,\"#\":GET#I,A$:PRINTASC(A$+Z$);:NEXT|RUN|~~",
        &[
            "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(",
            "98):Z$=CHR$(0):O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10)",
            "3 FORI=3TO7:OPENI,8,I,\"#\":GET#I,A$:PRINT",
            "ASC(A$+Z$);:NEXT",
            "RUN",
            " 2  0  199  199  199",
            "READY.",
        ],
    ),
    (
        "tworel",
        "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"R,L,\"+CHR$(10):OPEN9,8,9,\"Q,L,\"+CHR$(10):INPUT#15,A,B$:PRINTA;B$|3 FORI=3TO7:OPENI,8,I,\"#\":GET#I,A$:PRINTASC(A$+Z$);:NEXT|RUN|~~",
        &[
            "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(",
            "98):Z$=CHR$(0):O$=CHR$(1)",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):OPEN9,8,9,\"Q",
            ",L,\"+CHR$(10):INPUT#15,A,B$:PRINTA;B$",
            "3 FORI=3TO7:OPENI,8,I,\"#\":GET#I,A$:PRINT",
            "ASC(A$+Z$);:NEXT",
            "RUN",
            " 70 NO CHANNEL",
            " 2  0  199  199  199",
            "READY.",
        ],
    ),
    (
        "huge1000",
        "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|2 OPEN2,8,2,\"H,L,\"+CHR$(254):PRINT#15,P$CHR$(232)CHR$(3)O$|3 INPUT#15,A,B$:PRINTA;B$:PRINT#2,\"X\":CLOSE2|9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~~~~~~~~~~~~~~~~~~~~~~~",
        &[
            "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(",
            "98):Z$=CHR$(0):O$=CHR$(1)",
            "2 OPEN2,8,2,\"H,L,\"+CHR$(254):PRINT#15,P$",
            "CHR$(232)CHR$(3)O$",
            "3 INPUT#15,A,B$:PRINTA;B$:PRINT#2,\"X\":CL",
            "OSE2",
            "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D",
            "RUN",
            " 50 RECORD NOT PRESENT",
            " 52 FILE TOO LARGE 0  0",
            "",
            "READY.",
        ],
    ),
];

#[test]
fn records_are_written_and_read_back_as_the_originals_are() {
    on_commands(&WRITES);
}

#[test]
fn relative_files_open_position_list_and_go_as_the_originals_do() {
    on_commands(&OPENS);
}

#[test]
fn records_at_their_edges_are_read_and_written_as_the_originals_are() {
    on_commands(&EDGES);
}

#[test]
fn this_dos_departs_from_the_original_only_where_the_plan_records() {
    on_commands(&DEPARTURES);
}

/// The line each disk scene starts with, and the one its status follows.
const OPENING: &str = "1 POKE56333,127:OPEN15,8,15:P$=\"P\"+CHR$(98):Z$=CHR$(0):O$=CHR$(1)|";
const STATUS: &str = "9 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|~~";
/// `R`, records of ten bytes, made with `AB` in its first and closed.
const WRITE_R: &str = "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$O$Z$O$:PRINT#2,\"AB\":CLOSE2|";

/// `R` as [`WRITE_R`] leaves it.
fn r_written(disk: &mut Disk) {
    disk.relative(&Relative {
        name: b"R",
        length: 10,
        blocks: &[(17, 0)],
        sides: &[(17, 10)],
        records: &[(0, b"AB\r")],
        spare: None,
        counted: 2,
    });
}

/// A relative file made and never closed, of `length` with no record
/// written, its data block and side sector those given.
fn made(disk: &mut Disk, name: &[u8], length: u8, block: (u8, u8), side: (u8, u8)) {
    disk.relative(&Relative {
        name,
        length,
        blocks: &[block],
        sides: &[side],
        records: &[],
        spare: None,
        counted: 0,
    });
}

/// A scene on the commands' disk: `lines` typed between the opening and
/// the status line, `status` shown and `after` the disk left.
fn scene(name: &'static str, lines: &str, status: &'static str, after: Vec<u8>) -> Scene {
    Scene::new(
        name,
        commands().image(),
        format!("{OPENING}{lines}{STATUS}"),
        status,
        after,
    )
}

#[test]
fn relative_files_are_laid_out_as_the_originals_are() {
    check_disks(vec![
        scene(
            "create",
            "2 OPEN2,8,2,\"REL,L,\"+CHR$(10):PRINT#15,P$CHR$(5)Z$O$:PRINT#2,\"HELLO\":CLOSE2|",
            " 0 OK 0  0",
            commands_with(|disk| {
                disk.relative(&Relative {
                    name: b"REL",
                    length: 10,
                    blocks: &[(17, 0)],
                    sides: &[(17, 10)],
                    records: &[(4, b"HELLO\r")],
                    spare: None,
                    counted: 2,
                });
            }),
        ),
        scene(
            "span",
            "2 OPEN2,8,2,\"BIG,L,\"+CHR$(100):PRINT#15,P$CHR$(6)Z$O$:PRINT#2,\"SPAN\":CLOSE2|",
            " 50 RECORD NOT PRESENT 0  0",
            commands_with(|disk| {
                disk.relative(&Relative {
                    name: b"BIG",
                    length: 100,
                    blocks: &[(17, 0), (17, 11), (17, 1)],
                    sides: &[(17, 10)],
                    records: &[(5, b"SPAN\r")],
                    spare: None,
                    counted: 4,
                });
            }),
        ),
        scene(
            "pastgrow",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):PRINT#15,P$CHR$(26)Z$O$:PRINT#2,\"Z\"|",
            " 50 RECORD NOT PRESENT 0  0",
            commands_with(|disk| {
                disk.relative(&Relative {
                    name: b"R",
                    length: 10,
                    blocks: &[(17, 0), (17, 11)],
                    sides: &[(17, 10)],
                    records: &[(25, b"Z\r")],
                    spare: None,
                    counted: 0,
                });
            }),
        ),
        scene(
            "len255",
            "2 OPEN2,8,2,\"NEWR,L,\"+CHR$(255)|",
            " 0 OK 0  0",
            commands_with(|disk| made(disk, b"NEWR", 255, (17, 0), (17, 10))),
        ),
        scene(
            "len80",
            "2 OPEN2,8,2,\"R8,L,\"+CHR$(80):CLOSE2:OPEN2,8,2,\"R9,L,\"+CHR$(87):CLOSE2|",
            " 0 OK 0  0",
            commands_with(|disk| {
                made(disk, b"R8", 80, (17, 0), (17, 10));
                made(disk, b"R9", 87, (17, 1), (17, 11));
            }),
        ),
    ]);
}

#[test]
fn a_refused_or_scratched_relative_file_leaves_the_disk_as_the_originals_does() {
    check_disks(vec![
        scene(
            "otherlen",
            &format!("{WRITE_R}3 OPEN2,8,2,\"R,L,\"+CHR$(20)|"),
            " 50 RECORD NOT PRESENT 0  0",
            commands_with(r_written),
        ),
        scene(
            "copyrel",
            &format!("{WRITE_R}3 PRINT#15,\"C0:R2=R\"|"),
            " 71 DIR ERROR 2  2",
            commands_with(r_written),
        ),
        scene(
            "scratchrel",
            &format!("{WRITE_R}3 PRINT#15,\"S0:R\"|"),
            " 1 FILES SCRATCHED 1  0",
            commands_with(|disk| {
                r_written(disk);
                disk.release((17, 0));
                disk.release((17, 10));
                disk.retype(b"R", 0);
            }),
        ),
        scene(
            "relseq",
            "2 OPEN2,8,2,\"R,L,\"+CHR$(10):OPEN3,8,3,\"NS,S,W\":PRINT#3,\"HI\":CLOSE3|",
            " 0 OK 0  0",
            commands_with(|disk| {
                made(disk, b"R", 10, (17, 0), (17, 10));
                let (first, blocks) = disk.chain_along(&[(17, 1)], b"HI\r");
                disk.entry(super::disk::closed(kind::SEQ), b"NS", first, blocks);
            }),
        ),
    ]);
}

/// A block of `image`.
fn block(image: &[u8], (track, sector): (u8, u8)) -> &[u8] {
    &image[offset(track, sector)..][..256]
}

/// Whether the BAM of `image` records `(track, sector)` used.
fn used(image: &[u8], (track, sector): (u8, u8)) -> bool {
    let at = offset(DIRECTORY_TRACK, 0)
        + usize::from(bam::TRACK_ENTRY_BYTES) * usize::from(track)
        + 1
        + usize::from(sector / 8);
    image[at] & (1 << (sector % 8)) == 0
}

/// The data blocks a side sector lists.
fn listed(image: &[u8], at: (u8, u8)) -> Vec<(u8, u8)> {
    block(image, at)[usize::from(side::POINTERS)..]
        .chunks(2)
        .map(|pair| (pair[0], pair[1]))
        .take_while(|&(track, _)| track != 0)
        .collect()
}

/// The emulated time 241 data blocks take to add, the original's two
/// minutes and more.
const EXTENDED: u64 = 6000 * JIFFY;

/// The disk after `B`, records of 254 bytes and so one data block a
/// record, is made and written by `lines`, then closed.
fn whole_blocks(lines: &str) -> Vec<u8> {
    let keys = format!(
        "{OPENING}2 OPEN2,8,2,\"B,L,\"+CHR$(254):{lines}:CLOSE2|{}",
        &STATUS[..STATUS.len() - 2]
    );
    let mut machine = played(surface(&commands().image()), None, &keys);
    let end = machine.cycles() + EXTENDED;
    while !screen_rows(&machine)
        .iter()
        .any(|row| row == " 50 RECORD NOT PRESENT 0  0")
    {
        assert!(machine.cycles() < end, "the file was extended");
        run_for(&mut machine, 10 * JIFFY);
    }
    image_of(&machine)
}

/// `B`'s entry: its first side sector, record length and blocks.
fn entry_of_b(image: &[u8]) -> ((u8, u8), u8, u16) {
    let sector = block(image, (DIRECTORY_TRACK, 1));
    let found = sector
        .chunks(usize::from(entry::BYTES))
        .find(|slot| slot[usize::from(entry::NAME)] == b'B' && slot[usize::from(entry::TYPE)] != 0)
        .expect("B's entry");
    assert_eq!(found[usize::from(entry::TYPE)], kind::REL | kind::CLOSED);
    let at = |field: u8| found[usize::from(field)];
    (
        (at(entry::SIDE), at(entry::SIDE + 1)),
        at(entry::RECORD_LENGTH),
        u16::from_le_bytes([at(entry::BLOCKS), at(entry::BLOCKS + 1)]),
    )
}

/// A block opening a side sector is followed by a spare it links to, and
/// the side sector after that, as the original takes them: a file going on
/// takes the spare next, and one ending there keeps it, uncounted and in no
/// list. A file of 241 blocks opens two.
#[test]
fn a_block_opening_a_side_sector_is_followed_by_a_spare() {
    let sides = [(17, 10), (12, 9), (6, 19)];
    let image = whole_blocks("PRINT#15,P$CHR$(241)Z$O$:PRINT#2,\"END\"");
    assert_eq!(entry_of_b(&image), ((17, 10), 254, 241 + 3));
    let mut blocks = Vec::new();
    for (number, &at) in sides.iter().enumerate() {
        let sector = block(&image, at);
        let table: Vec<(u8, u8)> = sector[usize::from(side::TABLE)..usize::from(side::POINTERS)]
            .chunks(2)
            .map(|pair| (pair[0], pair[1]))
            .take_while(|&(track, _)| track != 0)
            .collect();
        assert_eq!(table, sides, "side sector {number}'s table");
        assert_eq!(usize::from(sector[usize::from(side::NUMBER)]), number);
        blocks.extend(listed(&image, at));
    }
    assert_eq!(blocks.len(), 241, "the blocks listed");
    assert_eq!(&blocks[..4], &[(17, 0), (17, 11), (17, 1), (17, 12)]);
    // The spare after block 120 is block 121; the one after 240 is left.
    assert_eq!(&blocks[119..122], &[(12, 4), (12, 14), (12, 2)]);
    assert_eq!(blocks.last(), Some(&(6, 8)), "the last block");
    let tail = block(&image, (6, 8));
    assert_eq!((tail[0], tail[1]), (6, 18), "the last links to the spare");
    assert_eq!(&tail[2..6], b"END\r");
    assert!(used(&image, (6, 18)), "the spare is used");
    assert!(!blocks.contains(&(6, 18)), "the spare is in no list");
}

/// An extension adding a single block opens a side sector with no spare,
/// as the original's does, the next block the next free after the side
/// sector; one adding more takes the spare. Each grows a file of 120 blocks.
#[test]
fn a_single_block_opening_a_side_sector_takes_no_spare() {
    for (name, lines, side, after) in [
        (
            "one at a time",
            "PRINT#15,P$CHR$(120)Z$O$:PRINT#2,\"X\"|3 PRINT#15,P$CHR$(121)Z$O$:PRINT#2,\"Y\"|4 PRINT#15,P$CHR$(122)Z$O$:PRINT#2,\"Z\"",
            (12, 2),
            (12, 9),
        ),
        (
            "two at once",
            "PRINT#15,P$CHR$(120)Z$O$:PRINT#2,\"X\"|3 PRINT#15,P$CHR$(122)Z$O$:PRINT#2,\"Z\"",
            (12, 9),
            (12, 2),
        ),
    ] {
        let image = whole_blocks(lines);
        assert_eq!(entry_of_b(&image), ((17, 10), 254, 122 + 2), "{name}");
        let blocks: Vec<(u8, u8)> = [(17, 10), side]
            .iter()
            .flat_map(|&at| listed(&image, at))
            .collect();
        assert_eq!(&blocks[119..], &[(12, 4), (12, 14), after], "{name}");
        let middle = block(&image, (12, 14));
        assert_eq!((middle[0], middle[1]), after, "{name}: block 120 links on");
        let tail = block(&image, after);
        assert_eq!((tail[0], tail[1]), (0, 255), "{name}: the last block");
        assert_eq!(&tail[2..4], b"Z\r", "{name}");
    }
}

/// A long record's bytes read one at a time, the disk turning from the
/// reopen's reads: each is sent once, though BYTE READY sets V through SO as
/// they go, as V is never what a read's state is taken from. The program
/// counts bytes read other than written.
#[test]
fn a_record_read_a_byte_at_a_time_sends_each_byte_once() {
    let rows = screen_of(
        &commands().image(),
        None,
        "1 OPEN15,8,15:OPEN2,8,2,\"R,L,\"+CHR$(200):P$=\"P\"+CHR$(98)+CHR$(1)+CHR$(0)|2 A$=\"ABCDEFGHIJKLMNOPQRSTUVWXYZ\":A$=A$+A$:A$=A$+A$:A$=LEFT$(A$+A$,199)|3 PRINT#15,P$+CHR$(1):PRINT#2,A$;:CLOSE2:OPEN2,8,2,\"R\":PRINT#15,P$+CHR$(1)|4 FORI=0TO198:GET#2,B$:IFASC(B$)<>65+I-INT(I/26)*26THENE=E+1|5 NEXT:PRINTE|RUN|~~~",
    );
    let run = rows
        .iter()
        .position(|row| row == "RUN")
        .expect("the program ran");
    assert_eq!(
        rows.get(run + 1).map(String::as_str),
        Some(" 0"),
        "{rows:#?}"
    );
}

/// Commands alone, the disk kept turning by the first's `I`: the drive
/// never takes CLOCK, as only a talker does, though BYTE READY sets V
/// through SO throughout.
#[test]
fn a_drive_whose_disk_turns_talks_only_when_asked() {
    let mut machine = with_drive(surface(&commands().image()));
    press(&mut machine, &[Key::LEFT_SHIFT], Key::HOME);
    if let Some(drive) = machine.drive_mut(Unit::Eight) {
        drive.monitor_io(true);
    }
    type_text(
        &mut machine,
        "10 OPEN15,8,15,\"I\":FORI=1TO200:PRINT#15,\"X\":NEXT\nRUN\n",
    );
    let mut taken = Vec::new();
    for _ in 0..6000 {
        run_for(&mut machine, 500);
        if let Some(drive) = machine.drive_mut(Unit::Eight) {
            taken.extend(
                drive
                    .take_io_trace()
                    .filter(|access| {
                        access.write
                            && access.addr & 0x1C0F == VIA1 + u16::from(reg::ORB)
                            && access.value & bus::CLOCK_OUT != 0
                    })
                    .map(|access| access.cycle),
            );
        }
    }
    assert!(
        taken.is_empty(),
        "CLOCK taken unasked at drive cycles {taken:?}"
    );
}
