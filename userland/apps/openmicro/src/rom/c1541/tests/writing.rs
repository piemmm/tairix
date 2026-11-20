//! Writing on the true drive, this tree's KERNAL and BASIC driving it: the
//! disk each scene leaves, and the error it reports, are the original
//! DOS's with the same disk in the same drive, but for the bytes past a
//! last block's end, which this DOS zeroes where the original leaves what
//! its buffer held.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::disk::{basic, closed, odd_chains, offset, printing, Disk};
use super::{played, surface};
use crate::machine::c64::drive::Unit;
use crate::machine::c64::Machine;
use crate::media::c1541::d64;
use crate::media::c1541::layout::{
    entry, kind, DIRECTORY_TRACK, FIRST_DIRECTORY_SECTOR, NAME_BYTES, PADDING, TRACKS,
};
use crate::rom::c64::testing::{run_for, screen_rows, type_text, JIFFY};

/// A line that reads the error channel and shows it, leaving the channel
/// open so no file is closed by it.
pub(super) const STATUS: &str = "9 OPEN15,8,15:INPUT#15,A,B$,C,D:PRINTA;B$;C;D|";

/// A scene: the disk it starts from, the keys typed, the error row it shows
/// and the disk the original leaves.
pub(super) struct Scene {
    name: &'static str,
    disk: Vec<u8>,
    protected: bool,
    keys: String,
    status: &'static str,
    after: Vec<u8>,
}

impl Scene {
    pub(super) fn new(
        name: &'static str,
        disk: Vec<u8>,
        keys: String,
        status: &'static str,
        after: Vec<u8>,
    ) -> Self {
        Self {
            name,
            disk,
            protected: false,
            keys,
            status,
            after,
        }
    }
}

/// The drive's disk as a D64 image.
pub(super) fn image_of(machine: &mut Machine) -> Vec<u8> {
    machine
        .drives()
        .unit(Unit::Eight)
        .and_then(|drive| drive.disk())
        .and_then(|disk| d64::encode(disk, TRACKS))
        .expect("the disk encodes as a D64")
}

/// `actual` against `expected`, naming the first sector that differs.
fn assert_disk(scene: &str, actual: &[u8], expected: &[u8]) {
    for track in 1..=TRACKS {
        for sector in 0..d64::sectors(track) {
            let at = offset(track, sector);
            let (found, wanted) = (
                &actual[at..at + d64::SECTOR_BYTES],
                &expected[at..at + d64::SECTOR_BYTES],
            );
            assert!(found == wanted, "{scene}: track {track}, sector {sector} holds {found:02x?} where {wanted:02x?} is left");
        }
    }
}

/// The emulated time a scene's status may take to show, the drive's work
/// done by then.
const SETTLE: u64 = 3000 * JIFFY;

/// `machine` run until `status` is shown, which fails `scene` where it is
/// not within [`SETTLE`].
pub(super) fn shown(scene: &str, machine: &mut Machine, status: &str) {
    let end = machine.cycles() + SETTLE;
    while !screen_rows(machine).iter().any(|row| row == status) {
        assert!(
            machine.cycles() < end,
            "{scene}: {status:?} is shown in {:#?}",
            screen_rows(machine)
        );
        run_for(machine, 10 * JIFFY);
    }
}

pub(super) fn check(scenes: Vec<Scene>) {
    for scene in scenes {
        let mut disk = surface(&scene.disk);
        disk.write_protected = scene.protected;
        let mut machine = played(disk, None, &scene.keys);
        shown(scene.name, &mut machine, scene.status);
        assert_disk(scene.name, &image_of(&mut machine), &scene.after);
    }
}

/// A blank disk.
fn blank() -> Disk {
    Disk::new(b"WRITES", *b"WR")
}

/// The commands' disk: two programs, one locked, one never closed, and two
/// sequential files.
pub(super) fn commands() -> Disk {
    let mut disk = Disk::new(b"COMMANDS", *b"CO");
    disk.file(b"ONE", kind::PRG, &printing(b"ONE"));
    disk.file(b"TWO", kind::PRG, &two());
    let (first, blocks) = disk.chain(&printing(b"LOCKED"));
    disk.entry(closed(kind::PRG) | kind::LOCKED, b"LOCKED", first, blocks);
    let mut splat = printing(b"SPLAT");
    splat.extend_from_slice(&[0; 300]);
    let (first, blocks) = disk.chain(&splat);
    disk.entry(kind::PRG, b"SPLAT", first, blocks);
    disk.file(b"DATA", kind::SEQ, &data());
    disk.file(b"DOT", kind::SEQ, b"D\r");
    disk
}

/// The commands' disk's two-block program and sequential file.
fn two() -> Vec<u8> {
    let mut two = printing(b"TWO");
    two.extend_from_slice(&[0; 300]);
    two
}

fn data() -> Vec<u8> {
    b"HELLO WORLD\r".repeat(25)
}

