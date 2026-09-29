//! CSS.
//!
//! The state word records the mode, the brace depth, and one bit per open
//! block saying whether that block holds rules (a conditional group such as
//! `@media`) or declarations, so a selector nested in `@media` is read as a
//! selector. It fits in 24 bits, which is what HTML carries it in.

use tairix_theme::SyntaxRole;

use crate::clike::quoted;
use crate::lex::{
    at, find, ident_end, is_ident_continue, is_ident_start, number_end, skip_space, Emit, LineState,
};

/// Reading selectors: the top level, or inside a block that holds rules.
const SELECTOR: u32 = 0;
/// Inside a declaration block, expecting a property.
const PROPERTY: u32 = 1;
/// After a property's `:`.
const VALUE: u32 = 2;
/// After an at-keyword, before its block or its `;`.
const PRELUDE: u32 = 3;

/// The deepest nesting whose kind is remembered; a block beyond it is read
/// as declarations.
const MAX_DEPTH: u32 = 15;

/// One line's working state.
#[derive(Copy, Clone)]
struct State {
    mode: u32,
    /// A `/* */` comment is open; `mode` resumes after it.
    in_comment: bool,
    depth: u32,
    /// Bit `n` set: the block at depth `n + 1` holds rules.
    rules: u32,
    /// The at-rule being read opens a block of rules.
    prelude_rules: bool,
}

impl State {
    fn decode(raw: u32) -> Self {
        let state = Self {
            mode: raw & 0b11,
            in_comment: raw & 0b100 != 0,
            depth: (raw >> 3) & 0xf,
            rules: (raw >> 7) & 0x7fff,
            prelude_rules: raw & (1 << 22) != 0,
        };
        if raw >> 23 != 0 {
            Self::decode(0)
        } else {
            state
        }
    }

    fn encode(self) -> LineState {
        LineState::from_raw(
            self.mode
                | (u32::from(self.in_comment) << 2)
                | (self.depth << 3)
                | ((self.rules & 0x7fff) << 7)
                | (u32::from(self.prelude_rules) << 22),
        )
    }

    /// The mode the current block reads in.
    fn block_mode(self) -> u32 {
        if self.depth == 0 || (self.rules >> (self.depth - 1)) & 1 == 1 {
            SELECTOR
        } else {
            PROPERTY
        }
    }
}

/// The at-rules whose block holds rules rather than declarations.
const GROUP_RULES: &[&[u8]] = &[
    b"container",
    b"document",
    b"keyframes",
    b"layer",
    b"media",
    b"scope",
    b"starting-style",
    b"supports",
];

/// Classify one line of CSS.
pub(crate) fn lex(state: LineState, line: &[u8], out: &mut Emit<'_>) -> LineState {
    let mut st = State::decode(state.raw());
    let mut i = 0;
    while i <= line.len() {
        if st.in_comment {
            let Some(close) = find(line, i, b"*/") else {
                out.push(i, line.len(), SyntaxRole::Comment);
                return st.encode();
            };
            out.push(i, close + 2, SyntaxRole::Comment);
            i = close + 2;
            st.in_comment = false;
            continue;
        }
        let Some(&byte) = line.get(i) else {
            break;
        };
        match byte {
            b' ' | b'\t' | b'\r' => i = skip_space(line, i),
            b'/' if at(line, i + 1) == b'*' => {
                out.push(i, i + 2, SyntaxRole::Comment);
                st.in_comment = true;
                i += 2;
            }
            b'{' => {
                out.push(i, i + 1, SyntaxRole::Punctuation);
                let holds_rules = st.mode == PRELUDE && st.prelude_rules;
                if st.depth < MAX_DEPTH {
                    st.depth += 1;
                    let bit = 1 << (st.depth - 1);
                    st.rules = if holds_rules {
                        st.rules | bit
                    } else {
                        st.rules & !bit
                    };
                }
                st.mode = if holds_rules { SELECTOR } else { PROPERTY };
                st.prelude_rules = false;
                i += 1;
            }
            b'}' => {
                out.push(i, i + 1, SyntaxRole::Punctuation);
                st.depth = st.depth.saturating_sub(1);
                st.mode = st.block_mode();
                i += 1;
            }
            b';' => {
                out.push(i, i + 1, SyntaxRole::Punctuation);
                st.mode = st.block_mode();
                st.prelude_rules = false;
                i += 1;
            }
            b'@' if starts_ident(line, i + 1) => {
                let end = word_end(line, i + 1);
                out.push(i, end, SyntaxRole::Directive);
                let name = &line[i + 1..end];
                // `@-webkit-keyframes` is `@keyframes` behind a vendor prefix.
                let bare = name
                    .strip_prefix(b"-")
                    .and_then(|vendored| {
                        let dash = vendored.iter().position(|&b| b == b'-')?;
                        vendored.get(dash + 1..)
                    })
                    .unwrap_or(name);
                st.mode = PRELUDE;
                st.prelude_rules = GROUP_RULES.contains(&bare);
                i = end;
            }
            quote @ (b'"' | b'\'') => {
                out.push(i, i + 1, SyntaxRole::String);
                i = quoted(line, i + 1, quote, out).0;
            }
            _ => i = token(line, i, &mut st, out),
        }
    }
    st.encode()
}

