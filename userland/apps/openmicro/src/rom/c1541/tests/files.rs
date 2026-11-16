//! The file layer and the listing on the true drive, this tree's KERNAL
//! and BASIC driving it: what each LOAD, OPEN and read gives is what the
//! original DOS gives with the same disk in the same drive.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::disk::{closed, odd_chains, printing, Disk};
use super::writing::commands;
use super::{played, surface, with_drive};
use crate::machine::c64::Machine;
use crate::media::c1541::layout::kind;
use crate::media::prg::Program;
use crate::rom::asm::{abs, byte, hi, imm, lo, Acc, Asm};
use crate::rom::c64::kernal::entry;
use crate::rom::c64::testing::{run_for, screen_rows, type_text, JIFFY};

/// Entries of every shape a listing shows, each the same short program.
fn catalogue() -> Vec<u8> {
    let mut disk = Disk::new(b"PROBE", *b"AB");
    let (hello, _) = disk.chain(&printing(b"HI"));
    let entries: [(u8, &[u8], u16); 27] = [
        (closed(kind::PRG), b"HELLO", 1),
        (closed(kind::PRG), b"HIDE\xa0,8,1", 9),
        (closed(kind::PRG), b"ABCDEFGHIJKLMNOP", 10),
        (closed(kind::SEQ), b"A\"B", 99),
        (0, b"SCRATCHED", 100),
        (closed(kind::DEL), b"DELFILE", 999),
        (closed(kind::USR), b"USRFILE", 1000),
        (closed(kind::REL), b"RELF", 9999),
        (closed(5), b"KIND5", 10000),
        (closed(kind::PRG), b"MOST", 65535),
        (closed(kind::PRG), b"NONE", 0),
        (kind::SEQ, b"OPENSEQ", 5),
        (kind::PRG, b"OPENPRG", 5),
        (closed(kind::PRG) | kind::LOCKED, b"LOCKED", 5),
        (kind::PRG | kind::LOCKED, b"LOCKOPEN", 5),
        (kind::LOCKED, b"LOCKDEL", 5),
        (closed(kind::PRG), b"X\xa0Y\xa0Z", 7),
        (closed(kind::PRG), b"", 3),
        (closed(kind::PRG), b"\xa0TEXT", 2),
        (closed(0x0A), b"KINDA", 4),
        (0x20, b"BIT5", 4),
        (closed(0x10), b"BIT4", 1),
        (closed(kind::PRG), b"ABCDEFG\"IJKLMNOP", 1),
        (closed(kind::PRG), b"AB\"", 1),
        (closed(kind::PRG), b"\"LEAD", 1),
        (closed(kind::PRG), b"A\"\xa0B", 1),
        (closed(kind::PRG), b"A\xa0\"B", 1),
    ];
    for (kind, name, blocks) in entries {
        disk.entry(kind, name, hello, blocks);
    }
    disk.image()
}

/// A name with a shifted space in it, and the BAM's bytes about the ID
/// that a listing does not show.
fn odd_header() -> Vec<u8> {
    let mut disk = Disk::new(b"DISK\xa0HID", *b"CD");
    disk.file(b"ONE", kind::PRG, &printing(b"ONE"));
    disk.bam(0xA0, b'X');
    disk.bam(0xA1, b'Y');
    disk.bam(0xA4, b'Z');
    for offset in 0xA7..0xAB {
        disk.bam(offset, b'W');
    }
    disk.image()
}

/// A sequential file first, then two programs.
fn order() -> Vec<u8> {
    let mut disk = Disk::new(b"ORDER", *b"OR");
    disk.file(b"FIRSTSEQ", kind::SEQ, b"SEQUENTIAL DATA\r");
    disk.file(b"SECOND", kind::PRG, &printing(b"SECOND"));
    disk.file(b"THIRD", kind::PRG, &printing(b"THIRD"));
    disk.image()
}

/// Names a search meets twice, or sixteen characters long.
fn dupes() -> Vec<u8> {
    let mut disk = Disk::new(b"DUPES", *b"DU");
    disk.file(b"DUP", kind::DEL, &printing(b"DELCONTENT"));
    disk.file(b"DUP", kind::PRG, &printing(b"PRGB"));
    let (open, blocks) = disk.chain(&printing(b"OPENC"));
    disk.entry(kind::PRG, b"OPEN", open, blocks);
    disk.file(b"OPEN", kind::PRG, &printing(b"OPEND"));
    disk.file(b"ABCDEFGHIJKLMNOP", kind::PRG, &printing(b"SIXTEENP"));
    disk.file(b"ABCDEFGHIJKLMNOQ", kind::PRG, &printing(b"SIXTEENQ"));
    disk.file(b"SEQFILE", kind::SEQ, b"SEQ DATA\r");
    disk.file(b"USRFILE", kind::USR, b"USR DATA\r");
    disk.image()
}

fn chains() -> Vec<u8> {
    odd_chains().image()
}

/// Names a short pattern meets more than once.
fn shorts() -> Vec<u8> {
    let mut disk = Disk::new(b"SHORTS", *b"SH");
    for (index, name) in [
        &b"\xa0TEXT"[..],
        b"\xa0\xa0X",
        b"",
        b"A\xa0B",
        b"AB\xa0C",
        b"A\xa0\xa0\xa0Q",
    ]
    .iter()
    .enumerate()
    {
        disk.file(
            name,
            kind::PRG,
            &printing(&[b'0' + u8::try_from(index).unwrap_or(0)]),
        );
    }
    disk.image()
}