/// The appends' disk: sequential files of one, two and three blocks, one
/// whose last block is full, and one whose length is wrong.
fn appends() -> Disk {
    let mut disk = Disk::new(b"APPEND", *b"AP");
    for (name, bytes, blocks) in [
        (&b"ONEB"[..], one_b(), 1),
        (b"TWOB", [&[b'B'; 300][..], b"\r"].concat(), 2),
        (b"THREEB", [&[b'C'; 600][..], b"\r"].concat(), 3),
        (b"FULLB", [b'D'; 254].to_vec(), 1),
        (b"WRONG", [b'E'; 20].to_vec(), 7),
    ] {
        let (first, _) = disk.chain(&bytes);
        disk.entry(closed(kind::SEQ), name, first, blocks);
    }
    disk
}

fn one_b() -> Vec<u8> {
    [&[b'A'; 20][..], b"\r"].concat()
}

/// A command sent on the error channel and its error shown.
fn command(text: &str) -> String {
    format!("8 X$=\"{text}\"|9 OPEN15,8,15,X$:INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN 8|")
}

/// `program` typed as line 1, run, and the error shown.
fn run(program: &str) -> String {
    format!("1 {program}|{STATUS}RUN|")
}

#[test]
fn programs_and_sequential_files_are_written_as_the_originals_are() {
    let one = |disk: &mut Disk| {
        let (first, blocks) = disk.chain_along(&[(17, 0)], &printing(b"A"));
        disk.entry(closed(kind::PRG), b"ONE", first, blocks);
    };
    let saved = format!("10 PRINT\"A\"|SAVE\"ONE\",8|~{STATUS}RUN 9|");
    check(vec![
        Scene::new("a program saved", blank().image(), saved.clone(), " 0 OK 0  0", {
            let mut disk = blank();
            one(&mut disk);
            disk.image()
        }),
        Scene::new(
            "a second program beside it",
            blank().image(),
            format!("10 PRINT\"A\"|SAVE\"ONE\",8|~SAVE\"TWO\",8|~{STATUS}RUN 9|"),
            " 0 OK 0  0",
            {
                let mut disk = blank();
                one(&mut disk);
                let (first, blocks) = disk.chain_along(&[(17, 1)], &printing(b"A"));
                disk.entry(closed(kind::PRG), b"TWO", first, blocks);
                disk.image()
            },
        ),
        Scene::new(
            "a name taken",
            blank().image(),
            format!("10 PRINT\"A\"|SAVE\"ONE\",8|~SAVE\"ONE\",8|~{STATUS}RUN 9|"),
            " 63 FILE EXISTS 0  0",
            {
                let mut disk = blank();
                one(&mut disk);
                disk.image()
            },
        ),
        Scene::new(
            "a program replaced",
            blank().image(),
            format!("10 PRINT\"A\"|SAVE\"ONE\",8|~20 PRINT\"B\"|SAVE\"@0:ONE\",8|~{STATUS}RUN 9|"),
            " 0 OK 0  0",
            {
                let mut disk = blank();
                disk.chain_along(&[(17, 0)], &printing(b"A"));
                disk.release((17, 0));
                let (first, blocks) = disk.chain_along(&[(17, 1)], &basic(&[(10, b"A"), (20, b"B")]));
                disk.entry(closed(kind::PRG), b"ONE", first, blocks);
                disk.image()
            },
        ),
        Scene::new("a sequential file", blank().image(), run("OPEN2,8,2,\"DATA,S,W\":PRINT#2,\"HELLO\":CLOSE2"), " 0 OK 0  0", {
            let mut disk = blank();
            let (first, blocks) = disk.chain_along(&[(17, 0)], b"HELLO\r");
            disk.entry(closed(kind::SEQ), b"DATA", first, blocks);
            disk.image()
        }),
        Scene::new("an empty file, which holds a RETURN", blank().image(), run("OPEN2,8,2,\"E,S,W\":CLOSE2"), " 0 OK 0  0", {
            let mut disk = blank();
            let (first, blocks) = disk.chain_along(&[(17, 0)], b"\r");
            disk.entry(closed(kind::SEQ), b"E", first, blocks);
            disk.image()
        }),
        Scene::new(
            "a file appended to, counted a block more",
            blank().image(),
            format!(
                "1 OPEN2,8,2,\"DATA,S,W\":PRINT#2,\"HELLO\":CLOSE2|2 OPEN2,8,2,\"DATA,S,A\":PRINT#2,\"MORE\":CLOSE2|{STATUS}RUN|"
            ),
            " 0 OK 0  0",
            {
                let mut disk = blank();
                let (first, _) = disk.chain_along(&[(17, 0)], b"HELLO\rMORE\r");
                disk.entry(closed(kind::SEQ), b"DATA", first, 2);
                disk.image()
            },
        ),
        Scene::new("a user file", blank().image(), run("OPEN2,8,2,\"U,U,W\":PRINT#2,\"X\";:CLOSE2"), " 0 OK 0  0", {
            let mut disk = blank();
            let (first, blocks) = disk.chain_along(&[(17, 0)], b"X");
            disk.entry(closed(kind::USR), b"U", first, blocks);
            disk.image()
        }),
        Scene::new("a program written as a file", blank().image(), run("OPEN2,8,2,\"P,P,W\":PRINT#2,\"X\";:CLOSE2"), " 0 OK 0  0", {
            let mut disk = blank();
            let (first, blocks) = disk.chain_along(&[(17, 0)], b"X");
            disk.entry(closed(kind::PRG), b"P", first, blocks);
            disk.image()
        }),
        Scene::new("a file asked for no mode, so read", blank().image(), run("OPEN2,8,2,\"N,S\":PRINT#2,\"X\";:CLOSE2"), " 62 FILE NOT FOUND 0  0", blank().image()),
    ]);
}

