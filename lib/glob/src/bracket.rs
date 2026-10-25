//! Bracket expressions (`[...]`): members, ranges, POSIX character classes,
//! equivalence classes and collating symbols (POSIX XBD 9.3.5, as the shell's
//! pattern matching notation uses them).

use alloc::vec;
use alloc::vec::Vec;
use core::cmp::Ordering;

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

    /// This class's bit in a [`Bracket`]'s set of classes.
    const fn bit(self) -> u16 {
        1 << (self as u16)
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
    ("minus", '-'),
    ("dash", '-'),
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

/// What a bracket expression's elements add up to as it is read.
#[derive(Default)]
struct Members {
    count: usize,
    ascii: u128,
    wide: Vec<(char, char)>,
    classes: u16,
}

impl Members {
    fn class(&mut self, class: CharClass) {
        self.count += 1;
        self.classes |= class.bit();
    }

    /// Add `lo..=hi`, its endpoints already case-folded when matching
    /// ignores case.
    fn range(&mut self, lo: char, hi: char) {
        self.count += 1;
        if lo.is_ascii() {
            let (from, to) = (u32::from(lo), u32::from(hi).min(0x7F));
            self.ascii |= (u128::MAX >> (127 - (to - from))) << from;
        }
        if !hi.is_ascii() {
            self.wide.push((lo.max('\u{80}'), hi));
        }
    }

    fn into_bracket(mut self, negated: bool) -> Bracket {
        self.wide.sort_unstable();
        self.wide.dedup_by(|later, kept| {
            let overlaps = later.0 <= kept.1;
            if overlaps {
                kept.1 = kept.1.max(later.1);
            }
            overlaps
        });
        Bracket {
            negated,
            ascii: self.ascii,
            wide: self.wide,
            classes: self.classes,
        }
    }
}

/// A compiled bracket expression, its characters case-folded when matching
/// ignores case: the ASCII ones a bit set, the rest sorted ranges that never
/// overlap, and its classes a bit set, so a test is a bit lookup, a binary
/// search and a check per class however many members it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Bracket {
    negated: bool,
    ascii: u128,
    wide: Vec<(char, char)>,
    classes: u16,
}

impl Bracket {
    /// The heap its wide ranges hold.
    pub(crate) fn heap_bytes(&self) -> usize {
        self.wide.capacity() * core::mem::size_of::<(char, char)>()
    }

    /// Whether the candidate character — `raw` as written, `folded` as
    /// compared — is accepted. Classes test `raw`, so `[[:upper:]]` keeps
    /// its meaning when matching ignores case, as bash's does.
    pub(crate) fn contains(&self, raw: char, folded: char) -> bool {
        let hit = self.holds(folded)
            || (self.classes != 0
                && CLASSES
                    .iter()
                    .any(|&(_, class)| self.classes & class.bit() != 0 && class.contains(raw)));
        hit != self.negated
    }

