//! The scripting languages: Python and the shell.

use tairix_hash::FastHash;
use tairix_theme::SyntaxRole;

use crate::clike::quoted;
use crate::lex::{
    ascending, at, find, ident_end, is_ident_continue, is_ident_start, listed, number_end,
    skip_space, starts_at, Emit, LineState,
};

const PYTHON_KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "case", "class",
    "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if",
    "import", "in", "is", "lambda", "match", "nonlocal", "not", "or", "pass", "raise", "return",
    "try", "type", "while", "with", "yield",
];

const PYTHON_TYPES: &[&str] = &[
    "bool",
    "bytearray",
    "bytes",
    "complex",
    "dict",
    "float",
    "frozenset",
    "int",
    "list",
    "object",
    "set",
    "str",
    "tuple",
];

const _: () = assert!(ascending(PYTHON_KEYWORDS) && ascending(PYTHON_TYPES));

/// A Python triple-quoted string is open: the state word's low byte, with
/// bit 8 set for `'''` rather than `"""` and bit 9 for a raw string.
const PYTHON_TRIPLE: u32 = 1;
const TRIPLE_SINGLE: u32 = 1 << 8;
const TRIPLE_RAW: u32 = 1 << 9;

/// Classify one line of Python.
pub(crate) fn python(state: LineState, line: &[u8], out: &mut Emit<'_>) -> LineState {
    let mut i = 0;
    if state.raw() & 0xff == PYTHON_TRIPLE {
        let quote = if state.raw() & TRIPLE_SINGLE == 0 {
            b'"'
        } else {
            b'\''
        };
        let Some(end) = triple_close(line, 0, quote, state.raw() & TRIPLE_RAW != 0, out) else {
            return state;
        };
        i = end;
    }
    // What the next word is being defined as: a name after `def` or
    // `class`.
    let mut defining = None;
    let lead = skip_space(line, 0);
    while i < line.len() {
        let byte = line[i];
        match byte {
            b' ' | b'\t' | b'\r' => i = skip_space(line, i),
            b'#' => {
                out.push(i, line.len(), SyntaxRole::Comment);
                break;
            }
            b'@' if i == lead => {
                let mut end = i + 1;
                while is_ident_continue(at(line, end)) || at(line, end) == b'.' {
                    end += 1;
                }
                out.push(i, end, SyntaxRole::Directive);
                i = end;
            }
            b'"' | b'\'' => match string(line, i, i, false, out) {
                Ok(end) => i = end,
                Err(open) => return open,
            },
            b'0'..=b'9' => {
                let end = number_end(line, i);
                out.push(i, end, SyntaxRole::Number);
                i = end;
            }
            b'.' if at(line, i + 1).is_ascii_digit()
                && !is_ident_continue(at(line, i.wrapping_sub(1))) =>
            {
                let end = number_end(line, i + 1);
                out.push(i, end, SyntaxRole::Number);
                i = end;
            }
            _ if is_ident_start(byte) => {
                let end = ident_end(line, i);
                let word = &line[i..end];
                if matches!(at(line, end), b'"' | b'\'') && is_string_prefix(word) {
                    let raw = word.iter().any(|b| b.eq_ignore_ascii_case(&b'r'));
                    match string(line, i, end, raw, out) {
                        Ok(after) => i = after,
                        Err(open) => return open,
                    }
                    continue;
                }
                let role = if let Some(role) = defining.take() {
                    role
                } else if listed(PYTHON_KEYWORDS, word) {
                    defining = match word {
                        b"def" => Some(SyntaxRole::Function),
                        b"class" => Some(SyntaxRole::Type),
                        _ => None,
                    };
                    SyntaxRole::Keyword
                } else if listed(PYTHON_TYPES, word) {
                    SyntaxRole::Type
                } else if at(line, skip_space(line, end)) == b'(' {
                    SyntaxRole::Function
                } else {
                    SyntaxRole::Plain
                };
                out.push(i, end, role);
                i = end;
            }
            _ if byte.is_ascii_punctuation() => {
                out.push(i, i + 1, SyntaxRole::Punctuation);
                i += 1;
            }
            _ => i += 1,
        }
    }
    LineState::START
}