#[test]
fn files_replaced_appended_and_refused_as_the_originals_are() {
    check(vec![
        Scene::new("a name taken", commands().image(), run("OPEN2,8,2,\"DATA,S,W\":PRINT#2,\"XYZ\":CLOSE2"), " 63 FILE EXISTS 0  0", commands().image()),
        Scene::new("a file replaced", commands().image(), run("OPEN2,8,2,\"@0:DATA,S,W\":PRINT#2,\"XYZ\":CLOSE2"), " 0 OK 0  0", {
            let mut disk = commands();
            disk.release((1, 6));
            disk.release((1, 7));
            let (first, blocks) = disk.chain_along(&[(17, 0)], b"XYZ\r");
            disk.refile(b"DATA", first, blocks);
            disk.image()
        }),
        Scene::new("an append to no file", commands().image(), run("OPEN2,8,2,\"NOPE,S,A\":PRINT#2,\"XYZ\":CLOSE2"), " 62 FILE NOT FOUND 0  0", commands().image()),
        Scene::new("an append to a file never closed", commands().image(), run("OPEN2,8,2,\"SPLAT,A\":PRINT#2,\"XYZ\":CLOSE2"), " 60 WRITE FILE OPEN 0  0", commands().image()),
        Scene::new("an append to one block", appends().image(), run("OPEN2,8,2,\"ONEB,S,A\":PRINT#2,\"X\":CLOSE2"), " 0 OK 0  0", {
            appends_with(|disk| {
                disk.chain_along(&[(1, 0)], &[one_b(), b"X\r".to_vec()].concat());
                disk.counted(b"ONEB", 2);
            })
        }),
        Scene::new("an append past a full block", appends().image(), run("OPEN2,8,2,\"FULLB,S,A\":PRINT#2,\"X\":CLOSE2"), " 0 OK 0  0", {
            appends_with(|disk| {
                disk.chain_along(&[(1, 6), (1, 16)], &[&[b'D'; 254][..], b"X\r"].concat());
                disk.counted(b"FULLB", 3);
            })
        }),
        Scene::new("an append to a file of the wrong length", appends().image(), run("OPEN2,8,2,\"WRONG,S,A\":PRINT#2,\"X\":CLOSE2"), " 0 OK 0  0", {
            appends_with(|disk| {
                disk.chain_along(&[(1, 7)], &[&[b'E'; 20][..], b"X\r"].concat());
                disk.counted(b"WRONG", 2);
            })
        }),
        Scene::new(
            "two appends",
            appends().image(),
            format!("1 OPEN2,8,2,\"ONEB,S,A\":PRINT#2,\"X\":CLOSE2|2 OPEN2,8,2,\"ONEB,S,A\":PRINT#2,\"Y\":CLOSE2|{STATUS}RUN|"),
            " 0 OK 0  0",
            appends_with(|disk| {
                disk.chain_along(&[(1, 0)], &[one_b(), b"X\rY\r".to_vec()].concat());
                disk.counted(b"ONEB", 2);
            }),
        ),
        Scene::new("a new file named by a pattern", commands().image(), run("OPEN2,8,2,\"A?,S,W\":PRINT#2,\"X\":CLOSE2"), " 33 SYNTAX ERROR 0  0", commands().image()),
        Scene::new(
            "a program saved under a pattern",
            commands().image(),
            format!("10 PRINT\"A\"|SAVE\"A*\",8|~{STATUS}RUN 9|"),
            " 33 SYNTAX ERROR 0  0",
            commands().image(),
        ),
        Scene::new("a replace of another kind", commands().image(), run("OPEN2,8,2,\"@0:DATA,P,W\":PRINT#2,\"X\":CLOSE2"), " 64 FILE TYPE MISMATCH 0  0", commands().image()),
        Scene::new(
            "a program replacing a sequential file",
            commands().image(),
            format!("10 PRINT\"A\"|SAVE\"@0:DATA\",8|~{STATUS}RUN 9|"),
            " 64 FILE TYPE MISMATCH 0  0",
            commands().image(),
        ),
        Scene::new("a replace whose pattern finds nothing", commands().image(), run("OPEN2,8,2,\"@0:Q*,S,W\":PRINT#2,\"X\":CLOSE2"), " 64 FILE TYPE MISMATCH 0  0", commands().image()),
        Scene::new("a replace by a pattern", commands().image(), run("OPEN2,8,2,\"@0:D?TA,S,W\":PRINT#2,\"X\":CLOSE2"), " 0 OK 0  0", {
            commands_with(|disk| {
                disk.release((1, 6));
                disk.release((1, 7));
                let (first, blocks) = disk.chain_along(&[(17, 0)], b"X\r");
                disk.refile(b"DATA", first, blocks);
            })
        }),
        Scene::new("an append by a pattern", commands().image(), run("OPEN2,8,2,\"DA*,S,A\":PRINT#2,\"X\":CLOSE2"), " 0 OK 0  0", {
            commands_with(|disk| {
                disk.chain_along(&[(1, 6), (1, 7)], &[data(), b"X\r".to_vec()].concat());
                disk.counted(b"DATA", 2);
            })
        }),
    ]);
}

