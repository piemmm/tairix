//! Brace expansion: `a{b,c}d` → `abd acd`, `{1..5}`, `{a..e..2}`, as bash
//! performs it, on the word as written and before any other expansion.
//!
//! Only unquoted text is brace syntax: a quoted or escaped `{`, `,`, `}` or
//! `..` is ordinary, and quoted text inside an alternative stays quoted in
//! every word it lands in. A `{` that opens no valid expression is literal
//! and the search moves past it, so `{{a,b}` gives `{a {b`. A `${…}` is a
//! parameter expansion and is skipped whole.

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use crate::error::ParseError;
use crate::expand::Budget;
use crate::lexer::{Segment, Word};

/// How a character of a word was quoted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Unquoted,
    Literal,
    Double,
}

impl Kind {
    /// How `segment` was quoted, and its text.
    fn of(segment: &Segment) -> (Self, &str) {
        match segment {
            Segment::Expandable(text) => (Self::Unquoted, text.as_str()),
            Segment::Literal(text) => (Self::Literal, text.as_str()),
            Segment::QuotedExpandable(text) => (Self::Double, text.as_str()),
        }
    }

    /// A segment of `text` quoted this way.
    fn segment(self, text: String) -> Segment {
        match self {
            Self::Unquoted => Segment::Expandable(text),
            Self::Literal => Segment::Literal(text),
            Self::Double => Segment::QuotedExpandable(text),
        }
    }
}

/// One character of a word, or (`ch: None`) an empty quoted run, which marks
/// the word quoted though it adds no text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Unit {
    ch: Option<char>,
    kind: Kind,
}

impl Unit {
    fn is(self, c: char) -> bool {
        self.kind == Kind::Unquoted && self.ch == Some(c)
    }
}

/// Expand `word`'s braces into the words it stands for, in order.
pub(crate) fn expand(word: &Word, budget: &mut Budget) -> Result<Vec<Word>, ParseError> {
    let opens_brace = word.iter().any(|segment| match segment {
        Segment::Expandable(text) => text.contains('{'),
        Segment::Literal(_) | Segment::QuotedExpandable(_) => false,
    });
    if !opens_brace {
        return Ok(alloc::vec![word.clone()]);
    }
    let units = flatten(word);
    let scanner = Scanner {
        closes: closes(&units),
        units: &units,
    };
    scanner.expand(0..units.len(), budget)
}

/// For each unquoted `{`, the index of the unquoted `}` that closes it.
///
/// Matching every brace once up front is what keeps a scan linear: with it,
/// skipping a group or a `${…}` is one step, never a rescan of what it holds.
fn closes(units: &[Unit]) -> Vec<Option<usize>> {
    let mut closes = alloc::vec![None; units.len()];
    let mut open = Vec::new();
    for (at, unit) in units.iter().enumerate() {
        if unit.is('{') {
            open.push(at);
        } else if unit.is('}') {
            if let Some(start) = open.pop() {
                closes[start] = Some(at);
            }
        }
    }
    closes
}

fn flatten(word: &Word) -> Vec<Unit> {
    let mut units = Vec::new();
    for segment in word {
        let (kind, text) = Kind::of(segment);
        if text.is_empty() {
            units.push(Unit { ch: None, kind });
        }
        units.extend(text.chars().map(|ch| Unit { ch: Some(ch), kind }));
    }
    units
}

/// The word `units` spell, each run of one quoting a segment, as the lexer
/// spells it, charged at what it holds.
fn spell(units: &[Unit], budget: &mut Budget) -> Result<Word, ParseError> {
    let runs = units.chunk_by(|a, b| a.kind == b.kind);
    let mut word = budget.list(runs.clone().count())?;
    for run in runs {
        let Some(&Unit { kind, .. }) = run.first() else {
            continue;
        };
        let len: usize = run
            .iter()
            .filter_map(|unit| unit.ch)
            .map(char::len_utf8)
            .sum();
        let mut text = budget.string(len)?;
        text.extend(run.iter().filter_map(|unit| unit.ch));
        word.push(kind.segment(text));
    }
    Ok(word)
}

/// `parts` spelled one after another as one word, a run of one quoting that
/// crosses from one part into the next kept one segment, charged at what it
/// holds.
fn join(parts: [&[Segment]; 3], budget: &mut Budget) -> Result<Word, ParseError> {
    let pieces = parts.into_iter().flatten().map(Kind::of);
    let mut runs = 0;
    let mut last = None;
    for (kind, _) in pieces.clone() {
        if last != Some(kind) {
            runs += 1;
            last = Some(kind);
        }
    }
    let mut word = budget.list(runs)?;
    let mut pieces = pieces.peekable();
    while let Some((kind, first)) = pieces.next() {
        let more: usize = pieces
            .clone()
            .map_while(|(next, text)| (next == kind).then_some(text.len()))
            .sum();
        let mut text = budget.string(first.len() + more)?;
        text.push_str(first);
        while let Some((_, next)) = pieces.next_if(|(next, _)| *next == kind) {
            text.push_str(next);
        }
        word.push(kind.segment(text));
    }
    Ok(word)
}

