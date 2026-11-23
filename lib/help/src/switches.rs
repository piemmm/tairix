//! The command line of a command that takes the help switches and nothing
//! else, read with the shared `getopt` and refused in its words.

use alloc::string::String;
use core::fmt;

use tairix_util::getopt::{Arg, Opt, Order, Refusal, Scan, Takes};
use tairix_util::quote::quote;

/// What such a command line asks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HelpOnly {
    /// Do what the command does.
    Run,
    /// Show the command's own short help.
    Help,
}

/// Why such a command line was refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HelpOnlyError {
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
    /// An operand; the command takes none.
    ExtraOperand(String),
}

impl fmt::Display for HelpOnlyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::ExtraOperand(operand) => {
                write!(f, "extra operand {operand}", operand = quote(operand))
            }
        }
    }
}

/// The reserved help switches.
const OPTIONS: &[Opt<()>] = &[
    Opt::flag('h', &["help"], ()),
    Opt::letter('?', Takes::Nothing, ()),
];

/// Read `args`, the command line after the program's name, for a command
/// taking the help switches alone: a help switch anywhere asks for help.
///
/// # Errors
///
/// * [`HelpOnlyError::Option`] — an option `getopt` refuses.
/// * [`HelpOnlyError::ExtraOperand`] — an operand.
pub fn parse_help_only(args: &[&str]) -> Result<HelpOnly, HelpOnlyError> {
    let mut operand = None;
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(HelpOnlyError::Option)? {
            Arg::Opt((), _) => return Ok(HelpOnly::Help),
            Arg::Operand(found) => {
                operand.get_or_insert(found);
            }
        }
    }
    match operand {
        Some(operand) => Err(HelpOnlyError::ExtraOperand(String::from(operand))),
        None => Ok(HelpOnly::Run),
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::{String, ToString};

    use super::{parse_help_only, HelpOnly, HelpOnlyError};

    #[test]
    fn the_command_line_takes_the_help_switches_and_nothing_else() {
        assert_eq!(parse_help_only(&[]), Ok(HelpOnly::Run));
        for help in ["-h", "-?", "--help"] {
            assert_eq!(parse_help_only(&[help]), Ok(HelpOnly::Help), "{help}");
            assert_eq!(
                parse_help_only(&["now", help]),
                Ok(HelpOnly::Help),
                "{help} after an operand"
            );
        }
        let extra = parse_help_only(&["now"]);
        assert_eq!(extra, Err(HelpOnlyError::ExtraOperand(String::from("now"))));
        assert_eq!(extra.unwrap_err().to_string(), "extra operand 'now'");
        assert!(matches!(
            parse_help_only(&["-q"]),
            Err(HelpOnlyError::Option(_))
        ));
    }
}