/// The appends' disk, changed.
fn appends_with(change: impl FnOnce(&mut Disk)) -> Vec<u8> {
    let mut disk = appends();
    change(&mut disk);
    disk.image()
}

/// The commands' disk, changed.
pub(super) fn commands_with(change: impl FnOnce(&mut Disk)) -> Vec<u8> {
    let mut disk = commands();
    change(&mut disk);
    disk.image()
}

/// The entry named `name` scratched and `blocks` freed.
fn scratched(disk: &mut Disk, name: &[u8], blocks: &[(u8, u8)]) {
    disk.retype(name, 0);
    for &block in blocks {
        disk.release(block);
    }
}

/// The commands' disk's file ONE scratched.
fn one(disk: &mut Disk) {
    scratched(disk, b"ONE", &[(1, 0)]);
}

/// The regression: the count of names before an `=` was kept past the five
/// the table holds, and the scratch read a sixth from past its end.
#[test]
fn names_past_the_fifth_before_an_equals_sign_are_not_counted() {
    let mut machine = played(
        surface(&commands().image()),
        None,
        "OPEN15,8,15,\"S:ONE,X,X,X,X,X=Y\":CLOSE15|~",
    );
    let counted = machine
        .drives()
        .unit(Unit::Eight)
        .map(|drive| drive.ram(crate::rom::c1541::ws::zp::NAME_EQUALS));
    assert!(
        counted.is_some_and(|count| count <= crate::rom::c1541::ws::PATTERNS),
        "{counted:?}"
    );
}

#[test]
fn files_are_scratched_as_the_originals_are() {
    check(vec![
        Scene::new(
            "one file",
            commands().image(),
            command("S0:ONE"),
            " 1 FILES SCRATCHED 1  0",
            commands_with(one),
        ),
        Scene::new(
            "no drive named",
            commands().image(),
            command("S:ONE"),
            " 1 FILES SCRATCHED 1  0",
            commands_with(one),
        ),
        Scene::new(
            "the command spelt out",
            commands().image(),
            command("SCRATCH0:ONE"),
            " 1 FILES SCRATCHED 1  0",
            commands_with(one),
        ),
        // Five names kept, as the patterns are: the sixth before the `=`
        // is passed over, not read from past the table.
        Scene::new(
            "six names before an equals sign",
            commands().image(),
            command("S:ONE,X,X,X,X,X=Y"),
            " 1 FILES SCRATCHED 1  0",
            commands_with(one),
        ),
        Scene::new(
            "no colon",
            commands().image(),
            command("SONE"),
            " 34 SYNTAX ERROR 0  0",
            commands().image(),
        ),
        Scene::new(
            "none matched",
            commands().image(),
            command("S0:NOPE"),
            " 1 FILES SCRATCHED 0  0",
            commands().image(),
        ),
        Scene::new(
            "a locked file left",
            commands().image(),
            command("S0:LOCKED"),
            " 1 FILES SCRATCHED 0  0",
            commands().image(),
        ),
        Scene::new(
            "a file never closed, its blocks kept",
            commands().image(),
            command("S0:SPLAT"),
            " 1 FILES SCRATCHED 1  0",
            commands_with(|disk| scratched(disk, b"SPLAT", &[])),
        ),
        Scene::new(
            "two names, each with a drive",
            commands().image(),
            command("S0:ONE,0:TWO"),
            " 1 FILES SCRATCHED 2  0",
            commands_with(|disk| {
                one(disk);
                scratched(disk, b"TWO", &[(1, 1), (1, 2)]);
            }),
        ),
    ]);
}

#[test]
fn patterns_scratch_as_the_originals_do() {
    check(vec![
        Scene::new(
            "patterns",
            commands().image(),
            command("S0:T*,D*"),
            " 1 FILES SCRATCHED 3  0",
            commands_with(|disk| {
                scratched(disk, b"TWO", &[(1, 1), (1, 2)]);
                scratched(disk, b"DATA", &[(1, 6), (1, 7)]);
                scratched(disk, b"DOT", &[(1, 8)]);
            }),
        ),
        Scene::new(
            "every file but the locked one",
            commands().image(),
            command("S0:*"),
            " 1 FILES SCRATCHED 5  0",
            commands_with(|disk| {
                one(disk);
                scratched(disk, b"TWO", &[(1, 1), (1, 2)]);
                scratched(disk, b"SPLAT", &[]);
                scratched(disk, b"DATA", &[(1, 6), (1, 7)]);
                scratched(disk, b"DOT", &[(1, 8)]);
            }),
        ),
        Scene::new(
            "a name after an equals sign ignored",
            commands().image(),
            command("S0:ONE=TWO"),
            " 1 FILES SCRATCHED 1  0",
            commands_with(one),
        ),
        Scene::new(
            "the names before an equals sign",
            commands().image(),
            command("S0:ONE,TWO=DOT"),
            " 1 FILES SCRATCHED 2  0",
            commands_with(|disk| {
                one(disk);
                scratched(disk, b"TWO", &[(1, 1), (1, 2)]);
            }),
        ),
    ]);
}

