//! Message files as MessageTrans reads them: lines of `token:value`, a token
//! starting a line or following a `/`, a token alone on its line naming the
//! next line's value, `?` in a file's token matching any one byte, `#` where
//! a token is compared skipping its line, and the dictionary escape in a value
//! taking the byte after it with it, for the dictionary's word that byte
//! numbers. A value ends at a line feed, a return or a zero; one the file ends
//! inside is not there.
//!
//! The file is untrusted data: every walk is bounded by its length.

use core::ops::Range;

use crate::dictionary::ESCAPE;

/// The most dictionary words one value's expansion nests: a containment
/// bound, since a dictionary may name itself.
const NESTING: u32 = 8;

/// Whether `byte` ends a caller's token.
const fn ends_token(byte: u8) -> bool {
    byte <= b' ' || byte == b',' || byte == b')' || byte == b':'
}

/// Whether `byte` ends a value.
const fn ends_value(byte: u8) -> bool {
    matches!(byte, b'\n' | b'\r' | 0)
}

/// Where a caller's token ends.
pub(crate) fn token_end(text: &[u8]) -> usize {
    text.iter()
        .position(|&b| ends_token(b))
        .unwrap_or(text.len())
}

/// The default a caller's token carries: what follows the first colon before
/// its first control character, to its line feed, return or zero.
pub(crate) fn default_of(text: &[u8]) -> Option<Range<usize>> {
    let colon = text
        .iter()
        .take_while(|&&b| b >= b' ')
        .position(|&b| b == b':')?;
    let start = colon + 1;
    let length = text[start..]
        .iter()
        .position(|&b| ends_value(b))
        .unwrap_or(text.len() - start);
    Some(start..start + length)
}

/// Whether `byte` ends a file's token: a `/`, a colon, or a control
/// character.
const fn ends_file_token(byte: u8) -> bool {
    byte == b'/' || byte == b':' || byte < b' '
}

/// Past a line the walk leaves at `byte`, read from before `at`: to just
/// after the first control character that is not a tab, `&1F` or an escape
/// and the byte it takes; none if the file ends first.
fn past_line(data: &[u8], mut at: usize, mut byte: u8) -> Option<usize> {
    loop {
        if byte >= b' ' || byte == b'\t' || byte == 0x1F {
            byte = *data.get(at)?;
            at += 1;
        } else if byte == ESCAPE {
            at += 1;
            byte = *data.get(at)?;
            at += 1;
        } else {
            return (at < data.len()).then_some(at);
        }
    }
}

/// Past the token the walk left at `byte`, read from before `at`: just after
/// its `/` or control character, or past its line where a colon comes first.
fn past_token(data: &[u8], mut at: usize, mut byte: u8) -> Option<usize> {
    loop {
        if byte == b'/' || byte < b' ' {
            return (at < data.len()).then_some(at);
        }
        let colon = byte == b':';
        byte = *data.get(at)?;
        at += 1;
        if colon {
            return past_line(data, at, byte);
        }
    }
}

/// Past the colon at or after `at`: where a found token's value begins.
fn past_colon(data: &[u8], at: usize) -> Option<usize> {
    let colon = data.get(at..)?.iter().position(|&b| b == b':')?;
    Some(at + colon + 1)
}

/// Where the value of the first token in `data` that `token` names begins.
pub(crate) fn find(data: &[u8], token: &[u8]) -> Option<usize> {
    let mut at = 0;
    'tokens: loop {
        let mut matched = 0;
        let mut byte;
        loop {
            byte = *data.get(at)?;
            at += 1;
            if matched == token.len() {
                if ends_file_token(byte) {
                    return past_colon(data, at - 1);
                }
                break;
            }
            if byte == b'#' {
                at = past_line(data, at, byte)?;
                continue 'tokens;
            }
            if ends_file_token(byte) || !(byte == b'?' || byte == token[matched]) {
                break;
            }
            matched += 1;
        }
        at = past_token(data, at, byte)?;
    }
}

/// The length of the value at `at`: the bytes before its line feed, return
/// or zero, an escape and its byte counted; none if the file ends first.
pub(crate) fn value_length(data: &[u8], at: usize) -> Option<usize> {
    let mut end = at;
    loop {
        let byte = *data.get(end)?;
        if byte == ESCAPE {
            end += 2;
        } else if ends_value(byte) {
            return Some(end - at);
        } else {
            end += 1;
        }
    }
}

