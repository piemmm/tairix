//! The lexer: WGSL's text read as tokens. Template lists are found first, by
//! the specification's discovery algorithm over the text, and each list's
//! `<` and `>` become its own delimiters; then the text is read token by
//! token, the longest match first. Both passes skip blankspace, comments,
//! literals and names through one set of scanners, so they agree on where
//! every token begins.

use alloc::vec::Vec;
use core::fmt::Write as _;

use crate::diag::{error, push, Code, Outcome, Span};

/// What a token is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
    /// A name: an identifier, or a context-dependent name.
    Ident,
    /// An integer literal.
    Int,
    /// A floating point literal.
    Float,
    /// A reserved word.
    Reserved,
    Alias,
    Break,
    Case,
    Const,
    ConstAssert,
    Continue,
    Continuing,
    Default,
    Diagnostic,
    Discard,
    Else,
    Enable,
    False,
    Fn,
    For,
    If,
    Let,
    Loop,
    Override,
    Requires,
    Return,
    Struct,
    Switch,
    True,
    Var,
    While,
    /// `&`
    And,
    /// `&&`
    AndAnd,
    /// `->`
    Arrow,
    /// `@`
    At,
    /// `/`
    Slash,
    /// `!`
    Bang,
    /// `[`
    BracketLeft,
    /// `]`
    BracketRight,
    /// `{`
    BraceLeft,
    /// `}`
    BraceRight,
    /// `:`
    Colon,
    /// `,`
    Comma,
    /// `=`
    Equal,
    /// `==`
    EqualEqual,
    /// `!=`
    NotEqual,
    /// `>`
    Greater,
    /// `>=`
    GreaterEqual,
    /// `>>`
    ShiftRight,
    /// `<`
    Less,
    /// `<=`
    LessEqual,
    /// `<<`
    ShiftLeft,
    /// `%`
    Percent,
    /// `-`
    Minus,
    /// `--`
    MinusMinus,
    /// `.`
    Period,
    /// `+`
    Plus,
    /// `++`
    PlusPlus,
    /// `|`
    Or,
    /// `||`
    OrOr,
    /// `(`
    ParenLeft,
    /// `)`
    ParenRight,
    /// `;`
    Semicolon,
    /// `*`
    Star,
    /// `~`
    Tilde,
    /// `_`
    Underscore,
    /// `^`
    Xor,
    /// `+=`
    PlusEqual,
    /// `-=`
    MinusEqual,
    /// `*=`
    StarEqual,
    /// `/=`
    SlashEqual,
    /// `%=`
    PercentEqual,
    /// `&=`
    AndEqual,
    /// `|=`
    OrEqual,
    /// `^=`
    XorEqual,
    /// `>>=`
    ShiftRightEqual,
    /// `<<=`
    ShiftLeftEqual,
    /// The `<` opening a template list.
    TemplateStart,
    /// The `>` closing a template list.
    TemplateEnd,
    /// The end of the text.
    End,
}

/// One token: what it is and where.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Token {
    pub(crate) kind: Kind,
    pub(crate) span: Span,
}

/// The keywords, by spelling, ascending.
const KEYWORDS: [(&str, Kind); 26] = [
    ("alias", Kind::Alias),
    ("break", Kind::Break),
    ("case", Kind::Case),
    ("const", Kind::Const),
    ("const_assert", Kind::ConstAssert),
    ("continue", Kind::Continue),
    ("continuing", Kind::Continuing),
    ("default", Kind::Default),
    ("diagnostic", Kind::Diagnostic),
    ("discard", Kind::Discard),
    ("else", Kind::Else),
    ("enable", Kind::Enable),
    ("false", Kind::False),
    ("fn", Kind::Fn),
    ("for", Kind::For),
    ("if", Kind::If),
    ("let", Kind::Let),
    ("loop", Kind::Loop),
    ("override", Kind::Override),
    ("requires", Kind::Requires),
    ("return", Kind::Return),
    ("struct", Kind::Struct),
    ("switch", Kind::Switch),
    ("true", Kind::True),
    ("var", Kind::Var),
    ("while", Kind::While),
];