#[test]
fn files_are_renamed_and_validated_as_the_originals_are() {
    check(vec![
        Scene::new(
            "a rename",
            commands().image(),
            command("R0:NEW=ONE"),
            " 0 OK 0  0",
            commands_with(|disk| disk.rename(b"ONE", b"NEW")),
        ),
        Scene::new(
            "a name too long",
            commands().image(),
            command("R0:ABCDEFGHIJKLMNOPQ=ONE"),
            " 0 OK 0  0",
            commands_with(|disk| disk.rename(b"ONE", b"ABCDEFGHIJKLMNOP")),
        ),
        Scene::new(
            "a file never closed",
            commands().image(),
            command("R0:NEW=SPLAT"),
            " 0 OK 0  0",
            commands_with(|disk| disk.rename(b"SPLAT", b"NEW")),
        ),
        Scene::new(
            "a name taken",
            commands().image(),
            command("R0:TWO=ONE"),
            " 63 FILE EXISTS 0  0",
            commands().image(),
        ),
        Scene::new(
            "no such file",
            commands().image(),
            command("R0:X=NOPE"),
            " 62 FILE NOT FOUND 0  0",
            commands().image(),
        ),
        Scene::new(
            "no new name",
            commands().image(),
            command("R0:NEW"),
            " 30 SYNTAX ERROR 0  0",
            commands().image(),
        ),
        Scene::new(
            "a validate",
            commands().image(),
            command("V"),
            " 0 OK 0  0",
            commands_with(|disk| {
                disk.retype(b"SPLAT", 0);
                disk.release((1, 4));
                disk.release((1, 5));
            }),
        ),
        Scene::new(
            "a rename to a pattern",
            commands().image(),
            command("R0:Q*=ONE"),
            " 30 SYNTAX ERROR 0  0",
            commands().image(),
        ),
        Scene::new(
            "a rename from a pattern",
            commands().image(),
            command("R0:Q=O*"),
            " 30 SYNTAX ERROR 0  0",
            commands().image(),
        ),
        Scene::new(
            "a rename of two files",
            commands().image(),
            command("R0:X=ONE,TWO"),
            " 30 SYNTAX ERROR 0  0",
            commands().image(),
        ),
    ]);
}

#[test]
fn files_are_copied_as_the_originals_are() {
    check(vec![
        Scene::new(
            "a copy",
            commands().image(),
            command("C0:COPY=ONE"),
            " 0 OK 0  0",
            commands_with(|disk| {
                let (first, blocks) = disk.chain_along(&[(17, 0)], &printing(b"ONE"));
                disk.entry(closed(kind::PRG), b"COPY", first, blocks);
            }),
        ),
        Scene::new(
            "two files joined",
            commands().image(),
            command("C0:CAT=ONE,TWO"),
            " 0 OK 0  0",
            commands_with(|disk| {
                let (first, blocks) =
                    disk.chain_along(&[(17, 0), (17, 10)], &[printing(b"ONE"), two()].concat());
                disk.entry(closed(kind::PRG), b"CAT", first, blocks);
            }),
        ),
        Scene::new(
            "a sequential file's copy",
            commands().image(),
            command("C0:DCOPY=DATA"),
            " 0 OK 0  0",
            commands_with(|disk| {
                let (first, blocks) = disk.chain_along(&[(17, 0), (17, 10)], &data());
                disk.entry(closed(kind::SEQ), b"DCOPY", first, blocks);
            }),
        ),
        Scene::new(
            "a copy onto a name taken",
            commands().image(),
            command("C0:ONE=TWO"),
            " 63 FILE EXISTS 0  0",
            commands().image(),
        ),
        Scene::new(
            "a copy of no file",
            commands().image(),
            command("C0:NEW=NOPE"),
            " 62 FILE NOT FOUND 0  0",
            commands().image(),
        ),
        Scene::new(
            "five names, the sixth ignored",
            commands().image(),
            command("C0:X=ONE,TWO,DATA,DOT,ONE"),
            " 0 OK 0  0",
            commands_with(|disk| {
                let bytes = [printing(b"ONE"), two(), data(), b"D\r".to_vec()].concat();
                let (first, blocks) = disk.chain_along(&[(17, 0), (17, 10), (17, 20)], &bytes);
                disk.entry(closed(kind::PRG), b"X", first, blocks);
            }),
        ),
        Scene::new(
            "a copy from a pattern",
            commands().image(),
            command("C0:DX=DAT?"),
            " 30 SYNTAX ERROR 0  0",
            commands().image(),
        ),
        Scene::new(
            "a copy to a pattern",
            commands().image(),
            command("C0:D*=DATA"),
            " 30 SYNTAX ERROR 0  0",
            commands().image(),
        ),
        Scene::new(
            "a copy to two names",
            commands().image(),
            command("C0:X,Y=ONE"),
            " 30 SYNTAX ERROR 0  0",
            commands().image(),
        ),
    ]);
}

