//! The expansions a command's words go through, in bash's order: brace
//! expansion, parameter expansion, then filename generation, after which an
//! unquoted word left empty is dropped.
//!
//! What a pattern that matches nothing becomes is bash's choice of three:
//! itself (the default), nothing (`nullglob`), or an error (`failglob`).
//! Assignment values, here-strings and here-documents take parameter
//! expansion alone, as do `export`'s `NAME=value` operands.

use alloc::string::String;
use alloc::vec::Vec;
use core::cell::OnceCell;

use crate::brace;
use crate::env::{assignment_split, Environment, Expansion};
use crate::error::ParseError;
use crate::lexer::{Segment, Word};
use crate::options::{ShellOption, ShellOptions};
use crate::pathexp::{self, GlobFs, Ignore, Settings};

/// Most bytes one command's expansion may hold: the largest argument block a
/// program can be handed, so a short line can never demand unbounded memory.
// The block bound is 16 MiB, which every target's `usize` holds.
#[allow(clippy::cast_possible_truncation)]
pub(crate) const MAX_EXPANSION_BYTES: usize = tairix_abi::PROCESS_START_MAX_TOTAL_LEN as usize;

const _: () = assert!(tairix_abi::PROCESS_START_MAX_TOTAL_LEN <= usize::MAX as u64);

/// What each word held costs beyond its text: its own bookkeeping, so a
/// flood of tiny words is bounded as surely as one long one.
const WORD_OVERHEAD: usize = core::mem::size_of::<String>();

/// A word that could not be expanded: the error, and the word it concerns
/// when there is one to name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExpandError {
    pub(crate) error: ParseError,
    pub(crate) subject: Option<String>,
}

impl From<ParseError> for ExpandError {
    fn from(error: ParseError) -> Self {
        Self {
            error,
            subject: None,
        }
    }
}

/// The bound on what one expansion may hold, charged as words are made.
pub(crate) struct Budget {
    remaining: usize,
}

impl Budget {
    pub(crate) fn new(limit: usize) -> Self {
        Self { remaining: limit }
    }

    /// Charge one word of `len` bytes.
    ///
    /// # Errors
    ///
    /// [`ParseError::ExpansionTooLarge`] once the bound is spent.
    pub(crate) fn charge(&mut self, len: usize) -> Result<(), ParseError> {
        self.remaining = self
            .remaining
            .checked_sub(len.saturating_add(WORD_OVERHEAD))
            .ok_or(ParseError::ExpansionTooLarge)?;
        Ok(())
    }
}

/// Expands words against the session's variables, options and filesystem.
pub(crate) struct Expander<'a> {
    env: &'a Environment,
    options: &'a ShellOptions,
    fs: &'a dyn GlobFs,
    settings: Settings,
    /// `GLOBIGNORE`'s text, when set and not empty.
    globignore: Option<&'a str>,
    /// Compiled the first time a pattern's matches need filtering, so an
    /// entry that is no pattern fails only the lines that generate names.
    ignore: OnceCell<Result<Ignore, ExpandError>>,
}

impl<'a> Expander<'a> {
    /// An expander for the session as it stands.
    pub(crate) fn new(env: &'a Environment, options: &'a ShellOptions, fs: &'a dyn GlobFs) -> Self {
        let on = |option| options.is_on(option);
        let globignore = env.get("GLOBIGNORE").filter(|text| !text.is_empty());
        let settings = Settings {
            word: Settings::word_options(
                on(ShellOption::Extglob),
                on(ShellOption::Nocaseglob),
                on(ShellOption::Dotglob) || globignore.is_some(),
            ),
            globstar: on(ShellOption::Globstar),
        };
        Self {
            env,
            options,
            fs,
            settings,
            globignore,
            ignore: OnceCell::new(),
        }
    }