/// Classify the token at `i` in the current mode, answering where it ends.
fn token(line: &[u8], i: usize, st: &mut State, out: &mut Emit<'_>) -> usize {
    let byte = line[i];
    match st.mode {
        SELECTOR => match byte {
            b'.' | b'#' if starts_ident(line, i + 1) => {
                let end = word_end(line, i + 1);
                out.push(i, end, SyntaxRole::Attribute);
                end
            }
            b':' => {
                let from = if at(line, i + 1) == b':' {
                    i + 2
                } else {
                    i + 1
                };
                let end = word_end(line, from);
                out.push(i, end, SyntaxRole::Keyword);
                end.max(i + 1)
            }
            b'[' => {
                let end = find(line, i, b"]").map_or(line.len(), |close| close + 1);
                out.push(i, end, SyntaxRole::Attribute);
                end
            }
            _ if is_ident_start(byte) || byte == b'-' || byte == b'*' => {
                let end = if byte == b'*' {
                    i + 1
                } else {
                    word_end(line, i)
                };
                out.push(i, end, SyntaxRole::Tag);
                end.max(i + 1)
            }
            _ => {
                out.push(i, i + 1, SyntaxRole::Punctuation);
                i + 1
            }
        },
        PROPERTY => match byte {
            b':' => {
                out.push(i, i + 1, SyntaxRole::Punctuation);
                st.mode = VALUE;
                i + 1
            }
            _ if is_ident_start(byte) || byte == b'-' => {
                let end = word_end(line, i);
                out.push(i, end, SyntaxRole::Attribute);
                end
            }
            _ => {
                out.push(i, i + 1, SyntaxRole::Punctuation);
                i + 1
            }
        },
        _ => value(line, i, out),
    }
}

/// Classify a value or at-rule prelude token at `i`.
fn value(line: &[u8], i: usize, out: &mut Emit<'_>) -> usize {
    let byte = line[i];
    match byte {
        b'#' => {
            let end = i
                + 1
                + line[i + 1..]
                    .iter()
                    .take_while(|b| b.is_ascii_alphanumeric())
                    .count();
            out.push(i, end, SyntaxRole::Number);
            end
        }
        b'!' => {
            let from = skip_space(line, i + 1);
            let end = ident_end(line, from);
            out.push(i, end, SyntaxRole::Keyword);
            end.max(i + 1)
        }
        b'0'..=b'9' => number(line, i, out),
        b'.' | b'+' | b'-' if at(line, i + 1).is_ascii_digit() => number(line, i, out),
        _ if is_ident_start(byte) || byte == b'-' => {
            let end = word_end(line, i);
            if at(line, end) == b'(' {
                out.push(i, end, SyntaxRole::Function);
                if line[i..end].eq_ignore_ascii_case(b"url")
                    && !matches!(at(line, skip_space(line, end + 1)), b'"' | b'\'')
                {
                    let close = find(line, end, b")").unwrap_or(line.len());
                    out.push(end, end + 1, SyntaxRole::Punctuation);
                    out.push(end + 1, close, SyntaxRole::String);
                    return close;
                }
            }
            end
        }
        _ => {
            out.push(i, i + 1, SyntaxRole::Punctuation);
            i + 1
        }
    }
}

/// A number with its unit or percent sign.
fn number(line: &[u8], i: usize, out: &mut Emit<'_>) -> usize {
    let digits = if matches!(line[i], b'+' | b'-' | b'.') {
        i + 1
    } else {
        i
    };
    let mut end = number_end(line, digits);
    if at(line, end) == b'%' {
        end += 1;
    }
    out.push(i, end, SyntaxRole::Number);
    end
}

/// Whether a CSS identifier starts at `from`: a name-start byte, or a hyphen
/// before one or before a second hyphen.
fn starts_ident(line: &[u8], from: usize) -> bool {
    match at(line, from) {
        b'-' => is_ident_start(at(line, from + 1)) || at(line, from + 1) == b'-',
        byte => is_ident_start(byte),
    }
}

/// Where the CSS identifier (dashes allowed) starting at `from` ends.
fn word_end(line: &[u8], from: usize) -> usize {
    let mut end = from;
    while end < line.len() && (is_ident_continue(line[end]) || line[end] == b'-') {
        end += 1;
    }
    end
}