#[test]
fn a_name_ending_where_it_starts_is_written_as_the_originals_is() {
    let mut renamed = commands_with(|disk| disk.rename(b"ONE", b"="));
    // The character the name ends at and a whole field's padding after it.
    renamed
        [offset(DIRECTORY_TRACK, FIRST_DIRECTORY_SECTOR) + usize::from(entry::NAME + NAME_BYTES)] =
        PADDING;
    check(vec![
        Scene::new(
            "a rename to no name",
            commands().image(),
            command("R0:=ONE"),
            " 0 OK 0  0",
            renamed,
        ),
        Scene::new(
            "a copy to no name",
            commands().image(),
            command("C0:=DOT"),
            " 0 OK 0  0",
            commands_with(|disk| {
                let (first, blocks) = disk.chain_along(&[(17, 0)], b"D\r");
                disk.entry(closed(kind::SEQ), b"=", first, blocks);
            }),
        ),
    ]);
    let mut machine = played(surface(&commands().image()), None, &run("SAVE\"0:\",8"));
    shown("a save with no name", &mut machine, " 0 OK 0  0");
    let program = program_in_memory(&machine);
    assert_disk(
        "a save with no name, named a zero",
        &image_of(&mut machine),
        &commands_with(|disk| {
            let (first, blocks) = disk.chain_along(&[(17, 0)], &program);
            disk.entry(closed(kind::PRG), &[0], first, blocks);
        }),
    );
}

/// A disk with every block used but `free`.
fn nearly_full(free: &[(u8, u8)]) -> Disk {
    let mut disk = Disk::new(b"ALLOC", *b"AL");
    for track in (1..=TRACKS).filter(|&track| track != DIRECTORY_TRACK) {
        disk.occupy(track);
    }
    for &block in free {
        disk.release(block);
    }
    disk
}

/// Where BASIC's program starts, which SAVE writes from.
const PROGRAM: u16 = 0x0801;

/// A program of `blocks` full blocks saved as `name` on `disk` from what
/// memory holds, and its bytes, read before the error is shown.
fn saved_from_memory(disk: &[u8], name: &str, blocks: u16) -> (Machine, Vec<u8>) {
    let end = PROGRAM + 254 * blocks - 2;
    let keys = format!(
        "POKE45,{}:POKE46,{}:SAVE\"{name}\",8|~",
        end & 0xFF,
        end >> 8
    );
    let mut machine = played(surface(disk), None, &keys);
    let mut program = PROGRAM.to_le_bytes().to_vec();
    program.extend((PROGRAM..end).map(|at| machine.ram(at)));
    type_text(&mut machine, &format!("{STATUS}RUN 9|").replace('|', "\n"));
    (machine, program)
}

#[test]
fn blocks_are_found_as_the_originals_finds_them() {
    // Ten sectors on, one less past the track's end: 0, 10, 20, then 8.
    let (mut machine, program) = saved_from_memory(&blank().image(), "FOUR", 4);
    shown("four blocks saved", &mut machine, " 0 OK 0  0");
    assert_disk(
        "interleaved on the directory's neighbour",
        &image_of(&mut machine),
        &{
            let mut disk = blank();
            let (first, blocks) =
                disk.chain_along(&[(17, 0), (17, 10), (17, 20), (17, 8)], &program);
            disk.entry(closed(kind::PRG), b"FOUR", first, blocks);
            disk.image()
        },
    );
    let mut full_17 = blank();
    full_17.occupy(17);
    let mut after = blank();
    after.occupy(17);
    let (first, blocks) = after.chain_along(&[(19, 0)], &printing(b"A"));
    after.entry(closed(kind::PRG), b"ONE", first, blocks);
    check(vec![
        Scene::new(
            "the track above the directory's once the one below is full",
            full_17.image(),
            format!("10 PRINT\"A\"|SAVE\"ONE\",8|~{STATUS}RUN 9|"),
            " 0 OK 0  0",
            after.image(),
        ),
        Scene::new(
            "a full disk, as a system block off the disk",
            nearly_full(&[]).image(),
            format!("10 PRINT\"A\"|SAVE\"ONE\",8|~{STATUS}RUN 9|"),
            " 67 ILLEGAL TRACK OR SECTOR 36  1",
            nearly_full(&[]).image(),
        ),
    ]);
    // Out from the nearest track, then on away from the directory, then the
    // other side from its start; the last block found is never written.
    let free = [(2, 7), (25, 0), (35, 3)];
    let (mut machine, program) = saved_from_memory(&nearly_full(&free).image(), "BIG", 6);
    shown(
        "six blocks saved where three are free",
        &mut machine,
        " 72 DISK FULL 0  0",
    );
    assert_disk("to the disk's edges", &image_of(&mut machine), &{
        let mut disk = nearly_full(&free);
        disk.block((25, 0), (35, 3), &program[..254]);
        disk.block((35, 3), (2, 7), &program[254..508]);
        for block in free {
            disk.release(block);
        }
        disk.entry(kind::PRG, b"BIG", (25, 0), 0);
        disk.image()
    });
}