    fn holds(&self, c: char) -> bool {
        if c.is_ascii() {
            return (self.ascii >> u32::from(c)) & 1 == 1;
        }
        self.wide
            .binary_search_by(|&(lo, hi)| {
                if hi < c {
                    Ordering::Less
                } else if c < lo {
                    Ordering::Greater
                } else {
                    Ordering::Equal
                }
            })
            .is_ok()
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

/// What the bracket expressions of one pattern share, so parsing them all
/// costs time linear in its length, a run of unclosed `[` included: where
/// each `[:…:]`, `[=…=]` and `[.….]` element can close, where a `/` lies in
/// pathname mode, and the element positions a scan is known to read on from
/// with no closing `]`.
pub(crate) struct Brackets {
    /// For `:`, `=` and `.`, the nearest index at or after each that holds
    /// that mark followed by `]`, both unescaped.
    pairs: [Vec<usize>; 3],
    /// In pathname mode, the nearest index at or after each that holds a
    /// `/`, which no bracket expression spans; empty otherwise.
    slashes: Vec<usize>,
    /// Element positions from which a scan reads on unclosed, to the end or
    /// to a `/`: the scan from a position never depends on where it began,
    /// so a later scan reaching one knows its answer.
    open_ended: Vec<bool>,
    /// The positions the scan in progress has read, to mark if it fails.
    trail: Vec<usize>,
    #[cfg(test)]
    pub(crate) elements_read: usize,
}

/// No such index ahead.
const NONE_AHEAD: usize = usize::MAX;

impl Brackets {
    pub(crate) fn new(input: &[Mark], pathname: bool) -> Self {
        let nearest = |holds: &dyn Fn(usize) -> bool| {
            let mut next = vec![NONE_AHEAD; input.len() + 1];
            for at in (0..input.len()).rev() {
                next[at] = if holds(at) { at } else { next[at + 1] };
            }
            next
        };
        let closing = |kind: char| {
            nearest(&|at| {
                let mark = input[at];
                !mark.literal
                    && mark.ch == kind
                    && input.get(at + 1).is_some_and(|b| !b.literal && b.ch == ']')
            })
        };
        Self {
            pairs: [closing(':'), closing('='), closing('.')],
            slashes: if pathname {
                nearest(&|at| input[at].ch == '/')
            } else {
                Vec::new()
            },
            open_ended: vec![false; input.len() + 1],
            trail: Vec::new(),
            #[cfg(test)]
            elements_read: 0,
        }
    }

    /// The index of the `kind` mark of the `kind]` pair closing an element
    /// whose `[kind` opens at `open`; in pathname mode a pair past a `/`
    /// closes none.
    fn terminator(&self, kind: char, open: usize) -> Option<usize> {
        let table = match kind {
            ':' => &self.pairs[0],
            '=' => &self.pairs[1],
            _ => &self.pairs[2],
        };
        let body = open + 2;
        table
            .get(body)
            .copied()
            .filter(|&at| at != NONE_AHEAD && !self.holds_slash(body, at))
    }

    /// Whether `input[from..to]` holds a `/` in pathname mode.
    fn holds_slash(&self, from: usize, to: usize) -> bool {
        self.slashes.get(from).is_some_and(|&at| at < to)
    }
}

/// Parse the bracket expression opening at `input[start] == '['`, `input`
/// being the whole pattern `brackets` was built for.
///
/// `None` when no `]` closes it, or in pathname mode when a `/` comes first:
/// the `[` is then an ordinary character, as POSIX specifies. Otherwise the
/// expression and the marks it spans, or the fault a complete expression
/// holds — so a bad range inside an expression that never closes cannot
/// refuse a pattern POSIX reads literally.
pub(crate) fn parse(
    input: &[Mark],
    start: usize,
    brackets: &mut Brackets,
    case_insensitive: bool,
) -> Option<Result<(Bracket, usize), GlobError>> {
    let fold_if = |c: char| if case_insensitive { fold(c) } else { c };
    let mut at = start + 1;
    let negated = input
        .get(at)
        .is_some_and(|m| !m.literal && matches!(m.ch, '!' | '^'));
    if negated {
        at += 1;
    }
    let first = at;
    let mut members = Members::default();
    let mut fault = None;
    brackets.trail.clear();
    loop {
        if at > first {
            if brackets.open_ended.get(at).copied().unwrap_or(true) {
                return unclosed(brackets);
            }
            brackets.trail.push(at);
        }
        let Some(&mark) = input.get(at) else {
            return unclosed(brackets);
        };
        if mark.ch == ']' && !mark.literal && at > first {
            return Some(match fault {
                Some(err) => Err(err),
                None => Ok((members.into_bracket(negated), at + 1 - start)),
            });
        }
        #[cfg(test)]
        {
            brackets.elements_read += 1;
        }
        let (element, width) = element_at(input, at, brackets);
        if brackets.holds_slash(at, at + width) {
            return unclosed(brackets);
        }
        at += width;
        if members.count >= MAX_CLASS_ITEMS {
            fault.get_or_insert(GlobError::TooManyTokens);
            continue;
        }
        match element {
            Err(err) => {
                fault.get_or_insert(err);
            }
            Ok(Element::Class(class)) => members.class(class),
            Ok(Element::Equivalent(c)) => members.range(fold_if(c), fold_if(c)),
            Ok(Element::Char(lo)) => match range_end(input, at, brackets) {
                Some((hi, width)) => {
                    if brackets.holds_slash(at, at + width) {
                        return unclosed(brackets);
                    }
                    at += width;
                    // Folded first, as bash orders a range: `[a-Z]` is
                    // `[a-z]` when matching ignores case.
                    let (lo, hi) = (fold_if(lo), fold_if(hi));
                    if hi < lo {
                        fault.get_or_insert(GlobError::ReversedRange);
                    } else {
                        members.range(lo, hi);
                    }
                }
                None => members.range(fold_if(lo), fold_if(lo)),
            },
        }
    }
}

/// A scan that ended with no `]`, at the end or at a `/`: every position it
/// read leads there too.
fn unclosed(brackets: &mut Brackets) -> Option<Result<(Bracket, usize), GlobError>> {
    for &at in &brackets.trail {
        brackets.open_ended[at] = true;
    }
    None
}

/// The endpoint of a range whose `-` sits at `input[at]`, and the marks the
/// `-` and endpoint span. `None` when this is no range: no unescaped `-`, a
/// `-` just before the closing `]` (a literal member), or an endpoint that is
/// not a single character.
fn range_end(input: &[Mark], at: usize, brackets: &Brackets) -> Option<(char, usize)> {
    let dash = input.get(at)?;
    let next = input.get(at + 1)?;
    if dash.literal || dash.ch != '-' || (!next.literal && next.ch == ']') {
        return None;
    }
    match element_at(input, at + 1, brackets) {
        (Ok(Element::Char(hi)), width) => Some((hi, width + 1)),
        _ => None,
    }
}

/// Read one bracket element at `input[at]` and its width in marks. A `[`
/// opens a `[:…:]`, `[=…=]` or `[.….]` element only when its terminator
/// follows; otherwise it is an ordinary member.
fn element_at(
    input: &[Mark],
    at: usize,
    brackets: &Brackets,
) -> (Result<Element, GlobError>, usize) {
    let head = input[at];
    let kind = input.get(at + 1).filter(|m| !m.literal).map(|m| m.ch);
    if let (false, '[', Some(kind @ (':' | '=' | '.'))) = (head.literal, head.ch, kind) {
        if let Some(end) = brackets.terminator(kind, at) {
            let body = &input[at + 2..end];
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
            return (element, end + 2 - at);
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

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::{parse, Brackets};
    use crate::{Mark, MAX_CLASS_ITEMS};

    /// Mark strings over the characters bracket syntax turns on, each mark
    /// escaped now and then.
    fn random_marks(rng: &mut tairix_fuzzseed::Prng, most: usize) -> Vec<Mark> {
        const ALPHABET: [char; 9] = ['[', ']', ':', '=', '.', '-', '!', 'a', '/'];
        (0..rng.at_most(most))
            .map(|_| Mark {
                ch: ALPHABET[rng.below(ALPHABET.len())],
                literal: rng.below(8) == 0,
            })
            .collect()
    }

    fn opens(input: &[Mark]) -> Vec<usize> {
        (0..input.len())
            .filter(|&at| !input[at].literal && input[at].ch == '[')
            .collect()
    }

    #[test]
    fn the_terminator_table_finds_what_a_forward_scan_would() {
        let mut rng = tairix_fuzzseed::Prng::new(0x6272_6163);
        for _ in 0..2_000 {
            let input = random_marks(&mut rng, 24);
            for pathname in [false, true] {
                let table = Brackets::new(&input, pathname);
                for kind in [':', '=', '.'] {
                    for open in 0..input.len() {
                        // In pathname mode no element reaches past a `/`.
                        let scanned = (open + 2..input.len().saturating_sub(1))
                            .take_while(|&at| !pathname || input[at].ch != '/')
                            .find(|&at| {
                                let (close, bracket) = (input[at], input[at + 1]);
                                !close.literal
                                    && close.ch == kind
                                    && !bracket.literal
                                    && bracket.ch == ']'
                            });
                        assert_eq!(table.terminator(kind, open), scanned, "{kind} at {open}");
                    }
                }
            }
        }
    }

    /// What one scan learns of where scans end is shared with every later
    /// one, in any order, and changes no answer — a `/` ending a scan in
    /// pathname mode included.
    #[test]
    fn a_shared_table_parses_every_bracket_as_a_fresh_one_would() {
        let mut rng = tairix_fuzzseed::Prng::new(0x6d65_6d6f);
        for _ in 0..2_000 {
            let input = random_marks(&mut rng, 24);
            let mut positions = opens(&input);
            if rng.below(2) == 0 {
                positions.reverse();
            }
            let fold = rng.below(2) == 0;
            let pathname = rng.below(2) == 0;
            let mut shared = Brackets::new(&input, pathname);
            for start in positions {
                let alone = parse(&input, start, &mut Brackets::new(&input, pathname), fold);
                assert_eq!(parse(&input, start, &mut shared, fold), alone, "{start}");
            }
        }
    }

    /// Each unclosed `[` would otherwise re-read every element after it, so
    /// a long run of them, each over collating elements, cost cubic time.
    #[test]
    fn parsing_a_run_of_unclosed_brackets_is_linear() {
        const RUN: usize = 1000;
        let marks = |text: &str| -> Vec<Mark> {
            text.chars().map(|ch| Mark { ch, literal: false }).collect()
        };
        let tails = [
            ("[.ab.]".repeat(500), false),
            ("[:".repeat(1548), false),
            ("a/]".repeat(500), true),
        ];
        for (tail, pathname) in tails {
            let input = marks(&alloc::format!("{}{tail}", "[".repeat(RUN)));
            let mut brackets = Brackets::new(&input, pathname);
            for start in 0..RUN {
                assert_eq!(parse(&input, start, &mut brackets, false), None);
            }
            assert!(
                brackets.elements_read <= 2 * input.len(),
                "{} elements read for {} marks",
                brackets.elements_read,
                input.len()
            );
        }
    }

    /// A bracket keeps its characters where one lookup finds them — ASCII in
    /// a bit set, the rest as sorted ranges that never overlap — and accepts
    /// exactly what its members name, at every size it may reach.
    #[test]
    fn a_bracket_finds_any_member_by_one_lookup() {
        const POOL: [char; 12] = [
            '\0',
            '-',
            '0',
            'a',
            'z',
            '~',
            '\u{7f}',
            '\u{80}',
            'é',
            'λ',
            '\u{ffff}',
            '\u{10fffc}',
        ];
        let near = |c: char, step: i64| {
            u32::try_from(i64::from(u32::from(c)) + step)
                .ok()
                .and_then(char::from_u32)
        };
        let mut rng = tairix_fuzzseed::Prng::new(0x6269_7473);
        for _ in 0..200 {
            let negated = rng.below(2) == 0;
            let mut input = alloc::vec![Mark {
                ch: '[',
                literal: false
            }];
            if negated {
                input.push(Mark {
                    ch: '!',
                    literal: false,
                });
            }
            let mut members = Vec::new();
            for _ in 0..=rng.below(MAX_CLASS_ITEMS) {
                let mut spread = || {
                    let base = *rng.pick(&POOL);
                    near(base, i64::try_from(rng.below(3)).unwrap_or(0)).unwrap_or(base)
                };
                let (x, y) = (spread(), spread());
                let (lo, hi) = if rng.below(2) == 0 {
                    (x, x)
                } else {
                    (x.min(y), x.max(y))
                };
                input.push(Mark {
                    ch: lo,
                    literal: true,
                });
                if hi != lo {
                    input.push(Mark {
                        ch: '-',
                        literal: false,
                    });
                    input.push(Mark {
                        ch: hi,
                        literal: true,
                    });
                }
                members.push((lo, hi));
            }
            input.push(Mark {
                ch: ']',
                literal: false,
            });
            let Some(Ok((bracket, width))) =
                parse(&input, 0, &mut Brackets::new(&input, false), false)
            else {
                panic!("a complete expression: {input:?}");
            };
            assert_eq!(width, input.len());
            assert!(bracket
                .wide
                .iter()
                .all(|&(lo, hi)| !lo.is_ascii() && lo <= hi));
            assert!(bracket.wide.windows(2).all(|pair| pair[0].1 < pair[1].0));
            let probes = members
                .iter()
                .flat_map(|&(lo, hi)| [Some(lo), Some(hi), near(lo, -1), near(hi, 1)])
                .flatten()
                .chain(POOL);
            for probe in probes {
                let named = members.iter().any(|&(lo, hi)| lo <= probe && probe <= hi);
                assert_eq!(
                    bracket.contains(probe, probe),
                    named != negated,
                    "{probe:?}"
                );
            }
        }
    }
}