/// Lines INPUT# reads: empty, numbers, a letter, fields.
fn lines() -> Vec<u8> {
    let mut disk = Disk::new(b"NUMBERS", *b"NU");
    for (name, text) in [
        (&b"CRONLY"[..], &b"\r"[..]),
        (b"CRFIVE", b"\r5\r"),
        (b"LETTER", b"X\r"),
        (b"FIVE", b"5\r"),
        (b"CRCRFIVE", b"\r\r5\r"),
        (b"FIVESIX", b"5,6\r"),
        (b"COMMA", b",6\r"),
        (b"CRWORD", b"\rAB\r"),
        (b"SPACE", b" \r7\r"),
    ] {
        disk.file(name, kind::SEQ, text);
    }
    disk.image()
}

/// The screen after CLR and `keys` typed, as [`played`] types them.
pub(super) fn screen_of(disk: &[u8], second: Option<&[u8]>, keys: &str) -> Vec<String> {
    screen_rows(&played(surface(disk), second, keys))
}

/// A machine with `disk` in the drive, a scene typed and the screen the
/// original DOS leaves with the same disk.
pub(super) struct Scene {
    pub(super) name: &'static str,
    pub(super) disk: Vec<u8>,
    pub(super) second: Option<Vec<u8>>,
    pub(super) keys: &'static str,
    pub(super) screen: &'static [&'static str],
}

pub(super) fn check(scenes: Vec<Scene>) {
    for scene in scenes {
        assert_eq!(
            screen_of(&scene.disk, scene.second.as_deref(), scene.keys),
            scene.screen,
            "{}",
            scene.name
        );
    }
}

/// A scene on the commands' disk: its name, the keys typed, and the screen
/// the original leaves.
pub(super) type Screen = (&'static str, &'static str, &'static [&'static str]);

pub(super) fn on_commands(scenes: &[Screen]) {
    check(
        scenes
            .iter()
            .map(|&(name, keys, screen)| Scene {
                name,
                disk: commands().image(),
                second: None,
                keys,
                screen,
            })
            .collect(),
    );
}

/// Where the loading routine runs, keeps its outcome and loads to.
const ROUTINE: u16 = 0xC000;
const OUTCOME: u16 = 0xC0F0;
const DESTINATION: u16 = 0xC100;
const DONE_MARK: u8 = 0x5A;

/// What LOAD reads for `command` on device 8, loaded by the KERNAL alone
/// so nothing relinks it: each line as its number and text, bytes outside
/// the printable set as themselves. None where the LOAD fails.
fn listing(machine: &mut Machine, command: &'static [u8]) -> Option<Vec<String>> {
    let mut a = Asm::new(ROUTINE, 0x100);
    a.lda(imm(byte(command.len())));
    a.ldx(imm(lo("command")));
    a.ldy(imm(hi("command")));
    a.jsr(entry::SETNAM);
    a.lda(imm(1));
    a.ldx(imm(8));
    a.ldy(imm(0));
    a.jsr(entry::SETLFS);
    a.lda(imm(0));
    a.ldx(imm(lo(DESTINATION)));
    a.ldy(imm(hi(DESTINATION)));
    a.jsr(entry::LOAD);
    a.stx(abs(OUTCOME));
    a.sty(abs(OUTCOME + 1));
    a.lda(imm(0));
    a.rol(Acc);
    a.sta(abs(OUTCOME + 2));
    a.lda(imm(DONE_MARK));
    a.sta(abs(OUTCOME + 3));
    a.rts();
    a.here("command");
    a.bytes(command);
    let routine = a.finish().expect("the loading routine assembles");
    machine.load_into_memory(&Program {
        address: ROUTINE,
        bytes: routine.bytes,
    });
    machine.load_into_memory(&Program {
        address: OUTCOME + 3,
        bytes: vec![0],
    });
    type_text(machine, "SYS49152\n");
    let end = machine.cycles() + 60 * crate::machine::c64::Standard::Pal.clock();
    while machine.ram(OUTCOME + 3) != DONE_MARK {
        assert!(machine.cycles() < end, "the LOAD of {command:?} finished");
        run_for(machine, 10 * JIFFY);
    }
    if machine.ram(OUTCOME + 2) != 0 {
        return None;
    }
    let stop = u16::from_le_bytes([machine.ram(OUTCOME), machine.ram(OUTCOME + 1)]);
    let bytes: Vec<u8> = (DESTINATION..stop).map(|at| machine.ram(at)).collect();
    let mut lines = Vec::new();
    let mut at = 0;
    while bytes[at..at + 2] != [0, 0] {
        assert_eq!(bytes[at..at + 2], [1, 1], "each line's link in {command:?}");
        let number = u16::from_le_bytes([bytes[at + 2], bytes[at + 3]]);
        let length = bytes[at + 4..]
            .iter()
            .position(|&byte| byte == 0)
            .expect("each line ends");
        let text: String = bytes[at + 4..at + 4 + length]
            .iter()
            .map(|&byte| char::from(byte))
            .collect();
        lines.push(alloc::format!("{number} {text}"));
        at += 4 + length + 1;
    }
    assert_eq!(
        at + 2,
        bytes.len(),
        "the program ends where LOAD stopped, in {command:?}"
    );
    Some(lines)
}

/// The names of a listing's files, as it shows them in quotes.
fn names(lines: &[String]) -> Vec<String> {
    lines[1..lines.len() - 1]
        .iter()
        .map(|line| {
            let text = line
                .split_once(' ')
                .map_or("", |(_, text)| text)
                .trim_start();
            String::from(text.chars().take(18).collect::<String>().trim_end())
        })
        .collect()
}

