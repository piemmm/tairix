//! System variable names: where one ends, how two sort, and how a pattern
//! matches one.

use core::cmp::Ordering;

/// `byte` in upper case as the kernel folds a name: the ASCII letters, and
/// Latin-1's `à`–`ö` and `ø`–`þ`.
#[must_use]
pub const fn upper(byte: u8) -> u8 {
    match byte {
        b'a'..=b'z' | 0xE0..=0xF6 | 0xF8..=0xFE => byte - 0x20,
        _ => byte,
    }
}

/// The name `text` starts with: everything before its first space or control
/// character.
#[must_use]
pub fn name(text: &[u8]) -> &[u8] {
    let end = text.iter().position(|&b| b <= b' ').unwrap_or(text.len());
    &text[..end]
}

/// How two names sort: byte by byte in upper case, a name before every name
/// it begins.
#[must_use]
pub fn compare(a: &[u8], b: &[u8]) -> Ordering {
    a.iter().map(|&b| upper(b)).cmp(b.iter().map(|&b| upper(b)))
}

/// Whether `pattern` holds a wildcard: `*` for any run of characters, `#` for
/// any one.
#[must_use]
pub fn is_wild(pattern: &[u8]) -> bool {
    pattern.iter().any(|&b| b == b'*' || b == b'#')
}

/// Whether a new variable may be called `name`: it is not empty and holds no
/// wildcard.
#[must_use]
pub fn is_creatable(name: &[u8]) -> bool {
    !name.is_empty() && !is_wild(name)
}

/// Whether `pattern` matches the whole of `name`, letters in either case.
#[must_use]
pub fn matches(pattern: &[u8], name: &[u8]) -> bool {
    let (mut p, mut n) = (0, 0);
    // Where to resume after the last `*`: the pattern past it, the name at
    // the next byte it has not yet stood for.
    let mut resume: Option<(usize, usize)> = None;
    while let Some(&wanted) = name.get(n) {
        match pattern.get(p) {
            Some(b'*') => {
                p += 1;
                resume = Some((p, n));
            }
            Some(&c) if c == b'#' || upper(c) == upper(wanted) => {
                p += 1;
                n += 1;
            }
            _ => match resume {
                Some((after, from)) => {
                    p = after;
                    n = from + 1;
                    resume = Some((after, n));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == b'*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_fold_in_ascii_and_latin_1_but_nothing_else_does() {
        assert_eq!(upper(b'a'), b'A');
        assert_eq!(upper(b'z'), b'Z');
        assert_eq!(upper(0xE9), 0xC9, "é");
        assert_eq!(upper(0xF7), 0xF7, "÷ is no letter");
        assert_eq!(upper(0xFF), 0xFF, "ÿ has no capital in Latin-1");
        assert_eq!(upper(b'{'), b'{');
    }

    #[test]
    fn names_sort_folded_and_a_prefix_first() {
        let mut names: [&[u8]; 5] = [b"A_B", b"ab", b"A$X", b"a", b"B"];
        names.sort_by(|a, b| compare(a, b));
        assert_eq!(names, [b"a" as &[u8], b"A$X", b"ab", b"A_B", b"B"]);
        assert_eq!(compare(b"Obey$Dir", b"OBEY$DIR"), Ordering::Equal);
    }

    #[test]
    fn a_pattern_matches_the_whole_name() {
        assert!(matches(b"Alias$*", b"alias$Cat"));
        assert!(matches(b"*", b""));
        assert!(matches(b"A#C", b"abc"));
        assert!(!matches(b"A#C", b"ac"));
        assert!(matches(b"*$Dir", b"Edit$Dir"));
        assert!(!matches(b"*$Dir", b"Edit$Dirs"));
        assert!(matches(b"a**b*c", b"aXbYbZc"));
        assert!(!matches(b"a*b", b"aXc"));
        assert!(matches(b"#*#", b"ab"));
        assert!(!matches(b"#*#", b"a"));
    }

    #[test]
    fn a_name_ends_at_its_first_space_or_control_character() {
        assert_eq!(name(b"Foo Bar"), b"Foo");
        assert_eq!(name(b"Foo\r"), b"Foo");
        assert_eq!(name(b"Fo\xE9o"), b"Fo\xE9o");
        assert!(is_creatable(b"Foo$Dir"));
        assert!(!is_creatable(b""));
        assert!(!is_creatable(b"Foo*"));
        assert!(!is_creatable(b"F#o"));
    }
}