#[test]
fn files_left_open_leave_the_disk_as_the_originals_do() {
    let opened = "1 OPEN2,8,2,\"NEWF,S,W\":PRINT#2,\"X\"";
    // The file is never closed, its track recorded wholly used.
    let open_newf = |disk: &mut Disk| {
        disk.entry(kind::SEQ, b"NEWF", (17, 0), 0);
        disk.filling(17);
    };
    check(vec![
        Scene::new("a file written", appends().image(), format!("{opened}|{STATUS}RUN|"), " 0 OK 0  0", appends_with(open_newf)),
        Scene::new(
            "two files written",
            appends().image(),
            format!("1 OPEN2,8,2,\"W1,S,W\":OPEN3,8,3,\"W2,S,W\":PRINT#2,\"A\":PRINT#3,\"B\"|{STATUS}RUN|"),
            " 0 OK 0  0",
            appends_with(|disk| {
                disk.entry(kind::SEQ, b"W1", (17, 0), 0);
                disk.entry(kind::SEQ, b"W2", (17, 1), 0);
                disk.filling(17);
            }),
        ),
        Scene::new(
            "a replace",
            appends().image(),
            format!("1 OPEN2,8,2,\"@0:TWOB,S,W\":PRINT#2,\"X\"|{STATUS}RUN|"),
            " 0 OK 0  0",
            appends_with(|disk| {
                disk.replacing(b"TWOB", (17, 0));
                disk.filling(17);
            }),
        ),
        Scene::new(
            "an append past a full block",
            appends().image(),
            format!("1 OPEN2,8,2,\"FULLB,S,A\":PRINT#2,\"XYZ\"|{STATUS}RUN|"),
            " 0 OK 0  0",
            appends_with(|disk| {
                disk.block((1, 6), (1, 16), &[b'D'; 254]);
                disk.retype(b"FULLB", kind::SEQ);
                disk.filling(1);
            }),
        ),
        Scene::new(
            "a file written across a scratch, the BAM written at its new track alone",
            appends().image(),
            String::from(
                "1 A$=\"\":FORI=1TO50:A$=A$+\"AAAAA\":NEXT|2 OPEN2,8,2,\"BIG,S,W\":PRINT#2,A$;:PRINT#2,A$;|\
                 3 OPEN15,8,15,\"S0:ONEB\":PRINT#2,A$;:PRINT#2,A$;|4 INPUT#15,A,B$,C,D:PRINTA;B$;C;D|RUN|",
            ),
            " 1 FILES SCRATCHED 1  0",
            appends_with(|disk| {
                disk.retype(b"ONEB", 0);
                disk.release((1, 0));
                let text = [b'A'; 1000];
                disk.block((17, 0), (17, 10), &text[..254]);
                disk.block((17, 10), (17, 20), &text[254..508]);
                disk.block((17, 20), (17, 8), &text[508..762]);
                // The BAM is the scratch's, written before the file found its
                // last two blocks.
                disk.release((17, 20));
                disk.entry(kind::SEQ, b"BIG", (17, 0), 0);
            }),
        ),
        Scene::new(
            "an initialize, after which a close writes nothing",
            appends().image(),
            format!("{opened}|2 OPEN15,8,15,\"I0\":CLOSE2|RUN|~{STATUS}RUN 9|"),
            " 0 OK 0  0",
            appends_with(open_newf),
        ),
        Scene::new(
            "a validate, which deletes the file and gives it up",
            appends().image(),
            format!("{opened}|2 OPEN15,8,15,\"V\":CLOSE2|RUN|~~{STATUS}RUN 9|"),
            " 0 OK 0  0",
            appends_with(|disk| disk.entry(0, b"NEWF", (17, 0), 0)),
        ),
        Scene::new(
            "a copy, which gives the file up and reads the BAM again",
            appends().image(),
            format!("{opened}|2 OPEN15,8,15,\"C0:K=ONEB\":CLOSE2|RUN|~~{STATUS}RUN 9|"),
            " 0 OK 0  0",
            appends_with(|disk| {
                open_newf(disk);
                let (first, blocks) = disk.chain_along(&[(19, 0)], &one_b());
                disk.entry(closed(kind::SEQ), b"K", first, blocks);
            }),
        ),
        Scene::new(
            "a rename, after which the file closes",
            appends().image(),
            format!("{opened}|2 OPEN15,8,15,\"R0:Q=ONEB\":CLOSE2|RUN|~{STATUS}RUN 9|"),
            " 0 OK 0  0",
            appends_with(|disk| {
                disk.rename(b"ONEB", b"Q");
                let (first, blocks) = disk.chain_along(&[(17, 0)], b"X\r");
                disk.entry(closed(kind::SEQ), b"NEWF", first, blocks);
            }),
        ),
    ]);
}

#[test]
fn copies_read_their_sources_as_the_originals_do() {
    let copied = |bytes: &[u8]| {
        let mut disk = odd_chains();
        let (first, blocks) = disk.chain_along(&[(17, 0)], bytes);
        disk.entry(closed(kind::SEQ), b"K", first, blocks);
        disk.image()
    };
    check(vec![
        Scene::new(
            "a lone block holding one byte",
            odd_chains().image(),
            command("C0:K=ONEBYTE"),
            " 0 OK 0  0",
            copied(b"Z"),
        ),
        // The original's also leaves the block after K's allocated.
        Scene::new(
            "a last block naming no byte, copied whole",
            odd_chains().image(),
            command("C0:K=EMPTY1"),
            " 0 OK 0  0",
            copied(&[&b"QQ"[..], &[0; 252]].concat()),
        ),
        Scene::new(
            "a chain leaving the disk, the copy left unclosed",
            odd_chains().image(),
            command("C0:K=BADTRACK"),
            " 66 ILLEGAL TRACK OR SECTOR 36  0",
            {
                let mut disk = odd_chains();
                disk.entry(kind::SEQ, b"K", (17, 0), 0);
                disk.filling(17);
                disk.filling(18);
                disk.image()
            },
        ),
        Scene::new(
            "a file written as the directory takes a new sector",
            odd_chains().image(),
            format!("1 OPEN2,8,2,\"NEWF,S,W\":PRINT#2,\"X\"|{STATUS}RUN|"),
            " 0 OK 0  0",
            {
                let mut disk = odd_chains();
                disk.entry(kind::SEQ, b"NEWF", (17, 0), 0);
                disk.filling(17);
                disk.filling(18);
                disk.image()
            },
        ),
    ]);
}

