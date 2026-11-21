//! The Supervisor's tests: a command line run as the application.

use crate::cli::tests::session;
use crate::input::InputEvent;
use crate::os::Outcome;
use crate::swi::X;
use crate::testkit::{Session, DATA};

/// `*GOS` entered with `typed` arriving at it, run until nothing more comes:
/// what it wrote.
fn supervised(s: &mut Session, typed: &[u8]) -> alloc::string::String {
    for (n, &key) in typed.iter().enumerate() {
        s.arrive(10 * (n as u64 + 1), InputEvent::Key(key));
    }
    s.stop_without_input.set(true);
    s.write(DATA, b"GOS\r");
    let outcome = s.start_swi(X | 0x05, [DATA, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(
        outcome,
        Outcome::Stopped,
        "it waits for more until the host stops it"
    );
    alloc::string::String::from_utf8_lossy(&s.output()).into_owned()
}

#[test]
fn the_supervisor_runs_each_line_and_prints_each_error() {
    let mut s = session();
    let text = supervised(&mut s, b"Echo hi\rError 5 Bad\r");
    assert!(text.starts_with("Supervisor\n\r\n\r*"), "{text}");
    assert!(
        text.contains("hi\n\r*"),
        "the command ran and the prompt came again: {text}"
    );
    assert!(text.contains("\n\rBad (number &5)\n\r*"), "{text}");
}

#[test]
fn the_supervisor_s_prompt_is_cli_prompt_and_escape_is_acknowledged() {
    let mut s = session();
    let text = supervised(&mut s, b"Set CLI$Prompt >\r\x1B");
    assert!(text.contains("\n\r>"), "the prompt it was given: {text}");
    assert!(text.contains("\n\rEscape\n\r>"), "{text}");
}