/// Whether `word` is a string prefix: `r`, `b`, `u`, `f` and their legal
/// pairs, in either case.
fn is_string_prefix(word: &[u8]) -> bool {
    let lower: [u8; 2] = match word {
        [a] => [a.to_ascii_lowercase(), 0],
        [a, b] => [a.to_ascii_lowercase(), b.to_ascii_lowercase()],
        _ => return false,
    };
    matches!(
        &lower,
        b"r\0" | b"b\0" | b"u\0" | b"f\0" | b"rb" | b"br" | b"fr" | b"rf"
    )
}

/// Classify the Python string whose prefix starts at `start` and whose
/// opening quote is at `quote_at`. Answers where it ends, or the state a
/// triple-quoted string left open.
fn string(
    line: &[u8],
    start: usize,
    quote_at: usize,
    raw: bool,
    out: &mut Emit<'_>,
) -> Result<usize, LineState> {
    let quote = line[quote_at];
    if starts_at(line, quote_at, &[quote; 3]) {
        out.push(start, quote_at + 3, SyntaxRole::String);
        let single = if quote == b'\'' { TRIPLE_SINGLE } else { 0 };
        let raw_bit = if raw { TRIPLE_RAW } else { 0 };
        return triple_close(line, quote_at + 3, quote, raw, out)
            .ok_or(LineState::from_raw(PYTHON_TRIPLE | single | raw_bit));
    }
    out.push(start, quote_at + 1, SyntaxRole::String);
    if raw {
        let end = find(line, quote_at + 1, &[quote]).map_or(line.len(), |close| close + 1);
        out.push(quote_at + 1, end, SyntaxRole::String);
        return Ok(end);
    }
    Ok(quoted(line, quote_at + 1, quote, out).0)
}

/// Scan a triple-quoted body from `from` to its closing triple, classifying
/// it; `None` when it stays open past the line.
fn triple_close(
    line: &[u8],
    from: usize,
    quote: u8,
    raw: bool,
    out: &mut Emit<'_>,
) -> Option<usize> {
    let close = [quote; 3];
    let mut run = from;
    let mut i = from;
    while i < line.len() {
        if line[i] == b'\\' && !raw {
            out.push(run, i, SyntaxRole::String);
            let end = crate::lex::escape_end(line, i);
            out.push(i, end, SyntaxRole::Escape);
            i = end;
            run = end;
        } else if starts_at(line, i, &close) {
            out.push(run, i + 3, SyntaxRole::String);
            return Some(i + 3);
        } else {
            i += 1;
        }
    }
    out.push(run, line.len(), SyntaxRole::String);
    None
}

const SHELL_KEYWORDS: &[&str] = &[
    "case", "do", "done", "elif", "else", "esac", "fi", "for", "function", "if", "in", "select",
    "then", "time", "until", "while",
];

const SHELL_BUILTINS: &[&str] = &[
    "alias", "bg", "break", "cd", "command", "continue", "echo", "eval", "exec", "exit", "export",
    "false", "fg", "getopts", "hash", "jobs", "kill", "local", "printf", "pwd", "read", "readonly",
    "return", "set", "shift", "source", "test", "times", "trap", "true", "type", "ulimit", "umask",
    "unalias", "unset", "wait",
];

const _: () = assert!(ascending(SHELL_KEYWORDS) && ascending(SHELL_BUILTINS));

/// What a shell line was left inside of.
const SHELL_SINGLE: u32 = 1;
const SHELL_DOUBLE: u32 = 2;
/// A here-document is open: its delimiter's hash above the low byte, with
/// bit 8 set when leading tabs are stripped (`<<-`).
const SHELL_HEREDOC: u32 = 3;
const HEREDOC_TABS: u32 = 1 << 8;
/// An arithmetic expansion or command is open, its unclosed parentheses above
/// the low byte: within one, `<<` is a shift and never a here-document.
const SHELL_ARITH: u32 = 4;

