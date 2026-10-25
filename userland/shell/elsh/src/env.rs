//! The shell's mutable state and the variable-expansion phase.
//!
//! An [`Environment`] holds the shell variables — `PWD` among them, the
//! working directory's path — and the exit status of the most recently
//! completed pipeline (`$?`). It owns
//! parameter expansion: turning a parsed [`Word`] — still a list of
//! [`Segment`]s — into its text.
//!
//! * `$NAME` and `${NAME}` expand a variable; an unset variable expands to
//!   the empty string (POSIX default).
//! * `$?` and `${?}` expand to the last exit status.
//! * A `$` not followed by a name, `{`, or `?` is a literal `$`.
//! * Expansion never field-splits its result, as zsh does not.
//!
//! Only [`Segment::Expandable`] and [`Segment::QuotedExpandable`] runs are
//! scanned; [`Segment::Literal`] runs (single quotes, backslash escapes) are
//! emitted verbatim, so a quoted or escaped `$` is never expanded.
//!
//! Expansion keeps, beside its text, where it holds text typed unquoted;
//! the rest — quoted or escaped text, and every variable's value — stands
//! for itself. A value is never re-read as a wildcard — the zsh rule — so
//! only pattern characters typed unquoted reach filename generation.

use alloc::borrow::Cow;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ops::Range;

use tairix_glob::Mark;

use crate::error::ParseError;
use crate::expand::Budget;
use crate::lexer::{Segment, Word};

/// The default interactive prompt format, stored as `ELSH_PROMPT` when the
/// session exported none. It renders `user@host working-directory% `; the
/// backslash escapes are expanded by [`Environment::render_prompt`]:
/// `\u` → `USER`, `\h` → `HOSTNAME`, `\w` → the working directory with the
/// user's home abbreviated to `~`.
pub const DEFAULT_PROMPT: &str = "\\u@\\h \\w% ";

/// The hostname the prompt shows when the session exported no `HOSTNAME`
/// (the system hostname is still unprovisioned). A fixed, honest default —
/// the analogue of a POSIX system's `localhost` — not a guess at the real
/// name.
pub const DEFAULT_HOSTNAME: &str = "tairix";

/// The user name the prompt shows when the session exported no `USER`.
const DEFAULT_USER: &str = "user";

/// Mutable shell state: variables and `$?`.
#[derive(Clone, Debug)]
pub struct Environment {
    vars: BTreeMap<String, String>,
    exported: BTreeSet<String>,
    last_status: i32,
}

impl Default for Environment {
    fn default() -> Self {
        Self::new()
    }
}

impl Environment {
    /// A fresh environment with no variables.
    #[must_use]
    pub fn new() -> Self {
        Self {
            vars: BTreeMap::new(),
            exported: BTreeSet::new(),
            last_status: 0,
        }
    }

    /// The exit status of the last completed pipeline.
    #[must_use]
    pub fn last_status(&self) -> i32 {
        self.last_status
    }

    /// Record the exit status of the just-completed pipeline.
    pub fn set_last_status(&mut self, status: i32) {
        self.last_status = status;
    }

