//! Unit tests for the save picker's words: every name it quotes is shown as
//! its listing row shows it, so what hides in a name cannot make one file's
//! message read as another's.

use super::{already_exists, replace_question, taken_by};

#[test]
fn the_replace_question_names_the_file_as_shown() {
    let spec = replace_question("a\u{202e}b.txt").expect("a question");
    assert_eq!(
        spec.headline(),
        "\u{201c}a\u{fffd}b.txt\u{201d} already exists. Replace it?"
    );
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