    /// Remove what `GLOBIGNORE` names from a pattern's `paths`.
    ///
    /// # Errors
    ///
    /// A `GLOBIGNORE` entry that is not a valid pattern.
    fn filter(&self, paths: &mut Vec<String>) -> Result<(), ExpandError> {
        let Some(text) = self.globignore.filter(|_| !paths.is_empty()) else {
            return Ok(());
        };
        let ignore = self.ignore.get_or_init(|| {
            Ignore::new(
                text,
                self.options.is_on(ShellOption::Extglob),
                self.options.is_on(ShellOption::Nocaseglob),
            )
        });
        match ignore {
            Ok(ignore) => {
                ignore.retain(paths);
                Ok(())
            }
            Err(err) => Err(err.clone()),
        }
    }

    /// The fields `words` expand to, in order. Under `export`, an operand
    /// spelled `NAME=value` skips filename generation, as bash's does.
    ///
    /// # Errors
    ///
    /// Why a word could not be expanded.
    pub(crate) fn fields(&self, words: &[Word]) -> Result<Vec<String>, ExpandError> {
        let mut budget = Budget::new(MAX_EXPANSION_BYTES);
        let declares = words
            .first()
            .is_some_and(|word| plain_text(word) == Some("export"));
        let mut fields = Vec::new();
        for (index, word) in words.iter().enumerate() {
            let assignment = declares && index > 0 && assignment_split(word).is_some();
            for braced in brace::expand(word, &mut budget)? {
                let expansion = self.env.expand_marked(&braced)?;
                if assignment {
                    budget.charge(expansion.marks.len())?;
                    fields.push(expansion.text());
                } else {
                    self.generate(&expansion, &mut budget, &mut fields)?;
                }
            }
        }
        Ok(fields)
    }

    /// The one word a redirection target expands to.
    ///
    /// # Errors
    ///
    /// [`ParseError::AmbiguousRedirectTarget`] when it expands to no word or
    /// to several; otherwise why it could not be expanded.
    pub(crate) fn target(&self, word: &Word) -> Result<String, ExpandError> {
        let mut fields = self.fields(core::slice::from_ref(word))?;
        match fields.pop() {
            Some(only) if fields.is_empty() => Ok(only),
            _ => Err(ExpandError {
                error: ParseError::AmbiguousRedirectTarget,
                subject: Some(source_text(word)),
            }),
        }
    }

    /// Filename generation for one expanded word, then the empty-word rule.
    fn generate(
        &self,
        expansion: &Expansion,
        budget: &mut Budget,
        fields: &mut Vec<String>,
    ) -> Result<(), ExpandError> {
        if !self.options.is_on(ShellOption::Noglob) {
            if let Some(mut paths) =
                pathexp::expand(&expansion.marks, &self.settings, self.fs, budget)?
            {
                self.filter(&mut paths)?;
                if !paths.is_empty() {
                    fields.extend(paths);
                    return Ok(());
                }
                if self.options.is_on(ShellOption::Failglob) {
                    return Err(ExpandError {
                        error: ParseError::NoMatch,
                        subject: Some(expansion.text()),
                    });
                }
                if self.options.is_on(ShellOption::Nullglob) {
                    return Ok(());
                }
            }
        }
        // An unquoted word that expanded to nothing is no argument at all.
        if expansion.marks.is_empty() && !expansion.quoted {
            return Ok(());
        }
        budget.charge(expansion.marks.len())?;
        fields.push(expansion.text());
        Ok(())
    }
}

/// The text of a word written with no quoting and no expansion.
fn plain_text(word: &Word) -> Option<&str> {
    match word.as_slice() {
        [Segment::Expandable(text)] if !text.contains('$') => Some(text),
        _ => None,
    }
}

/// A word's text as written, quotes left out, for naming it in a message.
fn source_text(word: &Word) -> String {
    word.iter()
        .map(|segment| match segment {
            Segment::Literal(s) | Segment::Expandable(s) | Segment::QuotedExpandable(s) => {
                s.as_str()
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "expand_tests.rs"]
mod tests;