/// A flattened word and its matched braces.
struct Scanner<'a> {
    units: &'a [Unit],
    closes: Vec<Option<usize>>,
}

impl Scanner<'_> {
    /// The words the units in `span` expand to. Every word made is charged
    /// to `budget` at what it holds — its text, its segments and its place
    /// in a list — the intermediate ones included, so neither the work nor
    /// the memory an expansion takes can outgrow the bound.
    fn expand(&self, span: Range<usize>, budget: &mut Budget) -> Result<Vec<Word>, ParseError> {
        let Some((open, close, alternatives)) = self.first_expression(span.start, span.end) else {
            let mut words = budget.list(1)?;
            words.push(spell(&self.units[span], budget)?);
            return Ok(words);
        };
        let joined = Joiner {
            preamble: spell(&self.units[span.start..open], budget)?,
            tails: self.expand(close + 1..span.end, budget)?,
        };
        let mut out = Vec::new();
        match alternatives {
            Alternatives::List(parts) => {
                for part in parts {
                    for middle in self.expand(part, budget)? {
                        joined.push(&middle, &mut out, budget)?;
                    }
                }
            }
            Alternatives::Sequence(sequence) => {
                for spelling in sequence.spellings() {
                    joined.push(&[Segment::Expandable(spelling)], &mut out, budget)?;
                }
            }
        }
        Ok(out)
    }

    /// The first valid brace expression from `at` to `end`: its `{` and `}`
    /// and what it offers.
    fn first_expression(&self, mut at: usize, end: usize) -> Option<(usize, usize, Alternatives)> {
        while at < end {
            if let Some(close) = self.parameter_end(at, end) {
                at = close + 1;
                continue;
            }
            if let Some(close) = self.closes[at] {
                let commas = self.commas(at, close);
                if !commas.is_empty() {
                    let mut parts = Vec::with_capacity(commas.len() + 1);
                    let mut from = at + 1;
                    for comma in commas {
                        parts.push(from..comma);
                        from = comma + 1;
                    }
                    parts.push(from..close);
                    return Some((at, close, Alternatives::List(parts)));
                }
                if let Some(sequence) = sequence(&self.units[at + 1..close]) {
                    return Some((at, close, Alternatives::Sequence(sequence)));
                }
            }
            at += 1;
        }
        None
    }

    /// The `}` closing a `${` that starts at `at`, before `end`.
    fn parameter_end(&self, at: usize, end: usize) -> Option<usize> {
        if self.units[at].is('$') && at + 1 < end && self.units[at + 1].is('{') {
            self.closes[at + 1]
        } else {
            None
        }
    }

    /// The top-level commas between the `{` at `open` and the `}` at
    /// `close`. A group nested between them is matched too, so it is
    /// stepped over whole.
    fn commas(&self, open: usize, close: usize) -> Vec<usize> {
        let mut commas = Vec::new();
        let mut at = open + 1;
        while at < close {
            if let Some(end) = self.closes[at] {
                at = end + 1;
                continue;
            }
            if self.units[at].is(',') {
                commas.push(at);
            }
            at += 1;
        }
        commas
    }
}

/// What surrounds a brace expression: the text before it, and the words the
/// text after it expands to.
struct Joiner {
    preamble: Word,
    tails: Vec<Word>,
}

impl Joiner {
    /// One word per tail, with `middle` between the preamble and the tail.
    fn push(
        &self,
        middle: &[Segment],
        out: &mut Vec<Word>,
        budget: &mut Budget,
    ) -> Result<(), ParseError> {
        for tail in &self.tails {
            let word = join([self.preamble.as_slice(), middle, tail.as_slice()], budget)?;
            budget.push(out, word)?;
        }
        Ok(())
    }
}

/// What one brace expression offers.
enum Alternatives {
    /// `{a,b,c}`: each alternative's span, itself brace-expanded.
    List(Vec<Range<usize>>),
    /// `{x..y[..step]}`.
    Sequence(Sequence),
}

/// A sequence expression, spelled one element at a time so its length is
/// charged as it is made rather than materialised first.
struct Sequence {
    from: i64,
    to: i64,
    step: u64,
    letters: bool,
    /// The zero-padded width of each number, or `0` for none.
    width: usize,
}

