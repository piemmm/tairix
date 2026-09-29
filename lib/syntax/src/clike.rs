//! The C family: Rust, C, Java and JavaScript share one scanner and differ
//! only in the table that describes each.

use tairix_theme::SyntaxRole;

use crate::lex::{
    ascending, at, char_end, continues, escape_end, find, ident_end, is_ident_continue,
    is_ident_start, listed, number_end, skip_space, starts_at, Emit, LineState,
};

/// What distinguishes one member of the family.
pub(crate) struct Lang {
    keywords: &'static [&'static str],
    types: &'static [&'static str],
    literals: &'static [&'static str],
    traits: Traits,
}

impl Lang {
    /// Whether every word table is [`ascending`].
    const fn sorted(&self) -> bool {
        ascending(self.keywords) && ascending(self.types) && ascending(self.literals)
    }
}

/// The lexical traits a member of the family has.
#[derive(Copy, Clone)]
struct Traits(u16);

impl Traits {
    /// Block comments nest (Rust).
    const NESTED_COMMENTS: Self = Self(1);
    /// A `#` opening a line begins a preprocessor directive (C).
    const PREPROCESSOR: Self = Self(1 << 1);
    /// `#[…]` and `#![…]` are attributes (Rust).
    const ATTRIBUTES: Self = Self(1 << 2);
    /// `'x'` is a character literal rather than a string.
    const CHARS: Self = Self(1 << 3);
    /// `'a` is a lifetime (Rust).
    const LIFETIMES: Self = Self(1 << 4);
    /// `'…'` is a string (JavaScript).
    const SINGLE_QUOTED: Self = Self(1 << 5);
    /// `` `…` `` is a template string that may span lines (JavaScript).
    const TEMPLATES: Self = Self(1 << 6);
    /// A `"…"` string may span lines without a continuation (Rust).
    const MULTILINE_STRINGS: Self = Self(1 << 7);
    /// `r"…"`, `r#"…"#` and their byte forms (Rust).
    const RAW_STRINGS: Self = Self(1 << 8);
    /// `"""…"""` text blocks (Java).
    const TEXT_BLOCKS: Self = Self(1 << 9);
    /// `name!` is a macro invocation (Rust).
    const MACROS: Self = Self(1 << 10);
    /// A keyword may join two words with a hyphen (Java's `non-sealed`).
    const HYPHENATED: Self = Self(1 << 11);

