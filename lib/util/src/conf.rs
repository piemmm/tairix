//! The line grammar TAIRiX's `#`-commented configuration stores share.
//!
//! Every line-oriented store in the tree — the boot-time system
//! configuration (`lib/sysconfig`), the network configuration
//! (`lib/netconfig`), the service enrolment overrides (`lib/enrolment`),
//! and the startup list (`userland/system/init`) — reads the same shape of
//! document: a `#`
//! begins a comment that runs to the end of the line, and blank lines
//! carry no setting. That tokenisation lives here once, so a change to how
//! a comment is recognised cannot apply to some stores and not others.
//!
//! No store's keys or values may contain `#`; each store's own validators
//! enforce that, which is what makes cutting at the first `#`
//! unambiguous.

/// What a configuration key accepts.
///
/// Shared because both closed registries state it and both a command-line
/// tool and a settings surface read it: a tool refusing a value names the
/// choices, and a surface offering the choices builds its list from the same
/// definition rather than a second copy that would drift.
///
/// Most keys carry a closed set of canonical spellings; a key whose value is
/// inherently open — an address, a list of host names — describes its
/// accepted form instead, and its own parser is what admits or refuses a
/// spelling.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ValueShape {
    /// One of these canonical spellings, and nothing else.
    Closed(&'static [&'static str]),
    /// Free-form text of this described form.
    Free(&'static str),
}

/// The portion of `line` before its first `#`, dropping an inline or
/// whole-line comment.
///
/// The returned slice keeps its surrounding whitespace: a caller trims it
/// with `str::trim` and treats an empty result as a line carrying no
/// setting.
#[must_use]
pub fn strip_comment(line: &str) -> &str {
    match comment_at(line) {
        Some(index) => &line[..index],
        None => line,
    }
}

/// The byte offset of `line`'s comment marker, if it carries one.
#[must_use]
pub fn comment_at(line: &str) -> Option<usize> {
    line.find('#')
}

/// One setting of a `key value` store, split the way every such store reads
/// it: the key is the first word, the value the rest of the line, trimmed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SettingLine<'a> {
    /// The key.
    pub key: &'a str,
    /// Where the key starts in the raw line.
    pub key_at: usize,
    /// The value, or `None` when the line names a key and nothing else.
    pub value: Option<&'a str>,
    /// Where the value starts in the raw line; the key's end when there is
    /// none.
    pub value_at: usize,
}

/// Split the raw `line` into its setting, or `None` for a line that carries
/// none (blank, or only a comment).
#[must_use]
pub fn setting_line(line: &str) -> Option<SettingLine<'_>> {
    let content = strip_comment(line);
    let key_at = content.len() - content.trim_start().len();
    let rest = content[key_at..].trim_end();
    if rest.is_empty() {
        return None;
    }
    let key_len = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let after = &rest[key_len..];
    let value = after.trim_start();
    Some(SettingLine {
        key: &rest[..key_len],
        key_at,
        value: (!value.is_empty()).then_some(value),
        value_at: key_at + key_len + (after.len() - value.len()),
    })
}

/// A store's refusal, and the 1-based line it was raised at: `None` for a
/// refusal of the whole document that no single line owns.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Located<E> {
    /// The 1-based source line, or `None` for a whole-document refusal.
    pub line: Option<usize>,
    /// What was refused.
    pub kind: E,
}

impl<E> Located<E> {
    /// A refusal of the 1-based `line`.
    #[must_use]
    pub const fn at(line: usize, kind: E) -> Self {
        Self {
            line: Some(line),
            kind,
        }
    }

    /// A refusal of the whole document.
    #[must_use]
    pub const fn whole(kind: E) -> Self {
        Self { line: None, kind }
    }
}

impl<E: core::fmt::Display> core::fmt::Display for Located<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.line {
            Some(line) => write!(f, "line {line}: {}", self.kind),
            None => write!(f, "{}", self.kind),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{setting_line, strip_comment, Located, SettingLine};

    #[test]
    fn a_line_without_a_comment_is_returned_whole() {
        assert_eq!(strip_comment("os.loginType text"), "os.loginType text");
    }

    #[test]
    fn an_inline_comment_is_cut_at_the_first_marker() {
        assert_eq!(strip_comment("key value # why # again"), "key value ");
    }

    #[test]
    fn a_whole_line_comment_leaves_nothing() {
        assert!(strip_comment("# a comment").trim().is_empty());
    }

    #[test]
    fn an_empty_line_stays_empty() {
        assert_eq!(strip_comment(""), "");
    }

    #[test]
    fn surrounding_whitespace_is_left_for_the_caller_to_trim() {
        assert_eq!(strip_comment("  key value  ").trim(), "key value");
    }

    #[test]
    fn a_setting_splits_at_the_first_whitespace_and_keeps_its_offsets() {
        let raw = "  os.loginType\t gui  # why";
        assert_eq!(
            setting_line(raw),
            Some(SettingLine {
                key: "os.loginType",
                key_at: 2,
                value: Some("gui"),
                value_at: 16,
            })
        );
        let parsed = setting_line(raw).map(|s| {
            (
                &raw[s.key_at..s.key_at + s.key.len()],
                &raw[s.value_at..s.value_at + 3],
            )
        });
        assert_eq!(parsed, Some(("os.loginType", "gui")));
    }

    #[test]
    fn a_value_keeps_its_inner_whitespace() {
        let line = setting_line("time.servers a.example  b.example");
        assert_eq!(line.and_then(|s| s.value), Some("a.example  b.example"));
    }

    #[test]
    fn a_key_alone_has_no_value_and_a_blank_line_no_setting() {
        let bare = setting_line("  key   ");
        assert_eq!(
            bare.map(|s| (s.key, s.value, s.value_at)),
            Some(("key", None, 5))
        );
        assert_eq!(setting_line(""), None);
        assert_eq!(setting_line("   # only a comment"), None);
        assert_eq!(setting_line("\t"), None);
    }

    #[test]
    fn a_located_refusal_names_its_line() {
        extern crate alloc;
        use alloc::string::ToString;
        assert_eq!(Located::at(7, "bad").to_string(), "line 7: bad");
        assert_eq!(Located::whole("too long").to_string(), "too long");
        assert_eq!(Located::at(3, 1u8).line, Some(3));
    }
}