#[test]
fn a_listing_is_the_originals_line_for_line() {
    let mut machine = with_drive(surface(&catalogue()));
    // The original shows the bytes after its table of kinds for a kind past
    // REL; this DOS shows ???.
    assert_eq!(
        listing(&mut machine, b"$"),
        Some(
            [
                "0 \u{12}\"PROBE           \" AB 2A",
                "1    \"HELLO\"            PRG  ",
                "9    \"HIDE\",8,1         PRG  ",
                "10   \"ABCDEFGHIJKLMNOP\" PRG   ",
                "99   \"A\"B               SEQ   ",
                "999  \"DELFILE\"          DEL    ",
                "1000  \"USRFILE\"          USR    ",
                "9999  \"RELF\"             REL    ",
                "10000  \"KIND5\"            ???    ",
                "65535  \"MOST\"             PRG    ",
                "0    \"NONE\"             PRG  ",
                "5    \"OPENSEQ\"         *SEQ  ",
                "5    \"OPENPRG\"         *PRG  ",
                "5    \"LOCKED\"           PRG< ",
                "5    \"LOCKOPEN\"        *PRG< ",
                "5    \"LOCKDEL\"         *DEL< ",
                "7    \"X\"Y Z             PRG  ",
                "3    \"\"                 PRG  ",
                "2    \"\"TEXT             PRG  ",
                "4    \"KINDA\"            ???  ",
                "4    \"BIT5\"            *DEL  ",
                "1    \"BIT4\"             DEL  ",
                "1    \"ABCDEFG\"IJKLMNOP  PRG  ",
                "1    \"AB\"               PRG  ",
                "1    \"\"LEAD             PRG  ",
                "1    \"A\" B              PRG  ",
                "1    \"A\"\"B              PRG  ",
                "663 BLOCKS FREE.             ",
            ]
            .map(String::from)
            .to_vec()
        )
    );
}

#[test]
fn a_listing_shows_the_bams_name_and_id_as_the_original_does() {
    let mut machine = with_drive(surface(&odd_header()));
    assert_eq!(
        listing(&mut machine, b"$"),
        Some(
            [
                "0 \u{12}\"DISK HID        \" CDZ2A",
                "1    \"ONE\"              PRG  ",
                "663 BLOCKS FREE.             "
            ]
            .map(String::from)
            .to_vec()
        )
    );
}

/// Listing commands and the names each lists, as the original lists them.
const PATTERN_CASES: [(&[u8], &[&str]); 18] = [
    (b"$:H*", &["\"HELLO\"", "\"HIDE\",8,1"]),
    (b"$0:H*", &["\"HELLO\"", "\"HIDE\",8,1"]),
    (b"$:*=S", &["\"A\"B", "\"OPENSEQ\""]),
    // The kind asked for after `=` holds for every pattern.
    (b"$:H*,A*=S", &["\"A\"B"]),
    // An empty pattern names the file without a name, except at the end.
    (b"$:,H*", &["\"HELLO\"", "\"HIDE\",8,1", "\"\""]),
    (b"$:H*,", &["\"HELLO\"", "\"HIDE\",8,1"]),
    (b"$:", &[]),
    (b"$=P", &["\"\""]),
    (b"$X", &["\"X\"Y Z"]),
    (b"$X*", &["\"X\"Y Z"]),
    // A pattern without a wildcard lists one file, and the fifth's
    // wildcards are not seen; a sixth is ignored.
    (
        b"$:A*,B*,D*,H*,K*,L*",
        &[
            "\"HELLO\"",
            "\"HIDE\",8,1",
            "\"ABCDEFGHIJKLMNOP\"",
            "\"A\"B",
            "\"DELFILE\"",
            "\"KIND5\"",
            "\"BIT5\"",
            "\"BIT4\"",
            "\"ABCDEFG\"IJKLMNOP",
            "\"AB\"",
            "\"A\" B",
            "\"A\"\"B",
        ],
    ),
    (
        b"$:HELLO,A*,B*,D*,H*",
        &[
            "\"HELLO\"",
            "\"ABCDEFGHIJKLMNOP\"",
            "\"A\"B",
            "\"DELFILE\"",
            "\"BIT5\"",
            "\"BIT4\"",
            "\"ABCDEFG\"IJKLMNOP",
            "\"AB\"",
            "\"A\" B",
            "\"A\"\"B",
        ],
    ),
    (
        b"$:L*,A*,B*,D*",
        &[
            "\"ABCDEFGHIJKLMNOP\"",
            "\"A\"B",
            "\"DELFILE\"",
            "\"LOCKED\"",
            "\"LOCKOPEN\"",
            "\"LOCKDEL\"",
            "\"BIT5\"",
            "\"BIT4\"",
            "\"ABCDEFG\"IJKLMNOP",
            "\"AB\"",
            "\"A\" B",
            "\"A\"\"B",
        ],
    ),
    (b"$:*=R", &["\"RELF\""]),
    (b"$:*=U", &["\"USRFILE\""]),
    (
        b"$:*=PRG",
        &[
            "\"HELLO\"",
            "\"HIDE\",8,1",
            "\"ABCDEFGHIJKLMNOP\"",
            "\"MOST\"",
            "\"NONE\"",
            "\"OPENPRG\"",
            "\"LOCKED\"",
            "\"LOCKOPEN\"",
            "\"X\"Y Z",
            "\"\"",
            "\"\"TEXT",
            "\"KINDA\"",
            "\"ABCDEFG\"IJKLMNOP",
            "\"AB\"",
            "\"\"LEAD",
            "\"A\" B",
            "\"A\"\"B",
        ],
    ),
    // A shifted space ends a name for a pattern, but `?` never matches it.
    (b"$:A", &["\"A\"\"B"]),
    (b"$:AB?", &["\"AB\""]),
];

