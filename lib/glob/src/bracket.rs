//! Bracket expressions (`[...]`): members, ranges, POSIX character classes,
//! equivalence classes and collating symbols (POSIX XBD 9.3.5, as the shell's
//! pattern matching notation uses them).

use alloc::vec::Vec;

use crate::compile::fold;
use crate::{GlobError, Mark, MAX_CLASS_ITEMS};

/// A named character class, `[:name:]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CharClass {
    Alnum,
    Alpha,
    Ascii,
    Blank,
    Cntrl,
    Digit,
    Graph,
    Lower,
    Print,
    Punct,
    Space,
    Upper,
    Word,
    Xdigit,
}

/// The POSIX class names, plus bash's `word` and `ascii`.
const CLASSES: &[(&str, CharClass)] = &[
    ("alnum", CharClass::Alnum),
    ("alpha", CharClass::Alpha),
    ("ascii", CharClass::Ascii),
    ("blank", CharClass::Blank),
    ("cntrl", CharClass::Cntrl),
    ("digit", CharClass::Digit),
    ("graph", CharClass::Graph),
    ("lower", CharClass::Lower),
    ("print", CharClass::Print),
    ("punct", CharClass::Punct),
    ("space", CharClass::Space),
    ("upper", CharClass::Upper),
    ("word", CharClass::Word),
    ("xdigit", CharClass::Xdigit),
];

impl CharClass {
    /// Membership over Unicode properties. `digit` and `xdigit` stay ASCII,
    /// as POSIX requires in every locale; `punct` is what remains of `graph`
    /// once letters and digits are removed.
    fn contains(self, c: char) -> bool {
        match self {
            Self::Alnum => c.is_alphanumeric(),
            Self::Alpha => c.is_alphabetic(),
            Self::Ascii => c.is_ascii(),
            Self::Blank => c.is_whitespace() && !is_vertical_space(c),
            Self::Cntrl => c.is_control(),
            Self::Digit => c.is_ascii_digit(),
            Self::Graph => !c.is_control() && !c.is_whitespace(),
            Self::Lower => c.is_lowercase(),
            Self::Print => !c.is_control(),
            Self::Punct => !c.is_control() && !c.is_whitespace() && !c.is_alphanumeric(),
            Self::Space => c.is_whitespace(),
            Self::Upper => c.is_uppercase(),
            Self::Word => c.is_alphanumeric() || c == '_',
            Self::Xdigit => c.is_ascii_hexdigit(),
        }
    }
}