    /// The value of a variable, if set.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(String::as_str)
    }

    /// Set a shell variable. Does not change its exported state.
    pub fn set(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.vars.insert(name.into(), value.into());
    }

    /// Set a variable and mark it for export to child processes.
    pub fn export(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let name = name.into();
        self.vars.insert(name.clone(), value.into());
        self.exported.insert(name);
    }

    /// Mark an already-set variable as exported. Returns `false` if the
    /// variable does not exist.
    pub fn mark_exported(&mut self, name: &str) -> bool {
        if self.vars.contains_key(name) {
            self.exported.insert(name.to_string());
            true
        } else {
            false
        }
    }

    /// Remove a variable (and its exported mark). Returns `true` if it was set.
    pub fn unset(&mut self, name: &str) -> bool {
        self.exported.remove(name);
        self.vars.remove(name).is_some()
    }

    /// The exported variables, sorted by name — the environment a child
    /// process inherits.
    #[must_use]
    pub fn exported_vars(&self) -> Vec<(&str, &str)> {
        self.exported
            .iter()
            .filter_map(|name| self.vars.get(name).map(|v| (name.as_str(), v.as_str())))
            .collect()
    }

    /// Expand a parsed word's parameters into its text, quotes removed,
    /// charged to `budget`.
    ///
    /// # Errors
    ///
    /// [`ParseError::UnterminatedExpansion`] if a `${` is never closed by
    /// `}`, [`ParseError::UnsupportedSubstitution`] for a command substitution
    /// or arithmetic expansion, [`ParseError::ExpansionTooLarge`] when the
    /// text would pass what `budget` has left, and [`ParseError::OutOfMemory`]
    /// when the heap refuses it.
    pub(crate) fn expand_word(
        &self,
        word: &Word,
        budget: &mut Budget,
    ) -> Result<String, ParseError> {
        Ok(self.expand_marked(word, budget)?.text)
    }

    /// Expand a parsed word's parameters, keeping where it holds text typed
    /// unquoted. Its length is learned first, so it is refused before
    /// anything is copied when it would pass what `budget` has left, and its
    /// text is charged.
    ///
    /// # Errors
    ///
    /// As [`Environment::expand_word`].
    pub(crate) fn expand_marked(
        &self,
        word: &Word,
        budget: &mut Budget,
    ) -> Result<Expansion, ParseError> {
        let room = budget.remaining();
        let mut len = 0usize;
        self.pieces(word, &mut |piece, _| {
            len = len
                .checked_add(piece.len())
                .filter(|total| *total <= room)
                .ok_or(ParseError::ExpansionTooLarge)?;
            Ok(())
        })?;
        let mut expansion = Expansion {
            text: budget.string(len)?,
            unquoted: Vec::new(),
            quoted: word
                .iter()
                .any(|segment| !matches!(segment, Segment::Expandable(_))),
        };
        self.pieces(word, &mut |piece, literal| {
            expansion.push(piece, literal);
            Ok(())
        })?;
        Ok(expansion)
    }

    /// Hand `word`'s expansion to `each` a piece at a time, in order, with
    /// whether the piece stands for itself: quoted or escaped text, and every
    /// parameter's value.
    fn pieces(
        &self,
        word: &Word,
        each: &mut dyn FnMut(&str, bool) -> Result<(), ParseError>,
    ) -> Result<(), ParseError> {
        for segment in word {
            match segment {
                Segment::Literal(text) => each(text, true)?,
                Segment::QuotedExpandable(text) => self.parameters(text, true, each)?,
                Segment::Expandable(text) => self.parameters(text, false, each)?,
            }
        }
        Ok(())
    }

    /// `source`'s pieces: its own text, standing for itself when `quoted`,
    /// with each parameter it names replaced by its value.
    fn parameters(
        &self,
        source: &str,
        quoted: bool,
        each: &mut dyn FnMut(&str, bool) -> Result<(), ParseError>,
    ) -> Result<(), ParseError> {
        // The lexer refuses these in a word; a here-document body reaches
        // expansion without it.
        if source.contains('`') {
            return Err(ParseError::UnsupportedSubstitution);
        }
        let mut rest = source;
        while let Some((before, after)) = rest.split_once('$') {
            each(before, quoted)?;
            if after.starts_with('(') {
                return Err(ParseError::UnsupportedSubstitution);
            }
            rest = if let Some(braced) = after.strip_prefix('{') {
                let (name, after) = braced
                    .split_once('}')
                    .ok_or(ParseError::UnterminatedExpansion)?;
                each(&self.value_of(name), true)?;
                after
            } else if let Some(after) = after.strip_prefix('?') {
                each(&self.last_status.to_string(), true)?;
                after
            } else if after.starts_with(is_name_start) {
                let end = after
                    .find(|c: char| !is_name_char(c))
                    .unwrap_or(after.len());
                let (name, after) = after.split_at(end);
                each(&self.value_of(name), true)?;
                after
            } else {
                each("$", quoted)?;
                after
            };
        }
        each(rest, quoted)
    }

    fn value_of(&self, name: &str) -> Cow<'_, str> {
        if name == "?" {
            return Cow::Owned(self.last_status.to_string());
        }
        Cow::Borrowed(self.get(name).unwrap_or(""))
    }

    /// Seed the standard interactive-session variables, exporting each so a
    /// child inherits it, and set the working directory to the user's home.
    ///
    /// `inherited(name)` returns the value the spawner (login) exported for
    /// `name`, or `None`. Identity and locale variables (`USER`, `LOGNAME`,
    /// `HOME`, `SHELL`, `TERM`, `LANG`) come from the inherited environment
    /// login built; the shell fills a defensive default only when one is
    /// absent (a shell run outside a normal login). `PATH` is passed on only
    /// as inherited, empty or not: the fixed lookup prefix already names every
    /// program store, so the shell has nothing of its own to add. The shell-owned
    /// variables — `HOSTNAME` (the prompt host, defaulting to
    /// [`DEFAULT_HOSTNAME`] until the system hostname is provisioned),
    /// `PWD`/`OLDPWD` (the inherited `PWD`, else `HOME`, which the shell then
    /// settles against its working directory), and the prompt format
    /// `ELSH_PROMPT` ([`DEFAULT_PROMPT`]) — are filled here. The lookup seam
    /// keeps this pure and host-testable: production passes a closure over
    /// the runtime's environment accessor, tests pass a map.
    pub fn seed_interactive(&mut self, inherited: impl Fn(&str) -> Option<String>) {
        let take = |name: &str, default: &str| -> String {
            inherited(name)
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| default.to_string())
        };
        let user = take("USER", DEFAULT_USER);
        let home = take("HOME", "/");
        let pwd = inherited("PWD")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| home.clone());
        let logname = inherited("LOGNAME")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| user.clone());

        self.export("USER", user);
        self.export("LOGNAME", logname);
        self.export("HOME", home);
        self.export("SHELL", take("SHELL", "/System/Commands/elsh.app/Run"));
        if let Some(path) = inherited("PATH") {
            self.export("PATH", path);
        }
        self.export("TERM", take("TERM", "xterm-256color"));
        self.export("LANG", take("LANG", "en-US"));
        self.export("HOSTNAME", take("HOSTNAME", DEFAULT_HOSTNAME));
        self.export("PWD", pwd.clone());
        // OLDPWD is defined from the first prompt (equal to PWD until the
        // first `cd`), so `$OLDPWD` and `cd -` are never unset surprises.
        self.export("OLDPWD", pwd);
        self.set("ELSH_PROMPT", take("ELSH_PROMPT", DEFAULT_PROMPT));
    }

    /// The `USER` value for the prompt, or [`DEFAULT_USER`] when unset.
    fn prompt_user(&self) -> &str {
        self.get("USER")
            .filter(|v| !v.is_empty())
            .unwrap_or(DEFAULT_USER)
    }

    /// The `HOSTNAME` value for the prompt, or [`DEFAULT_HOSTNAME`] when unset.
    fn prompt_host(&self) -> &str {
        self.get("HOSTNAME")
            .filter(|v| !v.is_empty())
            .unwrap_or(DEFAULT_HOSTNAME)
    }

    /// The working directory for the prompt — `PWD`, empty when unset — with
    /// the user's `HOME` abbreviated to `~` (exactly `~` at home, `~/rest`
    /// beneath it).
    fn prompt_cwd(&self) -> String {
        let cwd = self.get("PWD").unwrap_or_default();
        if let Some(home) = self.get("HOME").filter(|h| !h.is_empty()) {
            if cwd == home {
                return "~".to_string();
            }
            if let Some(rest) = cwd.strip_prefix(home) {
                if rest.starts_with('/') {
                    return alloc::format!("~{rest}");
                }
            }
        }
        cwd.to_string()
    }

    /// Render the interactive prompt from the `ELSH_PROMPT` format (or
    /// [`DEFAULT_PROMPT`] when unset), expanding the escapes `\u` (`USER`),
    /// `\h` (`HOSTNAME`), and `\w` (the working directory with `HOME`
    /// abbreviated to `~`). `\\` is a literal backslash; any other `\x` is
    /// left verbatim so an unknown escape is shown, never dropped.
    #[must_use]
    pub fn render_prompt(&self) -> String {
        let format = self
            .get("ELSH_PROMPT")
            .map_or_else(|| DEFAULT_PROMPT.to_string(), ToString::to_string);
        let mut out = String::new();
        let mut chars = format.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('u') => out.push_str(self.prompt_user()),
                Some('h') => out.push_str(self.prompt_host()),
                Some('w') => out.push_str(&self.prompt_cwd()),
                // A literal backslash, or a lone trailing backslash at the
                // end of the format, both render a single `\`.
                Some('\\') | None => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
            }
        }
        out
    }
}

