//! The parsed shape of a `man` command line.

use alloc::string::String;

use tairix_util::getopt::{Arg, Opt, Order, Scan, Takes};

use crate::error::ManError;

/// One thing the `man` tool can do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Render `man`'s own short help (`-h`/`-?`): the `NAME`, `SYNOPSIS`, and
    /// compact `OPTIONS` of `man`'s own Help document, through the same
    /// engine as any other command's short help (plans/APPS.md §4).
    ShortHelp,
    /// Render one command's Help document in full.
    Page {
        /// The command word whose owning bundle holds the document (resolved
        /// through the shared store-then-`PATH` policy, plans/APPS.md §8).
        word: String,
        /// An explicit topic within that bundle's `Help/` tree; the command
        /// word itself when absent (plans/APPS.md §7).
        topic: Option<String>,
    },
}

/// What each of `man`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Help,
}

/// `man`'s options: the reserved help switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
];

/// Parse `args` (the tool's arguments, excluding the program name) into a
/// [`Command`].
///
/// The grammar is `man [-h | -?] <command> [topic]`, read as GNU's
/// `getopt_long` reads it ([`Scan`]): options anywhere among the operands,
/// `--` ending them. The help switches (`-h`, `-?`, `--help`) render `man`'s
/// own short help at once. `<command>` is the command whose Help document to
/// render, `[topic]` a named topic within that command's bundle.
///
/// # Errors
///
/// * [`ManError::Option`] — an option `getopt` refuses, in its words.
/// * [`ManError::Usage`] — no operand (there is nothing to render), or a
///   third one.
pub fn parse(args: &[&str]) -> Result<Command, ManError> {
    let mut word: Option<&str> = None;
    let mut topic: Option<&str> = None;
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(ManError::Option)? {
            Arg::Opt(Flag::Help, _) => return Ok(Command::ShortHelp),
            Arg::Operand(operand) => {
                if word.is_none() {
                    word = Some(operand);
                } else if topic.is_none() {
                    topic = Some(operand);
                } else {
                    return Err(ManError::Usage);
                }
            }
        }
    }
    match word {
        Some(word) => Ok(Command::Page {
            word: String::from(word),
            topic: topic.map(String::from),
        }),
        None => Err(ManError::Usage),
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::{String, ToString};

    use super::{parse, Command};
    use crate::error::ManError;

    #[test]
    fn a_bare_word_names_the_command_and_its_own_document() {
        assert_eq!(
            parse(&["ps"]),
            Ok(Command::Page {
                word: "ps".into(),
                topic: None,
            })
        );
    }

    #[test]
    fn a_second_operand_names_a_topic_inside_the_bundle() {
        assert_eq!(
            parse(&["top", "keys"]),
            Ok(Command::Page {
                word: "top".into(),
                topic: Some("keys".into()),
            })
        );
    }

    #[test]
    fn the_reserved_short_help_switches_win_immediately() {
        assert_eq!(parse(&["-h"]), Ok(Command::ShortHelp));
        assert_eq!(parse(&["-?"]), Ok(Command::ShortHelp));
        assert_eq!(parse(&["--help"]), Ok(Command::ShortHelp));
        assert_eq!(parse(&["-h", "ps"]), Ok(Command::ShortHelp));
    }

    #[test]
    fn a_double_dash_ends_option_parsing() {
        // After `--` a dash-leading word is an operand, so a command whose
        // name starts with `-` is still reachable.
        assert_eq!(
            parse(&["--", "-weird"]),
            Ok(Command::Page {
                word: "-weird".into(),
                topic: None,
            })
        );
    }

    #[test]
    fn out_of_grammar_input_fails_closed_as_usage() {
        assert_eq!(parse(&[]), Err(ManError::Usage));
        assert_eq!(parse(&["ps", "topic", "extra"]), Err(ManError::Usage));
        assert_eq!(parse(&["--"]), Err(ManError::Usage));
    }

    /// GNU's words for each refusal.
    fn refused(args: &[&str]) -> String {
        parse(args).expect_err("refused").to_string()
    }

    #[test]
    fn a_refused_option_is_worded_as_getopt_words_it() {
        assert_eq!(refused(&["-x"]), "invalid option -- 'x'");
        assert_eq!(
            refused(&["--verbose", "ps"]),
            "unrecognized option '--verbose'"
        );
        assert_eq!(
            refused(&["--help=ps"]),
            "option '--help' doesn't allow an argument"
        );
    }

    #[test]
    fn a_long_option_may_be_abbreviated() {
        assert_eq!(parse(&["--he", "ps"]), Ok(Command::ShortHelp));
    }

    #[test]
    fn a_lone_dash_is_an_operand_not_an_option() {
        // `-` conventionally names standard input; `man` has no such source,
        // so it flows through as an (invalid) command word the resolver will
        // refuse — parsing does not invent a meaning for it.
        assert_eq!(
            parse(&["-"]),
            Ok(Command::Page {
                word: "-".into(),
                topic: None,
            })
        );
    }
}
