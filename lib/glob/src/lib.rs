//! TAIRiX shared filename-glob matcher (`lib/glob`).
//!
//! The one definition of shell wildcard patterns. The shell's filename
//! generation, `ls --ignore`/`--hide` and the tree file manager's filters all
//! compile their patterns here rather than each embedding a matcher.
//!
//! # Pattern syntax
//!
//! A pattern matches a whole candidate: it is anchored at both ends.
//!
//! - `*` matches any run of characters, `?` exactly one.
//! - `[...]` matches one character: members, code-point ranges `a-z`, a
//!   leading `!` or `^` to negate, the POSIX classes `[:alpha:]`, `[:digit:]`
//!   and the rest (plus bash's `[:word:]` and `[:ascii:]`), equivalence
//!   classes `[=c=]`, and collating symbols `[.c.]` or `[.space.]`. A `]`
//!   first in the expression, or a `-` first or last, is a member. A `[` that
//!   opens no complete expression matches itself, as POSIX specifies.
//! - `\` makes the next character literal, inside an expression too.
//! - With [`Options::EXTENDED`], bash's `extglob` operators over
//!   `|`-separated alternatives: `?(…)` zero or one, `*(…)` zero or more,
//!   `+(…)` one or more, `@(…)` exactly one, `!(…)` anything but one.
//!
//! [`Options`] also selects case-insensitive matching, the leading-period rule
//! and pathname mode (glibc's `FNM_CASEFOLD`, `FNM_PERIOD` and `FNM_PATHNAME`).
//! [`Pattern::from_marked`] compiles text whose quoted characters the caller
//! already knows are literal, which is how the shell hands a word over
//! without re-escaping it.
//!
//! # Bounds
//!
//! A pattern is untrusted input, so its size is bounded ([`MAX_PATTERN_LEN`],
//! [`MAX_TOKENS`], [`MAX_CLASS_ITEMS`], [`MAX_GROUPS`], [`MAX_GROUP_DEPTH`])
//! and a malformed one is a typed [`GlobError`]. Compiling one costs time
//! linear in its length, its bracket expressions sharing one table of where
//! each can close. Matching never fails and
//! never backtracks: it tracks the set of positions each step can reach. A
//! pattern without groups matches in time linear in its tokens times the
//! candidate's length. A group's matches are memoised per start position,
//! which bounds an extended pattern by the cube of the candidate's length;
//! that is why one matches candidates of at most [`MAX_EXTENDED_LEN`]
//! characters, the longest name a filesystem stores.
//!
//! # Example
//!
//! ```
//! use tairix_glob::{Options, Pattern};
//!
//! let pat = Pattern::new("img_[[:digit:]][0-9].???").expect("valid pattern");
//! assert!(pat.matches("img_04.png"));
//! assert!(!pat.matches("img_4.png"));
//!
//! let extended = Options::EXTENDED.union(Options::PERIOD);
//! let pat = Pattern::with_options("!(*.o)", extended).expect("valid pattern");
//! assert!(pat.matches("main.rs"));
//! assert!(!pat.matches("main.o"));
//! assert!(!pat.matches(".profile"));
//! ```

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

mod bracket;
mod compile;
mod engine;
#[cfg(test)]
mod tests;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use compile::{fold, Node};

/// Largest pattern accepted, in bytes of UTF-8.
pub const MAX_PATTERN_LEN: usize = 4096;

/// Most nodes a pattern may compile to: each literal, `?`, bracket
/// expression, run of `*`, and group counts one.
pub const MAX_TOKENS: usize = 1024;

/// Most members (characters, ranges, classes) one bracket expression holds.
pub const MAX_CLASS_ITEMS: usize = 256;

/// Most extended groups one component of a pattern holds.
pub const MAX_GROUPS: usize = 16;

/// Deepest nesting of extended groups.
pub const MAX_GROUP_DEPTH: usize = 8;

/// Longest candidate, in characters, a component holding a group matches:
/// the longest name a filesystem stores, so every directory entry is in
/// reach. A longer candidate matches no such component.
pub const MAX_EXTENDED_LEN: usize = tairix_abi::fs::FS_NAME_MAX;