/// A word after parameter expansion: its text, quoting removed; where it
/// holds text typed unquoted, the only text that can be pattern syntax; and
/// whether any part of the word was quoted, which keeps an empty result as
/// an empty argument rather than no argument.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Expansion {
    pub(crate) text: String,
    /// The byte ranges of `text` typed unquoted, in order.
    pub(crate) unquoted: Vec<Range<usize>>,
    pub(crate) quoted: bool,
}

impl Expansion {
    /// Append `piece`, as typed unquoted unless it stands for itself.
    fn push(&mut self, piece: &str, literal: bool) {
        let at = self.text.len();
        self.text.push_str(piece);
        if literal || piece.is_empty() {
            return;
        }
        match self.unquoted.last_mut() {
            Some(run) if run.end == at => run.end = self.text.len(),
            _ => self.unquoted.push(at..self.text.len()),
        }
    }

    /// The characters typed unquoted.
    pub(crate) fn unquoted_chars(&self) -> impl Iterator<Item = char> + '_ {
        self.unquoted
            .iter()
            .flat_map(|run| self.text.get(run.clone()).unwrap_or_default().chars())
    }

    /// Each character as the matcher reads it: literal unless typed unquoted.
    pub(crate) fn marks(&self) -> impl Iterator<Item = Mark> + '_ {
        let mut runs = self.unquoted.iter().peekable();
        self.text.char_indices().map(move |(at, ch)| {
            while runs.next_if(|run| run.end <= at).is_some() {}
            let literal = runs.peek().is_none_or(|run| run.start > at);
            Mark { ch, literal }
        })
    }
}

