//! GNU `getopt_long`'s reading of a command line, once for every command app.
//!
//! A tool lists its options in a table ([`Opt`]) and [`Scan`] walks its
//! arguments as GNU does: short options clustered (`-sf`), a value attached
//! (`-tdir`, `--target-directory=dir`) or following (`-t dir`), a long option
//! by any unambiguous prefix (`--verb`), `--` ending the options, and operands
//! among them (`ln a -s b`) unless the tool's [`Order`] says the first operand
//! ends them. An argument it cannot read is refused in GNU's words
//! ([`Refusal`]).

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::argv::{resolve, WordMiss};
use crate::quote::quote;

/// What an option takes after its name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Takes {
    /// Nothing: a flag.
    Nothing,
    /// A value, attached (`-tdir`, `--target-directory=dir`) or the next
    /// argument.
    Value,
    /// A value only when attached (`--color=always`), GNU's optional argument.
    Attached,
}

/// One option a command accepts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Opt<T: 'static> {
    /// Its letter, if it has one.
    pub short: Option<char>,
    /// Its long names, without `--`; synonyms share one entry.
    pub long: &'static [&'static str],
    /// What it takes after its name.
    pub takes: Takes,
    /// What it means to the tool.
    pub means: T,
}

impl<T: 'static> Opt<T> {
    /// A flag with a letter and long names.
    #[must_use]
    pub const fn flag(short: char, long: &'static [&'static str], means: T) -> Self {
        Self {
            short: Some(short),
            long,
            takes: Takes::Nothing,
            means,
        }
    }

    /// An option with a letter and long names that takes a value.
    #[must_use]
    pub const fn valued(short: char, long: &'static [&'static str], means: T) -> Self {
        Self {
            short: Some(short),
            long,
            takes: Takes::Value,
            means,
        }
    }

    /// An option with a letter alone.
    #[must_use]
    pub const fn letter(short: char, takes: Takes, means: T) -> Self {
        Self {
            short: Some(short),
            long: &[],
            takes,
            means,
        }
    }

    /// An option with long names alone.
    #[must_use]
    pub const fn long(long: &'static [&'static str], takes: Takes, means: T) -> Self {
        Self {
            short: None,
            long,
            takes,
            means,
        }
    }
}

/// Where a tool's options may stand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Order {
    /// Anywhere among the operands, GNU's own reading.
    Permute,
    /// Before the operands: the first one ends the options, as POSIX reads a
    /// command line and as `seq` and `printf` must, whose operands may begin
    /// with a dash.
    OperandsEnd,
}

/// One argument as the scan reads it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Arg<'a, T> {
    /// An option, with its value when it took one.
    Opt(T, Option<&'a str>),
    /// An operand.
    Operand(&'a str),
}

/// An argument the scan refused, worded as GNU's `getopt_long` words it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// `invalid option -- 'x'`.
    UnknownShort(char),
    /// `unrecognized option '--frob'`, naming the argument as given.
    UnknownLong(String),
    /// `option '--n' is ambiguous; possibilities: '--no-dereference'
    /// '--no-target-directory'`, naming the argument as given.
    Ambiguous {
        /// The argument as given.
        given: String,
        /// The long names it abbreviates, in the tool's order.
        possibilities: Vec<&'static str>,
    },
    /// `option requires an argument -- 't'`.
    MissingShortValue(char),
    /// `option '--target-directory' requires an argument`.
    MissingLongValue(&'static str),
    /// `option '--force' doesn't allow an argument`.
    UnwantedValue(&'static str),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let letter = |letter: &char| {
            let mut utf8 = [0; 4];
            let spelled: &str = letter.encode_utf8(&mut utf8);
            alloc::format!("{}", quote(spelled))
        };
        match self {
            Self::UnknownShort(c) => write!(f, "invalid option -- {}", letter(c)),
            Self::UnknownLong(given) => write!(f, "unrecognized option {}", quote(given)),
            Self::Ambiguous {
                given,
                possibilities,
            } => {
                write!(f, "option {} is ambiguous; possibilities:", quote(given))?;
                for name in possibilities {
                    write!(f, " '--{name}'")?;
                }
                Ok(())
            }
            Self::MissingShortValue(c) => {
                write!(f, "option requires an argument -- {}", letter(c))
            }
            Self::MissingLongValue(name) => write!(f, "option '--{name}' requires an argument"),
            Self::UnwantedValue(name) => write!(f, "option '--{name}' doesn't allow an argument"),
        }
    }
}

/// A command line read as GNU's `getopt_long` reads it: an iterator over its
/// options and operands, in order.
#[derive(Clone, Debug)]
pub struct Scan<'a, T: 'static> {
    options: &'static [Opt<T>],
    args: &'a [&'a str],
    next: usize,
    /// The letters left of a short-option cluster being read.
    cluster: &'a str,
    order: Order,
    /// `--`, or under [`Order::OperandsEnd`] the first operand, has been read.
    ended: bool,
    /// A negative number (`-5`, `-.3`) is an operand, not options.
    numbers: bool,
}