#[test]
fn patterns_choose_what_a_listing_shows_as_the_originals_do() {
    let mut machine = with_drive(surface(&catalogue()));
    let every = listing(&mut machine, b"$")
        .map(|lines| names(&lines))
        .expect("the whole listing");
    for (command, expected) in PATTERN_CASES {
        let shown = listing(&mut machine, command)
            .map(|lines| names(&lines))
            .expect("a listing");
        assert_eq!(shown, expected, "{}", String::from_utf8_lossy(command));
    }
    // A lone drive number, or a letter naming no kind, lists every file.
    for command in [&b"$0"[..], b"$:*=D"] {
        assert_eq!(
            listing(&mut machine, command).map(|lines| names(&lines)),
            Some(every.clone()),
            "{}",
            String::from_utf8_lossy(command)
        );
    }
}

#[test]
fn a_pattern_without_a_wildcard_lists_only_the_first_file_it_names() {
    let mut machine = with_drive(surface(&shorts()));
    let cases: [(&[u8], &[&str]); 5] = [
        (b"$=P", &["\"\"TEXT"]),
        (b"$:A", &["\"A\"B"]),
        (b"$:A?", &["\"AB\"C"]),
        (b"$:A*", &["\"A\"B", "\"AB\"C", "\"A\"  Q"]),
        (b"$:,A", &["\"\"TEXT", "\"A\"B"]),
    ];
    for (command, expected) in cases {
        let shown = listing(&mut machine, command)
            .map(|lines| names(&lines))
            .expect("a listing");
        assert_eq!(shown, expected, "{}", String::from_utf8_lossy(command));
    }
}

#[test]
fn names_open_their_files_as_the_originals_do() {
    check(vec![
        Scene {
            name: "dup",
            disk: dupes(),
            second: None,
            keys: "LOAD\"DUP\",8|~9 OPEN15,8,15:INPUT#15,A,B$,C,D:PRINTA;B$;C;D:CLOSE15|RUN 9|~",
            screen: &[
            "LOAD\"DUP\",8",
            "",
            "SEARCHING FOR DUP",
            "?FILE NOT FOUND  ERROR",
            "READY.",
            "9 OPEN15,8,15:INPUT#15,A,B$,C,D:PRINTA;B",
            "$;C;D:CLOSE15",
            "RUN 9",
            " 64 FILE TYPE MISMATCH 0  0",
            "",
            "READY.",
            ],
        },
        Scene {
            name: "open",
            disk: dupes(),
            second: None,
            keys: "LOAD\"OPEN\",8|~9 OPEN15,8,15:INPUT#15,A,B$,C,D:PRINTA;B$;C;D:CLOSE15|RUN 9|~",
            screen: &[
            "LOAD\"OPEN\",8",
            "",
            "SEARCHING FOR OPEN",
            "?FILE NOT FOUND  ERROR",
            "READY.",
            "9 OPEN15,8,15:INPUT#15,A,B$,C,D:PRINTA;B",
            "$;C;D:CLOSE15",
            "RUN 9",
            " 60 WRITE FILE OPEN 0  0",
            "",
            "READY.",
            ],
        },
        Scene {
            name: "sixteen",
            disk: dupes(),
            second: None,
            keys: "LOAD\"ABCDEFGHIJKLMNOPQ\",8|~LIST|9 OPEN15,8,15:INPUT#15,A,B$,C,D:PRINTA;B$;C;D:CLOSE15|RUN 9|~",
            screen: &[
            "LOAD\"ABCDEFGHIJKLMNOPQ\",8",
            "",
            "SEARCHING FOR ABCDEFGHIJKLMNOPQ",
            "LOADING",
            "READY.",
            "LIST",
            "",
            "10 PRINT\"SIXTEENP\"",
            "READY.",
            "9 OPEN15,8,15:INPUT#15,A,B$,C,D:PRINTA;B",
            "$;C;D:CLOSE15",
            "RUN 9",
            " 0 OK 0  0",
            "SIXTEENP",
            "",
            "READY.",
            ],
        },
    ]);
}