/// Split a word of the form `NAME=VALUE` into its name and its (still
/// unexpanded) value word.
///
/// Returns `None` unless the word begins with a valid variable name followed
/// by `=` in unquoted text — so `'FOO'=bar` (a quoted name) and `1=x` (a name
/// starting with a digit) are *not* assignments. The value retains its
/// segments so it is expanded exactly like any other word: `FOO=$BAR` and
/// `FOO="a b"` work as expected.
#[must_use]
pub fn assignment_split(word: &Word) -> Option<(String, Word)> {
    let Segment::Expandable(first) = word.first()? else {
        return None;
    };
    let eq = first.find('=')?;
    let name = &first[..eq];
    if !is_valid_name(name) {
        return None;
    }
    let mut value: Word = Vec::new();
    let rest = &first[eq + 1..];
    if !rest.is_empty() {
        value.push(Segment::Expandable(rest.to_string()));
    }
    value.extend_from_slice(&word[1..]);
    Some((name.to_string(), value))
}

/// Split a command's words into its leading `NAME=VALUE` prefix assignments
/// and the remaining words (the command proper).
///
/// The split follows POSIX: assignment words bind only while they *lead* the
/// command — the first non-assignment word ends the prefix, and a later
/// `NAME=VALUE` is an ordinary argument. Both halves may be empty (an
/// assignment-only command, or a command with no prefix). Values keep their
/// segments so the caller expands them exactly like any other word.
#[must_use]
pub fn split_prefix_assignments(words: &[Word]) -> (Vec<(String, Word)>, &[Word]) {
    let mut assignments = Vec::new();
    for (index, word) in words.iter().enumerate() {
        match assignment_split(word) {
            Some(assignment) => assignments.push(assignment),
            None => return (assignments, &words[index..]),
        }
    }
    (assignments, &[])
}

