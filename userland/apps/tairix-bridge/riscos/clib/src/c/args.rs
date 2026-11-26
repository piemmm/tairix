//! A command line as `_main` reads it, as RISC OS 5.30's does: words parted
//! by spaces, a double quote turning quoting on or off anywhere in a word,
//! and a word starting with a redirection taken as one — with its file name
//! in the same word or the next.

use alloc::vec::Vec;

/// The standard stream a redirection sends.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Standard {
    Input,
    Output,
    Error,
    /// `stdout` and `stderr` both.
    OutputAndError,
}

/// One redirection, in the order the line gives them.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Redirection {
    /// `stream` to or from the file `name`, appended to where `append`.
    File {
        stream: Standard,
        name: Vec<u8>,
        append: bool,
    },
    /// `2>&1`: `stderr` where `stdout` goes.
    ErrorToOutput,
}

/// A command line read: its words, the program's name first, and its
/// redirections.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(crate) struct CommandLine {
    pub(crate) words: Vec<Vec<u8>>,
    pub(crate) redirections: Vec<Redirection>,
}

/// A command line `_main` refuses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Malformed {
    /// A double quote opened and never closed.
    MissingQuote,
    /// A redirection of a stream there is not, or the wrong way.
    IllegalRedirection,
    /// More than the host would hold.
    NoMemory,
}

/// A word as the line spelled it: its bytes with the quotes taken out, and
/// whether it started with a redirection outside quotes.
struct Word {
    text: Vec<u8>,
    bare_start: bool,
}

/// The line's words, quotes taken out.
fn words(line: &[u8]) -> Result<Vec<Word>, Malformed> {
    let mut words = Vec::new();
    let mut rest = line;
    loop {
        rest = &rest[rest.iter().take_while(|&&b| b == b' ').count()..];
        if rest.is_empty() {
            return Ok(words);
        }
        let mut text = Vec::new();
        let mut quoted = false;
        let bare_start = rest[0] != b'"';
        let mut taken = 0;
        for &b in rest {
            if b == b' ' && !quoted {
                break;
            }
            taken += 1;
            if b == b'"' {
                quoted = !quoted;
            } else {
                text.try_reserve(1).map_err(|_| Malformed::NoMemory)?;
                text.push(b);
            }
        }
        if quoted {
            return Err(Malformed::MissingQuote);
        }
        rest = &rest[taken..];
        words.try_reserve(1).map_err(|_| Malformed::NoMemory)?;
        words.push(Word { text, bare_start });
    }
}

/// The redirection a word starting with one opens, and what of the word
/// follows it: none for a word that does not start with one.
fn operator(word: &[u8]) -> Option<Result<(Operator, usize), Malformed>> {
    let (digit, rest) = match word {
        [d @ b'0'..=b'9', rest @ ..] if matches!(rest.first(), Some(b'<' | b'>')) => {
            (Some(*d), rest)
        }
        _ => (None, word),
    };
    let (operator, length) = match rest {
        [b'>', b'&', b'1', ..] if digit == Some(b'2') => (Operator::ErrorToOutput, 3),
        [b'>', b'&', ..] if digit.is_none() => (Operator::File(Standard::OutputAndError, false), 2),
        [b'>', b'>', ..] => (Operator::File(Standard::Output, true), 2),
        [b'>', ..] => (Operator::File(Standard::Output, false), 1),
        [b'<', ..] => (Operator::File(Standard::Input, false), 1),
        _ => return None,
    };
    let operator = match (digit, operator) {
        (None, operator) => operator,
        (Some(b'0'), Operator::File(Standard::Input, append)) => {
            Operator::File(Standard::Input, append)
        }
        (Some(b'1'), Operator::File(Standard::Output, append)) => {
            Operator::File(Standard::Output, append)
        }
        (Some(b'2'), Operator::File(Standard::Output, append)) => {
            Operator::File(Standard::Error, append)
        }
        (Some(b'2'), Operator::ErrorToOutput) => Operator::ErrorToOutput,
        _ => return Some(Err(Malformed::IllegalRedirection)),
    };
    Some(Ok((operator, length + usize::from(digit.is_some()))))
}

#[derive(Clone, Copy)]
enum Operator {
    File(Standard, bool),
    ErrorToOutput,
}

/// The command line `line`, to its first control character.
pub(crate) fn parse(line: &[u8]) -> Result<CommandLine, Malformed> {
    let end = line.iter().position(|&b| b < b' ').unwrap_or(line.len());
    let mut parsed = CommandLine::default();
    let mut words = words(&line[..end])?.into_iter();
    while let Some(word) = words.next() {
        let found = if word.bare_start {
            operator(&word.text)
        } else {
            None
        };
        let Some(found) = found else {
            parsed
                .words
                .try_reserve(1)
                .map_err(|_| Malformed::NoMemory)?;
            parsed.words.push(word.text);
            continue;
        };
        let (operator, length) = found?;
        let redirection = match operator {
            Operator::ErrorToOutput => Some(Redirection::ErrorToOutput),
            Operator::File(stream, append) => {
                let name = match &word.text[length..] {
                    [] => words.next().map(|next| next.text),
                    joined => Some(joined.to_vec()),
                };
                name.map(|name| Redirection::File {
                    stream,
                    name,
                    append,
                })
            }
        };
        if let Some(redirection) = redirection {
            parsed
                .redirections
                .try_reserve(1)
                .map_err(|_| Malformed::NoMemory)?;
            parsed.redirections.push(redirection);
        }
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests;
