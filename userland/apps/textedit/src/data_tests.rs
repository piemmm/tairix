use alloc::string::String;
use alloc::vec::Vec;

use tairix_image::inspect::ReportLine;
use tairix_sandbox::imageinspect::{InspectFailure, InspectRefusal};
use tairix_sandbox::imagerender::ViewFormat;

use super::{no_memory, too_large, unreported, DataDisplay, DataRow, READING};
use crate::answer::Unlisted;

/// The opening bytes of a PNG file.
const PNG_HEAD: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR";

fn report(lines: usize) -> Vec<ReportLine> {
    (0..lines)
        .map(|at| ReportLine {
            text: alloc::format!("line {at}"),
            heading: at == 0,
        })
        .collect()
}

#[test]
fn a_picture_is_offered_as_data_by_its_signature_or_a_sprite_by_its_name() {
    let png = DataDisplay::of(PNG_HEAD, "anything").expect("a picture");
    assert!(png.showing(), "a picture opens as data");
    assert_eq!(png.named(), None, "its content says what it is");
    let sprite = DataDisplay::of(b"\x01\0\0\0", "Sprites,ff9").expect("named");
    assert_eq!(sprite.named(), Some(ViewFormat::Sprite));
    assert!(DataDisplay::of(b"plain text", "notes.txt").is_none());
    assert!(
        DataDisplay::of(b"", "photo.jpg").is_none(),
        "a name alone names no JPEG"
    );
}

#[test]
fn the_rows_are_the_report_a_heading_then_a_row_a_sixteen_of_bytes() {
    let mut display = DataDisplay::of(PNG_HEAD, "a.png").expect("a picture");
    let len = 40;
    assert_eq!(display.row(0, len), Some(DataRow::Note(READING)));
    assert_eq!(display.row(1, len), Some(DataRow::Bytes));
    assert_eq!(display.row_count(len), 1 + 1 + 3);
    let id = display.want(1).expect("asked");
    assert!(display.reported(id, Ok(report(4)), len));
    assert_eq!(display.row_count(len), 4 + 1 + 3);
    assert!(matches!(display.row(0, len), Some(DataRow::Report(line)) if line.heading));
    assert_eq!(display.row(4, len), Some(DataRow::Bytes));
    assert_eq!(display.row(5, len), Some(DataRow::Hex(0)));
    assert_eq!(display.row(7, len), Some(DataRow::Hex(32)));
    assert_eq!(display.row(8, len), None, "no row past the last byte");
    assert_eq!(display.widest(10), 10);
    assert_eq!(display.widest(2), "line 0".len());
}

#[test]
fn a_report_is_asked_once_a_generation_and_only_its_own_answer_is_taken() {
    let mut display = DataDisplay::of(PNG_HEAD, "a.png").expect("a picture");
    let first = display.want(1).expect("asked");
    assert_eq!(display.want(1), None, "asked once");
    let second = display.want(2).expect("the document changed");
    assert!(
        !display.reported(first, Ok(report(2)), 0),
        "a stale answer is let go"
    );
    assert!(display.reported(second, Ok(report(2)), 0));
    assert_eq!(display.want(2), None);
}

#[test]
fn a_passing_refusal_is_asked_again_when_shown_afresh_and_a_lasting_one_stands() {
    let mut display = DataDisplay::of(PNG_HEAD, "a.png").expect("a picture");
    let id = display.want(1).expect("asked");
    assert!(display.reported(id, Err(no_memory()), 0));
    assert_eq!(
        display.row(0, 0),
        Some(DataRow::Note(no_memory().reason.as_str()))
    );
    assert!(display.show(false));
    assert_eq!(display.want(1), None, "not shown, not asked");
    assert!(display.show(true));
    let id = display.want(1).expect("asked again");
    assert!(display.reported(id, Err(too_large()), 0));
    assert!(display.show(false) && display.show(true));
    assert_eq!(display.want(1), None, "a lasting refusal stands");
}

#[test]
fn scrolling_stays_within_the_rows_there_are() {
    let mut display = DataDisplay::of(PNG_HEAD, "a.png").expect("a picture");
    let len = 16 * 10;
    assert!(display.set_top(usize::MAX, len));
    assert_eq!(display.top(), display.row_count(len) - 1);
    assert!(display.scroll(-3, len));
    assert!(!display.scroll(-1000, len) || display.top() == 0);
    assert_eq!(display.top(), 0);
}

#[test]
fn a_failure_to_read_says_why_and_whether_asking_again_could_help() {
    let refused = unreported(InspectFailure::Refused(InspectRefusal::NotAPicture));
    assert!(refused.lasting);
    assert!(!refused.reason.is_empty());
    let short = unreported(InspectFailure::NoMemory);
    assert_eq!(
        short,
        Unlisted {
            reason: String::from("there is not enough memory to hold the report"),
            lasting: false,
        }
    );
}
