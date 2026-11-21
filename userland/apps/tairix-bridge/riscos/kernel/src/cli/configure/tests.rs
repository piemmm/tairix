//! `*Configure` and `*Status`'s tests: how a keyword is named, what each
//! kind writes to CMOS, and what each shows.

use super::find;
use crate::cli::tests::{cli, refused, session, written};
use crate::swi::os::BYTE;
use crate::swi::X;
use crate::testkit::Session;

/// CMOS location `logical`.
fn cmos(s: &mut Session, logical: u32) -> u32 {
    let (read, _) = s.user_swi(X | BYTE, [161, logical, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    read.r[2]
}

/// What `*Status keyword` writes.
fn status(s: &mut Session, keyword: &str) -> alloc::string::String {
    let text = written(s, &alloc::format!("Status {keyword}"));
    alloc::string::String::from_utf8_lossy(&text).into_owned()
}

#[test]
fn a_keyword_is_named_whole_or_abbreviated_the_first_in_the_table_winning() {
    let name = |typed: &[u8]| find(typed).map(|(keyword, end)| (keyword.name, end));
    assert_eq!(name(b"Delay 30"), Some(("Delay", 5)));
    assert_eq!(name(b"delay"), Some(("Delay", 5)));
    assert_eq!(name(b"De. 30"), Some(("Delay", 3)));
    assert_eq!(name(b"D."), Some(("Data", 2)), "the first D in the table");
    assert_eq!(
        name(b"Boot."),
        Some(("Boot", 5)),
        "a whole name may have its dot"
    );
    assert_eq!(name(b"."), None, "a dot alone names nothing");
    assert_eq!(name(b"Nonesuch"), None);
}

#[test]
fn a_number_is_written_to_its_field_and_shown_back() {
    let mut s = session();
    written(&mut s, "Configure Delay 30");
    assert_eq!(cmos(&mut s, 0x0C), 30);
    assert_eq!(status(&mut s, "Delay"), "Delay      30\n\r");
    written(&mut s, "Configure Data 5");
    assert_eq!(cmos(&mut s, 0x10) >> 5, 5);
    assert_eq!(
        refused(&mut s, "Configure Delay x"),
        b"Numeric parameter needed"
    );
    assert_eq!(
        refused(&mut s, "Configure Delay 300"),
        b"Configure parameter too large"
    );
    assert_eq!(
        refused(&mut s, "Configure Delay 31 x"),
        b"Too many parameters"
    );
    assert_eq!(cmos(&mut s, 0x0C), 30, "a refused line changes nothing");
}

#[test]
fn a_keyword_with_no_value_sets_its_field_and_status_says_which_is_set() {
    let mut s = session();
    written(&mut s, "Configure NoBoot");
    assert_eq!(cmos(&mut s, 0x10) & 0x10, 0);
    assert_eq!(status(&mut s, "NoBoot"), "No Boot\n\r");
    assert_eq!(
        status(&mut s, "Boot"),
        "No Boot\n\r",
        "the one set, whichever is asked"
    );
    written(&mut s, "Configure Boot");
    assert_eq!(status(&mut s, "NoBoot"), "Boot\n\r");
    written(&mut s, "Configure ShCaps");
    assert_eq!(status(&mut s, "Caps"), "Shift Caps\n\r");
    written(&mut s, "Configure Caps");
    assert_eq!(status(&mut s, "NoCaps"), "Caps\n\r");
}

#[test]
fn a_size_counts_units_or_kibibytes_rounded_up() {
    let mut s = session();
    written(&mut s, "Configure FontSize 64K");
    assert_eq!(cmos(&mut s, 0x86), 16, "in 4 KiB units");
    assert_eq!(status(&mut s, "FontSize"), "FontSize   64K\n\r");
    written(&mut s, "Configure RMASize 5K");
    assert_eq!(cmos(&mut s, 0x92), 2, "rounded up to whole pages");
    written(&mut s, "Configure RMASize 3");
    assert_eq!(status(&mut s, "RMASize"), "RMASize    12K\n\r");
    written(&mut s, "Configure PrinterBufferSize 1K");
    assert_eq!(cmos(&mut s, 0x19), 1, "a page, as any size is rounded");
    assert_eq!(
        status(&mut s, "PrinterBufferSize"),
        "PrinterBufferSize 4K\n\r"
    );
    assert_eq!(
        refused(&mut s, "Configure PrinterBufferSize 128"),
        b"Configure parameter too large",
        "127 pages at most"
    );
    assert_eq!(
        refused(&mut s, "Configure FontSize 64M"),
        b"Numeric parameter needed"
    );
}

#[test]
fn baud_and_tv_keep_their_own_encodings() {
    let mut s = session();
    written(&mut s, "Configure Baud 0");
    assert_eq!((cmos(&mut s, 0x0F) >> 2) & 7, 6, "nought is stored as 9600");
    assert_eq!(status(&mut s, "Baud"), "Baud       7\n\r");
    assert_eq!(
        refused(&mut s, "Configure Baud 9"),
        b"Configure parameter too large"
    );
    written(&mut s, "Configure TV 255,1");
    assert_eq!(cmos(&mut s, 0x0A) >> 4, 0xF);
    assert_eq!(status(&mut s, "TV"), "TV         255,1\n\r");
    written(&mut s, "Configure TV 2");
    assert_eq!(status(&mut s, "TV"), "TV         2,0\n\r");
    written(&mut s, "Configure TV");
    assert_eq!(status(&mut s, "TV"), "TV         0,0\n\r");
    assert_eq!(refused(&mut s, "Configure TV 4"), b"Bad configure option");
    assert_eq!(
        refused(&mut s, "Configure TV 4294967295"),
        b"Bad configure option",
        "only the PRM's positions, however the number reads"
    );
    assert_eq!(
        refused(&mut s, "Configure TV 1,2"),
        b"Configure parameter too large"
    );
    assert_eq!(refused(&mut s, "Configure TV 1,"), b"Bad configure option");
}

#[test]
fn ignore_takes_its_first_number_and_turns_on_only_for_a_number_alone() {
    let mut s = session();
    written(&mut s, "Configure Ignore 9");
    assert_eq!(status(&mut s, "Ignore"), "Ignore     9\n\r");
    written(&mut s, "Configure Ignore");
    assert_eq!(status(&mut s, "Ignore"), "No Ignore\n\r");
    assert_eq!(cmos(&mut s, 0x0E), 0, "no ignore keeps nought");
    written(&mut s, "Configure Ignore 9 x");
    assert_eq!(cmos(&mut s, 0x0E), 9, "the number before what follows");
    assert_eq!(status(&mut s, "Ignore"), "No Ignore\n\r", "still off");
    written(&mut s, "Configure Ignore x");
    assert_eq!(cmos(&mut s, 0x0E), 0, "no number is nought");
    written(&mut s, "Configure Ignore &41");
    assert_eq!(
        status(&mut s, "Ignore"),
        "Ignore     65\n\r",
        "a number alone turns it on"
    );
    written(&mut s, "Configure Ignore");
    written(&mut s, "Configure Ignore 9 ,");
    assert_eq!(
        status(&mut s, "Ignore"),
        "No Ignore\n\r",
        "a comma after a space follows"
    );
    written(&mut s, "Configure Ignore 9x");
    assert_eq!(status(&mut s, "Ignore"), "No Ignore\n\r");
    written(&mut s, "Configure Ignore 7,");
    assert_eq!(
        status(&mut s, "Ignore"),
        "Ignore     7\n\r",
        "a comma straight after ends it"
    );
    written(&mut s, "Configure Ignore");
    written(&mut s, "Configure Ignore 8 ");
    assert_eq!(
        status(&mut s, "Ignore"),
        "Ignore     8\n\r",
        "spaces before the end"
    );
    assert_eq!(
        refused(&mut s, "Configure Ignore 300"),
        b"Configure parameter too large"
    );
    assert_eq!(cmos(&mut s, 0x0E), 8, "nothing changed");
}

#[test]
fn mode_sync_and_monitor_type_take_auto_and_are_read_by_os_read_sys_info() {
    let mut s = session();
    written(&mut s, "Configure Mode 28");
    assert_eq!(status(&mut s, "Mode"), "Mode       28\n\r");
    assert_eq!(
        status(&mut s, "WimpMode"),
        "WimpMode   28\n\r",
        "the same setting"
    );
    written(&mut s, "Configure WimpMode Auto");
    assert_eq!(status(&mut s, "Mode"), "Mode       Auto\n\r");
    written(&mut s, "Configure Sync 1");
    assert_eq!(status(&mut s, "Sync"), "Sync       1\n\r");
    assert_eq!(
        refused(&mut s, "Configure Sync 2"),
        b"Configure parameter too large"
    );
    written(&mut s, "Configure MonitorType 3");
    assert_eq!(status(&mut s, "MonitorType"), "MonitorType 3\n\r");
    written(&mut s, "Configure MonitorType Auto");
    assert_eq!(status(&mut s, "MonitorType"), "MonitorType Auto\n\r");
    assert_eq!(
        refused(&mut s, "Configure MonitorType 6"),
        b"Configure parameter too large"
    );
    written(&mut s, "Configure Mode 0");
    let (info, _) = s.user_swi(X | 0x58, [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(info.r[..3], [0, 3, 1], "mode nought is a mode");
}

#[test]
fn mouse_step_is_1_to_127_and_cache_is_on_or_off() {
    let mut s = session();
    written(&mut s, "Configure MouseStep 3");
    assert_eq!(cmos(&mut s, 0xC2), 3);
    assert_eq!(status(&mut s, "MouseStep"), "MouseStep  3\n\r");
    assert_eq!(
        refused(&mut s, "Configure MouseStep -2"),
        b"Numeric parameter needed"
    );
    assert_eq!(
        refused(&mut s, "Configure MouseStep 0"),
        b"Bad configure option"
    );
    assert_eq!(
        refused(&mut s, "Configure MouseStep 200"),
        b"Configure parameter too large"
    );
    written(&mut s, "Configure Cache Off");
    assert_eq!(status(&mut s, "Cache"), "Cache      Off\n\r");
    written(&mut s, "Configure Cache On");
    assert_eq!(status(&mut s, "Cache"), "Cache      On\n\r");
    assert_eq!(
        refused(&mut s, "Configure Cache Maybe"),
        b"Bad configure option"
    );
}

#[test]
fn an_unknown_keyword_is_refused_by_each() {
    let mut s = session();
    assert_eq!(
        refused(&mut s, "Configure Nonesuch 1"),
        b"Bad configure option"
    );
    assert_eq!(refused(&mut s, "Status Nonesuch"), b"Bad status option");
}

#[test]
fn configure_alone_lists_what_each_keyword_takes() {
    let mut s = session();
    let text = written(&mut s, "Configure");
    let text = alloc::string::String::from_utf8_lossy(&text);
    assert!(
        text.starts_with(
            "\x0EMachine configuration keywords:\n\r\n\rBaud       <D>\n\rBoot\n\rNoBoot\n\r"
        ),
        "{text}"
    );
    assert!(text.contains("FontSize   <D>[K]\n\r"));
    assert!(text.contains("TV         [<D> [[,] <D>]]\n\r"));
    assert!(text.contains("MonitorType <D> | Auto\n\r"));
    assert!(
        text.contains("PrinterBufferSize <D>[K]\n\r"),
        "a long name, one space"
    );
    assert!(
        text.ends_with("*Status lists the current settings.\n\r\x0F"),
        "{text}"
    );
}

#[test]
fn status_alone_shows_what_each_keyword_is_set_to() {
    let mut s = session();
    written(&mut s, "Configure Delay 30");
    let text = written(&mut s, "Status");
    let text = alloc::string::String::from_utf8_lossy(&text);
    assert!(
        text.starts_with("\x0EMachine configuration settings:\n\r\n\rBaud       "),
        "{text}"
    );
    assert!(text.contains("Delay      30\n\r"));
    let caps = text
        .split("\n\r")
        .filter(|line| matches!(*line, "Caps" | "No Caps" | "Shift Caps"))
        .count();
    assert_eq!(caps, 1, "one line for the Caps group");
    assert!(
        text.ends_with("To change a setting, use *Configure.\n\r\x0F"),
        "{text}"
    );
    assert_eq!(
        cli(&mut s, "Status Delay Baud").map_err(|(n, _)| n),
        Err(0xDC),
        "two keywords are a module's"
    );
}
