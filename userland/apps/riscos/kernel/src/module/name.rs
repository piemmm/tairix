//! Module names as the PRM has them matched: a title, then after `%` the
//! postfix naming one incarnation, each compared without case, a `.`
//! abbreviating the rest of a name that goes on.

use tairix_obey::name::upper;

/// The separator between a module's title and an incarnation's postfix.
pub(crate) const POSTFIX: u8 = b'%';

/// A module name as a string gives it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Given<'a> {
    /// The title, to its first control character, space or `%`.
    pub(crate) title: &'a [u8],
    /// The postfix after a `%`, to its first control character or space.
    pub(crate) postfix: Option<&'a [u8]>,
}

impl<'a> Given<'a> {
    /// The module name `text` starts with.
    pub(crate) fn parse(text: &'a [u8]) -> Self {
        let end = text
            .iter()
            .position(|&b| b <= b' ' || b == POSTFIX)
            .unwrap_or(text.len());
        let postfix = text
            .get(end)
            .is_some_and(|&b| b == POSTFIX)
            .then(|| word(&text[end + 1..]));
        Self {
            title: &text[..end],
            postfix,
        }
    }
}

/// `text` to its first control character or space.
pub(crate) fn word(text: &[u8]) -> &[u8] {
    &text[..text.iter().position(|&b| b <= b' ').unwrap_or(text.len())]
}

/// Whether `given` names `name`: the same but for case, or the same as far
/// as a `.` that follows at least one character, more of `name` going on
/// past it.
pub(crate) fn names(given: &[u8], name: &[u8]) -> bool {
    for (i, &g) in given.iter().enumerate() {
        if g == b'.' {
            return i > 0 && i < name.len();
        }
        if name.get(i).map(|&n| upper(n)) != Some(upper(g)) {
            return false;
        }
    }
    given.len() == name.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_its_title_and_the_postfix_after_a_percent() {
        let given = Given::parse(b"WindowManager%Second tail");
        assert_eq!(given.title, b"WindowManager");
        assert_eq!(given.postfix, Some(&b"Second"[..]));
        assert_eq!(Given::parse(b"Wimp\r").postfix, None);
        assert_eq!(Given::parse(b"Wimp% x").postfix, Some(&b""[..]));
    }

    #[test]
    fn names_match_without_case_over_latin_1() {
        assert!(names(b"windowmanager", b"WindowManager"));
        assert!(names(b"\xE9t\xE9", b"\xC9T\xC9"));
        assert!(!names(b"Window", b"WindowManager"));
        assert!(!names(b"WindowManagerX", b"WindowManager"));
    }

    #[test]
    fn a_dot_abbreviates_only_a_name_that_goes_on() {
        assert!(names(b"Wind.", b"WindowManager"));
        assert!(names(b"Wind.anything", b"WindowManager"));
        assert!(!names(b"WindowManager.", b"WindowManager"));
        assert!(!names(b".", b"WindowManager"));
        assert!(!names(b"Wx.", b"WindowManager"));
    }
}