impl Sequence {
    fn spellings(&self) -> impl Iterator<Item = String> + '_ {
        steps(self.from, self.to, self.step).filter_map(move |n| {
            if self.letters {
                u32::try_from(n)
                    .ok()
                    .and_then(char::from_u32)
                    .map(String::from)
            } else {
                Some(pad(n, self.width))
            }
        })
    }
}

/// The sequence expression `x..y` or `x..y..step` `body` spells, every
/// character unquoted: integers (zero-padded when either endpoint is
/// written with a leading zero) or single letters. `None` when `body` is not
/// one, so its braces stay literal.
fn sequence(body: &[Unit]) -> Option<Sequence> {
    // Stopping at the first character no sequence can hold keeps rejecting
    // a long body cheap.
    let mut text = String::new();
    for unit in body {
        match unit {
            Unit {
                ch: Some(ch),
                kind: Kind::Unquoted,
            } if ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.') => text.push(*ch),
            _ => return None,
        }
    }
    let mut parts = text.split("..");
    let (lhs, rhs, step) = (parts.next()?, parts.next()?, parts.next());
    if parts.next().is_some() {
        return None;
    }
    let step = match step {
        Some(step) => step.parse::<i64>().ok()?.unsigned_abs().max(1),
        None => 1,
    };
    if let (Some(from), Some(to)) = (letter(lhs), letter(rhs)) {
        return Some(Sequence {
            from: i64::from(u32::from(from)),
            to: i64::from(u32::from(to)),
            step,
            letters: true,
            width: 0,
        });
    }
    let width = if padded(lhs) || padded(rhs) {
        lhs.len().max(rhs.len())
    } else {
        0
    };
    Some(Sequence {
        from: integer(lhs)?,
        to: integer(rhs)?,
        step,
        letters: false,
        width,
    })
}

fn letter(text: &str) -> Option<char> {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphabetic() => Some(c),
        _ => None,
    }
}

/// A decimal integer with an optional sign, as a sequence endpoint.
fn integer(text: &str) -> Option<i64> {
    let digits = text.strip_prefix(['-', '+']).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.strip_prefix('+').unwrap_or(text).parse().ok()
}

/// Whether an endpoint is written with a leading zero (`05`, `-03`), which
/// asks for every number to be padded to the widest endpoint.
fn padded(text: &str) -> bool {
    let digits = text.strip_prefix(['-', '+']).unwrap_or(text);
    digits.len() > 1 && digits.starts_with('0')
}

fn pad(n: i64, width: usize) -> String {
    if n < 0 {
        alloc::format!("-{:0>w$}", n.unsigned_abs(), w = width.saturating_sub(1))
    } else {
        alloc::format!("{n:0>width$}")
    }
}