/// The reserved words, by spelling, ascending by byte.
const RESERVED: [&str; 146] = [
    "NULL",
    "Self",
    "abstract",
    "active",
    "alignas",
    "alignof",
    "as",
    "asm",
    "asm_fragment",
    "async",
    "attribute",
    "auto",
    "await",
    "become",
    "cast",
    "catch",
    "class",
    "co_await",
    "co_return",
    "co_yield",
    "coherent",
    "column_major",
    "common",
    "compile",
    "compile_fragment",
    "concept",
    "const_cast",
    "consteval",
    "constexpr",
    "constinit",
    "crate",
    "debugger",
    "decltype",
    "delete",
    "demote",
    "demote_to_helper",
    "do",
    "dynamic_cast",
    "enum",
    "explicit",
    "export",
    "extends",
    "extern",
    "external",
    "fallthrough",
    "filter",
    "final",
    "finally",
    "friend",
    "from",
    "fxgroup",
    "get",
    "goto",
    "groupshared",
    "highp",
    "impl",
    "implements",
    "import",
    "inline",
    "instanceof",
    "interface",
    "layout",
    "lowp",
    "macro",
    "macro_rules",
    "match",
    "mediump",
    "meta",
    "mod",
    "module",
    "move",
    "mut",
    "mutable",
    "namespace",
    "new",
    "nil",
    "noexcept",
    "noinline",
    "nointerpolation",
    "non_coherent",
    "noncoherent",
    "noperspective",
    "null",
    "nullptr",
    "of",
    "operator",
    "package",
    "packoffset",
    "partition",
    "pass",
    "patch",
    "pixelfragment",
    "precise",
    "precision",
    "premerge",
    "priv",
    "protected",
    "pub",
    "public",
    "readonly",
    "ref",
    "regardless",
    "register",
    "reinterpret_cast",
    "require",
    "resource",
    "restrict",
    "self",
    "set",
    "shared",
    "sizeof",
    "smooth",
    "snorm",
    "static",
    "static_assert",
    "static_cast",
    "std",
    "subroutine",
    "super",
    "target",
    "template",
    "this",
    "thread_local",
    "throw",
    "trait",
    "try",
    "type",
    "typedef",
    "typeid",
    "typename",
    "typeof",
    "union",
    "unless",
    "unorm",
    "unsafe",
    "unsized",
    "use",
    "using",
    "varying",
    "virtual",
    "volatile",
    "wgsl",
    "where",
    "with",
    "writeonly",
    "yield",
];

/// The punctuation, each spelling with what it is, longest first so a
/// longest match is the first found.
const PUNCTUATION: [(&[u8], Kind); 45] = [
    (b">>=", Kind::ShiftRightEqual),
    (b"<<=", Kind::ShiftLeftEqual),
    (b"&&", Kind::AndAnd),
    (b"->", Kind::Arrow),
    (b"==", Kind::EqualEqual),
    (b"!=", Kind::NotEqual),
    (b">=", Kind::GreaterEqual),
    (b">>", Kind::ShiftRight),
    (b"<=", Kind::LessEqual),
    (b"<<", Kind::ShiftLeft),
    (b"--", Kind::MinusMinus),
    (b"++", Kind::PlusPlus),
    (b"||", Kind::OrOr),
    (b"+=", Kind::PlusEqual),
    (b"-=", Kind::MinusEqual),
    (b"*=", Kind::StarEqual),
    (b"/=", Kind::SlashEqual),
    (b"%=", Kind::PercentEqual),
    (b"&=", Kind::AndEqual),
    (b"|=", Kind::OrEqual),
    (b"^=", Kind::XorEqual),
    (b"&", Kind::And),
    (b"@", Kind::At),
    (b"/", Kind::Slash),
    (b"!", Kind::Bang),
    (b"[", Kind::BracketLeft),
    (b"]", Kind::BracketRight),
    (b"{", Kind::BraceLeft),
    (b"}", Kind::BraceRight),
    (b":", Kind::Colon),
    (b",", Kind::Comma),
    (b"=", Kind::Equal),
    (b">", Kind::Greater),
    (b"<", Kind::Less),
    (b"%", Kind::Percent),
    (b"-", Kind::Minus),
    (b".", Kind::Period),
    (b"+", Kind::Plus),
    (b"|", Kind::Or),
    (b"(", Kind::ParenLeft),
    (b")", Kind::ParenRight),
    (b";", Kind::Semicolon),
    (b"*", Kind::Star),
    (b"~", Kind::Tilde),
    (b"^", Kind::Xor),
];

/// The shape of a numeric literal: whether it is a float, written in
/// hexadecimal, and the suffix it carries.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Shape {
    pub(crate) float: bool,
    pub(crate) hex: bool,
    pub(crate) suffix: Option<u8>,
}