/// `true` if `name` is a valid shell variable name (`[A-Za-z_][A-Za-z0-9_]*`).
#[must_use]
pub fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if is_name_start(c) => chars.all(is_name_char),
        _ => false,
    }
}

fn is_name_start(c: char) -> bool {
    c == '_' || c.is_ascii_alphabetic()
}

fn is_name_char(c: char) -> bool {
    c == '_' || c.is_ascii_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::{assignment_split, is_valid_name, split_prefix_assignments, Environment};
    use crate::error::ParseError;
    use crate::expand::{Budget, MAX_EXPANSION_BYTES};
    use crate::lexer::{Segment, Word};
    use alloc::string::{String, ToString};
    use alloc::vec;

    fn expanded(env: &Environment, word: &Word) -> Result<String, ParseError> {
        env.expand_word(word, &mut Budget::new(MAX_EXPANSION_BYTES))
    }

    fn expandable(s: &str) -> Word {
        vec![Segment::Expandable(s.to_string())]
    }

    fn flat(word: &Word) -> String {
        let mut out = String::new();
        for seg in word {
            match seg {
                Segment::Literal(s) | Segment::Expandable(s) | Segment::QuotedExpandable(s) => {
                    out.push_str(s);
                }
            }
        }
        out
    }

    #[test]
    fn expands_set_and_unset_variables() {
        let mut env = Environment::new();
        env.set("NAME", "tairix");
        assert_eq!(expanded(&env, &expandable("$NAME")).unwrap(), "tairix");
        assert_eq!(expanded(&env, &expandable("${NAME}!")).unwrap(), "tairix!");
        // Unset expands to empty.
        assert_eq!(expanded(&env, &expandable("[$MISSING]")).unwrap(), "[]");
    }

    #[test]
    fn expands_last_status() {
        let mut env = Environment::new();
        env.set_last_status(42);
        assert_eq!(expanded(&env, &expandable("$?")).unwrap(), "42");
        assert_eq!(expanded(&env, &expandable("${?}")).unwrap(), "42");
    }

    #[test]
    fn literal_segments_are_never_expanded() {
        let env = Environment::new();
        let word = vec![Segment::Literal("$NAME".to_string())];
        assert_eq!(expanded(&env, &word).unwrap(), "$NAME");
    }

    #[test]
    fn lone_dollar_is_literal() {
        let env = Environment::new();
        assert_eq!(expanded(&env, &expandable("a$ b")).unwrap(), "a$ b");
        assert_eq!(expanded(&env, &expandable("cost$")).unwrap(), "cost$");
    }

    #[test]
    fn unterminated_brace_expansion_fails_closed() {
        let env = Environment::new();
        assert_eq!(
            expanded(&env, &expandable("${oops")),
            Err(ParseError::UnterminatedExpansion)
        );
    }

    /// A word stops growing at what the line has left, before anything of it
    /// is copied, and is charged its text.
    #[test]
    fn a_word_s_expansion_stops_at_what_the_budget_has_left() {
        let mut env = Environment::new();
        env.set("A", "abcd");
        let word_cost = "abcdabcd".len();
        let mut budget = Budget::new(word_cost);
        assert_eq!(
            env.expand_word(&expandable("$A$A"), &mut budget).as_deref(),
            Ok("abcdabcd")
        );
        assert_eq!(budget.remaining(), 0);
        assert_eq!(
            env.expand_word(&expandable("$A$A"), &mut Budget::new(word_cost - 1)),
            Err(ParseError::ExpansionTooLarge)
        );
        assert_eq!(
            env.expand_marked(&expandable("$A$A"), &mut Budget::new(7)),
            Err(ParseError::ExpansionTooLarge)
        );
    }

    #[test]
    fn only_text_typed_unquoted_is_marked_as_pattern_syntax() {
        let mut env = Environment::new();
        env.set("V", "*v");
        let word = vec![
            Segment::Expandable("a$V*".to_string()),
            Segment::Literal("?".to_string()),
            Segment::QuotedExpandable("[".to_string()),
            Segment::Expandable("]".to_string()),
        ];
        let expansion = env
            .expand_marked(&word, &mut Budget::new(MAX_EXPANSION_BYTES))
            .expect("expands");
        assert_eq!(expansion.text, "a*v*?[]");
        assert_eq!(expansion.unquoted_chars().collect::<String>(), "a*]");
        let literal: alloc::vec::Vec<bool> = expansion.marks().map(|mark| mark.literal).collect();
        assert_eq!(literal, [false, true, true, false, true, true, false]);
    }

    #[test]
    fn concatenates_adjacent_expansions() {
        let mut env = Environment::new();
        env.set("A", "x");
        env.set("B", "y");
        assert_eq!(expanded(&env, &expandable("$A$B")).unwrap(), "xy");
    }

    #[test]
    fn export_controls_child_environment() {
        let mut env = Environment::new();
        env.set("SECRET", "1");
        env.export("PATH", "/Users/ada/bin");
        assert_eq!(env.exported_vars(), [("PATH", "/Users/ada/bin")]);
        // A plain `set` variable is visible to the shell but not exported.
        assert_eq!(env.get("SECRET"), Some("1"));
        assert!(env.mark_exported("SECRET"));
        assert_eq!(
            env.exported_vars(),
            [("PATH", "/Users/ada/bin"), ("SECRET", "1")]
        );
        assert!(!env.mark_exported("GHOST"));
    }

    #[test]
    fn unset_removes_value_and_export() {
        let mut env = Environment::new();
        env.export("X", "1");
        assert!(env.unset("X"));
        assert_eq!(env.get("X"), None);
        assert!(env.exported_vars().is_empty());
        assert!(!env.unset("X"));
    }

    #[test]
    fn seed_interactive_takes_inherited_values_and_exports_them() {
        let mut env = Environment::new();
        env.seed_interactive(|name| match name {
            "USER" => Some("ada".to_string()),
            "HOME" => Some("/Users/ada".to_string()),
            "SHELL" => Some("/System/Commands/elsh.app/Run".to_string()),
            "TERM" => Some("xterm-256color".to_string()),
            "PATH" => Some("/Users/ada/bin".to_string()),
            _ => None,
        });
        assert_eq!(env.get("USER"), Some("ada"));
        // LOGNAME defaults to USER when not separately inherited.
        assert_eq!(env.get("LOGNAME"), Some("ada"));
        assert_eq!(env.get("HOME"), Some("/Users/ada"));
        // PWD/OLDPWD follow HOME.
        assert_eq!(env.get("PWD"), Some("/Users/ada"));
        assert_eq!(env.get("OLDPWD"), Some("/Users/ada"));
        // The standard variables are exported to children.
        let exported: alloc::vec::Vec<&str> =
            env.exported_vars().into_iter().map(|(n, _)| n).collect();
        for name in ["USER", "LOGNAME", "HOME", "SHELL", "PATH", "TERM", "LANG"] {
            assert!(exported.contains(&name), "{name} exported: {exported:?}");
        }
    }

    #[test]
    fn seed_interactive_fills_defaults_when_nothing_is_inherited() {
        let mut env = Environment::new();
        env.seed_interactive(|_| None);
        assert_eq!(env.get("USER"), Some(super::DEFAULT_USER));
        assert_eq!(env.get("HOSTNAME"), Some(super::DEFAULT_HOSTNAME));
        assert_eq!(env.get("LANG"), Some("en-US"));
        assert_eq!(env.get("PATH"), None, "no PATH is invented");
        assert_eq!(env.get("ELSH_PROMPT"), Some(super::DEFAULT_PROMPT));
        // An empty inherited value is treated as absent (fills the default),
        // except PATH, which stays exactly as inherited.
        let mut empty = Environment::new();
        empty.seed_interactive(|name| matches!(name, "USER" | "PATH").then(String::new));
        assert_eq!(empty.get("USER"), Some(super::DEFAULT_USER));
        assert_eq!(empty.get("PATH"), Some(""), "an empty PATH stays empty");
    }

    #[test]
    fn render_prompt_shows_user_host_and_home_abbreviated_cwd() {
        let mut env = Environment::new();
        env.seed_interactive(|name| match name {
            "USER" => Some("ada".to_string()),
            "HOSTNAME" => Some("babbage".to_string()),
            "HOME" => Some("/Users/ada".to_string()),
            _ => None,
        });
        // At home the working directory renders as `~`.
        assert_eq!(env.render_prompt(), "ada@babbage ~% ");
        // Beneath home it is `~/rest`.
        env.export("PWD", "/Users/ada/Documents");
        assert_eq!(env.render_prompt(), "ada@babbage ~/Documents% ");
        // Outside home the absolute path is shown.
        env.export("PWD", "/Storage/disk");
        assert_eq!(env.render_prompt(), "ada@babbage /Storage/disk% ");
    }

    #[test]
    fn render_prompt_honours_a_custom_format_and_unknown_escapes() {
        let mut env = Environment::new();
        env.set("USER", "ada");
        env.set("HOSTNAME", "babbage");
        // A custom format with a literal and an unknown escape (kept verbatim).
        env.set("ELSH_PROMPT", "[\\u@\\h]\\z$ ");
        assert_eq!(env.render_prompt(), "[ada@babbage]\\z$ ");
    }

    #[test]
    fn assignment_split_recognises_valid_forms() {
        let (name, value) = assignment_split(&expandable("FOO=bar")).unwrap();
        assert_eq!(name, "FOO");
        assert_eq!(flat(&value), "bar");

        // Value keeps its segments for later expansion.
        let word = vec![
            Segment::Expandable("FOO=".to_string()),
            Segment::Literal("a b".to_string()),
        ];
        let (name, value) = assignment_split(&word).unwrap();
        assert_eq!(name, "FOO");
        assert_eq!(flat(&value), "a b");

        // Empty value is allowed.
        let (name, value) = assignment_split(&expandable("EMPTY=")).unwrap();
        assert_eq!(name, "EMPTY");
        assert_eq!(flat(&value), "");
    }

    #[test]
    fn assignment_split_rejects_non_assignments() {
        assert!(assignment_split(&expandable("notanassignment")).is_none());
        assert!(assignment_split(&expandable("1BAD=x")).is_none());
        // A quoted name is not an assignment, whether single- or double-quoted.
        let quoted = vec![
            Segment::Literal("FOO".to_string()),
            Segment::Expandable("=bar".to_string()),
        ];
        assert!(assignment_split(&quoted).is_none());
        let double_quoted = vec![
            Segment::QuotedExpandable("FOO".to_string()),
            Segment::Expandable("=bar".to_string()),
        ];
        assert!(assignment_split(&double_quoted).is_none());
    }

    #[test]
    fn prefix_assignments_split_at_the_first_command_word() {
        let words = vec![
            expandable("A=1"),
            expandable("B=2"),
            expandable("cmd"),
            expandable("C=3"),
        ];
        let (assignments, rest) = split_prefix_assignments(&words);
        assert_eq!(assignments.len(), 2);
        assert_eq!(assignments[0].0, "A");
        assert_eq!(assignments[1].0, "B");
        // `C=3` follows the command word, so it is an ordinary argument.
        assert_eq!(rest.len(), 2);

        // Assignment-only: everything splits, nothing remains.
        let words = vec![expandable("A=1")];
        let (assignments, rest) = split_prefix_assignments(&words);
        assert_eq!(assignments.len(), 1);
        assert!(rest.is_empty());

        // No prefix: the words pass through untouched.
        let words = vec![expandable("cmd"), expandable("A=1")];
        let (assignments, rest) = split_prefix_assignments(&words);
        assert!(assignments.is_empty());
        assert_eq!(rest.len(), 2);
    }

    #[test]
    fn valid_name_rules() {
        assert!(is_valid_name("FOO_bar9"));
        assert!(is_valid_name("_x"));
        assert!(!is_valid_name(""));
        assert!(!is_valid_name("9x"));
        assert!(!is_valid_name("a-b"));
    }
}