impl<'a, T: Copy + PartialEq + 'static> Scan<'a, T> {
    /// A scan of `args` (the program's word excluded) against `options`.
    #[must_use]
    pub const fn new(options: &'static [Opt<T>], args: &'a [&'a str], order: Order) -> Self {
        Self {
            options,
            args,
            next: 0,
            cluster: "",
            order,
            ended: false,
            numbers: false,
        }
    }

    /// The same scan, reading an argument that is a dash and a digit or a dot
    /// (`-5`, `-.3`) as an operand, as `seq` reads a negative number.
    #[must_use]
    pub const fn numbers_are_operands(self) -> Self {
        Self {
            numbers: true,
            ..self
        }
    }

    fn take(&mut self) -> Option<&'a str> {
        let arg = self.args.get(self.next).copied()?;
        self.next += 1;
        Some(arg)
    }

    fn short(&mut self) -> Result<Arg<'a, T>, Refusal> {
        let mut letters = self.cluster.chars();
        let Some(letter) = letters.next() else {
            return Err(Refusal::UnknownShort('-'));
        };
        self.cluster = letters.as_str();
        let Some(opt) = self.options.iter().find(|opt| opt.short == Some(letter)) else {
            return Err(Refusal::UnknownShort(letter));
        };
        let attached = core::mem::take(&mut self.cluster);
        let value = match opt.takes {
            Takes::Nothing => {
                self.cluster = attached;
                None
            }
            Takes::Attached => (!attached.is_empty()).then_some(attached),
            Takes::Value if !attached.is_empty() => Some(attached),
            Takes::Value => Some(self.take().ok_or(Refusal::MissingShortValue(letter))?),
        };
        Ok(Arg::Opt(opt.means, value))
    }

    fn long(&mut self, arg: &'a str) -> Result<Arg<'a, T>, Refusal> {
        let body = &arg[2..];
        let (key, attached) = match body.split_once('=') {
            Some((key, value)) => (key, Some(value)),
            None => (body, None),
        };
        let found = self.resolve(key).map_err(|miss| match miss {
            WordMiss::Unknown => Refusal::UnknownLong(String::from(arg)),
            WordMiss::Ambiguous => Refusal::Ambiguous {
                given: String::from(arg),
                possibilities: self
                    .options
                    .iter()
                    .flat_map(|opt| opt.long.iter().copied())
                    .filter(|name| name.starts_with(key))
                    .collect(),
            },
        })?;
        let (name, opt) = found;
        let value = match (opt.takes, attached) {
            (Takes::Nothing, Some(_)) => return Err(Refusal::UnwantedValue(name)),
            (Takes::Nothing, None) => None,
            (Takes::Attached, value) | (Takes::Value, value @ Some(_)) => value,
            (Takes::Value, None) => Some(self.take().ok_or(Refusal::MissingLongValue(name))?),
        };
        Ok(Arg::Opt(opt.means, value))
    }

    /// The option the long name `key` names, exactly or by its only prefix:
    /// synonyms, and names meaning one thing, are one option.
    fn resolve(&self, key: &str) -> Result<(&'static str, &'static Opt<T>), WordMiss> {
        let options = self.options;
        let mut names = options
            .iter()
            .flat_map(|opt| opt.long.iter().map(move |name| (*name, opt)));
        if let Some(exact) = names.clone().find(|(name, _)| *name == key) {
            return Ok(exact);
        }
        resolve(
            names.by_ref().filter(|(name, _)| name.starts_with(key)),
            |(_, a), (_, b)| a.means == b.means && a.takes == b.takes,
        )
    }
}

impl<'a, T: Copy + PartialEq + 'static> Iterator for Scan<'a, T> {
    type Item = Result<Arg<'a, T>, Refusal>;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.cluster.is_empty() {
            return Some(self.short());
        }
        loop {
            let arg = self.take()?;
            if self.ended {
                return Some(Ok(Arg::Operand(arg)));
            }
            if arg == "--" {
                self.ended = true;
                continue;
            }
            if arg.starts_with("--") {
                return Some(self.long(arg));
            }
            let number = self.numbers
                && arg
                    .strip_prefix('-')
                    .and_then(|rest| rest.bytes().next())
                    .is_some_and(|after| after == b'.' || after.is_ascii_digit());
            if arg.len() > 1 && arg.starts_with('-') && !number {
                self.cluster = &arg[1..];
                return Some(self.short());
            }
            self.ended = self.order == Order::OperandsEnd;
            return Some(Ok(Arg::Operand(arg)));
        }
    }
}

#[cfg(test)]
#[path = "getopt_tests.rs"]
mod tests;