/// The tokens of `text`, the last [`Kind::End`].
///
/// # Errors
///
/// A character no token begins with, an unterminated block comment, a
/// reserved word or a name spelled `_`-prefixed as no identifier may be, as
/// a stop naming its span; the allocator's refusal.
pub(crate) fn tokens(text: &str) -> Outcome<Vec<Token>> {
    let (mut starts, mut ends) = discover(text)?;
    starts.sort_unstable();
    ends.sort_unstable();
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let (mut start, mut end) = (0usize, 0usize);
    let mut at = 0usize;
    loop {
        at = skip(bytes, at)?;
        let here = offset(at);
        if at >= bytes.len() {
            push(
                &mut out,
                Token {
                    kind: Kind::End,
                    span: Span::new(here, here),
                },
            )?;
            return Ok(out);
        }
        while starts.get(start).is_some_and(|&s| s < here) {
            start += 1;
        }
        while ends.get(end).is_some_and(|&e| e < here) {
            end += 1;
        }
        let (kind, length) = if starts.get(start) == Some(&here) {
            start += 1;
            (Kind::TemplateStart, 1)
        } else if ends.get(end) == Some(&here) {
            end += 1;
            (Kind::TemplateEnd, 1)
        } else if let Some((length, shape)) = number(bytes, at) {
            let kind = if shape.float { Kind::Float } else { Kind::Int };
            (kind, length)
        } else if let Some(length) = name(text, at) {
            let spelled = text.get(at..at + length).unwrap_or("");
            (word(spelled, Span::new(here, offset(at + length)))?, length)
        } else if let Some((kind, length)) = punctuation(bytes, at, ends.get(end).copied()) {
            (kind, length)
        } else if bytes.get(at) == Some(&b'_') {
            (Kind::Underscore, 1)
        } else {
            let ch = text.get(at..).and_then(|rest| rest.chars().next());
            let span = Span::new(here, offset(at + ch.map_or(1, char::len_utf8)));
            return Err(error(Code::InvalidCharacter, span, |m| match ch {
                Some(ch) => write!(m, "no token begins with U+{:04X}", u32::from(ch)),
                None => write!(m, "no token begins here"),
            }));
        };
        push(
            &mut out,
            Token {
                kind,
                span: Span::new(here, offset(at + length)),
            },
        )?;
        at += length;
    }
}

/// What the name `spelled` at `span` is: a keyword, else an identifier.
fn word(spelled: &str, span: Span) -> Outcome<Kind> {
    if let Ok(found) = KEYWORDS.binary_search_by(|(k, _)| k.cmp(&spelled)) {
        return Ok(KEYWORDS[found].1);
    }
    if RESERVED.binary_search(&spelled).is_ok() {
        return Ok(Kind::Reserved);
    }
    if spelled.starts_with("__") {
        return Err(error(Code::InvalidName, span, |m| {
            m.write_str("a name must not begin with two underscores: ")?;
            m.quote(spelled)
        }));
    }
    Ok(Kind::Ident)
}

/// The punctuation at `at`, longest first — never past `before`, the next
/// template list's end, so `>>` closing two lists reads as two.
fn punctuation(bytes: &[u8], at: usize, before: Option<u32>) -> Option<(Kind, usize)> {
    let room = before
        .and_then(|end| usize::try_from(end).ok())
        .filter(|&end| end > at)
        .map_or(usize::MAX, |end| end - at);
    PUNCTUATION.iter().find_map(|&(spelled, kind)| {
        (spelled.len() <= room && bytes.get(at..)?.starts_with(spelled))
            .then_some((kind, spelled.len()))
    })
}

/// `at` as a token's offset: the text is no longer than a `u32` counts.
fn offset(at: usize) -> u32 {
    u32::try_from(at).unwrap_or(u32::MAX)
}

/// The first position from `at` past blankspace and comments.
fn skip(bytes: &[u8], mut at: usize) -> Outcome<usize> {
    loop {
        if let Some(length) = blank(bytes, at) {
            at += length;
        } else if let Some(length) = comment(bytes, at)? {
            at += length;
        } else {
            return Ok(at);
        }
    }
}

