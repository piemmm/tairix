//! The VIM ROM's checks: its header, and the editor at work in a Model B
//! with the MOS, BASIC and the DFS beside it, typed at and read back off
//! its screen and its disc.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_dfs::catalogue::{self, Catalogue, Entry, CATALOGUE_LEN, MAX_FILES, NAME_LEN};
use tairix_dfs::image::{Layout, SECTOR_LEN};
use tairix_dfs::name::PAD;
use tairix_openbasic::program::{self, Line};
use tairix_openbasic::tokenise::{tokenise, Start};
use tairix_openbasic::Dialect;

use super::build;
use super::paging::{REGISTER, UNSIGNED};
use super::ws::{page, zp};
use crate::firmware::{BASIC_IMAGE, DFS_IMAGE, MOS_IMAGE};
use crate::machine::bbc::fdc::PhysicalDrive;
use crate::machine::bbc::keyboard::Key;
use crate::machine::bbc::Machine;
use crate::media::disc::Disc;
use crate::rom::mos::ws::{var, vdu};
use crate::rom::os::ROM_TYPES;
use crate::scenario::{mode7_text, press, press_with, type_text, wait};

/// The slots the ROMs are fitted in: BASIC above the DFS above the editor.
const BASIC_SLOT: usize = 15;
const DFS_SLOT: usize = 14;
const VIM_SLOT: usize = 13;
/// An 80-track side's sectors.
const SECTORS: u16 = 800;
/// The status line's row in mode 7.
const STATUS_ROW: usize = 24;

/// A file on the test disc: its name in `$`, load and execution addresses,
/// and contents.
struct File<'a> {
    name: &'a str,
    load: u32,
    exec: u32,
    data: &'a [u8],
}