/// Classify one line of shell.
pub(crate) fn shell(state: LineState, line: &[u8], out: &mut Emit<'_>) -> LineState {
    let mut i = match resume(state, line, out) {
        Ok(start) => start,
        Err(open) => return open,
    };
    let mut arith = if state.raw() & 0xff == SHELL_ARITH {
        state.raw() >> 8
    } else {
        0
    };
    let mut command = true;
    let mut heredoc = None;
    while i < line.len() {
        let byte = line[i];
        let word_start = i == 0 || matches!(line[i - 1], b' ' | b'\t' | b';' | b'&' | b'|' | b'(');
        match byte {
            b' ' | b'\t' | b'\r' => i = skip_space(line, i),
            b'#' if word_start => {
                let role = if i == 0 && at(line, 1) == b'!' {
                    SyntaxRole::Directive
                } else {
                    SyntaxRole::Comment
                };
                out.push(i, line.len(), role);
                break;
            }
            b'\'' => {
                let Some(close) = find(line, i + 1, b"'") else {
                    out.push(i, line.len(), SyntaxRole::String);
                    return LineState::from_raw(SHELL_SINGLE);
                };
                out.push(i, close + 1, SyntaxRole::String);
                i = close + 1;
                // A word's start decides what follows it: the rest of an
                // assignment's value leaves command position as it was.
                command &= !word_start;
            }
            b'"' => {
                out.push(i, i + 1, SyntaxRole::String);
                let Some(end) = double_quoted(line, i + 1, out) else {
                    return LineState::from_raw(SHELL_DOUBLE);
                };
                i = end;
                command &= !word_start;
            }
            b'$' => {
                let end = expansion_end(line, i);
                out.push(i, end, SyntaxRole::Attribute);
                if at(line, i + 1) == b'(' && (arith > 0 || at(line, i + 2) == b'(') {
                    arith += 1;
                }
                // `$(` opens a command of its own.
                command = at(line, i + 1) == b'(' || (command && !word_start);
                i = end;
            }
            b'\\' => {
                let end = (i + 2).min(line.len());
                out.push(i, end, SyntaxRole::Escape);
                i = end;
                command &= !word_start;
            }
            b'<' if arith == 0 && at(line, i + 1) == b'<' && at(line, i + 2) != b'<' => {
                let (end, opened) = here_document(line, i, out);
                heredoc = opened.or(heredoc);
                i = end;
            }
            b';' | b'&' | b'|' | b'(' | b')' | b'<' | b'>' | b'{' | b'}' | b'!' => {
                match byte {
                    b'(' if arith > 0 || (command && at(line, i + 1) == b'(') => arith += 1,
                    b')' => arith = arith.saturating_sub(1),
                    _ => {}
                }
                out.push(i, i + 1, SyntaxRole::Punctuation);
                command = !matches!(byte, b')' | b'<' | b'>');
                i += 1;
            }
            _ => (i, command) = word(line, i, command, word_start, out),
        }
    }
    match heredoc {
        Some(open) => LineState::from_raw(open),
        None if arith > 0 => LineState::from_raw(SHELL_ARITH | arith.min(u32::MAX >> 8) << 8),
        None => LineState::START,
    }
}

/// Continue a construct the line before left open, answering where normal
/// scanning starts, or the state to leave with when the construct holds the
/// whole line.
fn resume(state: LineState, line: &[u8], out: &mut Emit<'_>) -> Result<usize, LineState> {
    match state.raw() & 0xff {
        SHELL_HEREDOC => {
            let body = if state.raw() & HEREDOC_TABS == 0 {
                line
            } else {
                let tabs = line.iter().take_while(|&&b| b == b'\t').count();
                &line[tabs..]
            };
            let body = body.strip_suffix(b"\r").unwrap_or(body);
            if delimiter_hash(body) == state.raw() >> 9 {
                out.push(0, line.len(), SyntaxRole::Directive);
                return Err(LineState::START);
            }
            out.push(0, line.len(), SyntaxRole::String);
            Err(state)
        }
        SHELL_SINGLE => {
            let Some(close) = find(line, 0, b"'") else {
                out.push(0, line.len(), SyntaxRole::String);
                return Err(state);
            };
            out.push(0, close + 1, SyntaxRole::String);
            Ok(close + 1)
        }
        SHELL_DOUBLE => double_quoted(line, 0, out).ok_or(state),
        _ => Ok(0),
    }
}

/// Classify the `<<` or `<<-` at `i` and the delimiter after it, answering
/// where it ends and the here-document state it opens.
fn here_document(line: &[u8], i: usize, out: &mut Emit<'_>) -> (usize, Option<u32>) {
    let tabs = at(line, i + 2) == b'-';
    let from = skip_space(line, i + 2 + usize::from(tabs));
    out.push(i, from, SyntaxRole::Punctuation);
    let (name, end) = heredoc_word(line, from);
    out.push(from, end, SyntaxRole::Directive);
    let flag = if tabs { HEREDOC_TABS } else { 0 };
    let opened = (!name.is_empty()).then(|| SHELL_HEREDOC | flag | (delimiter_hash(name) << 9));
    (end.max(i + 2), opened)
}

