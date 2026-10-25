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

use tairix_glob::OutOfMemory;

use crate::brace;
use crate::env::{assignment_split, Environment, Expansion};
use crate::error::ParseError;
use crate::lexer::{Segment, Word};
use crate::options::{ShellOption, ShellOptions};
use crate::pathexp::{self, GlobFs, Ignore, Settings};

/// Most bytes one command line's expansions may hold together — its
/// fields, redirection targets, here-strings, here-documents and assignment
/// values: the largest argument block a program can be handed, so a short
/// line can never demand unbounded memory.
// The block bound is 16 MiB, which every target's `usize` holds.
#[allow(clippy::cast_possible_truncation)]
pub(crate) const MAX_EXPANSION_BYTES: usize = tairix_abi::PROCESS_START_MAX_TOTAL_LEN as usize;

const _: () = assert!(tairix_abi::PROCESS_START_MAX_TOTAL_LEN <= usize::MAX as u64);

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

impl From<OutOfMemory> for ExpandError {
    fn from(_: OutOfMemory) -> Self {
        ParseError::OutOfMemory.into()
    }
}

/// The bound on what one line's expansions may hold, charged before it is
/// allocated: every byte of text, and every slot a list of words grows by.
/// Memory the allocator refuses is the bound spent, never the end of the
/// session.
pub(crate) struct Budget {
    remaining: usize,
}

impl Budget {
    pub(crate) fn new(limit: usize) -> Self {
        Self { remaining: limit }
    }

    /// What is left to charge.
    pub(crate) fn remaining(&self) -> usize {
        self.remaining
    }

    /// Charge `len` bytes.
    ///
    /// # Errors
    ///
    /// [`ParseError::ExpansionTooLarge`] once the bound is spent.
    pub(crate) fn spend(&mut self, len: usize) -> Result<(), ParseError> {
        self.remaining = self
            .remaining
            .checked_sub(len)
            .ok_or(ParseError::ExpansionTooLarge)?;
        Ok(())
    }

    /// An empty string with room for exactly `len` bytes, charged.
    ///
    /// # Errors
    ///
    /// [`ParseError::ExpansionTooLarge`] once the bound is spent, or
    /// [`ParseError::OutOfMemory`] when memory refuses the room.
    pub(crate) fn string(&mut self, len: usize) -> Result<String, ParseError> {
        let mut text = String::new();
        self.room(&mut text, len)?;
        Ok(text)
    }

    /// Room in `text` for exactly `more` bytes beyond its own, charged.
    ///
    /// # Errors
    ///
    /// As [`Budget::string`].
    pub(crate) fn room(&mut self, text: &mut String, more: usize) -> Result<(), ParseError> {
        self.spend(more)?;
        text.try_reserve_exact(more)
            .map_err(|_| ParseError::OutOfMemory)
    }

    /// An empty list with room for exactly `count` elements, charged at
    /// their size.
    ///
    /// # Errors
    ///
    /// As [`Budget::string`].
    pub(crate) fn list<T>(&mut self, count: usize) -> Result<Vec<T>, ParseError> {
        let mut list = Vec::new();
        self.reserve(&mut list, count)?;
        Ok(list)
    }

    /// Push `item` onto `list`, which, when full, grows to twice its room,
    /// the slots it gains charged at their size.
    ///
    /// # Errors
    ///
    /// As [`Budget::string`].
    pub(crate) fn push<T>(&mut self, list: &mut Vec<T>, item: T) -> Result<(), ParseError> {
        if list.len() == list.capacity() {
            self.reserve(list, list.capacity().max(1))?;
        }
        list.push(item);
        Ok(())
    }

    fn reserve<T>(&mut self, list: &mut Vec<T>, more: usize) -> Result<(), ParseError> {
        let bytes = more
            .checked_mul(core::mem::size_of::<T>())
            .ok_or(ParseError::ExpansionTooLarge)?;
        self.spend(bytes)?;
        list.try_reserve_exact(more)
            .map_err(|_| ParseError::OutOfMemory)
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
    /// The session's compiled `GLOBIGNORE`, compiled only when a pattern's
    /// matches need filtering, so an entry that is no pattern fails only the
    /// lines that generate names.
    ignore: &'a Ignore,
}

impl<'a> Expander<'a> {
    /// An expander for the session as it stands, filtering through its
    /// compiled `GLOBIGNORE`.
    pub(crate) fn new(
        env: &'a Environment,
        options: &'a ShellOptions,
        fs: &'a dyn GlobFs,
        ignore: &'a Ignore,
    ) -> Self {
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
            ignore,
        }
    }

    /// Remove what `GLOBIGNORE` names from a pattern's `paths`.
    ///
    /// # Errors
    ///
    /// A `GLOBIGNORE` entry that is not a valid pattern or whose group holds
    /// a `/`, the bound spent compiling one, or a match the heap could not
    /// hold.
    fn filter(&self, paths: &mut Vec<String>, budget: &mut Budget) -> Result<(), ExpandError> {
        match self.globignore {
            Some(text) if !paths.is_empty() => {
                self.ignore.retain(text, self.settings.word, paths, budget)
            }
            _ => Ok(()),
        }
    }

    /// The fields `words` expand to, in order, charged to `budget`. Under
    /// `export`, an operand spelled `NAME=value` skips filename generation,
    /// as bash's does.
    ///
    /// # Errors
    ///
    /// Why a word could not be expanded.
    pub(crate) fn fields(
        &self,
        words: &[Word],
        budget: &mut Budget,
    ) -> Result<Vec<String>, ExpandError> {
        let declares = words
            .first()
            .is_some_and(|word| plain_text(word) == Some("export"));
        let mut fields = Vec::new();
        for (index, word) in words.iter().enumerate() {
            let assignment = declares && index > 0 && assignment_split(word).is_some();
            for braced in brace::expand(word, budget)? {
                let expansion = self.env.expand_marked(&braced, budget)?;
                if assignment {
                    budget.push(&mut fields, expansion.text)?;
                } else {
                    self.generate(expansion, budget, &mut fields)?;
                }
            }
        }
        Ok(fields)
    }

    /// The one word a redirection target expands to, charged to `budget`.
    ///
    /// # Errors
    ///
    /// [`ParseError::AmbiguousRedirectTarget`] when it expands to no word or
    /// to several; otherwise why it could not be expanded.
    pub(crate) fn target(&self, word: &Word, budget: &mut Budget) -> Result<String, ExpandError> {
        let mut fields = self.fields(core::slice::from_ref(word), budget)?;
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
        expansion: Expansion,
        budget: &mut Budget,
        fields: &mut Vec<String>,
    ) -> Result<(), ExpandError> {
        if !self.options.is_on(ShellOption::Noglob) {
            if let Some(paths) = pathexp::expand(
                &expansion,
                &self.settings,
                self.fs,
                budget,
                &|paths, budget| self.filter(paths, budget),
            )? {
                if !paths.is_empty() {
                    for path in paths {
                        budget.push(fields, path)?;
                    }
                    return Ok(());
                }
                if self.options.is_on(ShellOption::Failglob) {
                    return Err(ExpandError {
                        error: ParseError::NoMatch,
                        subject: Some(expansion.text),
                    });
                }
                if self.options.is_on(ShellOption::Nullglob) {
                    return Ok(());
                }
            }
        }
        // An unquoted word that expanded to nothing is no argument at all.
        if expansion.text.is_empty() && !expansion.quoted {
            return Ok(());
        }
        budget.push(fields, expansion.text)?;
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