fn is_vertical_space(c: char) -> bool {
    matches!(
        c,
        '\n' | '\u{0B}' | '\u{0C}' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

/// The names a collating symbol may spell a character by (`[.space.]`): the
/// portable character set's (POSIX XBD 6.1) and the aliases bash accepts. A
/// single character needs no name (`[.a.]`).
const CHARACTER_NAMES: &[(&str, char)] = &[
    ("NUL", '\0'),
    ("SOH", '\u{01}'),
    ("STX", '\u{02}'),
    ("ETX", '\u{03}'),
    ("EOT", '\u{04}'),
    ("ENQ", '\u{05}'),
    ("ACK", '\u{06}'),
    ("alert", '\u{07}'),
    ("BEL", '\u{07}'),
    ("BS", '\u{08}'),
    ("backspace", '\u{08}'),
    ("HT", '\t'),
    ("tab", '\t'),
    ("LF", '\n'),
    ("newline", '\n'),
    ("VT", '\u{0B}'),
    ("vertical-tab", '\u{0B}'),
    ("FF", '\u{0C}'),
    ("form-feed", '\u{0C}'),
    ("CR", '\r'),
    ("carriage-return", '\r'),
    ("SO", '\u{0E}'),
    ("SI", '\u{0F}'),
    ("DLE", '\u{10}'),
    ("DC1", '\u{11}'),
    ("DC2", '\u{12}'),
    ("DC3", '\u{13}'),
    ("DC4", '\u{14}'),
    ("NAK", '\u{15}'),
    ("SYN", '\u{16}'),
    ("ETB", '\u{17}'),
    ("CAN", '\u{18}'),
    ("EM", '\u{19}'),
    ("SUB", '\u{1A}'),
    ("ESC", '\u{1B}'),
    ("IS4", '\u{1C}'),
    ("FS", '\u{1C}'),
    ("IS3", '\u{1D}'),
    ("GS", '\u{1D}'),
    ("IS2", '\u{1E}'),
    ("RS", '\u{1E}'),
    ("IS1", '\u{1F}'),
    ("US", '\u{1F}'),
    ("space", ' '),
    ("exclamation-mark", '!'),
    ("quotation-mark", '"'),
    ("number-sign", '#'),
    ("dollar-sign", '$'),
    ("percent-sign", '%'),
    ("ampersand", '&'),
    ("apostrophe", '\''),
    ("left-parenthesis", '('),
    ("right-parenthesis", ')'),
    ("asterisk", '*'),
    ("plus-sign", '+'),
    ("comma", ','),
    ("hyphen", '-'),
    ("hyphen-minus", '-'),
    ("period", '.'),
    ("full-stop", '.'),
    ("slash", '/'),
    ("solidus", '/'),
    ("zero", '0'),
    ("one", '1'),
    ("two", '2'),
    ("three", '3'),
    ("four", '4'),
    ("five", '5'),
    ("six", '6'),
    ("seven", '7'),
    ("eight", '8'),
    ("nine", '9'),
    ("colon", ':'),
    ("semicolon", ';'),
    ("less-than-sign", '<'),
    ("equals-sign", '='),
    ("greater-than-sign", '>'),
    ("question-mark", '?'),
    ("commercial-at", '@'),
    ("left-square-bracket", '['),
    ("backslash", '\\'),
    ("reverse-solidus", '\\'),
    ("right-square-bracket", ']'),
    ("circumflex", '^'),
    ("circumflex-accent", '^'),
    ("underscore", '_'),
    ("low-line", '_'),
    ("grave-accent", '`'),
    ("left-brace", '{'),
    ("left-curly-bracket", '{'),
    ("vertical-line", '|'),
    ("right-brace", '}'),
    ("right-curly-bracket", '}'),
    ("tilde", '~'),
    ("DEL", '\u{7F}'),
];

/// Whether `body` spells `name` with no escaped character: a class or
/// character name is always written plainly.
fn spells(body: &[Mark], name: &str) -> bool {
    body.len() == name.chars().count()
        && body
            .iter()
            .zip(name.chars())
            .all(|(m, c)| !m.literal && m.ch == c)
}

/// One member of a bracket expression.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Item {
    /// A single character, case-folded when matching ignores case.
    Char(char),
    /// An inclusive code-point range `lo..=hi`, its endpoints case-folded
    /// when matching ignores case.
    Range(char, char),
    /// A named class, tested on the candidate character as written.
    Class(CharClass),
}

/// A compiled bracket expression.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Bracket {
    negated: bool,
    items: Vec<Item>,
}

impl Bracket {
    /// Whether the candidate character — `raw` as written, `folded` as
    /// compared — is accepted. Classes test `raw`, so `[[:upper:]]` keeps
    /// its meaning when matching ignores case, as bash's does.
    pub(crate) fn contains(&self, raw: char, folded: char) -> bool {
        let hit = self.items.iter().any(|item| match *item {
            Item::Char(c) => c == folded,
            Item::Range(lo, hi) => lo <= folded && folded <= hi,
            Item::Class(class) => class.contains(raw),
        });
        hit != self.negated
    }
}

/// One element of a bracket expression.
enum Element {
    /// A plain or escaped member, or a collating symbol.
    Char(char),
    /// `[:name:]`.
    Class(CharClass),
    /// `[=c=]`, which in a code-point collation names exactly `c`.
    Equivalent(char),
}

