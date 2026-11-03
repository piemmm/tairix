//! Unit tests for the save picker's words: every name it quotes is shown as
//! its listing row shows it, so what hides in a name cannot make one file's
//! message read as another's.

use alloc::string::String;
use alloc::vec::Vec;

use super::{already_exists, replace_question, saving_into, taken_by, SAVING};

#[test]
fn the_replace_question_names_the_file_as_shown() {
    let spec = replace_question("a\u{202e}b.txt", "/Docs/a\u{202e}b.txt").expect("a question");
    assert_eq!(
        spec.headline(),
        "\u{201c}a\u{fffd}b.txt\u{201d} already exists. Replace it?"
    );
}

/// The question names the folder the file is in, so a save dropped on a
/// folder other than the one shown says which it would replace a file in.
#[test]
fn the_replace_question_names_the_folder() {
    let spec = replace_question("Report.txt", "/Users/ann/Desk\u{202e}top/Report.txt")
        .expect("a question");
    assert!(
        spec.detail().contains("\u{201c}Desk\u{fffd}top\u{201d}"),
        "{}",
        spec.detail()
    );
    let spec = replace_question("Report.txt", "/Report.txt").expect("a question");
    assert!(spec.detail().contains("already there"), "{}", spec.detail());
}

/// While a dropped save is written the status line says where it goes; one
/// saved into the folder shown says only that it is saving.
#[test]
fn a_dropped_save_says_where_it_goes() {
    let into: Vec<String> = ["Users", "ann", "Desktop"].map(String::from).to_vec();
    assert_eq!(
        saving_into(Some(&into)),
        "Saving into \u{201c}Desktop\u{201d}\u{2026}"
    );
    assert_eq!(saving_into(None), SAVING);
}

#[test]
fn a_refused_save_names_the_file_as_shown() {
    assert_eq!(
        already_exists("a\u{202e}b.txt"),
        "Not saved: \u{201c}a\u{fffd}b.txt\u{201d} already exists"
    );
    assert_eq!(
        taken_by("notes\u{202e}.txt", "a shortcut"),
        "\u{201c}notes\u{fffd}.txt\u{201d} is a shortcut; choose another name"
    );
}