    const fn and(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    const fn has(self, wanted: Self) -> bool {
        self.0 & wanted.0 != 0
    }
}

pub(crate) const RUST: Lang = Lang {
    keywords: &[
        "Self", "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else",
        "enum", "extern", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
        "mut", "pub", "ref", "return", "self", "static", "struct", "super", "trait", "type",
        "union", "unsafe", "use", "where", "while", "yield",
    ],
    types: &[
        "bool", "char", "f32", "f64", "i128", "i16", "i32", "i64", "i8", "isize", "str", "u128",
        "u16", "u32", "u64", "u8", "usize",
    ],
    literals: &["false", "true"],
    traits: Traits::NESTED_COMMENTS
        .and(Traits::ATTRIBUTES)
        .and(Traits::CHARS)
        .and(Traits::LIFETIMES)
        .and(Traits::MULTILINE_STRINGS)
        .and(Traits::RAW_STRINGS)
        .and(Traits::MACROS),
};

pub(crate) const C: Lang = Lang {
    keywords: &[
        "_Alignas",
        "_Alignof",
        "_Atomic",
        "_Generic",
        "_Noreturn",
        "_Static_assert",
        "_Thread_local",
        "auto",
        "break",
        "case",
        "const",
        "continue",
        "default",
        "do",
        "else",
        "enum",
        "extern",
        "for",
        "goto",
        "if",
        "inline",
        "register",
        "restrict",
        "return",
        "sizeof",
        "static",
        "struct",
        "switch",
        "typedef",
        "union",
        "volatile",
        "while",
    ],
    types: &[
        "_Bool",
        "_Complex",
        "bool",
        "char",
        "double",
        "float",
        "int",
        "int16_t",
        "int32_t",
        "int64_t",
        "int8_t",
        "intptr_t",
        "long",
        "ptrdiff_t",
        "short",
        "signed",
        "size_t",
        "ssize_t",
        "uint16_t",
        "uint32_t",
        "uint64_t",
        "uint8_t",
        "uintptr_t",
        "unsigned",
        "void",
    ],
    literals: &["NULL", "false", "true"],
    traits: Traits::PREPROCESSOR.and(Traits::CHARS),
};

pub(crate) const JAVA: Lang = Lang {
    keywords: &[
        "abstract",
        "assert",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "default",
        "do",
        "else",
        "enum",
        "extends",
        "final",
        "finally",
        "for",
        "goto",
        "if",
        "implements",
        "import",
        "instanceof",
        "interface",
        "native",
        "new",
        "non-sealed",
        "package",
        "permits",
        "private",
        "protected",
        "public",
        "record",
        "return",
        "sealed",
        "static",
        "strictfp",
        "super",
        "switch",
        "synchronized",
        "this",
        "throw",
        "throws",
        "transient",
        "try",
        "var",
        "volatile",
        "while",
        "yield",
    ],
    types: &[
        "boolean", "byte", "char", "double", "float", "int", "long", "short", "void",
    ],
    literals: &["false", "null", "true"],
    traits: Traits::CHARS
        .and(Traits::TEXT_BLOCKS)
        .and(Traits::HYPHENATED),
};

pub(crate) const JAVASCRIPT: Lang = Lang {
    keywords: &[
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "default",
        "delete",
        "do",
        "else",
        "export",
        "extends",
        "finally",
        "for",
        "from",
        "function",
        "get",
        "if",
        "import",
        "in",
        "instanceof",
        "let",
        "new",
        "of",
        "return",
        "set",
        "static",
        "super",
        "switch",
        "this",
        "throw",
        "try",
        "typeof",
        "var",
        "void",
        "while",
        "with",
        "yield",
    ],
    types: &[],
    literals: &["Infinity", "NaN", "false", "null", "true", "undefined"],
    traits: Traits::SINGLE_QUOTED.and(Traits::TEMPLATES),
};

const _: () = assert!(RUST.sorted() && C.sorted() && JAVA.sorted() && JAVASCRIPT.sorted());

/// What a line was left inside of, in the state word's low byte; the
/// construct's parameter (comment depth, raw-string hash count) above it.
const NORMAL: u32 = 0;
const BLOCK_COMMENT: u32 = 1;
const STRING: u32 = 2;
const RAW_STRING: u32 = 3;
const TEMPLATE: u32 = 4;
const DIRECTIVE: u32 = 5;
const TEXT_BLOCK: u32 = 6;

/// The deepest block-comment nesting a state word records; deeper nesting
/// is held at it, which only shortens how long the comment is believed.
const MAX_DEPTH: u32 = 0xff_ffff;

const fn pack(kind: u32, param: u32) -> LineState {
    LineState::from_raw(kind | (param << 8))
}

/// Classify one line of `lang`.
pub(crate) fn lex(lang: &Lang, state: LineState, line: &[u8], out: &mut Emit<'_>) -> LineState {
    let (mut kind, mut param) = (state.raw() & 0xff, state.raw() >> 8);
    let valid = match kind {
        NORMAL | STRING | DIRECTIVE => true,
        BLOCK_COMMENT => param > 0,
        RAW_STRING => lang.traits.has(Traits::RAW_STRINGS),
        TEMPLATE => lang.traits.has(Traits::TEMPLATES),
        TEXT_BLOCK => lang.traits.has(Traits::TEXT_BLOCKS),
        _ => false,
    };
    if !valid {
        (kind, param) = (NORMAL, 0);
    }
    let lead = skip_space(line, 0);
    let mut i = 0;
    loop {
        match kind {
            BLOCK_COMMENT => {
                let (end, depth) = block_comment(lang, line, i, param);
                out.push(i, end, SyntaxRole::Comment);
                i = end;
                if depth == 0 {
                    kind = NORMAL;
                } else {
                    return pack(BLOCK_COMMENT, depth);
                }
            }
            STRING => {
                let (end, closed) = quoted(line, i, b'"', out);
                i = end;
                if closed {
                    kind = NORMAL;
                } else if lang.traits.has(Traits::MULTILINE_STRINGS) || continues(line) {
                    return pack(STRING, 0);
                } else {
                    return LineState::START;
                }
            }
            RAW_STRING => {
                let Some(end) = raw_close(line, i, param) else {
                    out.push(i, line.len(), SyntaxRole::String);
                    return pack(RAW_STRING, param);
                };
                out.push(i, end, SyntaxRole::String);
                i = end;
                kind = NORMAL;
            }
            TEMPLATE => {
                let (end, closed) = quoted(line, i, b'`', out);
                i = end;
                if closed {
                    kind = NORMAL;
                } else {
                    return pack(TEMPLATE, 0);
                }
            }
            TEXT_BLOCK => {
                let Some(close) = find(line, i, b"\"\"\"") else {
                    out.push(i, line.len(), SyntaxRole::String);
                    return pack(TEXT_BLOCK, 0);
                };
                out.push(i, close + 3, SyntaxRole::String);
                i = close + 3;
                kind = NORMAL;
            }
            DIRECTIVE => {
                out.push(0, line.len(), SyntaxRole::Directive);
                return if continues(line) {
                    pack(DIRECTIVE, 0)
                } else {
                    LineState::START
                };
            }
            _ => match token(lang, line, i, lead, out) {
                Next::At(next) => i = next,
                Next::Enter(next_kind, next_param, next) => {
                    (kind, param, i) = (next_kind, next_param, next);
                }
                Next::Done => return LineState::START,
            },
        }
    }
}

/// What the scan does after one token.
enum Next {
    /// Carry on from this byte.
    At(usize),
    /// A multi-line construct opened: its kind, parameter, and where its body
    /// starts.
    Enter(u32, u32, usize),
    /// The line is finished and nothing is left open.
    Done,
}

/// Classify the token at `i`, in normal text; `lead` is the line's first
/// byte that is not white space.
fn token(lang: &Lang, line: &[u8], i: usize, lead: usize, out: &mut Emit<'_>) -> Next {
    let Some(&byte) = line.get(i) else {
        return Next::Done;
    };
    let next = at(line, i + 1);
    match byte {
        b' ' | b'\t' | b'\r' | 0x0b | 0x0c => Next::At(skip_space(line, i)),
        b'#' if lang.traits.has(Traits::PREPROCESSOR) && i == lead => Next::Enter(DIRECTIVE, 0, i),
        b'#' if lang.traits.has(Traits::ATTRIBUTES)
            && (next == b'[' || (next == b'!' && at(line, i + 2) == b'[')) =>
        {
            let end = bracket_close(line, i);
            out.push(i, end, SyntaxRole::Directive);
            Next::At(end)
        }
        b'/' if next == b'/' => {
            out.push(i, line.len(), SyntaxRole::Comment);
            Next::Done
        }
        b'/' if next == b'*' => {
            out.push(i, i + 2, SyntaxRole::Comment);
            Next::Enter(BLOCK_COMMENT, 1, i + 2)
        }
        b'"' if lang.traits.has(Traits::TEXT_BLOCKS) && starts_at(line, i, b"\"\"\"") => {
            out.push(i, i + 3, SyntaxRole::String);
            Next::Enter(TEXT_BLOCK, 0, i + 3)
        }
        b'"' => {
            out.push(i, i + 1, SyntaxRole::String);
            Next::Enter(STRING, 0, i + 1)
        }
        b'`' if lang.traits.has(Traits::TEMPLATES) => {
            out.push(i, i + 1, SyntaxRole::String);
            Next::Enter(TEMPLATE, 0, i + 1)
        }
        b'\'' if lang.traits.has(Traits::SINGLE_QUOTED) => {
            out.push(i, i + 1, SyntaxRole::String);
            let (end, _) = quoted(line, i + 1, b'\'', out);
            Next::At(end)
        }
        b'\'' if lang.traits.has(Traits::CHARS) => Next::At(char_or_lifetime(lang, line, i, out)),
        b'0'..=b'9' => {
            let end = number_end(line, i);
            out.push(i, end, SyntaxRole::Number);
            Next::At(end)
        }
        b'.' if next.is_ascii_digit() && !is_ident_continue(at(line, i.wrapping_sub(1))) => {
            let end = number_end(line, i + 1);
            out.push(i, end, SyntaxRole::Number);
            Next::At(end)
        }
        _ if is_ident_start(byte) => word(lang, line, i, out),
        _ if byte.is_ascii_punctuation() => {
            let mut end = i + 1;
            while end < line.len()
                && line[end].is_ascii_punctuation()
                && !matches!(line[end], b'"' | b'\'' | b'`' | b'#')
                && !(line[end] == b'/' && matches!(at(line, end + 1), b'/' | b'*'))
            {
                end += 1;
            }
            out.push(i, end, SyntaxRole::Punctuation);
            Next::At(end)
        }
        _ => Next::At(i + 1),
    }
}

/// Classify the identifier at `i`, or the prefixed string it opens.
fn word(lang: &Lang, line: &[u8], i: usize, out: &mut Emit<'_>) -> Next {
    let end = keyword_end(lang, line, i, ident_end(line, i));
    let text = &line[i..end];
    if lang.traits.has(Traits::RAW_STRINGS) {
        if let Some(entered) = prefixed_string(line, i, end, out) {
            return entered;
        }
    }
    let after = skip_space(line, end);
    let role = if listed(lang.keywords, text) || listed(lang.literals, text) {
        SyntaxRole::Keyword
    } else if listed(lang.types, text) {
        SyntaxRole::Type
    } else if lang.traits.has(Traits::MACROS)
        && at(line, end) == b'!'
        && matches!(at(line, end + 1), b'(' | b'[' | b'{')
    {
        out.push(i, end + 1, SyntaxRole::Function);
        return Next::At(end + 1);
    } else if at(line, after) == b'(' {
        SyntaxRole::Function
    } else if is_type_name(text) {
        SyntaxRole::Type
    } else {
        SyntaxRole::Plain
    };
    out.push(i, end, role);
    Next::At(end)
}

/// Where the word starting at `i` with the identifier ending at `end` ends:
/// past a hyphen and the identifier after it when the two spell one keyword.
fn keyword_end(lang: &Lang, line: &[u8], i: usize, end: usize) -> usize {
    if lang.traits.has(Traits::HYPHENATED)
        && at(line, end) == b'-'
        && is_ident_start(at(line, end + 1))
    {
        let joined = ident_end(line, end + 1);
        if listed(lang.keywords, &line[i..joined]) {
            return joined;
        }
    }
    end
}

/// A capitalised word with a lower-case letter in it: a type or a
/// constructor by the family's naming convention. An all-capitals word is
/// a constant, not a type.
fn is_type_name(word: &[u8]) -> bool {
    word.first().is_some_and(u8::is_ascii_uppercase) && word.iter().any(u8::is_ascii_lowercase)
}

/// Rust's `b"…"`, `b'…'`, `r"…"`, `r#"…"#`, `br"…"` and `rb"…"`, when the
/// word `line[i..end]` is one of their prefixes.
fn prefixed_string(line: &[u8], i: usize, end: usize, out: &mut Emit<'_>) -> Option<Next> {
    let prefix = &line[i..end];
    let next = at(line, end);
    match prefix {
        b"b" if next == b'"' => {
            out.push(i, end + 1, SyntaxRole::String);
            Some(Next::Enter(STRING, 0, end + 1))
        }
        b"b" if next == b'\'' => {
            let close = char_close(line, end).unwrap_or(line.len());
            out.push(i, close, SyntaxRole::String);
            Some(Next::At(close))
        }
        b"r" | b"br" | b"rb" if next == b'"' || next == b'#' => {
            let hashes = line[end..].iter().take_while(|&&b| b == b'#').count();
            if at(line, end + hashes) != b'"' {
                return None;
            }
            let body = end + hashes + 1;
            out.push(i, body, SyntaxRole::String);
            let hashes = u32::try_from(hashes).ok()?.min(0xff);
            Some(Next::Enter(RAW_STRING, hashes, body))
        }
        _ => None,
    }
}

/// Where the raw string closing with `"` and `hashes` hashes ends, if it
/// does on this line.
fn raw_close(line: &[u8], from: usize, hashes: u32) -> Option<usize> {
    let hashes = hashes as usize;
    let mut search = from;
    while let Some(quote) = find(line, search, b"\"") {
        let run = line[quote + 1..].iter().take_while(|&&b| b == b'#').count();
        if run >= hashes {
            return Some(quote + 1 + hashes);
        }
        search = quote + 1;
    }
    None
}

/// A `'` in a language with character literals: a literal, a lifetime, or a
/// stray quote.
fn char_or_lifetime(lang: &Lang, line: &[u8], i: usize, out: &mut Emit<'_>) -> usize {
    if let Some(close) = char_close(line, i) {
        out.push(i, close, SyntaxRole::String);
        return close;
    }
    if lang.traits.has(Traits::LIFETIMES) && is_ident_start(at(line, i + 1)) {
        let end = ident_end(line, i + 1);
        out.push(i, end, SyntaxRole::Type);
        return end;
    }
    out.push(i, i + 1, SyntaxRole::Punctuation);
    i + 1
}

/// Where the character literal opened by the quote at `open` closes: one
/// character or one escape, then the closing quote.
fn char_close(line: &[u8], open: usize) -> Option<usize> {
    let body = open + 1;
    let after = match at(line, body) {
        b'\\' => escape_end(line, body),
        0 | b'\'' => return None,
        _ => char_end(line, body),
    };
    (at(line, after) == b'\'').then_some(after + 1)
}

/// Scan a quoted body from `from` to its closing `quote`, classifying the
/// text as a string and its escapes as escapes. Answers where the scan
/// stopped and whether the quote closed.
pub(crate) fn quoted(line: &[u8], from: usize, quote: u8, out: &mut Emit<'_>) -> (usize, bool) {
    let mut run = from;
    let mut i = from;
    while i < line.len() {
        match line[i] {
            b'\\' => {
                out.push(run, i, SyntaxRole::String);
                let end = escape_end(line, i);
                out.push(i, end, SyntaxRole::Escape);
                i = end;
                run = end;
            }
            byte if byte == quote => {
                out.push(run, i + 1, SyntaxRole::String);
                return (i + 1, true);
            }
            _ => i += 1,
        }
    }
    out.push(run, line.len(), SyntaxRole::String);
    (line.len(), false)
}

/// Scan a block comment body from `from` at nesting `depth`, answering
/// where the scan stopped and the depth left open (zero when it closed).
fn block_comment(lang: &Lang, line: &[u8], from: usize, mut depth: u32) -> (usize, u32) {
    let mut i = from;
    while i + 1 < line.len() {
        if line[i] == b'*' && line[i + 1] == b'/' {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return (i, 0);
            }
        } else if lang.traits.has(Traits::NESTED_COMMENTS) && line[i] == b'/' && line[i + 1] == b'*'
        {
            depth = (depth + 1).min(MAX_DEPTH);
            i += 2;
        } else {
            i += 1;
        }
    }
    (line.len(), depth)
}

/// Where the attribute opened at `open` closes: the `]` matching its first
/// `[`, or the end of the line.
fn bracket_close(line: &[u8], open: usize) -> usize {
    let mut depth = 0usize;
    let mut i = open;
    while i < line.len() {
        match line[i] {
            b'[' => depth += 1,
            b']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i + 1;
                }
            }
            b'"' => {
                i = find(line, i + 1, b"\"").unwrap_or(line.len());
            }
            _ => {}
        }
        i += 1;
    }
    line.len()
}