#[test]
fn the_second_drive_and_the_modes_open_as_the_originals_do() {
    check(vec![
        Scene {
            name: "drive1",
            disk: dupes(),
            second: None,
            keys: "LOAD\"1:DUP\",8|~9 OPEN15,8,15:INPUT#15,A,B$,C,D:PRINTA;B$;C;D:CLOSE15|RUN 9|~",
            screen: &[
            "LOAD\"1:DUP\",8",
            "",
            "SEARCHING FOR 1:DUP",
            "?FILE NOT FOUND  ERROR",
            "READY.",
            "9 OPEN15,8,15:INPUT#15,A,B$,C,D:PRINTA;B",
            "$;C;D:CLOSE15",
            "RUN 9",
            " 74 DRIVE NOT READY 0  0",
            "",
            "READY.",
            ],
        },
        Scene {
            name: "reads",
            disk: dupes(),
            second: None,
            keys: "1 OPEN15,8,15:FORI=1TO6:READN$:PRINTI;N$:OPEN2,8,2,N$:GET#2,A$|2 INPUT#15,E,E$:PRINTN$;ASC(A$+CHR$(0));E;E$:CLOSE2:NEXT|3 DATADUP,\"SEQFILE,X\",\"SEQFILE,R\",\"SEQFILE,S,R\",\"USRFILE,S\",\":\"|RUN|~~",
            screen: &[
            "1 OPEN15,8,15:FORI=1TO6:READN$:PRINTI;N$",
            ":OPEN2,8,2,N$:GET#2,A$",
            "2 INPUT#15,E,E$:PRINTN$;ASC(A$+CHR$(0));",
            "E;E$:CLOSE2:NEXT",
            "3 DATADUP,\"SEQFILE,X\",\"SEQFILE,R\",\"SEQFI",
            "LE,S,R\",\"USRFILE,S\",\":\"",
            "RUN",
            " 1 DUP",
            "DUP 1  0 OK",
            " 2 SEQFILE,X",
            "SEQFILE,X 83  0 OK",
            " 3 SEQFILE,R",
            "SEQFILE,R 83  0 OK",
            " 4 SEQFILE,S,R",
            "SEQFILE,S,R 83  0 OK",
            " 5 USRFILE,S",
            "USRFILE,S 199  64 FILE TYPE MISMATCH",
            " 6 :",
            ": 199  62 FILE NOT FOUND",
            "",
            "READY.",
            ],
        },
    ]);
}

#[test]
fn the_last_program_loads_again_as_the_originals_does() {
    check(vec![
        Scene {
            name: "star",
            disk: order(),
            second: None,
            keys: "LOAD\"*\",8|~LIST|9 OPEN15,8,15:INPUT#15,A,B$,C,D:PRINTA;B$;C;D:CLOSE15|RUN 9|~",
            screen: &[
            "LOAD\"*\",8",
            "",
            "SEARCHING FOR *",
            "LOADING",
            "READY.",
            "LIST",
            "",
            "10 PRINT\"SECOND\"",
            "READY.",
            "9 OPEN15,8,15:INPUT#15,A,B$,C,D:PRINTA;B",
            "$;C;D:CLOSE15",
            "RUN 9",
            " 0 OK 0  0",
            "SECOND",
            "",
            "READY.",
            ],
        },
        Scene {
            name: "starafter",
            disk: order(),
            second: None,
            keys: "LOAD\"THIRD\",8|~LOAD\"*\",8|~LIST|9 OPEN15,8,15:INPUT#15,A,B$,C,D:PRINTA;B$;C;D:CLOSE15|RUN 9|~",
            screen: &[
            "LOAD\"THIRD\",8",
            "",
            "SEARCHING FOR THIRD",
            "LOADING",
            "READY.",
            "LOAD\"*\",8",
            "",
            "SEARCHING FOR *",
            "LOADING",
            "READY.",
            "LIST",
            "",
            "10 PRINT\"THIRD\"",
            "READY.",
            "9 OPEN15,8,15:INPUT#15,A,B$,C,D:PRINTA;B",
            "$;C;D:CLOSE15",
            "RUN 9",
            " 0 OK 0  0",
            "THIRD",
            "",
            "READY.",
            ],
        },
        Scene {
            name: "stars",
            disk: order(),
            second: None,
            keys: "LOAD\"*,S\",8|~9 OPEN15,8,15:INPUT#15,A,B$,C,D:PRINTA;B$;C;D:CLOSE15|RUN 9|~",
            screen: &[
            "LOAD\"*,S\",8",
            "",
            "SEARCHING FOR *,S",
            "?FILE NOT FOUND  ERROR",
            "READY.",
            "9 OPEN15,8,15:INPUT#15,A,B$,C,D:PRINTA;B",
            "$;C;D:CLOSE15",
            "RUN 9",
            " 64 FILE TYPE MISMATCH 0  0",
            "",
            "READY.",
            ],
        },
        Scene {
            name: "zerostar",
            disk: order(),
            second: None,
            keys: "LOAD\"0:*\",8|~9 OPEN15,8,15:INPUT#15,A,B$,C,D:PRINTA;B$;C;D:CLOSE15|RUN 9|~",
            screen: &[
            "LOAD\"0:*\",8",
            "",
            "SEARCHING FOR 0:*",
            "?FILE NOT FOUND  ERROR",
            "READY.",
            "9 OPEN15,8,15:INPUT#15,A,B$,C,D:PRINTA;B",
            "$;C;D:CLOSE15",
            "RUN 9",
            " 64 FILE TYPE MISMATCH 0  0",
            "",
            "READY.",
            ],
        },
    ]);
}

