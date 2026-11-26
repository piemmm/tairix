//! The file commands' tests: what the dump makes of a file's bytes, and what
//! each command asks of the filing system.

use alloc::vec::Vec;

use super::Dump;
use crate::cli::tests::{cli, plain, refused, session, written};
use crate::input::InputEvent;
use crate::listing::{RAW_TOP, SEVEN_BIT};
use crate::testkit::{OneFile, Session, DATA};

#[test]
fn a_dump_line_holds_what_fits_the_window_cut_to_a_power_of_two_and_a_half() {
    let per = |width| Dump::for_width(width, 0).per_line;
    assert_eq!(per(80), 16);
    assert_eq!(per(40), 6);
    assert_eq!(per(37), 6);
    assert_eq!(per(29), 4);
    assert_eq!(per(25), 3);
    assert_eq!(per(13), 1);
    assert_eq!(per(0), 1, "at least one byte");
    assert_eq!(per(5000), 256, "no more than RISC OS's line buffer");
}

#[test]
fn a_dump_heading_numbers_its_columns_and_centres_its_title() {
    let mut out = Vec::new();
    Dump::for_width(80, 0).heading(0x80F8, b"Location :", b"Characters", &mut out);
    let expected =
        "\n\rLocation : F8 F9 FA FB FC FD FE FF 00 01 02 03 04 05 06 07 :     Characters\n\r\n\r";
    assert_eq!(out, expected.as_bytes());
    out.clear();
    Dump::for_width(40, 0).heading(0, b"Location :", b"Characters", &mut out);
    assert_eq!(
        out, b"\n\rLocation : 00 01 02 03 04 05\n\r\n\r",
        "no room for the title"
    );
}

#[test]
fn a_dump_line_shows_each_byte_in_hexadecimal_and_as_a_character() {
    let mut out = Vec::new();
    Dump::for_width(29, 0).line(0x1_2340, b"A\x00\xE9", &mut out);
    assert_eq!(out, b"00012340 : 41 00 E9    : A..\n\r");
    out.clear();
    Dump::for_width(29, RAW_TOP).line(0, b"\xE9", &mut out);
    assert_eq!(out, b"00000000 : E9          : \xE9\n\r");
    out.clear();
    Dump::for_width(29, SEVEN_BIT).line(0, b"\xC1", &mut out);
    assert_eq!(out, b"00000000 : C1          : A\n\r");
}

/// A session with a file of `bytes` and its output captured.
fn with_file(bytes: &[u8]) -> (Session, OneFile) {
    let mut s = session();
    let file = s.one_file(bytes);
    (s, file)
}

#[test]
fn type_list_and_print_show_the_file_as_they_each_do() {
    let (mut s, file) = with_file(b"one\ttwo\r\xA9\x07");
    // The configured DumpFormat shows a top-bit character as it is.
    assert_eq!(written(&mut s, "Type text"), b"one|Itwo\n\r\xA9|G\n\r");
    assert_eq!(
        file.word(&mut s, OneFile::OPENED),
        0x4C,
        "opened for input, a missing file refused"
    );
    assert_eq!(file.word(&mut s, OneFile::HANDLE), 0, "closed again");
    assert_eq!(
        written(&mut s, "List text"),
        b"   1 one|Itwo\n\r   2 \xA9|G\n\r"
    );
    assert_eq!(
        written(&mut s, "Type -TabExpand text"),
        b"one        two\n\r\xA9|G\n\r"
    );
    let format = s.kernel.os().read_cmos(0x84).expect("read");
    crate::cli::tests::cli(&mut s, &alloc::format!("FX 162 132 {}", format | 0x10))
        .expect("written");
    assert_eq!(
        written(&mut s, "Type text"),
        b"one        two\n\r\xA9|G\n\r",
        "the configured DumpFormat expands them too"
    );
    assert_eq!(written(&mut s, "Print text"), b"one\ttwo\r\xA9\x07");
}