/// Parse the bracket expression opening at `input[0] == '['`.
///
/// `None` when no `]` closes it: the `[` is then an ordinary character, as
/// POSIX specifies. Otherwise the expression and the marks it spans, or the
/// fault a complete expression holds — so a bad range inside an expression
/// that never closes cannot refuse a pattern POSIX reads literally.
pub(crate) fn parse(
    input: &[Mark],
    case_insensitive: bool,
) -> Option<Result<(Bracket, usize), GlobError>> {
    let fold_if = |c: char| if case_insensitive { fold(c) } else { c };
    let mut at = 1;
    let negated = input
        .get(at)
        .is_some_and(|m| !m.literal && matches!(m.ch, '!' | '^'));
    if negated {
        at += 1;
    }
    let first = at;
    let mut items = Vec::new();
    let mut fault = None;
    loop {
        let mark = *input.get(at)?;
        if mark.ch == ']' && !mark.literal && at > first {
            return Some(fault.map_or(Ok((Bracket { negated, items }, at + 1)), Err));
        }
        let (element, width) = element_at(&input[at..]);
        at += width;
        if items.len() >= MAX_CLASS_ITEMS {
            fault.get_or_insert(GlobError::TooManyTokens);
            continue;
        }
        match element {
            Err(err) => {
                fault.get_or_insert(err);
            }
            Ok(Element::Class(class)) => items.push(Item::Class(class)),
            Ok(Element::Equivalent(c)) => items.push(Item::Char(fold_if(c))),
            Ok(Element::Char(lo)) => match range_end(&input[at..]) {
                Some((hi, width)) => {
                    at += width;
                    if hi < lo {
                        fault.get_or_insert(GlobError::ReversedRange);
                    } else {
                        items.push(Item::Range(fold_if(lo), fold_if(hi)));
                    }
                }
                None => items.push(Item::Char(fold_if(lo))),
            },
        }
    }
}

/// The endpoint of a range whose `-` sits at `input[0]`, and the marks the
/// `-` and endpoint span. `None` when this is no range: no unescaped `-`, a
/// `-` just before the closing `]` (a literal member), or an endpoint that is
/// not a single character.
fn range_end(input: &[Mark]) -> Option<(char, usize)> {
    let dash = input.first()?;
    let next = input.get(1)?;
    if dash.literal || dash.ch != '-' || (!next.literal && next.ch == ']') {
        return None;
    }
    match element_at(&input[1..]) {
        (Ok(Element::Char(hi)), width) => Some((hi, width + 1)),
        _ => None,
    }
}

/// Read one bracket element at `input[0]` and its width in marks. A `[`
/// opens a `[:…:]`, `[=…=]` or `[.….]` element only when its terminator
/// follows; otherwise it is an ordinary member.
fn element_at(input: &[Mark]) -> (Result<Element, GlobError>, usize) {
    let head = input[0];
    let kind = input.get(1).filter(|m| !m.literal).map(|m| m.ch);
    if let (false, '[', Some(kind @ (':' | '=' | '.'))) = (head.literal, head.ch, kind) {
        if let Some(end) = terminator(input, kind) {
            let body = &input[2..end];
            let element = match kind {
                ':' => CLASSES
                    .iter()
                    .find(|(name, _)| spells(body, name))
                    .map(|&(_, class)| Element::Class(class))
                    .ok_or(GlobError::UnknownClass),
                '=' => single(body)
                    .map(Element::Equivalent)
                    .ok_or(GlobError::UnknownCollatingElement),
                _ => single(body)
                    .or_else(|| {
                        CHARACTER_NAMES
                            .iter()
                            .find(|(name, _)| spells(body, name))
                            .map(|&(_, ch)| ch)
                    })
                    .map(Element::Char)
                    .ok_or(GlobError::UnknownCollatingElement),
            };
            return (element, end + 2);
        }
    }
    (Ok(Element::Char(head.ch)), 1)
}

/// The one character a one-mark body holds.
fn single(body: &[Mark]) -> Option<char> {
    match body {
        [only] => Some(only.ch),
        _ => None,
    }
}

/// The index of the `kind` mark of the `kind]` pair closing an element
/// opened at `input[0..2]`.
fn terminator(input: &[Mark], kind: char) -> Option<usize> {
    (2..input.len().saturating_sub(1)).find(|&at| {
        let (close, bracket) = (input[at], input[at + 1]);
        !close.literal && close.ch == kind && !bracket.literal && bracket.ch == ']'
    })
}