#[test]
fn the_channels_and_a_changed_disk_keep_what_the_originals_keep() {
    check(vec![
        Scene {
            name: "datastar",
            disk: order(),
            second: None,
            keys: "1 OPEN2,8,2,\"*\":GET#2,A$:PRINTA$|RUN|~9 OPEN15,8,15:INPUT#15,A,B$,C,D:PRINTA;B$;C;D:CLOSE15|RUN 9|~",
            screen: &[
            "1 OPEN2,8,2,\"*\":GET#2,A$:PRINTA$",
            "RUN",
            "S",
            "",
            "READY.",
            "9 OPEN15,8,15:INPUT#15,A,B$,C,D:PRINTA;B",
            "$;C;D:CLOSE15",
            "RUN 9",
            " 0 OK 0  0",
            "",
            "READY.",
            ],
        },
        Scene {
            name: "swap",
            disk: order(),
            second: Some(dupes()),
            keys: "LOAD\"THIRD\",8|~{LOAD\"*\",8|~9 OPEN15,8,15:INPUT#15,A,B$,C,D:PRINTA;B$;C;D:CLOSE15|RUN 9|~",
            screen: &[
            "LOAD\"THIRD\",8",
            "",
            "SEARCHING FOR THIRD",
            "LOADING",
            "READY.",
            "LOAD\"*\",8",
            "",
            "SEARCHING FOR *",
            "?FILE NOT FOUND  ERROR",
            "READY.",
            "9 OPEN15,8,15:INPUT#15,A,B$,C,D:PRINTA;B",
            "$;C;D:CLOSE15",
            "RUN 9",
            " 29 DISK ID MISMATCH 1  2",
            "THIRD",
            "",
            "READY.",
            ],
        },
        Scene {
            name: "full",
            disk: order(),
            second: None,
            keys: "1 OPEN15,8,15:FORI=2TO5:OPENI,8,I,\"THIRD\":INPUT#15,E,E$:PRINTI;E;E$:NEXT|2 OPEN6,8,0,\"$\":INPUT#15,E,E$:PRINT6;E;E$|RUN|~~",
            screen: &[
            "1 OPEN15,8,15:FORI=2TO5:OPENI,8,I,\"THIRD",
            "\":INPUT#15,E,E$:PRINTI;E;E$:NEXT",
            "2 OPEN6,8,0,\"$\":INPUT#15,E,E$:PRINT6;E;E",
            "$",
            "RUN",
            " 2  0 OK",
            " 3  0 OK",
            " 4  0 OK",
            " 5  70 NO CHANNEL",
            " 6  70 NO CHANNEL",
            "",
            "READY.",
            ],
        },
    ]);
}

#[test]
fn odd_chains_read_as_the_originals_do() {
    check(vec![
        Scene {
            name: "chains",
            disk: chains(),
            second: None,
            keys: "1 OPEN15,8,15:FORF=1TO7:READN$:OPEN2,8,2,N$:C=0:K=0|2 GET#2,A$:S=ST:C=C+1:K=K+ASC(A$+CHR$(0)):IFS=0ANDC<300THEN2|3 INPUT#15,E,E$,T,U:PRINTN$;C;ASC(A$+CHR$(0));K;S;E;T;U:CLOSE2:NEXT|4 DATAEMPTY1,ONEBYTE,TWOBYTE,TWOSEQ,BADTRACK,ENTRYBAD,ENTRYT0|RUN|~~~~~~~~~~",
            screen: &[
            "1 OPEN15,8,15:FORF=1TO7:READN$:OPEN2,8,2",
            ",N$:C=0:K=0",
            "2 GET#2,A$:S=ST:C=C+1:K=K+ASC(A$+CHR$(0)",
            "):IFS=0ANDC<300THEN2",
            "3 INPUT#15,E,E$,T,U:PRINTN$;C;ASC(A$+CHR",
            "$(0));K;S;E;T;U:CLOSE2:NEXT",
            "4 DATAEMPTY1,ONEBYTE,TWOBYTE,TWOSEQ,BADT",
            "RACK,ENTRYBAD,ENTRYT0",
            "RUN",
            "EMPTY1 254  0  162  64  0  0  0",
            "ONEBYTE 4  90  182  64  0  0  0",
            "TWOBYTE 2  88  177  64  0  0  0",
            "TWOSEQ 255  84  21166  64  0  0  0",
            "BADTRACK 254  199  17403  66  66  36  0",
            "",
            "ENTRYBAD 1  199  199  66  66  40  0",
            "ENTRYT0 1  199  199  66  62  0  0",
            "",
            "READY.",
            ],
        },
        Scene {
            name: "badsector",
            disk: chains(),
            second: None,
            keys: "1 OPEN15,8,15:OPEN2,8,2,\"BADSECTOR\":C=0|2 GET#2,A$:S=ST:C=C+1:IFS=0ANDC<300THEN2|3 INPUT#15,E,E$,T,U:PRINTC;ASC(A$+CHR$(0));S;E;E$;T;U|RUN|~~~~~~~~~~",
            screen: &[
            "1 OPEN15,8,15:OPEN2,8,2,\"BADSECTOR\":C=0",
            "2 GET#2,A$:S=ST:C=C+1:IFS=0ANDC<300THEN2",
            "",
            "3 INPUT#15,E,E$,T,U:PRINTC;ASC(A$+CHR$(0",
            "));S;E;E$;T;U",
            "RUN",
            " 254  199  66  20 READ ERROR 1  25",
            "",
            "READY.",
            ],
        },
    ]);
}