#[test]
fn escape_pressed_while_a_listing_runs_stops_it_and_closes_its_file() {
    // Nothing reads the keyboard: the key comes in on the clock's interrupt,
    // as the keyboard's would.
    let (mut s, file) = with_file(&[b'x'; 64]);
    s.clock.set(1);
    s.arrive(1, InputEvent::Key(0x1B));
    assert_eq!(refused(&mut s, "Type text"), b"Escape");
    assert_eq!(file.word(&mut s, OneFile::HANDLE), 0, "closed");
}

#[test]
fn dump_counts_from_the_load_address_of_a_file_that_has_one() {
    let (mut s, file) = with_file(b"ABCDEFGHIJKLMNOPQR");
    let lines = written(&mut s, "Dump data");
    let text = core::str::from_utf8(&lines).unwrap_or_default();
    assert!(
        text.contains("00000000 : 41 42"),
        "a typed file from nought: {text}"
    );
    assert!(text.contains("00000010 : 51 52"), "its second line: {text}");
    file.describe(&mut s, 1, (0x0000_8000, 0x8000), u32::MAX);
    let lines = written(&mut s, "Dump data 10");
    let text = core::str::from_utf8(&lines).unwrap_or_default();
    assert!(
        text.contains("00008010 : 51 52"),
        "from its load address and the offset: {text}"
    );
    let lines = written(&mut s, "Dump data 2 20000");
    let text = core::str::from_utf8(&lines).unwrap_or_default();
    assert!(
        text.contains("00020002 : 43 44"),
        "as if loaded at the address given: {text}"
    );
    assert_eq!(refused(&mut s, "Dump data 100"), b"Outside file");
    assert_eq!(
        file.word(&mut s, OneFile::HANDLE),
        0,
        "closed after the error"
    );
}

#[test]
fn build_and_append_write_each_line_typed_until_escape() {
    let (mut s, file) = with_file(b"kept\r");
    for (n, &key) in b"  <A>|Mx\r\x1B".iter().enumerate() {
        s.arrive(n as u64, InputEvent::Key(key));
    }
    written(&mut s, "Set A one");
    assert_eq!(cli(&mut s, "Append notes"), Ok(()));
    // Leading spaces as they are, the rest GSTrans'd; the last line, ended
    // by Escape, without its CR.
    assert_eq!(file.bytes(&mut s), b"kept\r  one\rx\r");
    assert_eq!(
        file.word(&mut s, OneFile::OPENED),
        0xCC,
        "the file opened to update"
    );
    let shown = s.output();
    assert!(
        shown.starts_with(b"   1 "),
        "each line's number is its prompt"
    );
}

#[test]
fn load_save_and_create_ask_os_file_as_the_prm_says() {
    let (mut s, file) = with_file(b"");
    assert_eq!(
        refused(&mut s, "Load f"),
        b"No loader is set for files of type &FFD"
    );
    written(&mut s, "Set Alias$@LoadType_FFD Load %0 9000");
    written(&mut s, "Load f");
    let call = file.file_call(&mut s);
    assert_eq!(
        [call[0], call[2], call[3]],
        [255, 0x9000, 0],
        "as its type's alias says"
    );
    file.describe(&mut s, 1, (0x8000, 0x8000), u32::MAX);
    written(&mut s, "Load f");
    assert_eq!(
        file.file_call(&mut s)[..4],
        [255, DATA + 5, 0, 0xFF],
        "untyped, at its own address"
    );
    written(&mut s, "Load f 9000");
    assert_eq!(file.file_call(&mut s)[..4], [255, DATA + 5, 0x9000, 0]);
    assert_eq!(refused(&mut s, "Load f 9000x"), b"Invalid address");
    written(&mut s, "Save f 8000 9000");
    assert_eq!(
        file.file_call(&mut s)[..6],
        [0, DATA + 5, 0x8000, 0x8000, 0x8000, 0x9000]
    );
    written(&mut s, "Save f 8000 + 100 A000 B000");
    assert_eq!(
        file.file_call(&mut s)[..6],
        [0, DATA + 5, 0xB000, 0xA000, 0x8000, 0x8100]
    );
    let syntax =
        crate::cli::tests::cli(&mut s, "Save f 8000 9000 1 2 3 4").map_err(|(number, _)| number);
    assert_eq!(syntax, Err(0xDC), "seven parameters are one too many");
    assert_eq!(
        refused(&mut s, "Save f 8000 9000 1 2 3"),
        b"*Save was given the wrong number of parameters"
    );
    assert_eq!(
        refused(&mut s, "Save f 8000 9000 x"),
        b"*Save was given the wrong number of parameters"
    );
    written(&mut s, "Create f");
    assert_eq!(
        file.file_call(&mut s)[..6],
        [11, DATA + 7, 0xFFFF_FFFD, 0, 0, 0]
    );
    written(&mut s, "Create f 400");
    assert_eq!(
        file.file_call(&mut s)[..6],
        [11, DATA + 7, 0xFFFF_FFFD, 0, 0, 0x400]
    );
    written(&mut s, "Create f 400 8000 9000");
    assert_eq!(
        file.file_call(&mut s)[..6],
        [7, DATA + 7, 0x9000, 0x8000, 0, 0x400]
    );
}