/// An SSD holding `files` from sector 2, the catalogue in DFS's order: the
/// file furthest up the disc first; write-protected where `protected`.
fn disc(files: &[File<'_>], protected: bool) -> Disc {
    let mut image = alloc::vec![0; usize::from(SECTORS) * SECTOR_LEN];
    let mut entries = Vec::new();
    let mut sector = catalogue::CATALOGUE_SECTORS;
    for file in files {
        let mut name = [PAD; NAME_LEN];
        name[..file.name.len()].copy_from_slice(file.name.as_bytes());
        let length = u32::try_from(file.data.len()).expect("short");
        entries.push(Entry {
            name,
            dir: b'$',
            locked: false,
            load: catalogue::field(file.load).expect("an address a field holds"),
            exec: catalogue::field(file.exec).expect("an address a field holds"),
            length,
            start: sector,
        });
        let at = usize::from(sector) * SECTOR_LEN;
        image[at..at + file.data.len()].copy_from_slice(file.data);
        sector += u16::try_from(file.data.len().div_ceil(SECTOR_LEN)).expect("short");
    }
    assert!(entries.len() <= usize::from(MAX_FILES));
    entries.reverse();
    let mut catalogue = Catalogue::blank(SECTORS);
    catalogue.set_files(&entries).expect("the files fit");
    catalogue.write((&mut image[..CATALOGUE_LEN]).try_into().expect("whole"));
    Disc::decode(&image, Layout::Single, protected).expect("the image decodes")
}

/// A Model B with the clean-room MOS, BASIC, DFS and the editor, `files` in
/// drive 0, at BASIC's prompt.
fn beeb(files: &[File<'_>]) -> Machine {
    beeb_with(files, false)
}

/// The same, the disc write-protected where `protected`.
fn beeb_with(files: &[File<'_>], protected: bool) -> Machine {
    beeb_fitted(files, protected, &[], &[])
}

/// The same, with `extra` ROMs fitted in their slots and sideways RAM in
/// the slots `ram` names before it starts.
fn beeb_fitted(
    files: &[File<'_>],
    protected: bool,
    extra: &[(usize, &[u8])],
    ram: &[usize],
) -> Machine {
    let vim = build().expect("VIM assembles");
    let mut machine = Machine::new(MOS_IMAGE).expect("the MOS fits");
    for &(slot, image) in [
        (BASIC_SLOT, BASIC_IMAGE),
        (DFS_SLOT, DFS_IMAGE),
        (VIM_SLOT, &vim.bytes[..]),
    ]
    .iter()
    .chain(extra)
    {
        machine.insert_rom(slot, image).expect("the ROM fits");
    }
    for &slot in ram {
        machine.insert_ram(slot).expect("the RAM fits");
    }
    machine.insert_disc(PhysicalDrive::Zero, disc(files, protected));
    wait(&mut machine, 100);
    machine
}

/// `text` typed at the editor, whose locks are off: a capital takes SHIFT.
fn keys(machine: &mut Machine, text: &str) {
    for ch in text.chars() {
        if ch.is_ascii_lowercase() {
            let (key, _) = Key::typing(ch.to_ascii_uppercase()).expect("a letter's key");
            press(machine, key);
        } else if ch.is_ascii_uppercase() {
            let (key, _) = Key::typing(ch).expect("a letter's key");
            press_with(machine, key, &[Key::SHIFT]);
        } else if ch == '\u{1b}' {
            press(machine, Key::ESCAPE);
        } else if ch == '\u{7f}' {
            press(machine, Key::DELETE);
        } else {
            type_text(machine, &String::from(ch)).expect("a key types it");
        }
    }
}

/// CTRL with `letter`'s key.
fn ctrl(machine: &mut Machine, letter: char) {
    let (key, _) = Key::typing(letter).expect("a letter's key");
    press_with(machine, key, &[Key::CTRL]);
}

/// The machine run on until `done` holds of its screen, at most `seconds`;
/// the screen then.
fn until(machine: &mut Machine, seconds: u64, done: impl Fn(&[String]) -> bool) -> Vec<String> {
    for _ in 0..seconds * 10 {
        let screen = mode7_text(machine);
        if done(&screen) {
            return screen;
        }
        wait(machine, 10);
    }
    let screen = mode7_text(machine);
    panic!("the screen never showed it:\n{}", screen.join("\n"));
}

/// The status line shows `text`.
fn status_shows(machine: &mut Machine, text: &str) -> Vec<String> {
    until(machine, 20, |screen| screen[STATUS_ROW].contains(text))
}

/// `*VIM`, with `args`, typed at BASIC's prompt, and the editor up.
fn start(machine: &mut Machine, args: &str) -> Vec<String> {
    type_text(machine, &alloc::format!("*VIM{args}\n")).expect("typed");
    until(machine, 20, |screen| !screen[STATUS_ROW].is_empty())
}

/// The file `name` on drive 0's disc: its load and execution addresses and
/// its contents.
fn file_on_disc(machine: &mut Machine, name: &str) -> Option<(u32, u32, Vec<u8>)> {
    let disc = machine.eject_disc(PhysicalDrive::Zero).expect("a disc");
    let image = disc.encode().image;
    machine.insert_disc(PhysicalDrive::Zero, disc);
    let catalogue = Catalogue::read(image[..CATALOGUE_LEN].try_into().ok()?)?;
    let entry = catalogue.files().iter().find(|entry| {
        let len = entry
            .name
            .iter()
            .position(|&b| b == PAD)
            .unwrap_or(NAME_LEN);
        entry.dir == b'$' && entry.name[..len].eq_ignore_ascii_case(name.as_bytes())
    })?;
    let at = usize::from(entry.start) * SECTOR_LEN;
    let length = usize::try_from(entry.length).ok()?;
    Some((
        catalogue::address(entry.load),
        catalogue::address(entry.exec),
        image[at..at + length].to_vec(),
    ))
}

#[test]
fn the_header_names_a_language_with_a_service_entry() {
    let image = build().expect("VIM assembles");
    let bytes = &image.bytes;
    assert_eq!(bytes.len(), 0x4000);
    assert_eq!(bytes[0], 0x4C, "a language entry");
    assert_eq!(bytes[3], 0x4C, "a service entry");
    assert_eq!(bytes[6], 0xC2, "a language and service ROM of 6502 code");
    assert_eq!(&bytes[9..12], b"VIM");
    let copyright = usize::from(bytes[7]);
    assert_eq!(&bytes[copyright..copyright + 4], b"\0(C)");
}

#[test]
fn typed_text_is_written_to_a_new_file() {
    let mut machine = beeb(&[]);
    start(&mut machine, "");
    keys(&mut machine, "ihello world\u{1b}");
    let screen = until(&mut machine, 5, |screen| screen[0] == "hello world");
    assert!(!screen[STATUS_ROW].contains("INSERT"), "{screen:?}");
    keys(&mut machine, ":w NOTE\r");
    status_shows(&mut machine, "written");
    let (_, _, data) = file_on_disc(&mut machine, "NOTE").expect("NOTE written");
    assert_eq!(data, b"hello world\r");
}

/// A row without the colours' control cells.
fn plain(row: &str) -> String {
    row.chars().filter(|&c| c >= ' ').collect()
}

/// The program BASIC II stores for `lines`, each a number and the text
/// typed after it.
fn program_of(lines: &[(u16, &str)]) -> Vec<u8> {
    let tokenised: Vec<(u16, Vec<u8>)> = lines
        .iter()
        .map(|&(number, text)| {
            let mut out = Vec::new();
            tokenise(Dialect::BasicII, text.as_bytes(), Start::Line, &mut out).expect("tokenised");
            (number, out)
        })
        .collect();
    program::image(tokenised.iter().map(|(number, text)| Line {
        number: *number,
        text,
    }))
    .expect("an image")
}

/// The load and execution addresses BASIC's SAVE gives a program.
const PROGRAM_LOAD: u32 = 0xFFFF_1900;
const PROGRAM_EXEC: u32 = 0xFFFF_0000 | crate::rom::basic::SAVED_EXEC as u32;

/// A text in each line-end style is shown a line a row and written back
/// in the style it was read in, a last line given its end.
#[test]
fn each_line_end_style_is_read_and_written_back_as_found() {
    for (name, text, written) in [
        ("CR", &b"one\rtwo\rthree"[..], &b"one\rtwo\rthree\r"[..]),
        ("LF", b"one\ntwo\nthree\n", b"one\ntwo\nthree\n"),
        (
            "CRLF",
            b"one\r\ntwo\r\nthree\r\n",
            b"one\r\ntwo\r\nthree\r\n",
        ),
        (
            "LFCR",
            b"\n\rone\n\rtwo\n\rthree",
            b"\n\rone\n\rtwo\n\rthree\n\r",
        ),
    ] {
        let mut machine = beeb(&[File {
            name,
            load: 0x1900,
            exec: 0x8023,
            data: text,
        }]);
        let screen = start(&mut machine, &alloc::format!(" {name}"));
        let shown: Vec<&str> = screen.iter().take(4).map(String::as_str).collect();
        let first = usize::from(name == "LFCR");
        assert_eq!(
            &shown[first..first + 3],
            ["one", "two", "three"],
            "{name}: {screen:?}"
        );
        keys(&mut machine, ":w\r");
        status_shows(&mut machine, "written");
        let (load, exec, data) = file_on_disc(&mut machine, name).expect("written");
        assert_eq!(data, written, "{name}");
        assert_eq!((load, exec), (0x1900, 0x8023), "{name}'s addresses kept");
    }
}

/// A BASIC program is listed into the text and tokenised back to the same
/// image, its addresses kept.
#[test]
fn a_basic_program_lists_and_saves_back_to_itself() {
    let lines = [
        (10, " REM A test: PRINT \"x\""),
        (20, " PRINT \"HELLO\";:GOTO 40"),
        (30, " FOR I%=1 TO 10:PRINT I%:NEXT"),
        (40, " IF X THEN 20 ELSE 30"),
        (50, " *FX 4,1"),
        (60, " DATA 1,2,PRINT"),
        (70, " A$=\"REM\":PAGE=&1900:TIME=0:X=PAGE"),
        (80, " DEF PROCx(A):LOCAL B:ENDPROC"),
        (90, " GOSUB 100:ON X GOTO 10,20"),
        (100, " PRINT ~&FFFF;1.5E3"),
    ];
    let image = program_of(&lines);
    let mut machine = beeb(&[File {
        name: "PROG",
        load: PROGRAM_LOAD,
        exec: PROGRAM_EXEC,
        data: &image,
    }]);
    start(&mut machine, " PROG");
    keys(&mut machine, ":syntax off\r");
    let screen = until(&mut machine, 5, |screen| {
        screen[..lines.len()].iter().all(|row| plain(row) == *row)
    });
    for (row, (number, text)) in lines.iter().enumerate() {
        assert_eq!(screen[row], alloc::format!("{number}{text}"), "{screen:?}");
    }
    keys(&mut machine, ":w\r");
    status_shows(&mut machine, "written");
    let (load, exec, data) = file_on_disc(&mut machine, "PROG").expect("written");
    assert_eq!(data, image);
    assert_eq!((load, exec), (PROGRAM_LOAD, PROGRAM_EXEC));
}

/// Lines typed into a BASIC program are tokenised as BASIC II tokenises
/// them typed after their numbers.
#[test]
fn typed_basic_lines_are_tokenised_as_basic_does() {
    let image = program_of(&[(10, " PRINT 1")]);
    let mut machine = beeb(&[File {
        name: "PROG",
        load: PROGRAM_LOAD,
        exec: PROGRAM_EXEC,
        data: &image,
    }]);
    start(&mut machine, " PROG");
    keys(
        &mut machine,
        "o20 IF A=1 THEN 10 ELSE PRINT \"OK\"\r30 REPEAT:A=A+1:UNTIL A>5:END\u{1b}:w\r",
    );
    status_shows(&mut machine, "written");
    let (_, _, data) = file_on_disc(&mut machine, "PROG").expect("written");
    assert_eq!(
        data,
        program_of(&[
            (10, " PRINT 1"),
            (20, " IF A=1 THEN 10 ELSE PRINT \"OK\""),
            (30, " REPEAT:A=A+1:UNTIL A>5:END"),
        ])
    );
}

/// The normal mode's commands change the text as vi's do.
#[test]
fn normal_mode_commands_change_the_text() {
    let text = b"alpha beta gamma\rsecond line\rthird\rfourth\r";
    for (typed, wanted) in [
        ("x", &b"lpha beta gamma\rsecond line\rthird\rfourth\r"[..]),
        ("3x", b"ha beta gamma\rsecond line\rthird\rfourth\r"),
        ("dw", b"beta gamma\rsecond line\rthird\rfourth\r"),
        ("wD", b"alpha \rsecond line\rthird\rfourth\r"),
        ("dd", b"second line\rthird\rfourth\r"),
        ("2dd", b"third\rfourth\r"),
        ("jddp", b"alpha beta gamma\rthird\rsecond line\rfourth\r"),
        (
            "yyP",
            b"alpha beta gamma\ralpha beta gamma\rsecond line\rthird\rfourth\r",
        ),
        ("J", b"alpha beta gamma second line\rthird\rfourth\r"),
        (
            "cwomega\u{1b}",
            b"omega beta gamma\rsecond line\rthird\rfourth\r",
        ),
        (
            "$aX\u{1b}",
            b"alpha beta gammaX\rsecond line\rthird\rfourth\r",
        ),
        (
            "Onew\u{1b}",
            b"new\ralpha beta gamma\rsecond line\rthird\rfourth\r",
        ),
        ("rZ", b"Zlpha beta gamma\rsecond line\rthird\rfourth\r"),
        ("~~", b"ALpha beta gamma\rsecond line\rthird\rfourth\r"),
        ("ddu", text),
        ("xu", text),
        ("x.", b"pha beta gamma\rsecond line\rthird\rfourth\r"),
        ("Gdd", b"alpha beta gamma\rsecond line\rthird\r"),
        ("jjdG", b"alpha beta gamma\rsecond line\r"),
        ("/thi\rD", b"alpha beta gamma\rsecond line\r\rfourth\r"),
        (":2,3d\r", b"alpha beta gamma\rfourth\r"),
    ] {
        for ram in [Vec::new(), sideways_ram()] {
            let file = File {
                name: "T",
                load: 0,
                exec: 0,
                data: text,
            };
            let mut machine = beeb_fitted(&[file], false, &[], &ram);
            start(&mut machine, " T");
            keys(&mut machine, typed);
            keys(&mut machine, ":w\r");
            status_shows(&mut machine, "written");
            let (_, _, data) = file_on_disc(&mut machine, "T").expect("written");
            assert_eq!(
                String::from_utf8_lossy(&data),
                String::from_utf8_lossy(wanted),
                "{typed:?} with {ram:?}: {:?}",
                mode7_text(&machine)
            );
        }
    }
}

/// The sideways RAM a new Model B is fitted with.
fn sideways_ram() -> Vec<usize> {
    crate::front::library::SIDEWAYS_RAM_SLOTS.collect()
}

/// The register's bank and its length, as the editor keeps them.
fn register(machine: &Machine) -> (u8, u16) {
    let len = u16::from_le_bytes([machine.peek(page::REG_LEN), machine.peek(page::REG_LEN + 1)]);
    (machine.peek(page::REG_BANK), len)
}

/// The zero page word at `at`.
fn word(machine: &Machine, at: u16) -> u16 {
    u16::from_le_bytes([machine.peek(at), machine.peek(at + 1)])
}

#[test]
fn the_register_is_held_in_the_highest_free_sideways_bank() {
    let file = File {
        name: "T",
        load: 0,
        exec: 0,
        data: b"alpha\rbeta\rgamma\r",
    };
    let mut machine = beeb_fitted(&[file], false, &[], &sideways_ram());
    start(&mut machine, " T");
    keys(&mut machine, "2yy");
    wait(&mut machine, 10);
    assert_eq!(register(&machine), (7, 11));
    assert_eq!(
        word(&machine, zp::REG),
        word(&machine, zp::LIMIT),
        "none below HIMEM"
    );
    let bank = machine.sideways_ram(7).expect("RAM in slot 7");
    assert_eq!(bank[7], UNSIGNED, "the bank marked as no ROM");
    let at = usize::from(REGISTER - 0x8000);
    assert_eq!(&bank[at..at + 11], b"alpha\rbeta\r");
    keys(&mut machine, "Gp:w\r");
    status_shows(&mut machine, "written");
    assert_eq!(
        file_on_disc(&mut machine, "T").expect("written").2,
        b"alpha\rbeta\rgamma\ralpha\rbeta\r"
    );
}

#[test]
fn without_sideways_ram_the_register_lies_below_himem() {
    let mut machine = beeb(&[File {
        name: "T",
        load: 0,
        exec: 0,
        data: b"alpha\rbeta\r",
    }]);
    start(&mut machine, " T");
    keys(&mut machine, "yy");
    wait(&mut machine, 10);
    assert_eq!(register(&machine), (13, 6));
    assert_eq!(word(&machine, zp::LIMIT) - word(&machine, zp::REG), 6);
}

/// A thousand lines yanked from a text that leaves less room than that.
fn yank_past_the_room(ram: &[usize]) -> Machine {
    let text = numbered(2000);
    let mut machine = beeb_fitted(
        &[File {
            name: "T",
            load: 0,
            exec: 0,
            data: &text,
        }],
        false,
        &[],
        ram,
    );
    start(&mut machine, " T");
    status_shows(&mut machine, "2000 lines");
    keys(&mut machine, "1000yy");
    machine
}

#[test]
fn a_yank_memory_has_no_room_for_is_held_in_sideways_ram() {
    let wanted = numbered(1000);
    let mut machine = yank_past_the_room(&[]);
    status_shows(&mut machine, "No room");
    let mut machine = yank_past_the_room(&sideways_ram());
    // Keys are taken in turn, so the numbers show once the yank is done.
    keys(&mut machine, ":set nu\r");
    until(&mut machine, 20, |screen| {
        screen[0].trim_start().starts_with("1 line 1")
    });
    assert_eq!(
        register(&machine),
        (7, u16::try_from(wanted.len()).expect("short"))
    );
    let bank = machine.sideways_ram(7).expect("RAM in slot 7");
    let at = usize::from(REGISTER - 0x8000);
    assert_eq!(&bank[at..at + wanted.len()], &wanted[..]);
}

/// Text of 2000 lines with line 1 yanked and line 2's first character
/// taken, then the first 600 lines deleted: more than the free memory holds
/// twice over, once for the register and once for the undo record. Before
/// the room was found first, the register took the lines and then the undo
/// record had no room, so the refused delete left them in the register.
fn delete_past_the_room(ram: &[usize]) -> Machine {
    let text = numbered(2000);
    let mut machine = beeb_fitted(
        &[File {
            name: "T",
            load: 0,
            exec: 0,
            data: &text,
        }],
        false,
        &[],
        ram,
    );
    start(&mut machine, " T");
    status_shows(&mut machine, "2000 lines");
    keys(&mut machine, "jxkyy600dd");
    machine
}

#[test]
fn a_delete_refused_for_want_of_room_changes_nothing() {
    let mut machine = delete_past_the_room(&[]);
    status_shows(&mut machine, "No room");
    assert_eq!(register(&machine), (13, 7), "the register as it was");
    // The undo record still holds the `x`, and the register line 1.
    keys(&mut machine, "up:w\r");
    status_shows(&mut machine, "written");
    let text = numbered(2000);
    let first_two = b"line 1\rline 2\r".len();
    let wanted = [&text[..first_two], b"line 1\r", &text[first_two..]].concat();
    assert_eq!(file_on_disc(&mut machine, "T").expect("written").2, wanted);
}

#[test]
fn with_sideways_ram_a_delete_memory_alone_cannot_hold_is_made() {
    let mut machine = delete_past_the_room(&sideways_ram());
    keys(&mut machine, ":w\r");
    status_shows(&mut machine, "written");
    let text = numbered(2000);
    let deleted = numbered(600).len();
    assert_eq!(
        file_on_disc(&mut machine, "T").expect("written").2,
        &text[deleted..]
    );
    // Less the character `x` took from line 2.
    assert_eq!(
        register(&machine),
        (7, u16::try_from(deleted - 1).expect("short"))
    );
}

#[test]
fn a_free_bank_holding_a_rom_image_is_left_as_it_is() {
    let mut machine = beeb_fitted(
        &[File {
            name: "T",
            load: 0,
            exec: 0,
            data: b"alpha\r",
        }],
        false,
        &[],
        &[6, 7],
    );
    // Loaded since the MOS looked, so not one it found.
    machine
        .sideways_ram(7)
        .expect("RAM in slot 7")
        .copy_from_slice(BASIC_IMAGE);
    start(&mut machine, " T");
    keys(&mut machine, "yy");
    wait(&mut machine, 10);
    assert_eq!(register(&machine), (6, 6));
    assert_eq!(machine.sideways_ram(7).expect("RAM in slot 7"), BASIC_IMAGE);
}

#[test]
fn break_keeps_a_register_in_sideways_ram_and_registers_no_rom() {
    let mut machine = beeb_fitted(
        &[File {
            name: "T",
            load: 0,
            exec: 0,
            data: b"alpha\rbeta\r",
        }],
        false,
        &[],
        &sideways_ram(),
    );
    start(&mut machine, " T");
    keys(&mut machine, "yy");
    crate::scenario::press_break(&mut machine, &[]);
    status_shows(&mut machine, "Text kept");
    assert_eq!(machine.peek(ROM_TYPES + 7), 0, "the claimed bank is no ROM");
    keys(&mut machine, "GP:w\r");
    status_shows(&mut machine, "written");
    assert_eq!(
        file_on_disc(&mut machine, "T").expect("written").2,
        b"alpha\ralpha\rbeta\r"
    );
}

#[test]
fn a_register_bank_another_program_took_is_given_up() {
    let mut machine = beeb_fitted(
        &[File {
            name: "T",
            load: 0,
            exec: 0,
            data: b"alpha\r",
        }],
        false,
        &[],
        &[6, 7],
    );
    start(&mut machine, " T");
    keys(&mut machine, "yy");
    wait(&mut machine, 10);
    assert_eq!(register(&machine).0, 7);
    machine
        .sideways_ram(7)
        .expect("RAM in slot 7")
        .copy_from_slice(BASIC_IMAGE);
    keys(&mut machine, ":!CAT\r");
    until(&mut machine, 10, |screen| {
        screen.iter().any(|row| row.contains("Press a key"))
    });
    keys(&mut machine, " ");
    until(&mut machine, 5, |screen| screen[0] == "alpha");
    assert_eq!(register(&machine), (6, 0), "the register emptied");
    assert_eq!(machine.sideways_ram(7).expect("RAM in slot 7"), BASIC_IMAGE);
}

/// Row `row` of mode 7's screen as its bytes, however it has scrolled.
fn raw_row(machine: &Machine, row: u16) -> Vec<u8> {
    let start = u16::from_le_bytes([machine.peek(0x0350), machine.peek(0x0351)]);
    (0..40u16)
        .map(|column| machine.peek(0x7C00 + (start - 0x7C00 + row * 40 + column) % 0x400))
        .collect()
}

/// `count` numbered lines.
fn numbered(count: usize) -> Vec<u8> {
    (1..=count)
        .flat_map(|n| alloc::format!("line {n}\r").into_bytes())
        .collect()
}

/// The text written to `name` by `:w` after `typed`, from `text`.
fn edited(text: &[u8], typed: &str) -> Vec<u8> {
    let mut machine = beeb(&[File {
        name: "T",
        load: 0,
        exec: 0,
        data: text,
    }]);
    start(&mut machine, " T");
    keys(&mut machine, typed);
    keys(&mut machine, ":w\r");
    status_shows(&mut machine, "written");
    file_on_disc(&mut machine, "T").expect("written").2
}

#[test]
fn the_keys_are_given_back_when_the_editor_is_left() {
    let mut machine = beeb(&[]);
    let found = machine.peek(var::KEYBOARD_STATUS);
    start(&mut machine, "");
    assert_eq!(machine.peek(var::ESCAPE_ACTION), 1, "ESCAPE a key");
    assert_eq!(
        machine.peek(var::CURSOR_EDITING),
        1,
        "the cursor keys give codes"
    );
    assert_eq!(
        machine.peek(var::KEYBOARD_STATUS) & 0x30,
        0x30,
        "the locks off"
    );
    keys(&mut machine, "ix\u{1b}:q\r");
    status_shows(&mut machine, "Not written (add !)");
    keys(&mut machine, ":q!\r");
    until(&mut machine, 5, |screen| {
        screen.iter().any(|row| row == ">")
    });
    assert_eq!(machine.peek(var::ESCAPE_ACTION), 0);
    assert_eq!(machine.peek(var::CURSOR_EDITING), 0);
    assert_eq!(machine.peek(var::KEYBOARD_STATUS), found);
    type_text(&mut machine, "PRINT 6*7\n").expect("typed");
    until(&mut machine, 5, |screen| {
        screen.iter().any(|row| row == "        42")
    });
}

#[test]
fn break_keeps_the_text() {
    let mut machine = beeb(&[]);
    start(&mut machine, "");
    keys(&mut machine, "ikept\rtext\u{1b}");
    crate::scenario::press_break(&mut machine, &[]);
    let screen = status_shows(&mut machine, "Text kept");
    assert_eq!(&screen[..2], ["kept", "text"]);
    keys(&mut machine, ":w KEPT\r");
    status_shows(&mut machine, "written");
    assert_eq!(
        file_on_disc(&mut machine, "KEPT").expect("written").2,
        b"kept\rtext\r"
    );
}

#[test]
fn an_os_error_is_shown_and_the_text_kept() {
    let mut machine = beeb_with(
        &[File {
            name: "T",
            load: 0,
            exec: 0,
            data: b"safe\r",
        }],
        true,
    );
    start(&mut machine, " T");
    keys(&mut machine, "x:w\r");
    let screen = status_shows(&mut machine, "read only");
    assert_eq!(screen[0], "afe");
    keys(&mut machine, "u");
    until(&mut machine, 5, |screen| screen[0] == "safe");
}

#[test]
fn a_mode_with_room_for_the_text_is_taken_and_one_without_refused() {
    let text = numbered(1500);
    let mut machine = beeb(&[File {
        name: "T",
        load: 0,
        exec: 0,
        data: &text,
    }]);
    start(&mut machine, " T");
    keys(&mut machine, ":mode 0\r");
    status_shows(&mut machine, "Not enough memory");
    assert_eq!(machine.peek(vdu::MODE), 7);
    keys(&mut machine, ":mode 4\r");
    wait(&mut machine, 200);
    assert_eq!(machine.peek(vdu::MODE), 4);
    keys(&mut machine, "dd:mode 7\r");
    wait(&mut machine, 200);
    assert_eq!(machine.peek(vdu::MODE), 7);
    keys(&mut machine, ":w\r");
    status_shows(&mut machine, "written");
    assert_eq!(
        file_on_disc(&mut machine, "T").expect("written").2,
        &text[7..]
    );
}

#[test]
fn a_boot_file_is_coloured_in_mode_7() {
    let mut machine = beeb(&[File {
        name: "!BOOT",
        load: 0,
        exec: 0,
        data: b"*BASIC\rCHAIN \"PROG\"\r",
    }]);
    start(&mut machine, " !BOOT");
    until(&mut machine, 5, |screen| {
        plain(&screen[1]) == "CHAIN\"PROG\"" || plain(&screen[1]) == "CHAIN \"PROG\""
    });
    let row = raw_row(&machine, 1);
    let colours: Vec<u8> = row
        .iter()
        .copied()
        .filter(|b| (0x81..=0x87).contains(b))
        .collect();
    assert!(colours.len() >= 2, "{row:02X?}");
    assert_ne!(
        colours[0], colours[1],
        "the keyword and the string apart: {row:02X?}"
    );
}

#[test]
fn a_search_finds_and_wraps() {
    assert_eq!(
        edited(b"one\rtwo\rone again\r", "/one\rx"),
        b"one\rtwo\rne again\r"
    );
    assert_eq!(
        edited(b"one\rtwo\rone again\r", "/one\rnx"),
        b"ne\rtwo\rone again\r"
    );
    assert_eq!(
        edited(b"one\rtwo\rone again\r", "G?two\rx"),
        b"one\rwo\rone again\r"
    );
}

#[test]
fn zz_writes_and_leaves() {
    let mut machine = beeb(&[File {
        name: "T",
        load: 0,
        exec: 0,
        data: b"abc\r",
    }]);
    start(&mut machine, " T");
    keys(&mut machine, "xZZ");
    until(&mut machine, 10, |screen| {
        screen.iter().any(|row| row == ">")
    });
    assert_eq!(file_on_disc(&mut machine, "T").expect("written").2, b"bc\r");
}

#[test]
fn an_os_command_shows_its_output_until_a_key() {
    let mut machine = beeb(&[File {
        name: "T",
        load: 0,
        exec: 0,
        data: b"abc\r",
    }]);
    start(&mut machine, " T");
    keys(&mut machine, ":!CAT\r");
    until(&mut machine, 10, |screen| {
        screen.iter().any(|row| row.contains("Press a key"))
    });
    assert!(mode7_text(&machine).iter().any(|row| row.contains('T')));
    keys(&mut machine, " ");
    until(&mut machine, 5, |screen| screen[0] == "abc");
}

#[test]
fn the_screen_pages_through_a_long_text() {
    let text = numbered(100);
    let mut machine = beeb(&[File {
        name: "T",
        load: 0,
        exec: 0,
        data: &text,
    }]);
    start(&mut machine, " T");
    ctrl(&mut machine, 'F');
    until(&mut machine, 5, |screen| screen[0] == "line 23");
    ctrl(&mut machine, 'B');
    until(&mut machine, 5, |screen| screen[0] == "line 1");
    keys(&mut machine, "G");
    until(&mut machine, 5, |screen| screen[23] == "line 100");
    keys(&mut machine, "gg");
    until(&mut machine, 5, |screen| screen[0] == "line 1");
    let without_50: Vec<u8> = (1..=100)
        .filter(|&n| n != 50)
        .flat_map(|n| alloc::format!("line {n}\r").into_bytes())
        .collect();
    assert_eq!(edited(&text, "50Gdd"), without_50);
}

#[test]
fn a_change_is_repeated_and_undone() {
    let text = b"one two\rthree four\r";
    assert_eq!(
        edited(text, "ihey \u{1b}j0."),
        b"hey one two\rhey three four\r"
    );
    assert_eq!(edited(text, "dwj0."), b"two\rfour\r");
    assert_eq!(edited(text, "dwu"), text);
    assert_eq!(edited(text, "dwuu"), b"two\rthree four\r");
    assert_eq!(edited(text, "A!\u{1b}j."), b"one two!\rthree four!\r");
    assert_eq!(
        edited(text, "ofive\u{1b}."),
        b"one two\rfive\rfive\rthree four\r"
    );
    assert_eq!(
        edited(text, "iab\x7fc\rd\u{1b}"),
        b"ac\rdone two\rthree four\r"
    );
}

#[test]
fn line_numbers_are_shown_when_set() {
    let mut machine = beeb(&[File {
        name: "T",
        load: 0,
        exec: 0,
        data: b"one\rtwo\r",
    }]);
    start(&mut machine, " T");
    keys(&mut machine, ":set nu\r");
    until(&mut machine, 5, |screen| {
        screen[0].trim_start() == "1 one" && screen[1].trim_start() == "2 two"
    });
    keys(&mut machine, ":set nonu\r");
    until(&mut machine, 5, |screen| screen[0] == "one");
}

#[test]
fn another_file_is_edited_once_the_text_is_safe() {
    let mut machine = beeb(&[
        File {
            name: "A",
            load: 0,
            exec: 0,
            data: b"first\r",
        },
        File {
            name: "B",
            load: 0,
            exec: 0,
            data: b"second\r",
        },
    ]);
    start(&mut machine, " A");
    keys(&mut machine, ":e B\r");
    until(&mut machine, 5, |screen| screen[0] == "second");
    keys(&mut machine, "x:e A\r");
    status_shows(&mut machine, "Not written (add !)");
    keys(&mut machine, ":e! A\r");
    until(&mut machine, 5, |screen| screen[0] == "first");
    keys(&mut machine, "x:w C\r");
    status_shows(&mut machine, "written");
    assert_eq!(
        file_on_disc(&mut machine, "C").expect("written").2,
        b"irst\r"
    );
    assert_eq!(file_on_disc(&mut machine, "A").expect("kept").2, b"first\r");
}

/// `:help` lists the commands and each mode's size, colours and the room it
/// leaves with the DFS's workspace below the text.
#[test]
fn help_shows_the_commands_and_each_mode_s_room() {
    let mut machine = beeb(&[]);
    start(&mut machine, "");
    keys(&mut machine, ":help\r");
    let screen = until(&mut machine, 10, |screen| {
        screen.iter().any(|row| row == "Press a key")
    });
    assert!(
        screen.iter().any(|row| row.starts_with("Move  h j k l")),
        "{screen:?}"
    );
    for wanted in [
        " 0   80x32   2      5888",
        " 2   20x32  16      5888",
        " 4   40x32   2      16128",
        " 7   40x25   8      25344",
    ] {
        assert!(
            screen.iter().any(|row| row == wanted),
            "{wanted:?} in {screen:?}"
        );
    }
    keys(&mut machine, " ");
    until(&mut machine, 5, |screen| {
        screen[0].is_empty() && screen[1] == "~" && !screen.iter().any(|row| row == "Press a key")
    });
}

/// The rows `:dis` shows for `code` loaded at `base`: each instruction's
/// address, bytes and text as the emulator's own disassembler spells it;
/// an undocumented opcode, or one the code ends inside, as `EQUB`.
fn disassembly(base: u16, code: &[u8]) -> Vec<String> {
    use core::fmt::Write as _;

    use crate::cpu::disasm::Instruction;
    use crate::cpu::isa::OPCODES;

    let mut rows = Vec::new();
    let mut at = 0;
    while at < code.len() {
        let opcode = OPCODES[usize::from(code[at])];
        let size = usize::from(opcode.size());
        let pc = base + u16::try_from(at).expect("short");
        let (size, text) = if opcode.documented && at + size <= code.len() {
            let byte = |n: usize| code.get(at + n).copied().unwrap_or(0);
            (
                size,
                alloc::format!("{}", Instruction::decode(pc, [byte(0), byte(1), byte(2)])),
            )
        } else {
            (1, alloc::format!("EQUB &{:02X}", code[at]))
        };
        let mut row = alloc::format!("{pc:04X} ");
        for n in 0..3 {
            if n < size {
                write!(row, "{:02X} ", code[at + n]).expect("written");
            } else {
                row.push_str("   ");
            }
        }
        row.push(' ');
        row.push_str(&text);
        // Mode 7's screen holds `#` as the teletext character &5F.
        rows.push(row.trim_end().replace('#', "_"));
        at += size;
    }
    rows
}

#[test]
fn object_code_is_disassembled_and_paged() {
    let mut code = alloc::vec![
        0xA9, 0x00, 0x85, 0x70, 0x95, 0x71, 0xB6, 0x72, 0xAD, 0x00, 0x19, 0x9D, 0x00, 0x19, 0xB9,
        0x00, 0x19, 0x6C, 0xFE, 0xFF, 0xA1, 0x70, 0xB1, 0x70, 0x0A, 0xEA, 0xD0, 0xE4, 0x10, 0x02,
        0x02, 0x60, 0x20, 0xEE, 0xFF,
    ];
    code.extend([0xEA; 30]);
    code.push(0x4C);
    let rows = disassembly(0x2000, &code);
    let mut machine = beeb(&[File {
        name: "CODE",
        load: 0x2000,
        exec: 0x2000,
        data: &code,
    }]);
    start(&mut machine, "");
    keys(&mut machine, ":dis CODE\r");
    let screen = until(&mut machine, 10, |screen| {
        screen[STATUS_ROW - 1].starts_with("20")
    });
    assert_eq!(&screen[..STATUS_ROW], &rows[..STATUS_ROW]);
    keys(&mut machine, "j");
    until(&mut machine, 5, |screen| screen[0] == rows[1]);
    keys(&mut machine, "k");
    until(&mut machine, 5, |screen| screen[0] == rows[0]);
    keys(&mut machine, " ");
    let screen = until(&mut machine, 5, |screen| {
        screen[rows.len() - STATUS_ROW - 1] == *rows.last().expect("rows")
    });
    assert_eq!(screen[0], rows[STATUS_ROW]);
    assert!(screen[rows.len() - STATUS_ROW].is_empty());
    keys(&mut machine, "b");
    until(&mut machine, 5, |screen| screen[0] == rows[0]);
    keys(&mut machine, "q");
    until(&mut machine, 5, |screen| screen[1] == "~");
}

/// Every opcode, each followed by operand bytes, is disassembled as the
/// emulator's own disassembler has it, a screen at a time.
#[test]
fn every_opcode_is_disassembled_as_the_cpu_knows_it() {
    let code: Vec<u8> = (0..=255u8)
        .flat_map(|opcode| [opcode, 0x12, 0x84])
        .collect();
    let rows = disassembly(0x3000, &code);
    let mut machine = beeb(&[File {
        name: "ALL",
        load: 0x3000,
        exec: 0x3000,
        data: &code,
    }]);
    start(&mut machine, "");
    keys(&mut machine, ":dis ALL\r");
    for page in rows.chunks(STATUS_ROW) {
        let screen = until(&mut machine, 10, |screen| {
            screen[0] == page[0] && screen[page.len() - 1] == page[page.len() - 1]
        });
        assert_eq!(&screen[..page.len()], page);
        keys(&mut machine, " ");
    }
}

#[test]
fn a_file_is_read_in_after_the_cursor_s_line() {
    let mut machine = beeb(&[
        File {
            name: "T",
            load: 0,
            exec: 0,
            data: b"one\rtwo\r",
        },
        File {
            name: "IN",
            load: 0,
            exec: 0,
            data: b"alpha\nbeta\n",
        },
    ]);
    start(&mut machine, " T");
    keys(&mut machine, ":r IN\r");
    until(&mut machine, 5, |screen| {
        screen[..4] == ["one", "alpha", "beta", "two"]
    });
    keys(&mut machine, "u:w\r");
    status_shows(&mut machine, "written");
    assert_eq!(
        file_on_disc(&mut machine, "T").expect("written").2,
        b"one\rtwo\r"
    );
    keys(&mut machine, "uG:r IN\r:w\r");
    status_shows(&mut machine, "written");
    assert_eq!(
        file_on_disc(&mut machine, "T").expect("written").2,
        b"one\ralpha\rbeta\rtwo\ralpha\rbeta\r"
    );
}

#[test]
fn a_substitution_changes_the_range_as_one_change() {
    let text = b"cat cat\rdog cat\rcat\r";
    assert_eq!(edited(text, ":s/cat/bird/\r"), b"bird cat\rdog cat\rcat\r");
    assert_eq!(
        edited(text, ":s/cat/bird/g\r"),
        b"bird bird\rdog cat\rcat\r"
    );
    assert_eq!(edited(text, ":%s/cat/ox/g\r"), b"ox ox\rdog ox\rox\r");
    assert_eq!(edited(text, ":%s/cat/ox/\r"), b"ox cat\rdog ox\rox\r");
    assert_eq!(edited(text, ":2,3s/cat//\r"), b"cat cat\rdog \r\r");
    assert_eq!(edited(text, ":%s/cat/ox/g\ru"), text);
    assert_eq!(edited(text, ":%s/cow/ox/g\r"), text);
    assert_eq!(edited(text, "/dog\r:s//pig/\r"), b"cat cat\rpig cat\rcat\r");
    assert_eq!(edited(text, ":%s/cat/c/g\rn"), b"c c\rdog c\rc\r");
    assert_eq!(edited(text, ":%s/cat/ox/\rx"), b"ox cat\rdog ox\rx\r");
}

/// A text whose paired line ends the gap cannot hold once built is written
/// straight to its file instead, its addresses kept.
#[test]
fn a_text_too_big_to_build_in_memory_is_written_straight_to_its_file() {
    let text: Vec<u8> = (1..=1500)
        .flat_map(|n| alloc::format!("\n\rline {n}").into_bytes())
        .collect();
    let mut machine = beeb(&[File {
        name: "BIG",
        load: 0x1234,
        exec: 0x5678,
        data: &text,
    }]);
    start(&mut machine, " BIG");
    status_shows(&mut machine, "lines");
    keys(&mut machine, "jx:w\r");
    status_shows(&mut machine, "written");
    let (load, exec, data) = file_on_disc(&mut machine, "BIG").expect("written");
    let mut wanted = text.clone();
    wanted.remove(2);
    wanted.extend(b"\n\r");
    assert_eq!(data.len(), wanted.len());
    assert!(data == wanted, "the file as written differs");
    assert_eq!((load, exec), (0x1234, 0x5678));
}

/// A program whose image the gap cannot hold beside its listing is written
/// straight to its file instead, the same image.
#[test]
fn a_program_too_big_to_build_in_memory_is_written_straight_to_its_file() {
    let lines: Vec<(u16, String)> = (1..=400)
        .map(|n| (n * 10, String::from(" PRINT \"ABCDEFGHIJ\";A$;B%:GOTO 10")))
        .collect();
    let refs: Vec<(u16, &str)> = lines.iter().map(|(n, t)| (*n, t.as_str())).collect();
    let image = program_of(&refs);
    let mut machine = beeb(&[File {
        name: "PROG",
        load: PROGRAM_LOAD,
        exec: PROGRAM_EXEC,
        data: &image,
    }]);
    start(&mut machine, " PROG");
    status_shows(&mut machine, "lines");
    keys(&mut machine, ":w\r");
    status_shows(&mut machine, "written");
    let (load, exec, data) = file_on_disc(&mut machine, "PROG").expect("written");
    assert_eq!(data.len(), image.len());
    assert!(data == image, "the program as written differs");
    assert_eq!((load, exec), (PROGRAM_LOAD, PROGRAM_EXEC));
}

/// A file that cannot be read, its sectors running off the disc, leaves an
/// empty text without its name, so `:w` cannot write the empty text over
/// it.
#[test]
fn a_file_that_fails_to_load_is_not_written_over() {
    let mut image = alloc::vec![0; usize::from(SECTORS) * SECTOR_LEN];
    let mut name = [PAD; NAME_LEN];
    name[0] = b'T';
    let mut catalogue = Catalogue::blank(SECTORS);
    catalogue
        .set_files(&[Entry {
            name,
            dir: b'$',
            locked: false,
            load: 0,
            exec: 0,
            length: 0x0C00,
            start: SECTORS - 4,
        }])
        .expect("the entry fits");
    catalogue.write((&mut image[..CATALOGUE_LEN]).try_into().expect("whole"));
    let mut machine = beeb(&[]);
    machine.insert_disc(
        PhysicalDrive::Zero,
        Disc::decode(&image, Layout::Single, false).expect("the image decodes"),
    );
    start(&mut machine, " T");
    until(&mut machine, 60, |screen| {
        screen[STATUS_ROW].contains("fault")
    });
    keys(&mut machine, "ifound\u{1b}:w\r");
    status_shows(&mut machine, "No file name");
}

/// An OS command that starts another language gives the text up: started
/// again, the editor does not take what that language may have overwritten
/// for its text.
#[test]
fn a_language_started_from_an_os_command_takes_the_text_s_memory() {
    use crate::rom::asm::{abs, imm, lo, zp, Asm};
    use crate::rom::os::entry::{OSBYTE, OSCLI};
    const OTHER_SLOT: u8 = 12;
    const IN_RAM: u16 = 0x0600;
    // A language of the test's own that `*OTHER` enters and that starts the
    // editor again at once, touching none of its workspace: the command is
    // passed from RAM, as the ROM is not paged in while the editor reads it.
    let mut a = Asm::new(0x8000, 0x4000);
    a.jmp("lang");
    a.jmp("svc");
    a.byte(0xC2);
    a.byte(lo("copyright"));
    a.byte(1);
    a.text("OTHER");
    a.here("copyright");
    a.byte(0);
    a.text("(C)");
    a.byte(0);
    a.here("svc");
    a.cmp(imm(4));
    let pass = a.local();
    a.bne(pass);
    a.pha();
    a.lda(zp(0xF2u16).ind_y());
    a.cmp(imm(b'O'));
    let others = a.local();
    a.bne(others);
    a.lda(imm(0x8E));
    a.ldx(imm(OTHER_SLOT));
    a.jmp(abs(OSBYTE));
    a.bind(others);
    a.pla();
    a.bind(pass);
    a.rts();
    a.here("lang");
    a.cli();
    a.ldx(imm(3));
    let copy = a.here_local();
    a.lda(abs("command").x());
    a.sta(abs(IN_RAM).x());
    a.dex();
    a.bpl(copy);
    let [low, high] = IN_RAM.to_le_bytes();
    a.ldx(imm(low));
    a.ldy(imm(high));
    a.jsr(OSCLI);
    let wait_here = a.here_local();
    a.jmp(wait_here);
    a.here("command");
    a.text("VIM\r");
    let other = a.finish().expect("the language assembles").bytes;
    let mut machine = beeb_fitted(&[], false, &[(usize::from(OTHER_SLOT), &other)], &[]);
    start(&mut machine, "");
    keys(&mut machine, "iwritten here\u{1b}");
    until(&mut machine, 5, |screen| screen[0] == "written here");
    keys(&mut machine, ":!OTHER\r");
    let screen = until(&mut machine, 10, |screen| {
        screen[1] == "~" && screen[0].is_empty()
    });
    assert!(!screen[STATUS_ROW].contains("Text kept"), "{screen:?}");
}

/// Insert mode stays in insert mode when the text is full, saying so.
#[test]
fn a_full_text_refuses_a_character_without_leaving_insert_mode() {
    // The text fills all but four bytes of mode 7's room above the DFS.
    let room = 0x7C00 - 0x1900;
    let lines = (room - 4) / 8;
    let mut text: Vec<u8> = (0..lines).flat_map(|_| *b"0123456\r").collect();
    text.resize(room - 4, b'\r');
    let mut machine = beeb(&[File {
        name: "T",
        load: 0,
        exec: 0,
        data: &text,
    }]);
    start(&mut machine, " T");
    status_shows(&mut machine, "lines");
    keys(&mut machine, "iabcdef");
    status_shows(&mut machine, "No room");
    keys(&mut machine, "\u{1b}");
    let screen = until(&mut machine, 5, |screen| {
        screen[0].starts_with("abcd0123456")
    });
    assert!(!screen[STATUS_ROW].contains("INSERT"), "{screen:?}");
}

/// An OS command's error leaves the editor with its keys taken again.
#[test]
fn an_os_command_s_error_takes_the_keys_again() {
    let mut machine = beeb(&[]);
    start(&mut machine, "");
    keys(&mut machine, ":!NOSUCH\r");
    status_shows(&mut machine, "Bad command");
    assert_eq!(machine.peek(var::ESCAPE_ACTION), 1, "ESCAPE a key again");
    assert_eq!(
        machine.peek(var::CURSOR_EDITING),
        1,
        "the cursor keys giving codes again"
    );
    keys(&mut machine, "iok\u{1b}");
    until(&mut machine, 5, |screen| screen[0] == "ok");
}

/// ESCAPE pressed to dismiss an OS command's output is taken as a key, its
/// Escape condition acknowledged, and the editor goes on.
#[test]
fn escape_after_an_os_command_leaves_the_editor_working() {
    let mut machine = beeb(&[File {
        name: "T",
        load: 0,
        exec: 0,
        data: b"abc\r",
    }]);
    start(&mut machine, " T");
    keys(&mut machine, ":!CAT\r");
    until(&mut machine, 10, |screen| {
        screen.iter().any(|row| row.contains("Press a key"))
    });
    keys(&mut machine, "\u{1b}");
    until(&mut machine, 5, |screen| screen[0] == "abc");
    keys(&mut machine, "x");
    until(&mut machine, 5, |screen| screen[0] == "bc");
}

/// A count of lines past 255 moves as far as the text goes.
#[test]
fn a_motion_of_more_than_255_lines_reaches_the_end() {
    let text = numbered(257);
    let without_last: Vec<u8> = (1..=256)
        .flat_map(|n| alloc::format!("line {n}\r").into_bytes())
        .collect();
    assert_eq!(edited(&text, "300jdd"), without_last);
}

/// `*HELP VIM` at another language's prompt names the editor's command.
#[test]
fn help_vim_names_the_command() {
    let mut machine = beeb(&[]);
    type_text(&mut machine, "*HELP VIM\n").expect("typed");
    until(&mut machine, 5, |screen| {
        screen.iter().any(|row| row == "VIM 1.00") && screen.iter().any(|row| row == "  VIM <file>")
    });
}

/// The editor starts with no file open, whatever the language before left
/// in the pages it now uses: a program written there is written whole.
#[test]
fn a_program_is_written_after_basic_has_used_the_editor_s_pages() {
    let image = program_of(&[(10, " PRINT 1")]);
    let mut machine = beeb(&[File {
        name: "PROG",
        load: PROGRAM_LOAD,
        exec: PROGRAM_EXEC,
        data: &image,
    }]);
    // BASIC reads a typed line into page 7, the editor's own.
    type_text(
        &mut machine,
        "REM ABCDEFGHIJKLMNOPQRSTUVWXYZABCDEFGHIJKLMNOPQRSTUVWXYZ\n",
    )
    .expect("typed");
    start(&mut machine, " PROG");
    keys(&mut machine, ":w\r");
    status_shows(&mut machine, "written");
    assert_eq!(
        file_on_disc(&mut machine, "PROG").expect("written").2,
        image
    );
}

/// A text never read has no search and no addresses, whatever the language
/// before left in the editor's pages.
#[test]
fn a_fresh_text_takes_nothing_from_the_language_before() {
    let mut machine = beeb(&[]);
    // BASIC keeps its FOR stack in page 5 and reads a typed line into page 7.
    type_text(
        &mut machine,
        "FOR I%=1 TO 3:NEXT:REM ABCDEFGHIJKLMNOPQRSTUVWXYZABCDEF\n",
    )
    .expect("typed");
    start(&mut machine, "");
    keys(&mut machine, "n");
    status_shows(&mut machine, "No previous pattern");
    keys(&mut machine, "inew text\u{1b}:w NEW\r");
    status_shows(&mut machine, "written");
    let (load, exec, data) = file_on_disc(&mut machine, "NEW").expect("written");
    assert_eq!(data, b"new text\r");
    assert_eq!((load, exec), (0, 0));
}

/// A characterwise register is put back as characters, not lines.
#[test]
fn a_word_deleted_is_put_back_as_characters() {
    assert_eq!(edited(b"one two\rthree\r", "dwp"), b"tone wo\rthree\r");
}

/// A repeat that fails part-way plays none of the rest of its keys.
#[test]
fn a_failed_repeat_types_no_more_of_its_keys() {
    assert_eq!(edited(b"abc def\r", "cexyz\u{1b}$."), b"xyz def\r");
}

/// Whole lines taken from a last line without a CR are put back as lines.
#[test]
fn a_last_line_without_its_cr_is_put_back_as_a_line() {
    let mut machine = beeb(&[]);
    start(&mut machine, "");
    keys(&mut machine, "ione\rtwo\u{1b}yykp:w A\r");
    status_shows(&mut machine, "written");
    assert_eq!(
        file_on_disc(&mut machine, "A").expect("written").2,
        b"one\rtwo\rtwo\r"
    );
    keys(&mut machine, ":e! A\rGddP:w B\r");
    status_shows(&mut machine, "\"B\" written");
    assert_eq!(
        file_on_disc(&mut machine, "B").expect("written").2,
        b"one\rtwo\rtwo\r"
    );
    let mut machine = beeb(&[]);
    start(&mut machine, "");
    keys(&mut machine, "ione\rtwo\u{1b}ddP:w C\r");
    status_shows(&mut machine, "written");
    assert_eq!(
        file_on_disc(&mut machine, "C").expect("written").2,
        b"two\rone\r"
    );
}

/// `cw` on a word's last character changes that character alone.
#[test]
fn cw_at_a_word_s_end_changes_that_character() {
    assert_eq!(edited(b"ab cd\r", "lcwX\u{1b}"), b"aX cd\r");
    assert_eq!(edited(b"a\rbc\r", "cwX\u{1b}"), b"X\rbc\r");
    assert_eq!(edited(b"ab", "$cwX\u{1b}"), b"aX\r");
    assert_eq!(edited(b"ab cd ef\r", "lc2wX\u{1b}"), b"aX ef\r");
}

/// A change refused for want of room leaves the change before it to undo.
#[test]
fn a_change_refused_for_room_leaves_the_last_to_undo() {
    // Ten bytes free: room for `dw`'s register and record, not for `20~`'s.
    let room = 0x7C00 - 0x1900;
    let mut text = b"ab cd\r".to_vec();
    while text.len() + 26 <= room - 10 - 4 {
        text.extend_from_slice(b"0123456789012345678901234\r");
    }
    text.resize(room - 10 - 4, b'x');
    text.extend_from_slice(b"xyz\r");
    assert_eq!(text.len(), room - 10);
    let mut machine = beeb(&[File {
        name: "T",
        load: 0,
        exec: 0,
        data: &text,
    }]);
    start(&mut machine, " T");
    status_shows(&mut machine, "lines");
    keys(&mut machine, "dwj20~");
    status_shows(&mut machine, "No room");
    keys(&mut machine, "u:w\r");
    status_shows(&mut machine, "written");
    let data = file_on_disc(&mut machine, "T").expect("written").2;
    assert!(
        data.starts_with(b"ab cd\r0123456789012345678901234\r"),
        "{:?}",
        &data[..40]
    );
}

/// `J` joins only lines there are, and never one past 255 characters.
#[test]
fn j_joins_only_lines_there_are_and_within_a_line_s_length() {
    let mut machine = beeb(&[File {
        name: "T",
        load: 0,
        exec: 0,
        data: b"abc\r",
    }]);
    start(&mut machine, " T");
    keys(&mut machine, "J:q\r");
    until(&mut machine, 10, |screen| {
        screen.iter().any(|row| row == ">")
    });
    assert_eq!(edited(b"abc\rdef\r", "3J"), b"abc def\r");
    let long: Vec<u8> = [&[b'a'; 150][..], b"\r", &[b'b'; 150][..], b"\r"].concat();
    let mut machine = beeb(&[File {
        name: "T",
        load: 0,
        exec: 0,
        data: &long,
    }]);
    start(&mut machine, " T");
    keys(&mut machine, "J");
    status_shows(&mut machine, "Line too long");
    keys(&mut machine, ":w\r");
    status_shows(&mut machine, "written");
    assert_eq!(file_on_disc(&mut machine, "T").expect("written").2, long);
}

/// A put or a substitution that would make a line past 255 characters is
/// refused, the text as it was.
#[test]
fn a_put_or_substitution_past_a_line_s_length_is_refused() {
    let text: Vec<u8> = [&[b'a'; 200][..], b"\r", &[b'b'; 100][..], b"\r"].concat();
    for typed in ["y$jp", ":s/a/aaa/g\r"] {
        let mut machine = beeb(&[File {
            name: "T",
            load: 0,
            exec: 0,
            data: &text,
        }]);
        start(&mut machine, " T");
        keys(&mut machine, typed);
        status_shows(&mut machine, "Line too long");
        keys(&mut machine, ":w\r");
        status_shows(&mut machine, "written");
        assert_eq!(
            file_on_disc(&mut machine, "T").expect("written").2,
            text,
            "{typed:?}"
        );
    }
}

/// Deleting the last line leaves the cursor at the new last line's first
/// character that is not a space.
#[test]
fn deleting_the_last_line_lands_on_the_one_before() {
    assert_eq!(edited(b"  abc\rdef\r", "jddx"), b"  bc\r");
}

/// A line broken on the bottom row shows both halves after the scroll.
#[test]
fn a_line_broken_on_the_bottom_row_shows_both_halves() {
    let mut text = numbered(23);
    text.extend_from_slice(b"abcdef\r");
    let mut machine = beeb(&[File {
        name: "T",
        load: 0,
        exec: 0,
        data: &text,
    }]);
    start(&mut machine, " T");
    keys(&mut machine, "G3li\r\u{1b}");
    until(&mut machine, 5, |screen| {
        screen[22] == "abc" && screen[23] == "def"
    });
}

/// `e` with a count past 255 reaches the last word's end.
#[test]
fn e_with_a_count_past_255_reaches_the_last_word() {
    let line: Vec<u8> = [&b"ab ".repeat(63)[..], b"ab\r"].concat();
    let text = line.repeat(4);
    assert_eq!(edited(&text, "d300e"), b"\r");
}