#[test]
fn kinds_and_patterns_open_and_the_directory_reads_as_the_originals_do() {
    check(vec![
        Scene {
            name: "names",
            disk: catalogue(),
            second: None,
            keys: "1 OPEN15,8,15:FORI=1TO7:READN$:OPEN2,8,0,N$:INPUT#15,E,E$:PRINTN$;E;E$|2 CLOSE2:NEXT|3 DATAKINDA,KIND5,OPENPRG,BIT5,HELLO?,HEL?O,H*O|RUN|~~",
            screen: &[
            "1 OPEN15,8,15:FORI=1TO7:READN$:OPEN2,8,0",
            ",N$:INPUT#15,E,E$:PRINTN$;E;E$",
            "2 CLOSE2:NEXT",
            "3 DATAKINDA,KIND5,OPENPRG,BIT5,HELLO?,HE",
            "L?O,H*O",
            "RUN",
            "KINDA 0 OK",
            "KIND5 64 FILE TYPE MISMATCH",
            "OPENPRG 60 WRITE FILE OPEN",
            "BIT5 60 WRITE FILE OPEN",
            "HELLO? 62 FILE NOT FOUND",
            "HEL?O 0 OK",
            "H*O 0 OK",
            "",
            "READY.",
            ],
        },
        Scene {
            name: "rawdir",
            disk: catalogue(),
            second: None,
            keys: "1 OPEN15,8,15:OPEN2,8,2,\"$\":INPUT#15,E,E$,T,U:PRINTE;E$;T;U:CLOSE2|2 PRINT#15,\"I\":OPEN2,8,2,\"$\":FORI=1TO6:GET#2,A$|3 PRINTASC(A$+CHR$(0));:NEXT:INPUT#15,E,E$:PRINTE;E$|RUN|~~",
            screen: &[
            "1 OPEN15,8,15:OPEN2,8,2,\"$\":INPUT#15,E,E",
            "$,T,U:PRINTE;E$;T;U:CLOSE2",
            "2 PRINT#15,\"I\":OPEN2,8,2,\"$\":FORI=1TO6:G",
            "ET#2,A$",
            "3 PRINTASC(A$+CHR$(0));:NEXT:INPUT#15,E,",
            "E$:PRINTE;E$",
            "RUN",
            " 29 DISK ID MISMATCH 18  0",
            " 65  0  20  254  255  31  0 OK",
            "",
            "READY.",
            ],
        },
        Scene {
            name: "closeall",
            disk: catalogue(),
            second: None,
            keys: "1 OPEN15,8,15:OPEN2,8,2,\"HELLO\":CLOSE15:GET#2,A$:PRINTASC(A$+CHR$(0));ST|RUN|~",
            screen: &[
            "1 OPEN15,8,15:OPEN2,8,2,\"HELLO\":CLOSE15:",
            "GET#2,A$:PRINTASC(A$+CHR$(0));ST",
            "RUN",
            " 199  66",
            "",
            "READY.",
            ],
        },
        Scene {
            name: "loadaddress",
            disk: catalogue(),
            second: None,
            keys: "1 OPEN2,8,0,\"$\":FORI=1TO8:GET#2,A$:PRINTASC(A$+CHR$(0));:NEXT|RUN|~",
            screen: &[
            "1 OPEN2,8,0,\"$\":FORI=1TO8:GET#2,A$:PRINT",
            "ASC(A$+CHR$(0));:NEXT",
            "RUN",
            " 1  4  1  1  0  0  18  34",
            "READY.",
            ],
        },
    ]);
}

#[test]
fn an_input_line_from_a_file_is_read_as_the_originals_reads_it() {
    check(vec![
        Scene {
            name: "crfive",
            disk: lines(),
            second: None,
            keys: "1 OPEN2,8,2,\"CRFIVE\":INPUT#2,A:PRINTA;ST|RUN|~~",
            screen: &[
                "1 OPEN2,8,2,\"CRFIVE\":INPUT#2,A:PRINTA;ST",
                "",
                "RUN",
                " 5  64",
                "",
                "READY.",
            ],
        },
        Scene {
            name: "crfivestr",
            disk: lines(),
            second: None,
            keys: "1 OPEN2,8,2,\"CRFIVE\":INPUT#2,A$:PRINTLEN(A$);A$;ST|RUN|~~",
            screen: &[
                "1 OPEN2,8,2,\"CRFIVE\":INPUT#2,A$:PRINTLEN",
                "(A$);A$;ST",
                "RUN",
                " 1 5 64",
                "",
                "READY.",
            ],
        },
        Scene {
            name: "crcrfive",
            disk: lines(),
            second: None,
            keys: "1 OPEN2,8,2,\"CRCRFIVE\":INPUT#2,A:PRINTA;ST|RUN|~~",
            screen: &[
                "1 OPEN2,8,2,\"CRCRFIVE\":INPUT#2,A:PRINTA;",
                "ST",
                "RUN",
                " 5  64",
                "",
                "READY.",
            ],
        },
        Scene {
            name: "fivesix",
            disk: lines(),
            second: None,
            keys: "1 OPEN2,8,2,\"FIVESIX\":INPUT#2,A,B:PRINTA;B;ST|RUN|~~",
            screen: &[
                "1 OPEN2,8,2,\"FIVESIX\":INPUT#2,A,B:PRINTA",
                ";B;ST",
                "RUN",
                " 5  6  64",
                "",
                "READY.",
            ],
        },
        Scene {
            name: "comma",
            disk: lines(),
            second: None,
            keys: "1 OPEN2,8,2,\"COMMA\":INPUT#2,A,B:PRINTA;B;ST|RUN|~~",
            screen: &[
                "1 OPEN2,8,2,\"COMMA\":INPUT#2,A,B:PRINTA;B",
                ";ST",
                "RUN",
                " 0  6  64",
                "",
                "READY.",
            ],
        },
        Scene {
            name: "crword",
            disk: lines(),
            second: None,
            keys: "1 OPEN2,8,2,\"CRWORD\":INPUT#2,A$:PRINTLEN(A$);A$;ST|RUN|~~",
            screen: &[
                "1 OPEN2,8,2,\"CRWORD\":INPUT#2,A$:PRINTLEN",
                "(A$);A$;ST",
                "RUN",
                " 2 AB 64",
                "",
                "READY.",
            ],
        },
    ]);
}