#[test]
fn delete_says_when_there_was_nothing_and_remove_does_not() {
    let (mut s, file) = with_file(b"");
    written(&mut s, "Delete f");
    assert_eq!(file.file_call(&mut s)[0], 6, "deleted");
    file.describe(&mut s, 0, (0, 0), 0);
    s.poke(DATA + 0xE00, &[0x1_00D6]);
    s.write(DATA + 0xE04, b"File 'f' not found\0");
    s.poke(file.state + u32::from(OneFile::ERROR), &[DATA + 0xE00]);
    assert_eq!(refused(&mut s, "Delete f"), b"File 'f' not found");
    assert_eq!(
        file.file_call(&mut s)[..3],
        [19, DATA + 7, 0],
        "the filing system's error made"
    );
    written(&mut s, "Remove f");
    assert_eq!(file.file_call(&mut s)[0], 6, "nothing said");
}

#[test]
fn close_closes_every_file() {
    let (mut s, file) = with_file(b"");
    s.poke(file.state + u32::from(OneFile::HANDLE), &[OneFile::OPEN]);
    written(&mut s, "Close");
    assert_eq!(file.word(&mut s, OneFile::HANDLE), 0);
}

#[test]
fn spool_copies_the_output_to_a_file_until_it_is_ended() {
    let mut s = plain();
    let file = s.one_file(b"stale");
    assert_eq!(cli(&mut s, "Spool log"), Ok(()));
    assert_eq!(
        file.word(&mut s, OneFile::OPENED),
        0x8C,
        "opened for output"
    );
    assert_eq!(cli(&mut s, "Echo spooled"), Ok(()));
    assert_eq!(cli(&mut s, "Spool"), Ok(()));
    assert_eq!(file.bytes(&mut s), b"spooled\n\r");
    assert_eq!(file.word(&mut s, OneFile::HANDLE), 0, "closed");
    assert_eq!(cli(&mut s, "SpoolOn log"), Ok(()));
    assert_eq!(file.word(&mut s, OneFile::OPENED), 0xCC, "opened to update");
    assert_eq!(cli(&mut s, "Echo more"), Ok(()));
    assert_eq!(cli(&mut s, "Spool"), Ok(()));
    assert_eq!(
        file.bytes(&mut s),
        b"spooled\n\rmore\n\r",
        "added at its end"
    );
}

#[test]
fn exec_takes_the_keyboard_s_input_from_a_file() {
    let mut s = session();
    let file = s.one_file(b"Echo from the file\r");
    assert_eq!(cli(&mut s, "Exec script"), Ok(()));
    assert_eq!(file.word(&mut s, OneFile::OPENED), 0x4C);
    let (read, _) = s.user_swi(crate::swi::X | 0x04, [0; 13]);
    assert_eq!(read.r[0], u32::from(b'E'), "OS_ReadC reads the file");
    assert_eq!(cli(&mut s, "Exec"), Ok(()));
    assert_eq!(file.word(&mut s, OneFile::HANDLE), 0, "closed");
}