/// The first token at or after `at` that `pattern` names, as
/// `MessageTrans_EnumerateTokens` takes it — `?` in either matching one byte,
/// a final `*` any rest — and where the walk goes on from; none when the file
/// has no more. Empty tokens are passed over.
pub(crate) fn next_token(
    data: &[u8],
    pattern: &[u8],
    mut at: usize,
) -> Option<(Range<usize>, usize)> {
    'tokens: loop {
        let start = at;
        let mut matched = 0;
        let mut byte;
        loop {
            byte = *data.get(at)?;
            at += 1;
            let whole = if matched == pattern.len() {
                if !ends_file_token(byte) {
                    break;
                }
                past_colon(data, at - 1)?;
                true
            } else if byte == b'#' {
                at = past_line(data, at, byte)?;
                continue 'tokens;
            } else {
                pattern[matched] == b'*'
            };
            if whole {
                let (token, next) = found(data, start)?;
                if token.is_empty() {
                    at = next;
                    continue 'tokens;
                }
                return Some((token, next));
            }
            let p = pattern[matched];
            if ends_file_token(byte) || !(byte == b'?' || p == byte || p == b'?') {
                break;
            }
            matched += 1;
        }
        at = past_token(data, at, byte)?;
    }
}

/// The token at `start`, and past it.
fn found(data: &[u8], start: usize) -> Option<(Range<usize>, usize)> {
    let length = data
        .get(start..)?
        .iter()
        .position(|&b| ends_file_token(b))?;
    let end = start + length;
    let next = past_token(data, end + 1, data[end]).unwrap_or(data.len());
    Some((start..end, next))
}

/// The value the token at `token` names: past the next colon on.
pub(crate) fn value_of(data: &[u8], token: &Range<usize>) -> Option<usize> {
    past_colon(data, token.end)
}

/// Where a pattern has a `*` anywhere but at its end: a syntax error.
pub(crate) fn pattern_misplaced_star(pattern: &[u8]) -> bool {
    pattern
        .iter()
        .take(pattern.len().saturating_sub(1))
        .any(|&b| b == b'*')
}

/// Dictionary word `n`, from one: each a length byte — the step to the
/// next — its text and a zero; a zero length ends them.
pub(crate) fn dictionary_word(dictionary: &[u8], n: u8) -> Option<&[u8]> {
    let mut at = 0;
    for _ in 1..n {
        let step = usize::from(*dictionary.get(at)?);
        if step == 0 {
            return None;
        }
        at += step;
    }
    let step = *dictionary.get(at)?;
    if step == 0 {
        return None;
    }
    let text = dictionary.get(at + 1..)?;
    Some(&text[..text.iter().position(|&b| b == 0).unwrap_or(text.len())])
}

/// What a value expands to: `%0` to `%3` its parameters, a parameter not
/// given kept as written, `%%` one `%`; an escape and a byte the dictionary
/// word it numbers, nought the first parameter. It ends where the value
/// does.
pub(crate) struct Expansion<'a> {
    /// The parameters, each to its line feed, return or zero.
    pub(crate) parameters: [Option<&'a [u8]>; 4],
    /// The dictionary.
    pub(crate) dictionary: &'a [u8],
}

impl Expansion<'_> {
    /// Expands `value` into `out`, cut short at its end: the length of what
    /// it holds.
    pub(crate) fn expand(&self, value: &[u8], out: &mut [u8]) -> usize {
        let mut length = 0;
        self.expand_into(value, out, &mut length, 0);
        length
    }

    fn put(out: &mut [u8], length: &mut usize, bytes: &[u8]) {
        for &byte in bytes {
            if let Some(slot) = out.get_mut(*length) {
                *slot = byte;
                *length += 1;
            }
        }
    }

    fn parameter(text: &[u8]) -> &[u8] {
        &text[..text
            .iter()
            .position(|&b| ends_value(b))
            .unwrap_or(text.len())]
    }

    fn expand_into(&self, value: &[u8], out: &mut [u8], length: &mut usize, depth: u32) {
        let mut at = 0;
        while let Some(&byte) = value.get(at) {
            if ends_value(byte) {
                return;
            }
            at += 1;
            match byte {
                b'%' => match value.get(at) {
                    Some(&digit @ b'0'..=b'3') => {
                        at += 1;
                        match self.parameters[usize::from(digit - b'0')] {
                            Some(text) => Self::put(out, length, Self::parameter(text)),
                            None => Self::put(out, length, &[b'%', digit]),
                        }
                    }
                    Some(b'%') => {
                        at += 1;
                        Self::put(out, length, b"%");
                    }
                    _ => Self::put(out, length, b"%"),
                },
                ESCAPE => {
                    let Some(&n) = value.get(at) else {
                        return;
                    };
                    at += 1;
                    if n == 0 {
                        if let Some(text) = self.parameters[0] {
                            Self::put(out, length, Self::parameter(text));
                        }
                    } else if depth < NESTING {
                        if let Some(word) = dictionary_word(self.dictionary, n) {
                            self.expand_into(word, out, length, depth + 1);
                        }
                    }
                }
                _ => Self::put(out, length, &[byte]),
            }
        }
    }
}

#[cfg(test)]
mod tests;