#[test]
fn inputs_a_file_cannot_satisfy_are_read_as_the_originals_reads_them() {
    check(vec![
        Scene {
            name: "space",
            disk: lines(),
            second: None,
            keys: "1 OPEN2,8,2,\"SPACE\":INPUT#2,A:PRINTA;ST|RUN|~~",
            screen: &[
                "1 OPEN2,8,2,\"SPACE\":INPUT#2,A:PRINTA;ST",
                "RUN",
                " 0  0",
                "",
                "READY.",
            ],
        },
        Scene {
            name: "twoline",
            disk: lines(),
            second: None,
            keys: "1 OPEN2,8,2,\"FIVE\":INPUT#2,A,B:PRINTA;B;ST|RUN|~~~~",
            screen: &[
                "1 OPEN2,8,2,\"FIVE\":INPUT#2,A,B:PRINTA;B;",
                "ST",
                "RUN",
                " 5  0  64",
                "",
                "READY.",
            ],
        },
        Scene {
            name: "letter",
            disk: lines(),
            second: None,
            keys: "1 OPEN2,8,2,\"LETTER\":INPUT#2,A:PRINTA;ST|RUN|~~",
            screen: &[
                "1 OPEN2,8,2,\"LETTER\":INPUT#2,A:PRINTA;ST",
                "",
                "RUN",
                "",
                "?FILE DATA  ERROR IN 1",
                "READY.",
            ],
        },
        Scene {
            name: "dead",
            disk: lines(),
            second: None,
            keys: "1 OPEN2,8,2,\"NOPE\":INPUT#2,A:PRINTA;ST|RUN|~~~~",
            screen: &[
                "1 OPEN2,8,2,\"NOPE\":INPUT#2,A:PRINTA;ST",
                "RUN",
                " 0  66",
                "",
                "READY.",
            ],
        },
        Scene {
            name: "deadstring",
            disk: lines(),
            second: None,
            keys: "1 OPEN2,8,2,\"NOPE\":INPUT#2,A$,B$:PRINTLEN(A$);LEN(B$);ST|RUN|~~",
            screen: &[
                "1 OPEN2,8,2,\"NOPE\":INPUT#2,A$,B$:PRINTLE",
                "N(A$);LEN(B$);ST",
                "RUN",
                " 0  0  66",
                "",
                "READY.",
            ],
        },
    ]);
}

#[test]
fn a_byte_nothing_sends_reads_as_the_bus_port_stands() {
    check(vec![
        Scene {
            name: "bank1",
            disk: lines(),
            second: None,
            keys: "1 POKE56576,(PEEK(56576)AND252)OR2:OPEN2,8,2,\"NOPE\":GET#2,A$|2 B=PEEK(56576):POKE56576,PEEK(56576)OR3:PRINTASC(A$+CHR$(0));ST;B|RUN|~",
            screen: &[
            "1 POKE56576,(PEEK(56576)AND252)OR2:OPEN2",
            ",8,2,\"NOPE\":GET#2,A$",
            "2 B=PEEK(56576):POKE56576,PEEK(56576)OR3",
            ":PRINTASC(A$+CHR$(0));ST;B",
            "RUN",
            " 198  66  198",
            "",
            "READY.",
            ],
        },
        Scene {
            name: "twice",
            disk: chains(),
            second: None,
            keys: "1 OPEN2,8,2,\"NOPE\":GET#2,A$:PRINTASC(A$+CHR$(0));ST;|2 GET#2,A$:PRINTASC(A$+CHR$(0));ST|RUN|~",
            screen: &[
            "1 OPEN2,8,2,\"NOPE\":GET#2,A$:PRINTASC(A$+",
            "CHR$(0));ST;",
            "2 GET#2,A$:PRINTASC(A$+CHR$(0));ST",
            "RUN",
            " 199  66  199  66",
            "",
            "READY.",
            ],
        },
    ]);
}

#[test]
fn a_device_absent_beside_a_drive_is_not_present() {
    check(vec![
        Scene {
            name: "openabsent",
            disk: lines(),
            second: None,
            keys: "OPEN2,9,2,\"NOPE\"|~",
            screen: &[
                "OPEN2,9,2,\"NOPE\"",
                "",
                "?DEVICE NOT PRESENT  ERROR",
                "READY.",
            ],
        },
        Scene {
            name: "openabsent1",
            disk: lines(),
            second: None,
            keys: "OPEN2,9,2,\"N\"|~",
            screen: &[
                "OPEN2,9,2,\"N\"",
                "",
                "?DEVICE NOT PRESENT  ERROR",
                "READY.",
            ],
        },
        Scene {
            name: "loadabsent",
            disk: lines(),
            second: None,
            keys: "LOAD\"X\",9|~",
            screen: &[
                "LOAD\"X\",9",
                "",
                "SEARCHING FOR X",
                "?DEVICE NOT PRESENT  ERROR",
                "READY.",
            ],
        },
        Scene {
            name: "openpresent",
            disk: lines(),
            second: None,
            keys: "OPEN2,8,2,\"N\"|~PRINTST|",
            screen: &[
                "OPEN2,8,2,\"N\"",
                "",
                "READY.",
                "PRINTST",
                " 0",
                "",
                "READY.",
            ],
        },
        Scene {
            name: "cmdabsent",
            disk: lines(),
            second: None,
            keys: "OPEN15,9,15,\"I\"|~",
            screen: &[
                "OPEN15,9,15,\"I\"",
                "",
                "?DEVICE NOT PRESENT  ERROR",
                "READY.",
            ],
        },
    ]);
}