/// Classify the word at `i`, answering where it ends and whether the next
/// word stands in command position. Past a word's start — an assignment's
/// value, text after an expansion — only a number is picked out, and command
/// position stays as the start left it.
fn word(
    line: &[u8],
    i: usize,
    command: bool,
    word_start: bool,
    out: &mut Emit<'_>,
) -> (usize, bool) {
    let end = shell_word_end(line, i).max(i + 1);
    let word = &line[i..end];
    if !word_start {
        if word.iter().all(u8::is_ascii_digit) {
            out.push(i, end, SyntaxRole::Number);
        }
        return (end, command);
    }
    if let Some(eq) = word.iter().position(|&b| b == b'=') {
        if command && eq > 0 && word[..eq].iter().all(|&b| is_ident_continue(b)) {
            out.push(i, i + eq, SyntaxRole::Attribute);
            out.push(i + eq, i + eq + 1, SyntaxRole::Punctuation);
            return (i + eq + 1, true);
        }
        return (end, false);
    }
    let role = if listed(SHELL_KEYWORDS, word) {
        SyntaxRole::Keyword
    } else if command && listed(SHELL_BUILTINS, word) {
        SyntaxRole::Function
    } else if word.iter().all(u8::is_ascii_digit) {
        SyntaxRole::Number
    } else {
        SyntaxRole::Plain
    };
    out.push(i, end, role);
    let next_is_command =
        role == SyntaxRole::Keyword && !matches!(word, b"in" | b"esac" | b"done" | b"fi");
    (end, next_is_command)
}

/// Where the unquoted shell word starting at `from` ends.
fn shell_word_end(line: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < line.len()
        && !matches!(
            line[i],
            b' ' | b'\t'
                | b'\r'
                | b';'
                | b'&'
                | b'|'
                | b'('
                | b')'
                | b'<'
                | b'>'
                | b'"'
                | b'\''
                | b'$'
        )
    {
        i += 1;
    }
    i
}

/// Scan a double-quoted body from `from`, classifying escapes and
/// expansions inside it; `None` when it stays open past the line.
fn double_quoted(line: &[u8], from: usize, out: &mut Emit<'_>) -> Option<usize> {
    let mut run = from;
    let mut i = from;
    while i < line.len() {
        match line[i] {
            b'\\' => {
                out.push(run, i, SyntaxRole::String);
                let end = (i + 2).min(line.len());
                out.push(i, end, SyntaxRole::Escape);
                i = end;
                run = end;
            }
            b'$' => {
                out.push(run, i, SyntaxRole::String);
                let end = expansion_end(line, i);
                out.push(i, end, SyntaxRole::Attribute);
                i = end;
                run = end;
            }
            b'"' => {
                out.push(run, i + 1, SyntaxRole::String);
                return Some(i + 1);
            }
            _ => i += 1,
        }
    }
    out.push(run, line.len(), SyntaxRole::String);
    None
}

/// Where the parameter expansion at the `$` at `from` ends: `$name`,
/// `${…}`, `$(…)` (the opening only), or a special parameter.
fn expansion_end(line: &[u8], from: usize) -> usize {
    let next = from + 1;
    match at(line, next) {
        b'{' => find(line, next, b"}").map_or(line.len(), |close| close + 1),
        b'(' | b'?' | b'#' | b'@' | b'*' | b'$' | b'!' | b'-' | b'0'..=b'9' => next + 1,
        byte if is_ident_start(byte) => ident_end(line, next),
        _ => next,
    }
}

/// The here-document delimiter word starting at `from`, unquoted, and
/// where it ends.
fn heredoc_word(line: &[u8], from: usize) -> (&[u8], usize) {
    let quote = at(line, from);
    if matches!(quote, b'\'' | b'"') {
        let close = find(line, from + 1, &[quote]).unwrap_or(line.len());
        return (&line[from + 1..close], (close + 1).min(line.len()));
    }
    let end = shell_word_end(line, from);
    (&line[from..end], end)
}

/// A 23-bit hash of a here-document delimiter: what the state word can carry
/// to recognise the line that closes the document. Unkeyed on purpose: a
/// chosen collision only mis-ends the colouring of the document that chose it.
fn delimiter_hash(word: &[u8]) -> u32 {
    (FastHash::hash_bytes(0, word) & 0x7f_ffff) as u32
}