/// How a pattern is read and matched: a set of flags, glibc's `FNM_*` in
/// spirit. The empty set is POSIX `fnmatch` with no flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options(u8);

impl Options {
    /// Recognise the extended operators `?(…)`, `*(…)`, `+(…)`, `@(…)` and
    /// `!(…)` (bash's `extglob`). Otherwise their characters are ordinary.
    pub const EXTENDED: Self = Self(1 << 0);
    /// Ignore case: literals and ranges compare case-folded, while classes
    /// keep testing the character as written, as bash's `nocaseglob` does.
    pub const CASE_INSENSITIVE: Self = Self(1 << 1);
    /// A `.` beginning a candidate (or, in pathname mode, any component)
    /// must be matched by a literal `.`: no `*`, `?`, bracket expression or
    /// `!(…)` matches it.
    pub const PERIOD: Self = Self(1 << 2);
    /// Split pattern and candidate at every `/` and match them component by
    /// component, so only a `/` matches a `/`.
    pub const PATHNAME: Self = Self(1 << 3);

    /// No flags.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// The flags of both.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// These flags with `flag` set when `on`, cleared otherwise.
    #[must_use]
    pub const fn with(self, flag: Self, on: bool) -> Self {
        if on {
            Self(self.0 | flag.0)
        } else {
            Self(self.0 & !flag.0)
        }
    }

    /// Whether every flag of `flag` is set.
    #[must_use]
    pub const fn contains(self, flag: Self) -> bool {
        self.0 & flag.0 == flag.0
    }
}

/// Why a pattern was rejected. A malformed or over-large pattern always fails
/// closed with one of these rather than being matched as something else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlobError {
    /// The pattern was longer than [`MAX_PATTERN_LEN`] bytes.
    PatternTooLong,
    /// The pattern compiled to more than [`MAX_TOKENS`] nodes, or a bracket
    /// expression held more than [`MAX_CLASS_ITEMS`] members.
    TooManyTokens,
    /// The pattern ended with a `\` that escaped nothing.
    DanglingEscape,
    /// A bracket range's endpoints were out of order (`[z-a]`).
    ReversedRange,
    /// A `[:name:]` named no character class.
    UnknownClass,
    /// A `[.name.]` or `[=name=]` named no single character.
    UnknownCollatingElement,
    /// An extended group was opened but never closed.
    UnterminatedGroup,
    /// A component held more than [`MAX_GROUPS`] extended groups.
    TooManyGroups,
    /// Extended groups nested deeper than [`MAX_GROUP_DEPTH`].
    TooDeep,
}

impl fmt::Display for GlobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::PatternTooLong => "glob pattern exceeds the maximum length",
            Self::TooManyTokens => "glob pattern has too many tokens",
            Self::DanglingEscape => "glob pattern ends with a dangling escape",
            Self::ReversedRange => "glob bracket range endpoints are out of order",
            Self::UnknownClass => "glob bracket expression names an unknown character class",
            Self::UnknownCollatingElement => {
                "glob bracket expression names an unknown collating element"
            }
            Self::UnterminatedGroup => "glob pattern group is not closed",
            Self::TooManyGroups => "glob pattern has too many groups",
            Self::TooDeep => "glob pattern groups are nested too deeply",
        })
    }
}

/// Whether `text` holds an unquoted character that can begin a wildcard —
/// `*`, `?`, `[`, or with `extended` a group's `(`. Every pattern with
/// wildcards holds one, so text holding none need not be compiled to learn
/// it stands for itself.
#[must_use]
pub fn may_hold_wildcards(text: &[Mark], extended: bool) -> bool {
    text.iter().any(|mark| {
        !mark.literal && (matches!(mark.ch, '*' | '?' | '[') || (extended && mark.ch == '('))
    })
}

/// One pattern character and whether it is literal — escaped, or quoted by
/// the caller — rather than pattern syntax.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    /// The character.
    pub ch: char,
    /// `true` when the character stands for itself.
    pub literal: bool,
}

/// A compiled glob pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern {
    components: Vec<Component>,
    pathname: bool,
}

