//! `*Help`'s tests: how a keyword names an entry or a module, and what it
//! writes for each.

use alloc::vec::Vec;

use super::{names, next_keyword};
use crate::cli::tests::{session, written};
use crate::module::{utility::UTILITY, Help};

/// `text` as `*Help` lays out a string of no more than a line's width:
/// each CR a newline.
fn laid_out(text: &str) -> Vec<u8> {
    text.replace('\r', "\n\r").into_bytes()
}

/// The `UtilityModule` command `name`'s help string.
fn help_of(name: &str) -> &'static str {
    match UTILITY
        .commands
        .iter()
        .find(|c| c.name == name)
        .map(|c| &c.help)
    {
        Some(Help::Text(text)) => text,
        _ => panic!("{name} has help text"),
    }
}

/// `found`'s heading and `body`, paged.
fn paged(found: &str, body: &[u8]) -> Vec<u8> {
    let mut out = alloc::format!("\x0E\rHelp on {found}:\n\r").into_bytes();
    out.extend_from_slice(body);
    out.push(0x0F);
    out
}

#[test]
fn a_keyword_names_an_entry_whole_or_by_a_dot_where_the_entry_goes_on() {
    assert!(names(b"Echo", b"Echo"));
    assert!(names(b"echo more", b"Echo"));
    assert!(names(b"Ec.", b"Echo"));
    assert!(names(b".", b"Echo"), "a dot alone names everything");
    assert!(!names(b"Echo.", b"Echo"), "no dot after the whole name");
    assert!(!names(b"Ech", b"Echo"));
    assert!(!names(b"Echoes", b"Echo"));
}

#[test]
fn keywords_follow_one_another_after_a_space_or_a_dot() {
    assert_eq!(next_keyword(b"Echo Set", 0), Some(5));
    assert_eq!(next_keyword(b"Echo   Set", 0), Some(7));
    assert_eq!(next_keyword(b"Ec.Set", 0), Some(3));
    assert_eq!(next_keyword(b"Echo", 0), None);
    assert_eq!(next_keyword(b"Echo  ", 0), None);
}

#[test]
fn help_on_a_command_is_its_help_string_laid_out() {
    let mut s = session();
    let mut body = laid_out(help_of("Echo"));
    body.extend_from_slice(b"\n\r");
    assert_eq!(written(&mut s, "Help Echo"), paged("Echo", &body));
    assert_eq!(
        written(&mut s, "Help ec."),
        paged("Echo", &body),
        "by its abbreviation"
    );
    // *Help's own help is longer than a line, so it is wrapped.
    let alone = written(&mut s, "Help");
    assert!(
        alone.starts_with(b"\x0E\rHelp on Help:\n\r*Help gives"),
        "*Help alone is help on itself"
    );
    assert!(alone.ends_with(b"Syntax: *Help [<keywords>]\n\r\x0F"));
}

#[test]
fn help_runs_on_from_one_keyword_to_the_next() {
    let mut s = session();
    let text = written(&mut s, "Help Echo Time");
    let text = alloc::string::String::from_utf8_lossy(&text);
    let echo = text.find("Help on Echo:").expect("Echo's help");
    let time = text.find("Help on Time:").expect("Time's help");
    assert!(echo < time, "in the order given");
}

#[test]
fn help_on_nothing_it_knows_says_so() {
    let mut s = session();
    assert_eq!(
        written(&mut s, "Help Nonesuch"),
        b"\x0ENo help is available on that.\n\r\x0F"
    );
}

#[test]
fn help_on_a_module_s_title_lists_what_its_table_holds() {
    let mut s = session();
    let text = written(&mut s, "Help UtilityModule");
    let text = alloc::string::String::from_utf8_lossy(&text);
    assert!(
        text.starts_with("\x0E\rHelp on UtilityModule:\n\rHelp string: UtilityModule"),
        "{text}"
    );
    let commands = text.find("Commands provided:").expect("its commands");
    let topics = text
        .find("Other help topics:")
        .expect("its help-only entries");
    assert!(commands < topics);
    assert!(text[commands..topics].contains("Append"));
    assert!(
        !text[commands..topics].contains("Break"),
        "help alone is not a command"
    );
    assert!(text[topics..].contains("Break"));
    assert!(
        !text.contains("Filing system commands provided:"),
        "a list it has nothing in is left out"
    );
    assert!(written(&mut s, "Help Util.").starts_with(b"\x0E\rHelp on UtilityModule:"));
}

#[test]
fn help_on_a_dot_gives_every_piece_of_help_there_is() {
    let mut s = session();
    let text = written(&mut s, "Help .");
    let text = alloc::string::String::from_utf8_lossy(&text);
    for command in UTILITY.commands {
        assert!(
            text.contains(&alloc::format!("Help on {}:\n\r", command.name)),
            "{}",
            command.name
        );
    }
    assert!(
        text.contains("Help on UtilityModule:\n\r"),
        "and the module itself"
    );
}

#[test]
fn the_kernel_s_topics_list_the_commands_and_the_modules() {
    let mut s = session();
    let text = written(&mut s, "Help Commands");
    let text = alloc::string::String::from_utf8_lossy(&text);
    assert!(text.contains("Commands, listed under"));
    assert!(
        text.contains("\n\r\n\rUtilityModule"),
        "each module's list under its help string"
    );
    assert!(text.contains("Append") && !text.contains("PowerOn"));
    let text = written(&mut s, "Help Modules");
    let text = alloc::string::String::from_utf8_lossy(&text);
    assert!(
        text.contains("UtilityModule\t") || text.contains("UtilityModule "),
        "{text}"
    );
    let text = written(&mut s, "Help FileCommands");
    let text = alloc::string::String::from_utf8_lossy(&text);
    assert!(text.contains("Filing system commands, listed under"));
    assert!(
        !text.contains("Append"),
        "the kernel has no filing system commands"
    );
}