/// The length of the blankspace code point at `at`: `Pattern_White_Space`.
fn blank(bytes: &[u8], at: usize) -> Option<usize> {
    match bytes.get(at..)? {
        [b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r', ..] => Some(1),
        [0xc2, 0x85, ..] => Some(2),
        [0xe2, 0x80, 0x8e | 0x8f | 0xa8 | 0xa9, ..] => Some(3),
        _ => None,
    }
}

/// Whether the code point at `at` breaks a line.
fn line_break(bytes: &[u8], at: usize) -> bool {
    matches!(
        bytes.get(at..),
        Some([b'\n' | 0x0b | 0x0c | b'\r', ..] | [0xc2, 0x85, ..] | [0xe2, 0x80, 0xa8 | 0xa9, ..])
    )
}

/// The length of the comment at `at`, if one begins there: a line comment to
/// its line break, or a block comment, nested ones within it.
///
/// # Errors
///
/// A block comment never closed.
fn comment(bytes: &[u8], at: usize) -> Outcome<Option<usize>> {
    match bytes.get(at..at + 2) {
        Some(b"//") => {
            let mut end = at + 2;
            while end < bytes.len() && !line_break(bytes, end) {
                end += 1;
            }
            Ok(Some(end - at))
        }
        Some(b"/*") => {
            let mut depth = 1usize;
            let mut end = at + 2;
            while depth > 0 {
                match bytes.get(end..end + 2) {
                    Some(b"/*") => {
                        depth += 1;
                        end += 2;
                    }
                    Some(b"*/") => {
                        depth -= 1;
                        end += 2;
                    }
                    Some(_) => end += 1,
                    None => {
                        let span = Span::new(offset(at), offset(bytes.len()));
                        return Err(error(Code::UnterminatedComment, span, |m| {
                            m.write_str("a block comment is never closed")
                        }));
                    }
                }
            }
            Ok(Some(end - at))
        }
        _ => Ok(None),
    }
}

/// The run of bytes from `at` that `pick` takes.
fn run(bytes: &[u8], at: usize, pick: fn(u8) -> bool) -> usize {
    bytes
        .get(at..)
        .map_or(0, |rest| rest.iter().take_while(|&&b| pick(b)).count())
}

/// The length of the exponent at `at` — `marker`, an optional sign, then
/// decimal digits — if one is there.
fn exponent(bytes: &[u8], at: usize, marker: [u8; 2]) -> Option<usize> {
    if !bytes.get(at).is_some_and(|b| marker.contains(b)) {
        return None;
    }
    let sign = usize::from(matches!(bytes.get(at + 1), Some(b'+' | b'-')));
    let digits = run(bytes, at + 1 + sign, |b| b.is_ascii_digit());
    (digits > 0).then_some(1 + sign + digits)
}

/// The longest numeric literal at `at`, if one begins there, and its shape.
pub(crate) fn number(bytes: &[u8], at: usize) -> Option<(usize, Shape)> {
    let first = *bytes.get(at)?;
    let fractional = first == b'.' && bytes.get(at + 1)?.is_ascii_digit();
    if !(first.is_ascii_digit() || fractional) {
        return None;
    }
    if first == b'0' && matches!(bytes.get(at + 1), Some(b'x' | b'X')) {
        if let Some(found) = hex_number(bytes, at) {
            return Some(found);
        }
    }
    let whole = run(bytes, at, |b| b.is_ascii_digit());
    let mut end = at + whole;
    let mut float = false;
    if bytes.get(end) == Some(&b'.') {
        let fraction = run(bytes, end + 1, |b| b.is_ascii_digit());
        if whole > 0 || fraction > 0 {
            end += 1 + fraction;
            float = true;
        }
    }
    if let Some(length) = exponent(bytes, end, *b"eE") {
        end += length;
        float = true;
    }
    if !float && whole > 1 && first == b'0' {
        // A leading zero begins no longer integer: `0` alone is the literal.
        end = at + 1;
    }
    let suffix = match bytes.get(end) {
        Some(&s @ (b'f' | b'h')) => {
            float = true;
            end += 1;
            Some(s)
        }
        Some(&s @ (b'i' | b'u')) if !float => {
            end += 1;
            Some(s)
        }
        _ => None,
    };
    Some((
        end - at,
        Shape {
            float,
            hex: false,
            suffix,
        },
    ))
}

/// The longest hexadecimal literal at `at`, which begins `0x`, if digits or
/// a point and digits follow.
fn hex_number(bytes: &[u8], at: usize) -> Option<(usize, Shape)> {
    let hex = |b: u8| b.is_ascii_hexdigit();
    let whole = run(bytes, at + 2, hex);
    let mut end = at + 2 + whole;
    let mut float = false;
    if bytes.get(end) == Some(&b'.') {
        let fraction = run(bytes, end + 1, hex);
        if whole > 0 || fraction > 0 {
            end += 1 + fraction;
            float = true;
        }
    }
    if whole == 0 && !float {
        return None;
    }
    let mut suffix = None;
    if let Some(length) = exponent(bytes, end, *b"pP") {
        end += length;
        float = true;
        if let Some(&s @ (b'f' | b'h')) = bytes.get(end) {
            end += 1;
            suffix = Some(s);
        }
    } else if !float {
        if let Some(&s @ (b'i' | b'u')) = bytes.get(end) {
            end += 1;
            suffix = Some(s);
        }
    }
    Some((
        end - at,
        Shape {
            float,
            hex: true,
            suffix,
        },
    ))
}

/// The length of the name at `at`, if one begins there: an `XID_Start` or
/// `_` then `XID_Continue` code points, but never `_` alone.
pub(crate) fn name(text: &str, at: usize) -> Option<usize> {
    let rest = text.get(at..)?;
    let mut chars = rest.char_indices();
    let (_, first) = chars.next()?;
    if !(first == '_' || tairix_unicode::is_xid_start(first)) {
        return None;
    }
    let length = chars
        .find(|&(_, ch)| !tairix_unicode::is_xid_continue(ch))
        .map_or(rest.len(), |(index, _)| index);
    (first != '_' || length > 1).then_some(length)
}

/// The template lists of `text`, as the specification's discovery algorithm
/// finds them: the offsets of each list's opening `<` and of its closing `>`.
fn discover(text: &str) -> Outcome<(Vec<u32>, Vec<u32>)> {
    let bytes = text.as_bytes();
    let mut starts = Vec::new();
    let mut ends = Vec::new();
    // Each unclosed `<`, with the nesting depth it opened at.
    let mut pending: Vec<(u32, u32)> = Vec::new();
    let mut depth = 0u32;
    let mut at = 0usize;
    loop {
        loop {
            at = skip(bytes, at)?;
            match number(bytes, at) {
                Some((length, _)) => at += length,
                None => break,
            }
        }
        let Some(&here) = bytes.get(at) else {
            return Ok((starts, ends));
        };
        if let Some(length) = name(text, at) {
            at = skip(bytes, at + length)?;
            if bytes.get(at) == Some(&b'<') {
                push(&mut pending, (offset(at), depth))?;
                at += 1;
                // No template parameter begins with `<` or `=`: these were
                // `<<` and `<=`.
                if matches!(bytes.get(at), Some(b'<' | b'=')) {
                    pending.pop();
                    at += 1;
                }
            }
            continue;
        }
        let next = bytes.get(at + 1).copied();
        match here {
            b'>' => {
                if let Some(&(opened, open_depth)) = pending.last() {
                    if open_depth == depth {
                        push(&mut starts, opened)?;
                        push(&mut ends, offset(at))?;
                        pending.pop();
                        at += 1;
                        continue;
                    }
                }
                at += if next == Some(b'=') { 2 } else { 1 };
            }
            b'(' | b'[' => {
                depth = depth.saturating_add(1);
                at += 1;
            }
            b')' | b']' => {
                while pending.last().is_some_and(|&(_, d)| d >= depth) {
                    pending.pop();
                }
                depth = depth.saturating_sub(1);
                at += 1;
            }
            b'!' => at += if next == Some(b'=') { 2 } else { 1 },
            b'=' => {
                if next == Some(b'=') {
                    at += 2;
                } else {
                    // An assignment: no expression, so no template list,
                    // spans it.
                    depth = 0;
                    pending.clear();
                    at += 1;
                }
            }
            b';' | b'{' | b':' => {
                depth = 0;
                pending.clear();
                at += 1;
            }
            b'&' | b'|' if next == Some(here) => {
                // Below comparison in precedence: no list spans them at this
                // depth.
                while pending.last().is_some_and(|&(_, d)| d >= depth) {
                    pending.pop();
                }
                at += 2;
            }
            _ => {
                at += text
                    .get(at..)
                    .and_then(|rest| rest.chars().next())
                    .map_or(1, char::len_utf8);
            }
        }
    }
}

#[cfg(test)]
#[path = "lex_tests.rs"]
mod tests;
