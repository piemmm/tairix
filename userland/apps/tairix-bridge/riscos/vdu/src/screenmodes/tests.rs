use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::*;

/// A mode whose lines are `keys`, each `key:value`, the rest of a good
/// mode's after them.
fn mode_with(keys: &[&str]) -> String {
    let good = [
        "mode_name:VGA",
        "x_res:640",
        "y_res:480",
        "pixel_rate:25175",
        "h_timings:96,40,8,640,8,8",
        "v_timings:2,25,8,480,8,2",
        "sync_pol:3",
    ];
    let named = |line: &str| line.split(':').next().unwrap_or("").to_ascii_lowercase();
    let mut lines: Vec<String> = keys.iter().map(|k| (*k).to_owned()).collect();
    for line in good {
        if !keys.iter().any(|k| named(k) == named(line)) {
            lines.push(line.to_owned());
        }
    }
    lines.retain(|line| !line.ends_with(":-"));
    format!("startmode\n{}\nendmode\n", lines.join("\n"))
}

const HEAD: &str = "file_format:1\nmonitor_title:T\n";

fn refused(text: &str) -> Option<Unread> {
    parse(text.as_bytes()).err()
}

fn fault(text: &str) -> Option<Fault> {
    match refused(text) {
        Some(Unread::Line(_, fault)) => Some(fault),
        _ => None,
    }
}

#[test]
fn a_monitor_file_s_title_dpms_state_and_modes_are_read() {
    let read = parse(
        format!(
            "file_format:1\nmonitor_title:Test monitor\n# a comment\nDPMS_state:2\n{}",
            mode_with(&[])
        )
        .as_bytes(),
    )
    .expect("read");
    assert_eq!(read.title, b"Test monitor");
    assert_eq!(read.dpms, Some(2));
    assert_eq!(read.modes.len(), 1);
    let mode = &read.modes[0];
    assert_eq!((mode.width(), mode.height()), (640, 480));
    assert_eq!(mode.frame_rate(), 60);
    assert_eq!(mode.name, b"VGA");
}

#[test]
fn a_mode_s_size_is_its_timings_display_which_a_given_size_must_match() {
    let read = parse(
        format!(
            "{HEAD}{}",
            mode_with(&["x_res:-", "y_res:-", "h_timings:96,40,8,800,8,8"])
        )
        .as_bytes(),
    )
    .expect("read");
    assert_eq!((read.modes[0].width(), read.modes[0].height()), (800, 480));
    assert_eq!(
        fault(&format!("{HEAD}{}", mode_with(&["x_res:800"]))),
        Some(Fault::SizeDisagrees)
    );
    assert_eq!(
        fault(&format!("{HEAD}{}", mode_with(&["y_res:400"]))),
        Some(Fault::SizeDisagrees)
    );
    for zero in [
        "x_res:0",
        "y_res:0",
        "pixel_rate:0",
        "h_timings:0,0,0,0,0,0",
    ] {
        assert_eq!(
            fault(&format!("{HEAD}{}", mode_with(&[zero]))),
            Some(Fault::NoSize),
            "{zero}"
        );
    }
}

#[test]
fn numbers_are_decimal_digits_bounded_and_alone() {
    let with = |line: &str| fault(&format!("{HEAD}{}", mode_with(&[line])));
    for line in [
        "x_res:abc",
        "x_res:+640",
        "x_res:&280",
        "h_timings:96,40,8,640,8",
        "sync_pol:x",
    ] {
        assert_eq!(with(line), Some(Fault::NoNumber), "{line}");
    }
    for line in [
        "pixel_rate:12x",
        "h_timings:96,40,8,640,8,8,9",
        "x_res:640,480",
        "x_res:640 # c",
    ] {
        assert_eq!(with(line), Some(Fault::Trailing), "{line}");
    }
    assert_eq!(with("sync_pol:4"), Some(Fault::TooLarge));
    assert_eq!(
        fault(&format!("{HEAD}DPMS_state:4294967296\n{}", mode_with(&[]))),
        Some(Fault::TooLarge)
    );
    for spaced in [
        "x_res: 640",
        "x_res:640 ",
        "X_RES:640",
        "h_timings:96, 40 ,8,640,8,8",
    ] {
        assert!(
            refused(&format!("{HEAD}{}", mode_with(&[spaced]))).is_none(),
            "{spaced}"
        );
    }
}