#[test]
fn names_and_their_fields_are_read_as_the_original_reads_them() {
    let made = |name: &[u8], kind: u8| {
        let mut disk = appends();
        let (first, blocks) = disk.chain_along(&[(17, 0)], b"X\r");
        disk.entry(closed(kind), name, first, blocks);
        disk.image()
    };
    check(vec![
        Scene::new(
            "a comma first, the name and the fields' parting",
            appends().image(),
            run("OPEN2,8,2,\"0:,P,W\":PRINT#2,\"X\":CLOSE2"),
            " 0 OK 0  0",
            made(b",", kind::PRG),
        ),
        Scene::new(
            "a mode before a type",
            appends().image(),
            run("OPEN2,8,2,\"0:A,W,P\":PRINT#2,\"X\":CLOSE2"),
            " 0 OK 0  0",
            made(b"A", kind::PRG),
        ),
        Scene::new(
            "a third field ignored",
            appends().image(),
            run("OPEN2,8,2,\"0:A,U,W,P\":PRINT#2,\"X\":CLOSE2"),
            " 0 OK 0  0",
            made(b"A", kind::USR),
        ),
        Scene::new(
            "a mode in a third field",
            appends().image(),
            run("OPEN2,8,2,\"0:A,P,X,W\":PRINT#2,\"X\":CLOSE2"),
            " 62 FILE NOT FOUND 0  0",
            appends().image(),
        ),
        Scene::new(
            "a later mode replacing an earlier",
            appends().image(),
            run("OPEN2,8,2,\"0:A,W,R\":PRINT#2,\"X\":CLOSE2"),
            " 62 FILE NOT FOUND 0  0",
            appends().image(),
        ),
    ]);
}

#[test]
fn a_protected_disk_refuses_the_first_write_as_the_original_does() {
    let protected = |name, keys: String, status| Scene {
        protected: true,
        ..Scene::new(name, commands().image(), keys, status, commands().image())
    };
    check(vec![
        protected(
            "a save, its entry written first",
            format!("10 PRINT\"Q\"|SAVE\"NEW\",8|~{STATUS}RUN 9|"),
            " 26 WRITE PROTECT ON 18  1",
        ),
        protected(
            "a sequential file, the BAM written first",
            run("OPEN2,8,2,\"NEW,S,W\":PRINT#2,\"X\":CLOSE2"),
            " 26 WRITE PROTECT ON 18  0",
        ),
        protected("a scratch", command("S0:ONE"), " 26 WRITE PROTECT ON 18  1"),
        protected(
            "a rename",
            command("R0:NEW=ONE"),
            " 26 WRITE PROTECT ON 18  1",
        ),
        protected("a validate", command("V"), " 26 WRITE PROTECT ON 18  1"),
    ]);
}

/// BASIC's program as SAVE writes it: its load address, then what lies
/// from its start to its variables.
fn program_in_memory(machine: &Machine) -> Vec<u8> {
    const VARIABLES: u16 = 0x2D;
    let end = u16::from_le_bytes([machine.ram(VARIABLES), machine.ram(VARIABLES + 1)]);
    let mut bytes = PROGRAM.to_le_bytes().to_vec();
    bytes.extend((PROGRAM..end).map(|at| machine.ram(at)));
    bytes
}

#[test]
fn a_validate_that_fails_leaves_the_disks_own_bam_in_force() {
    // A file whose chain leaves the disk, listed before one on track 17.
    let broken = || {
        let mut disk = Disk::new(b"BROKEN", *b"BR");
        let bytes = [printing(b"A"), [0; 300].to_vec()].concat();
        disk.chain_along(&[(1, 0), (1, 1)], &bytes);
        disk.block((1, 0), (40, 0), &bytes[..254]);
        disk.entry(closed(kind::PRG), b"A", (1, 0), 2);
        let (first, blocks) = disk.chain_along(&[(17, 0)], &printing(b"B"));
        disk.entry(closed(kind::PRG), b"B", first, blocks);
        disk
    };
    let keys = format!("{}~10 PRINT\"C\"|SAVE\"C\",8|~", command("V"));
    let mut machine = played(surface(&broken().image()), None, &keys);
    shown(
        "the validate",
        &mut machine,
        " 66 ILLEGAL TRACK OR SECTOR 40  0",
    );
    // The original keeps the BAM it had half made, which has yet to see
    // B, and puts C over it.
    let mut after = broken();
    let (first, blocks) = after.chain_along(&[(17, 1)], &program_in_memory(&machine));
    after.entry(closed(kind::PRG), b"C", first, blocks);
    assert_disk(
        "a save after the validate",
        &image_of(&mut machine),
        &after.image(),
    );
}