/// `from` to `to` inclusive in steps of `step`, ascending or descending.
fn steps(from: i64, to: i64, step: u64) -> impl Iterator<Item = i64> {
    let descending = from > to;
    let span = from.abs_diff(to);
    (0..=span / step).map_while(move |k| {
        let offset = i64::try_from(k.checked_mul(step)?).ok()?;
        if descending {
            from.checked_sub(offset)
        } else {
            from.checked_add(offset)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::expand;
    use crate::error::ParseError;
    use crate::expand::Budget;
    use crate::lexer::{tokenize, Segment, Token, Word};
    use alloc::string::String;
    use alloc::vec::Vec;

    fn word(line: &str) -> Word {
        match tokenize(line).expect("lexes").as_slice() {
            [Token::Word(word)] => word.clone(),
            other => panic!("one word: {other:?}"),
        }
    }

    fn flat(word: &Word) -> String {
        word.iter()
            .map(|segment| match segment {
                Segment::Literal(s) | Segment::Expandable(s) | Segment::QuotedExpandable(s) => {
                    s.as_str()
                }
            })
            .collect()
    }

    fn braces(line: &str) -> Vec<String> {
        expand(&word(line), &mut Budget::new(1 << 20))
            .expect("expands")
            .iter()
            .map(flat)
            .collect()
    }

    #[test]
    fn lists_expand_with_preamble_and_postscript() {
        assert_eq!(braces("{a,b}"), ["a", "b"]);
        assert_eq!(braces("x{a,b}y"), ["xay", "xby"]);
        assert_eq!(braces("{a,b}{1,2}"), ["a1", "a2", "b1", "b2"]);
        assert_eq!(braces("-{a,b}-"), ["-a-", "-b-"]);
        assert_eq!(braces("{a,b}={c,d}"), ["a=c", "a=d", "b=c", "b=d"]);
        assert_eq!(braces("{a,{b,c}}"), ["a", "b", "c"]);
        assert_eq!(braces("x{,}y"), ["xy", "xy"]);
        assert_eq!(braces("{,a}"), ["", "a"]);
    }

    #[test]
    fn integer_sequences() {
        assert_eq!(braces("{1..5}"), ["1", "2", "3", "4", "5"]);
        assert_eq!(braces("{5..1}"), ["5", "4", "3", "2", "1"]);
        assert_eq!(braces("{1..10..3}"), ["1", "4", "7", "10"]);
        assert_eq!(braces("{10..1..3}"), ["10", "7", "4", "1"]);
        assert_eq!(braces("{1..10..-3}"), ["1", "4", "7", "10"]);
        assert_eq!(braces("{1..2..0}"), ["1", "2"]);
        assert_eq!(braces("{+1..3}"), ["1", "2", "3"]);
        assert_eq!(braces("{-5..5..5}"), ["-5", "0", "5"]);
        assert_eq!(
            braces("{9223372036854775806..9223372036854775807}"),
            ["9223372036854775806", "9223372036854775807"]
        );
    }

    #[test]
    fn zero_padding_follows_the_widest_endpoint() {
        assert_eq!(braces("{05..10}"), ["05", "06", "07", "08", "09", "10"]);
        assert_eq!(
            braces("{5..010}"),
            ["005", "006", "007", "008", "009", "010"]
        );
        assert_eq!(braces("{-05..5..5}"), ["-05", "000", "005"]);
    }

    #[test]
    fn letter_sequences() {
        assert_eq!(braces("{a..e}"), ["a", "b", "c", "d", "e"]);
        assert_eq!(braces("{e..a..2}"), ["e", "c", "a"]);
        assert_eq!(braces("{a..Z}").len(), 8, "a down to Z by code point");
    }

    #[test]
    fn non_expressions_stay_literal() {
        for line in [
            "{a}",
            "{}",
            "{a..}",
            "{1..a}",
            "{1.5..3}",
            "{aa..cc}",
            "a,b}",
            "{a,b",
            "{1..99999999999999999999}",
        ] {
            assert_eq!(braces(line), [line], "{line}");
        }
        assert_eq!(braces("{{a,b}"), ["{a", "{b"]);
        assert_eq!(braces("{a,b}}"), ["a}", "b}"]);
    }

    #[test]
    fn quoting_disables_brace_syntax_but_survives_inside_alternatives() {
        assert_eq!(braces(r#""{a,b}""#), ["{a,b}"]);
        assert_eq!(braces("'{a,b}'"), ["{a,b}"]);
        assert_eq!(braces(r"\{a,b}"), ["{a,b}"]);
        assert_eq!(braces(r"{a\,b}"), ["{a,b}"]);
        assert_eq!(braces(r#"{"1..3"}"#), ["{1..3}"]);
        assert_eq!(braces(r#"{1.."3"}"#), ["{1..3}"]);
        let words = expand(&word(r#"{"a b",c}"#), &mut Budget::new(1024)).expect("expands");
        assert_eq!(words.len(), 2);
        assert_eq!(words[0], [Segment::QuotedExpandable("a b".into())]);
        assert_eq!(words[1], [Segment::Expandable("c".into())]);
    }

    #[test]
    fn a_parameter_expansion_is_not_a_brace_expression() {
        assert_eq!(braces("${x}"), ["${x}"]);
        assert_eq!(braces("{${x},y}"), ["${x}", "y"]);
        assert_eq!(braces("{$x,y}"), ["$x", "y"]);
    }

    #[test]
    fn an_empty_quoted_alternative_stays_quoted() {
        let words = expand(&word(r#"{"",a}"#), &mut Budget::new(1024)).expect("expands");
        assert_eq!(words[0], [Segment::QuotedExpandable(String::new())]);
    }

    #[test]
    fn a_scan_visits_each_character_a_bounded_number_of_times() {
        // At this length a scan revisiting what it had passed would not
        // finish: unclosed `${`, unclosed `{` and nesting that offers nothing.
        let n = 1 << 16;
        for text in [
            "${".repeat(n) + "a}",
            "{".repeat(n),
            "{".repeat(n) + "x" + &"}".repeat(n),
            "{1".repeat(n) + &"}".repeat(n),
        ] {
            let word = alloc::vec![Segment::Expandable(text.clone())];
            let words = expand(&word, &mut Budget::new(8 * n)).expect("expands");
            assert_eq!(words.iter().map(flat).collect::<Vec<_>>(), [text]);
        }
    }

    #[test]
    fn expansion_is_bounded() {
        assert_eq!(
            expand(&word("{1..100000000}"), &mut Budget::new(1 << 20)),
            Err(ParseError::ExpansionTooLarge)
        );
        assert_eq!(
            expand(
                &word("{a,b}{a,b}{a,b}{a,b}{a,b}{a,b}{a,b}{a,b}{a,b}{a,b}{a,b}{a,b}"),
                &mut Budget::new(4096)
            ),
            Err(ParseError::ExpansionTooLarge)
        );
    }
}