#[test]
fn each_line_out_of_place_is_refused_as_risc_os_5_refuses_it() {
    let good = mode_with(&[]);
    for (text, expected) in [
        (
            format!("monitor_title:T\nfile_format:1\n{good}"),
            Fault::Unreadable,
        ),
        (format!("file_format:1\n{HEAD}{good}"), Fault::Unreadable),
        (
            format!("file_format:2\nmonitor_title:T\n{good}"),
            Fault::Format,
        ),
        (
            format!("file_format:x\nmonitor_title:T\n{good}"),
            Fault::NoNumber,
        ),
        (format!("{HEAD}monitor_title:U\n{good}"), Fault::Unreadable),
        (
            format!("{HEAD}DPMS_state:1\nDPMS_state:2\n{good}"),
            Fault::Unreadable,
        ),
        (format!("{HEAD}DPMS_state:x\n{good}"), Fault::NoNumber),
        (format!("{HEAD}lcd_support:2\n{good}"), Fault::Unreadable),
        (
            format!("{HEAD}lcd_support:1\nlcd_support:0\n{good}"),
            Fault::Unreadable,
        ),
        (format!("{HEAD}foo:1\n{good}"), Fault::Unreadable),
        (format!("{HEAD}startmodex\n{good}"), Fault::Unreadable),
        (format!("{HEAD}endmode\n{good}"), Fault::Unreadable),
        (
            format!("{HEAD}{}", good.replace("startmode\n", "startmode extra\n")),
            Fault::Trailing,
        ),
        (
            format!("{HEAD}{}", good.replace("endmode\n", "endmode extra\n")),
            Fault::Trailing,
        ),
        (format!("{HEAD}startmode\n{good}"), Fault::NotInMode),
        (
            format!("{HEAD}{}", mode_with(&["monitor_title:X"])),
            Fault::NotInMode,
        ),
        (
            format!("{HEAD}{}", mode_with(&["foo:1"])),
            Fault::UnknownInMode,
        ),
        (
            format!("{HEAD}{}", mode_with(&["x_res:640", "x_res:640"])),
            Fault::Repeated,
        ),
        (
            format!("{HEAD}{}", mode_with(&["mode_name:-"])),
            Fault::Unnamed,
        ),
        (
            format!("{HEAD}{}", mode_with(&["v_timings:-"])),
            Fault::Incomplete,
        ),
        (
            format!("{HEAD}{}", mode_with(&["sync_pol:-"])),
            Fault::Incomplete,
        ),
        (
            format!("{HEAD}{}", good.replace("endmode\n", "")),
            Fault::EndsInMode,
        ),
        (HEAD.to_owned(), Fault::NoModes),
        (
            format!("file_format:1\nmonitor_title:   \n{good}"),
            Fault::BlankTitle,
        ),
        (format!("file_format:1\n{good}"), Fault::Unreadable),
    ] {
        assert_eq!(fault(&text), Some(expected), "{text:?}");
    }
}

#[test]
fn a_refusal_names_the_line_it_fails_at() {
    assert_eq!(
        refused("# a comment\n\nfile_format:2\nmonitor_title:x\n"),
        Some(Unread::Line(3, Fault::Format))
    );
    assert_eq!(
        refused("file_format:1\r\nmonitor_title:x\r\nwhat:3\r\n"),
        Some(Unread::Line(3, Fault::Unreadable))
    );
    assert_eq!(
        refused("file_format:1\nmonitor_title:x\nstartmode\n"),
        Some(Unread::Line(3, Fault::EndsInMode)),
        "the last line read"
    );
}

#[test]
fn comments_are_whole_lines_and_a_title_keeps_31_bytes_and_any_hash() {
    let read = parse(
        format!(
            "  # indented\nfile_format :1\nmonitor_title :T # not a comment\n{}",
            mode_with(&["mode_name:A#B"])
        )
        .as_bytes(),
    )
    .expect("read");
    assert_eq!(read.title, b"T # not a comment");
    assert_eq!(read.modes[0].name, b"A#B");
    let long = "L".repeat(40);
    let read = parse(format!("file_format:1\nmonitor_title:{long}\n{}", mode_with(&[])).as_bytes())
        .expect("read");
    assert_eq!(read.title.len(), TITLE_MOST as usize);
}
