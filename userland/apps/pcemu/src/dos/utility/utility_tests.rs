//! The utilities: their command lines and pure parts, then each run at the
//! shell's prompt on a machine booted from a diskette built here.

use super::find::{self, switch};
use super::more::advance;
use super::sort::{self, sorted};
use super::Refusal;
use crate::dos::harness::{screen, settle, type_ahead, typed, File};
use crate::dos::name;
use crate::machine::Machine;
use alloc::string::String;
use alloc::vec::Vec;

#[test]
fn refusals_say_what_dos_says() {
    assert_eq!(
        Refusal::Switch(b"/q").text(b"FIND: "),
        b"FIND: Invalid switch - /Q\r\n"
    );
    assert_eq!(
        Refusal::Missing.text(b""),
        b"Required parameter missing\r\n"
    );
    assert_eq!(
        Refusal::TooMany(b"x").text(b"SORT: "),
        b"SORT: Too many parameters - X\r\n"
    );
}

#[test]
fn find_reads_its_switches_string_and_files() {
    let request = find::request(br#" /v /C "a ""b"" c" one.txt two.txt /i"#).unwrap();
    assert_eq!(
        request.switches,
        switch::NOT | switch::COUNT | switch::IGNORE_CASE
    );
    assert_eq!(request.string, br#"a "b" c"#);
    assert_eq!(request.files, [b"ONE.TXT".to_vec(), b"TWO.TXT".to_vec()]);
    assert_eq!(find::request(b" two file"), Err(Refusal::Format(b"two")));
    assert_eq!(find::request(b" /n"), Err(Refusal::Missing));
    assert_eq!(find::request(br#" /x "a""#), Err(Refusal::Switch(b"/x")));
    assert_eq!(
        find::request(br#" "unclosed"#),
        Err(Refusal::Format(br#""unclosed"#))
    );
    assert_eq!(find::request(br#" "" x"#).unwrap().string, b"");
}

#[test]
fn more_counts_the_cursor_as_the_screen_moves_it() {
    assert_eq!(advance(b'a', (1, 1), 80), (1, 2));
    assert_eq!(
        advance(b'a', (1, 80), 80),
        (2, 1),
        "the 80th character wraps"
    );
    assert_eq!(advance(b'\t', (1, 1), 80), (1, 9));
    assert_eq!(advance(b'\t', (1, 9), 80), (1, 17));
    assert_eq!(advance(0x08, (1, 1), 80), (1, 1));
    assert_eq!(advance(0x08, (1, 5), 80), (1, 4));
    assert_eq!(advance(0x07, (1, 5), 80), (1, 5), "a bell takes no column");
    assert_eq!(advance(b'\n', (3, 5), 80), (4, 5));
    assert_eq!(advance(b'\r', (3, 5), 80), (3, 1));
}

fn plain() -> [u8; 256] {
    core::array::from_fn(|n| u8::try_from(n).unwrap())
}

#[test]
fn sort_orders_lines_from_their_column_and_keeps_equals_in_order() {
    let table = plain();
    assert_eq!(
        sorted(b"pear\r\napple\r\nfig\r\n", &table, 1, false),
        b"apple\r\nfig\r\npear\r\n"
    );
    assert_eq!(
        sorted(b"pear\r\napple\r\nfig", &table, 1, true),
        b"pear\r\nfig\r\napple\r\n",
        "a last line keeps its place"
    );
    assert_eq!(
        sorted(b"1 b\r\n2 a\r\n3 b\r\n", &table, 3, false),
        b"2 a\r\n1 b\r\n3 b\r\n",
        "equal keys keep their order"
    );
    assert_eq!(
        sorted(b"1 b\r\n2 a\r\n3 b\r\n", &table, 3, true),
        b"1 b\r\n3 b\r\n2 a\r\n"
    );
    assert_eq!(
        sorted(b"b\r\n\x1Aa\r\n", &table, 1, false),
        b"b\r\n",
        "a Ctrl+Z ends the input"
    );
    assert_eq!(sorted(b"", &table, 1, false), b"");
    // A line too short for the column sorts as 256 spaces: after a line
    // whose key starts lower than a space, before one starting higher.
    assert_eq!(
        sorted(b"!x\r\n\r\n\tx\r\n", &table, 1, false),
        b"\tx\r\n\r\n!x\r\n"
    );
    let mut upper = plain();
    upper[usize::from(b'a')] = b'A';
    assert_eq!(
        sorted(b"b\r\na\r\nA\r\n", &upper, 1, false),
        b"a\r\nA\r\nb\r\n",
        "through the country's table"
    );
}

#[test]
fn sort_reads_its_switches() {
    assert_eq!(sort::request(b" /r /+3"), Ok((3, true)));
    assert_eq!(sort::request(b""), Ok((1, false)));
    assert_eq!(sort::request(b" /+0"), Err(Refusal::Range(b"/+0")));
    assert_eq!(sort::request(b" /+70000"), Err(Refusal::Range(b"/+70000")));
    assert_eq!(sort::request(b" /+x"), Err(Refusal::Format(b"/+x")));
    assert_eq!(sort::request(b" /q"), Err(Refusal::Switch(b"/q")));
    assert_eq!(sort::request(b" file"), Err(Refusal::TooMany(b"file")));
}

/// A machine at the shell's prompt with the utilities and `files` on its
/// diskette.
pub(super) fn with_utilities(files: &[(&str, &[u8])]) -> Machine {
    with_utilities_on(
        &crate::machine::Template::IbmPc.config(),
        &crate::dos::harness::diskette(),
        files,
    )
}

/// The same, on the machine `config` builds, from a diskette of `volume`'s
/// shape.
pub(super) fn with_utilities_on(
    config: &crate::board::BoardConfig,
    volume: &tairix_fat::boot::Bpb,
    files: &[(&str, &[u8])],
) -> Machine {
    with_utilities_in(
        crate::dos::harness::machine_of(config, [None, None]),
        volume,
        files,
    )
}

/// The same, `m` booted.
pub(crate) fn with_utilities_in(
    m: Machine,
    volume: &tairix_fat::boot::Bpb,
    files: &[(&str, &[u8])],
) -> Machine {
    let utilities = crate::dos::utilities().unwrap();
    let names: Vec<[u8; 11]> = utilities
        .iter()
        .map(|(name, _)| *name)
        .chain(files.iter().map(|(name, _)| *name))
        .map(|spelt| name::field(spelt.as_bytes(), false).unwrap())
        .collect();
    let bytes = utilities
        .iter()
        .map(|(_, image)| image.as_slice())
        .chain(files.iter().map(|(_, bytes)| *bytes));
    let all: Vec<File<'_>> = names
        .iter()
        .zip(bytes)
        .map(|(name, bytes)| File { name, bytes })
        .collect();
    crate::dos::harness::prompted_in(m, volume, &all)
}

/// The screen after `line` is typed, the screen cleared first.
pub(super) fn run(m: &mut Machine, line: &str) -> String {
    typed(m, "cls");
    typed(m, line);
    screen(m)
}

const LINES: &[u8] = b"one\r\ntwo\r\nThree\r\nfour two\r\n";

#[test]
fn find_shows_the_lines_of_each_file_that_hold_its_string() {
    let mut m = with_utilities(&[("LINES.TXT", LINES), ("OTHER.TXT", b"nothing here\r\n")]);
    let text = run(&mut m, "find \"two\" lines.txt other.txt");
    assert!(
        text.contains("---------- LINES.TXT\ntwo\nfour two\n"),
        "{text}"
    );
    assert!(text.contains("---------- OTHER.TXT\n"), "{text}");
    let text = run(&mut m, "find /n /i \"THREE\" lines.txt");
    assert!(text.contains("[3]Three\n"), "{text}");
    let text = run(&mut m, "find /c /v \"two\" lines.txt other.txt");
    assert!(text.contains("---------- LINES.TXT: 2\n"), "{text}");
    assert!(text.contains("---------- OTHER.TXT: 1\n"), "{text}");
    let text = run(&mut m, "type lines.txt | find \"o\"");
    assert!(
        text.contains("\none\ntwo\nfour two\n"),
        "the standard input, no heading:\n{text}"
    );
    assert!(!text.contains("----------"), "{text}");
}

#[test]
fn find_says_what_is_wrong_and_ends_with_its_code() {
    let batch = b"@echo off\r\nfind \"two\" lines.txt >nul\r\nif not errorlevel 1 echo found\r\nfind \"none\" lines.txt >nul\r\nif errorlevel 1 if not errorlevel 2 echo absent\r\nfind \"x\" gone.txt >nul\r\nif errorlevel 2 echo failed\r\n";
    let mut m = with_utilities(&[("LINES.TXT", LINES), ("CODES.BAT", batch)]);
    assert!(run(&mut m, "find two lines.txt").contains("FIND: Parameter format not correct - TWO"));
    assert!(run(&mut m, "find").contains("FIND: Required parameter missing"));
    assert!(run(&mut m, "find /q \"x\"").contains("FIND: Invalid switch - /Q"));
    let text = run(&mut m, "find \"x\" gone.txt >nul");
    assert!(
        text.contains("FIND: File not found - GONE.TXT"),
        "errors go to the standard error:\n{text}"
    );
    let text = run(&mut m, "codes");
    assert!(
        text.contains("found\nabsent\nFIND: File not found - GONE.TXT\nfailed\n"),
        "{text}"
    );
}

#[test]
fn more_stops_a_screen_at_a_time() {
    let long: Vec<u8> = (1..=30)
        .flat_map(|n| alloc::format!("line {n}\r\n").into_bytes())
        .collect();
    let mut m = with_utilities(&[("LONG.TXT", &long)]);
    let text = run(&mut m, "more < long.txt");
    assert!(
        text.contains("line 23\n") && !text.contains("line 25"),
        "{text}"
    );
    assert!(text.trim_end().ends_with("-- More --"), "{text}");
    type_ahead(&mut m, " ");
    settle(&mut m);
    let text = screen(&m);
    assert!(text.contains("line 30\n"), "{text}");
    let text = run(&mut m, "type long.txt | more");
    assert!(
        text.trim_end().ends_with("-- More --"),
        "keys come from the console, not the pipe:\n{text}"
    );
    type_ahead(&mut m, " ");
    settle(&mut m);
    assert!(screen(&m).contains("line 30\n"), "{}", screen(&m));
}

#[test]
fn sort_sorts_its_input_to_its_output() {
    let mut m = with_utilities(&[("NAMES.TXT", b"pear\r\napple\r\nfig\r\n")]);
    let text = run(&mut m, "sort < names.txt");
    assert!(text.contains("\napple\nfig\npear\n"), "{text}");
    let text = run(&mut m, "type names.txt | sort /r");
    assert!(text.contains("\npear\nfig\napple\n"), "{text}");
    let text = run(&mut m, "sort /+2 < names.txt > sorted.txt");
    assert!(!text.contains("apple"), "{text}");
    let text = run(&mut m, "type sorted.txt");
    assert!(
        text.contains("\npear\nfig\napple\n"),
        "from the second column, e < i < p:\n{text}"
    );
    assert!(run(&mut m, "sort /x").contains("SORT: Invalid switch - /X"));
}

#[test]
fn attrib_reads_its_changes_path_and_switch() {
    use super::attrib::request;
    use tairix_fat::dir::attr;
    let got = request(b" +r -a +s+h file.txt /s").unwrap();
    assert_eq!(got.set, attr::READ_ONLY | attr::SYSTEM | attr::HIDDEN);
    assert_eq!(got.clear, attr::ARCHIVE);
    assert!(got.subdirectories);
    assert_eq!(got.path, b"file.txt");
    assert_eq!(request(b" a b"), Err(Refusal::TooMany(b"b")));
    assert_eq!(request(b" +x"), Err(Refusal::Format(b"+x")));
    assert_eq!(request(b" /q"), Err(Refusal::Switch(b"/q")));
    assert!(request(b" +r -r").is_err(), "a bit both set and cleared");
}

#[test]
fn attrib_lists_a_file_as_dos_does_and_keeps_a_hidden_one() {
    use super::attrib::{kept, shown};
    use tairix_fat::dir::attr;
    assert_eq!(
        shown(attr::ARCHIVE | attr::READ_ONLY, b"A:\\X.TXT"),
        b"A      R   A:\\X.TXT\r\n"
    );
    assert_eq!(
        shown(attr::SYSTEM | attr::HIDDEN, b"A:\\Y"),
        b"     SH    A:\\Y\r\n"
    );
    assert!(kept(attr::HIDDEN, attr::READ_ONLY, 0).is_some());
    assert!(kept(attr::HIDDEN, 0, attr::HIDDEN).is_none());
    assert!(
        kept(attr::SYSTEM | attr::HIDDEN, 0, attr::HIDDEN).is_some(),
        "the system bit is not named"
    );
    assert!(kept(attr::ARCHIVE, attr::READ_ONLY, 0).is_none());
}

#[test]
fn attrib_shows_and_changes_the_files_a_pattern_matches() {
    let mut m = with_utilities(&[("ONE.TXT", b"1"), ("TWO.DAT", b"2")]);
    typed(&mut m, "md sub");
    typed(&mut m, "copy one.txt sub\\three.txt");
    let text = run(&mut m, "attrib *.txt");
    assert!(text.contains("A          A:\\ONE.TXT\n"), "{text}");
    assert!(!text.contains("THREE"), "{text}");
    typed(&mut m, "attrib +r -a one.txt");
    let text = run(&mut m, "attrib one.txt");
    assert!(text.contains("       R   A:\\ONE.TXT\n"), "{text}");
    let text = run(&mut m, "attrib +h *.txt /s");
    assert!(!text.contains(".TXT"), "changing lists nothing:\n{text}");
    let text = run(&mut m, "attrib /s");
    assert!(text.contains("      HR   A:\\ONE.TXT\n"), "{text}");
    assert!(text.contains("A     H    A:\\SUB\\THREE.TXT\n"), "{text}");
    assert!(text.contains("A          A:\\TWO.DAT\n"), "{text}");
    let text = run(&mut m, "attrib -r one.txt");
    assert!(
        text.contains("Not resetting hidden file - A:\\ONE.TXT"),
        "{text}"
    );
    assert!(run(&mut m, "attrib none.txt").contains("File not found - A:\\NONE.TXT"));
}

#[test]
fn a_hidden_program_still_runs_and_a_directory_never_does() {
    // The shell looked for programs among plain files alone, so a program
    // ATTRIB hid could no longer be run by its name.
    let hello = crate::dos::harness::com(|asm| {
        crate::dos::harness::print(asm, b"hello from it\r\n$");
        asm.mov_imm16(crate::asm::Reg16::Ax, 0x4C00);
        asm.int(0x21);
    });
    let mut m = with_utilities(&[("HELLO.COM", &hello)]);
    typed(&mut m, "attrib +h +r hello.com");
    assert!(
        run(&mut m, "hello").contains("hello from it"),
        "{}",
        screen(&m)
    );
    typed(&mut m, "md prog.com");
    let text = run(&mut m, "prog");
    assert!(text.contains("Bad command or file name"), "{text}");
}

#[test]
fn label_reads_its_drive_and_label_and_spells_a_field() {
    use super::label::{field, request};
    assert_eq!(request(b" a:my disk"), (Some(0), b"my disk".to_vec()));
    assert_eq!(request(b" plain"), (None, b"plain".to_vec()));
    assert_eq!(request(b""), (None, Vec::new()));
    assert_eq!(field(b"MY DISK"), Some(*b"MY DISK    "));
    assert_eq!(
        field(b"ABCDEFGHIJKLMN"),
        Some(*b"ABCDEFGHIJK"),
        "cut to eleven"
    );
    assert_eq!(field(b"BAD*NAME"), None);
    assert_eq!(field(b"A.B"), None);
}

#[test]
fn label_makes_changes_and_deletes_a_disk_s_label() {
    let mut m = with_utilities(&[]);
    typed(&mut m, "label a:my disk");
    assert!(
        run(&mut m, "vol").contains("Volume in drive A is MY DISK"),
        "{}",
        screen(&m)
    );
    let text = run(&mut m, "label");
    assert!(text.contains("Volume in drive A is MY DISK"), "{text}");
    assert!(
        text.trim_end()
            .ends_with("Volume label (11 characters, ENTER for none)?"),
        "{text}"
    );
    typed(&mut m, "second");
    assert!(
        run(&mut m, "vol").contains("Volume in drive A is SECOND"),
        "{}",
        screen(&m)
    );
    run(&mut m, "label");
    typed(&mut m, "");
    assert!(
        screen(&m)
            .trim_end()
            .ends_with("Delete current volume label (Y/N)?"),
        "{}",
        screen(&m)
    );
    type_ahead(&mut m, "y");
    settle(&mut m);
    assert!(
        run(&mut m, "vol").contains("Volume in drive A has no label"),
        "{}",
        screen(&m)
    );
    assert!(run(&mut m, "label a:bad*name").contains("Invalid characters in volume label"));
    assert!(run(&mut m, "label q:x").contains("Invalid drive specification"));
}

#[test]
fn tree_reads_its_line_and_indents_each_level() {
    use super::tree::{indent, request, switch};
    assert_eq!(
        request(b" a:\\sub /f /a"),
        Ok((switch::FILES | switch::PLAIN, b"a:\\sub".to_vec()))
    );
    assert_eq!(request(b""), Ok((0, b".".to_vec())));
    assert_eq!(request(b" a b"), Err(Refusal::TooMany(b"b")));
    assert_eq!(request(b" /x"), Err(Refusal::Switch(b"/x")));
    assert_eq!(indent(0b01, 2, *b"\\-+|"), b"|       ");
    assert_eq!(indent(0b10, 2, *b"\\-+|"), b"    |   ");
    assert_eq!(indent(0, 0, *b"\\-+|"), b"");
}

#[test]
fn tree_draws_each_directory_below_its_parent() {
    let mut m = with_utilities(&[("A.TXT", b"a")]);
    for line in [
        "md sub1",
        "md sub1\\deep",
        "md sub2",
        "copy a.txt sub1\\c.txt",
        "copy a.txt sub1\\deep\\d.txt",
        "copy a.txt sub2\\e.txt",
    ] {
        typed(&mut m, line);
    }
    let text = run(&mut m, "tree /a");
    assert!(
        text.contains("Directory PATH listing\nVolume Serial Number is "),
        "{text}"
    );
    assert!(
        text.contains("\nA:\\\n+---SUB1\n|   \\---DEEP\n\\---SUB2\n"),
        "{text}"
    );
    let text = run(&mut m, "tree /f /a");
    let drawn = "|   A.TXT\n|\n+---SUB1\n|   |   C.TXT\n|   |\n|   \\---DEEP\n|           D.TXT\n|\n\\---SUB2\n        E.TXT\n";
    assert!(text.contains(drawn), "{text}");
    let text = run(&mut m, "tree");
    let lines: String = ['\u{C3}', '\u{C4}', '\u{C4}', '\u{C4}']
        .iter()
        .chain("SUB1".chars().collect::<Vec<_>>().iter())
        .collect();
    assert!(text.contains(&lines), "the line characters:\n{text}");
    assert!(
        run(&mut m, "tree sub2").contains("No sub-directories exist"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "tree nowhere").contains("Invalid path - A:\\NOWHERE"),
        "{}",
        screen(&m)
    );
}

#[test]
fn label_stops_asking_at_its_input_s_end() {
    // A redirected input ends in Ctrl+Z after Ctrl+Z, none a yes or a no,
    // so LABEL asked whether to delete the label for ever.
    let mut m = with_utilities(&[]);
    typed(&mut m, "label a:kept");
    let text = run(&mut m, "label < nul");
    assert!(
        text.contains("Delete current volume label (Y/N)?"),
        "{text}"
    );
    assert!(text.trim_end().ends_with("A:\\>"), "it ends:\n{text}");
    assert!(
        run(&mut m, "vol").contains("Volume in drive A is KEPT"),
        "{}",
        screen(&m)
    );
}

#[test]
fn a_ctrl_z_typed_at_the_console_answers_nothing() {
    let mut m = with_utilities(&[]);
    typed(&mut m, "label a:kept");
    typed(&mut m, "cls");
    run(&mut m, "label");
    typed(&mut m, "");
    crate::dos::harness::control(&mut m, crate::chips::keyboard::Scan::plain(0x2C));
    settle(&mut m);
    assert_eq!(
        screen(&m)
            .matches("Delete current volume label (Y/N)?")
            .count(),
        2,
        "asked again:\n{}",
        screen(&m)
    );
    type_ahead(&mut m, "n");
    settle(&mut m);
    assert!(
        run(&mut m, "vol").contains("Volume in drive A is KEPT"),
        "{}",
        screen(&m)
    );
}

#[test]
fn mem_reads_its_switches_and_spells_its_lines() {
    use super::mem::{class_line, hex, map_line, request, switch};
    assert_eq!(request(b" /p"), Ok(switch::PROGRAM));
    assert_eq!(request(b" /debug"), Ok(switch::PROGRAM | switch::DEBUG));
    assert_eq!(request(b" /CLASSIFY"), Ok(switch::CLASSIFY));
    assert_eq!(request(b" /x"), Err(Refusal::Switch(b"/x")));
    assert_eq!(request(b" x"), Err(Refusal::TooMany(b"x")));
    assert_eq!(hex(0x400, 6), b"000400");
    assert_eq!(
        hex(0x123_4567, 6),
        b"1234567",
        "a wider number keeps its digits"
    );
    assert_eq!(
        map_line(0x700, b"TAIRIX", 0x1F0, b"System Data"),
        b"  000700      TAIRIX       0001F0     System Data\r\n"
    );
    assert_eq!(
        class_line(b"COMMAND", 4512),
        b"  COMMAND             4512   (  4.4K)         11A0\r\n"
    );
}

#[test]
fn mem_shows_the_memory_its_blocks_and_their_owners() {
    let mut m = with_utilities(&[]);
    let text = run(&mut m, "mem");
    assert!(
        text.contains("    655360 bytes total conventional memory\n"),
        "{text}"
    );
    assert!(text.contains("bytes available to TAIRiX DOS"), "{text}");
    assert!(text.contains("largest executable program size"), "{text}");
    assert!(
        !text.contains("EMS"),
        "no board, no expanded memory:\n{text}"
    );
    let text = run(&mut m, "mem /p");
    assert!(
        text.contains("  000000                   000400     Interrupt Vector\n"),
        "{text}"
    );
    assert!(
        text.contains("COMMAND") && text.contains("Program") && text.contains("Environment"),
        "{text}"
    );
    assert!(
        text.contains("MEM") && text.contains("-- Free --"),
        "{text}"
    );
    let text = run(&mut m, "mem /d");
    assert!(
        text.contains("COM4") && text.contains("System Device Driver"),
        "{text}"
    );
    assert!(
        text.contains("A: - B:"),
        "the block driver's units by their drives:\n{text}"
    );
    let text = run(&mut m, "mem /c");
    assert!(
        text.contains("  COMMAND") && text.contains("Total  FREE :"),
        "{text}"
    );
    assert!(run(&mut m, "mem /x").contains("Invalid switch - /X"));
}

#[test]
fn mem_counts_the_expanded_memory_board_s_pages() {
    let workstation = crate::machine::Template::Workstation186.config();
    let mut m = with_utilities_on(
        &workstation,
        &crate::dos::formats::Diskette::K1440.volume(0x1993_0601),
        &[],
    );
    let text = run(&mut m, "mem");
    assert!(
        text.contains("   8388608 bytes total EMS memory\n"),
        "{text}"
    );
    assert!(
        text.contains("   8388608 bytes free EMS memory\n"),
        "{text}"
    );
    let text = run(&mut m, "mem /d");
    assert!(text.contains("Handle      EMS Name"), "{text}");
}

#[test]
fn deltree_reads_its_switch_and_paths() {
    use super::deltree::request;
    use crate::dos::name::directory_of;
    assert_eq!(
        request(b" /y a b"),
        Ok((true, alloc::vec![&b"a"[..], b"b"]))
    );
    assert_eq!(request(b" a"), Ok((false, alloc::vec![&b"a"[..]])));
    assert_eq!(request(b""), Err(Refusal::Missing));
    assert_eq!(request(b" /x a"), Err(Refusal::Switch(b"/x")));
    assert_eq!(directory_of(b"a:\\temp\\*.*"), b"a:\\temp\\");
    assert_eq!(directory_of(b"a:temp"), b"a:");
    assert_eq!(directory_of(b"temp"), b"");
}

#[test]
fn deltree_removes_a_tree_whatever_its_attributes() {
    let mut m = with_utilities(&[("A.TXT", b"a"), ("KEEP.TXT", b"k")]);
    for line in [
        "md tree",
        "md tree\\sub",
        "md tree\\sub\\deep",
        "copy a.txt tree\\x.txt",
        "copy a.txt tree\\sub\\y.txt",
        "copy a.txt tree\\sub\\deep\\z.txt",
        "attrib +h +r +s tree\\sub\\y.txt",
    ] {
        typed(&mut m, line);
    }
    let text = run(&mut m, "deltree tree");
    assert!(
        text.trim_end()
            .ends_with("Delete directory \"TREE\" and all its subdirectories? [yn]"),
        "{text}"
    );
    type_ahead(&mut m, "n");
    settle(&mut m);
    assert!(
        run(&mut m, "dir /b tree\\sub").contains("DEEP"),
        "no keeps it:\n{}",
        screen(&m)
    );
    run(&mut m, "deltree tree");
    type_ahead(&mut m, "y");
    settle(&mut m);
    assert!(
        screen(&m).contains("? [yn] y\nDeleting TREE...\n"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "dir /a tree.").contains("File not found"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "dir /b keep.txt").contains("KEEP.TXT"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "deltree").contains("Required parameter missing"),
        "{}",
        screen(&m)
    );
}

#[test]
fn deltree_takes_patterns_and_asks_nothing_with_y() {
    let mut m = with_utilities(&[("ONE.TXT", b"1"), ("TWO.TXT", b"2"), ("THREE.DAT", b"3")]);
    typed(&mut m, "attrib +r one.txt");
    let text = run(&mut m, "deltree /y *.txt");
    assert!(
        text.contains("Deleting ONE.TXT...\nDeleting TWO.TXT...\n") && !text.contains("[yn]"),
        "{text}"
    );
    let text = run(&mut m, "dir /b");
    assert!(
        !text.contains(".TXT") && text.contains("THREE.DAT"),
        "{text}"
    );
    let text = run(&mut m, "deltree three.dat");
    assert!(
        text.trim_end().ends_with("Delete file \"THREE.DAT\"? [yn]"),
        "{text}"
    );
    type_ahead(&mut m, "y");
    settle(&mut m);
    assert!(!run(&mut m, "dir /b").contains("THREE"), "{}", screen(&m));
}

#[test]
fn move_reads_its_sources_destination_and_switches() {
    use super::move_files::{copycmd_yes, request, Request};
    let sources = |list: &[&'static [u8]]| list.to_vec();
    assert_eq!(
        request(b" a.txt b.txt sub"),
        Ok(Request {
            sources: sources(&[b"a.txt", b"b.txt"]),
            destination: b"sub",
            overwrite: None
        })
    );
    assert_eq!(
        request(b" a.txt,b.txt sub").map(|r| r.sources),
        Ok(sources(&[b"a.txt", b"b.txt"]))
    );
    assert_eq!(request(b" /y a sub").map(|r| r.overwrite), Ok(Some(true)));
    assert_eq!(request(b" /-y a sub").map(|r| r.overwrite), Ok(Some(false)));
    assert_eq!(request(b" a"), Err(Refusal::Missing));
    assert_eq!(request(b""), Err(Refusal::Missing));
    assert_eq!(request(b" /x a b"), Err(Refusal::Switch(b"/x")));
    assert!(copycmd_yes(b"/Y"));
    assert!(!copycmd_yes(b"/-Y"));
    assert!(copycmd_yes(b"/-y /y"), "the last one decides");
    assert!(!copycmd_yes(b""));
}

#[test]
fn move_renames_files_into_a_directory_or_onto_a_name() {
    let mut m = with_utilities(&[("ONE.TXT", b"1"), ("TWO.TXT", b"2"), ("A.DAT", b"a")]);
    typed(&mut m, "md sub");
    let text = run(&mut m, "move one.txt sub");
    assert!(
        text.contains("a:\\one.txt => a:\\sub\\one.txt [ok]"),
        "{text}"
    );
    assert!(
        run(&mut m, "type sub\\one.txt").contains("\n1"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "dir one.txt").contains("File not found"),
        "{}",
        screen(&m)
    );
    let text = run(&mut m, "move two.txt three.txt");
    assert!(text.contains("a:\\two.txt => a:\\three.txt [ok]"), "{text}");
    typed(&mut m, "copy a.dat b.dat");
    let text = run(&mut m, "move *.dat one.dat");
    assert!(
        text.contains("Cannot move multiple files to a single file"),
        "{text}"
    );
    let text = run(&mut m, "dir /b *.dat");
    assert!(
        text.contains("A.DAT\n") && text.contains("B.DAT\n") && !text.contains("ONE.DAT"),
        "nothing moved:\n{text}"
    );
    assert!(
        run(&mut m, "move gone.txt sub").contains("File not found - GONE.TXT"),
        "{}",
        screen(&m)
    );
    let text = run(&mut m, "move sub renamed");
    assert!(
        text.contains("a:\\sub => a:\\renamed [ok]"),
        "a directory renamed:\n{text}"
    );
    assert!(
        run(&mut m, "dir /b renamed").contains("ONE.TXT"),
        "{}",
        screen(&m)
    );
}

#[test]
fn move_asks_before_overwriting_unless_told() {
    let mut m = with_utilities(&[("A.TXT", b"new"), ("B.TXT", b"newer")]);
    for line in ["md sub", "echo old>sub\\a.txt", "echo old>sub\\b.txt"] {
        typed(&mut m, line);
    }
    let text = run(&mut m, "move a.txt sub");
    assert!(
        text.trim_end()
            .ends_with("Overwrite a:\\sub\\a.txt (Yes/No/All)?"),
        "{text}"
    );
    type_ahead(&mut m, "n");
    settle(&mut m);
    assert!(
        run(&mut m, "type sub\\a.txt").contains("old"),
        "no keeps it:\n{}",
        screen(&m)
    );
    let text = run(&mut m, "move /y a.txt sub");
    assert!(
        text.contains("a:\\a.txt => a:\\sub\\a.txt [ok]") && !text.contains("Overwrite"),
        "{text}"
    );
    assert!(
        run(&mut m, "type sub\\a.txt").contains("new"),
        "{}",
        screen(&m)
    );
    typed(&mut m, "set copycmd=/Y");
    assert!(
        !run(&mut m, "move b.txt sub").contains("Overwrite"),
        "COPYCMD answers: {}",
        screen(&m)
    );
    assert!(
        run(&mut m, "type sub\\b.txt").contains("newer"),
        "{}",
        screen(&m)
    );
}

#[test]
fn move_copies_across_drives_and_deletes_the_source() {
    let mut m = with_utilities(&[("FAR.TXT", b"far away")]);
    let blank = crate::dos::harness::image_of(&crate::dos::harness::diskette(), &[]);
    m.board_mut()
        .insert_floppy(1, crate::dos::harness::floppy(blank))
        .unwrap();
    typed(&mut m, "attrib +r far.txt");
    let text = run(&mut m, "move far.txt b:");
    assert!(text.contains("a:\\far.txt => b:\\far.txt [ok]"), "{text}");
    assert!(
        run(&mut m, "type b:far.txt").contains("far away"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "attrib b:far.txt").contains("R   B:\\FAR.TXT"),
        "the source's attributes kept:\n{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "dir far.txt").contains("File not found"),
        "{}",
        screen(&m)
    );
}

/// Each SFT entry's count of the handles referring to it.
fn sft_handles(m: &Machine) -> Vec<u16> {
    use crate::cpu::Bus;
    use crate::dos::data::{sft, sysvars, SEGMENT, SFT_ENTRY, SFT_HEADER, SYSVARS};
    let peek16 = |segment: u16, offset: u16| {
        let byte = |at: u16| u16::from(m.board().peek(crate::cpu::linear(segment, at)));
        byte(offset) | byte(offset.wrapping_add(1)) << 8
    };
    let first = u16::try_from(i32::from(SYSVARS) + i32::from(sysvars::FIRST_SFT)).unwrap();
    let (mut segment, mut offset) = (peek16(SEGMENT, first + 2), peek16(SEGMENT, first));
    let mut counts = Vec::new();
    while offset != 0xFFFF && counts.len() < 0x100 {
        for n in 0..peek16(segment, offset + 4) {
            counts.push(peek16(
                segment,
                offset + SFT_HEADER + n * SFT_ENTRY + sft::HANDLES,
            ));
        }
        (segment, offset) = (peek16(segment, offset + 2), peek16(segment, offset));
    }
    counts
}

#[test]
fn a_program_a_break_ends_at_the_shells_question_leaves_every_handle_counted() {
    // A break at the question ended XCOPY through its own INT 23h: the
    // shell's handler was then resumed by the end of the program, and the
    // program's standard handles, pointed at the console uncounted, closed
    // the console twice at its end and never their own files.
    use crate::dos::harness::{control, settle, type_ahead, C};
    let mut m = with_utilities(&[]);
    let before = sft_handles(&m);
    for _ in 0..3 {
        type_ahead(&mut m, "xcopy /p /m  con>aux\r");
        settle(&mut m);
        assert!(
            screen(&m)
                .trim_end()
                .ends_with("Write fault error writing device AUX\nAbort, Retry, Ignore, Fail?"),
            "{}",
            screen(&m)
        );
        control(&mut m, C);
        settle(&mut m);
        assert!(
            screen(&m).trim_end().ends_with("Fail?^C\n\nA:\\>"),
            "{}",
            screen(&m)
        );
    }
    assert_eq!(
        sft_handles(&m),
        before,
        "every handle the program held was closed, and only those"
    );
    assert!(
        run(&mut m, "echo done").contains("\ndone\n"),
        "{}",
        screen(&m)
    );
}