impl Pattern {
    /// Compile `pattern` with no [`Options`].
    ///
    /// # Errors
    ///
    /// The [`GlobError`] naming why the pattern is malformed or too large.
    pub fn new(pattern: &str) -> Result<Self, GlobError> {
        Self::with_options(pattern, Options::empty())
    }

    /// Compile `pattern` read and matched as `options` say.
    ///
    /// # Errors
    ///
    /// The [`GlobError`] naming why the pattern is malformed or too large.
    pub fn with_options(pattern: &str, options: Options) -> Result<Self, GlobError> {
        if pattern.len() > MAX_PATTERN_LEN {
            return Err(GlobError::PatternTooLong);
        }
        Self::compiled(&compile::marks_of(pattern)?, options)
    }

    /// Compile text whose literal characters are already marked, so a
    /// caller holding quoting needs no escaping round trip. `\` is an
    /// ordinary character here unless marked.
    ///
    /// # Errors
    ///
    /// The [`GlobError`] naming why the pattern is malformed or too large.
    pub fn from_marked(text: &[Mark], options: Options) -> Result<Self, GlobError> {
        let len: usize = text.iter().map(|mark| mark.ch.len_utf8()).sum();
        if len > MAX_PATTERN_LEN {
            return Err(GlobError::PatternTooLong);
        }
        Self::compiled(text, options)
    }

    fn compiled(marks: &[Mark], options: Options) -> Result<Self, GlobError> {
        Ok(Self {
            components: compile::compile(marks, options)?,
            pathname: options.contains(Options::PATHNAME),
        })
    }

    /// The pattern's components: one, or one per `/`-separated piece in
    /// pathname mode.
    #[must_use]
    pub fn components(&self) -> &[Component] {
        &self.components
    }

    /// Whether any component holds a wildcard, so the pattern can match
    /// something other than its own literal text.
    #[must_use]
    pub fn has_wildcards(&self) -> bool {
        self.components.iter().any(|c| c.literal.is_none())
    }

    /// Whether `candidate` matches in full.
    #[must_use]
    pub fn matches(&self, candidate: &str) -> bool {
        self.matches_with(candidate, &mut Scratch::new())
    }

    /// [`Self::matches`] reusing `scratch`, so matching many candidates
    /// allocates once.
    #[must_use]
    pub fn matches_with(&self, candidate: &str, scratch: &mut Scratch) -> bool {
        if !self.pathname {
            return self
                .components
                .iter()
                .all(|c| c.matches_with(candidate, scratch));
        }
        let mut pieces = candidate.split('/');
        self.components.iter().all(|component| {
            pieces
                .next()
                .is_some_and(|piece| component.matches_with(piece, scratch))
        }) && pieces.next().is_none()
    }
}

/// One compiled component of a [`Pattern`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Component {
    nodes: Vec<Node>,
    literal: Option<String>,
    double_star: bool,
    leaves: usize,
    groups: usize,
    options: Options,
}

impl Component {
    /// The exact name this component matches when it holds no wildcard,
    /// escapes removed.
    #[must_use]
    pub fn literal(&self) -> Option<&str> {
        self.literal.as_deref()
    }

    /// Whether the component is spelled exactly `**`, the recursive
    /// wildcard of bash's `globstar`.
    #[must_use]
    pub fn is_double_star(&self) -> bool {
        self.double_star
    }

    /// Whether the name `name` matches this component, reusing `scratch`.
    #[must_use]
    pub fn matches_with(&self, name: &str, scratch: &mut Scratch) -> bool {
        match &self.literal {
            Some(literal) if self.options.contains(Options::CASE_INSENSITIVE) => {
                literal.chars().map(fold).eq(name.chars().map(fold))
            }
            Some(literal) => literal == name,
            None => engine::matches(self, name, scratch),
        }
    }
}

/// Reusable working memory for matching, so a caller testing many candidates
/// against one pattern allocates once.
#[derive(Debug, Default)]
pub struct Scratch {
    raw: Vec<char>,
    folded: Vec<char>,
    pool: Vec<Vec<u64>>,
    masks: Vec<u64>,
    masked: Vec<bool>,
    rows: Vec<u64>,
    slots: Vec<usize>,
    star_floor: Vec<usize>,
}

impl Scratch {
    /// Empty working memory.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}
