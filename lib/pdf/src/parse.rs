//! Objects read from tokens: direct objects, indirect objects and the bodies
//! of streams.

use alloc::vec::Vec;
use core::cell::OnceCell;

use tairix_inline::RingBuf;
use tairix_util::fallible::try_push;

use crate::error::tolerated;
use crate::lex::{is_white, Lexer, Token};
use crate::limits::MAX_NESTING;
use crate::object::{Dict, Name, ObjRef, Object, Stream};
use crate::PdfError;

/// The object the tokens at `lexer` spell, references read as such.
///
/// # Errors
///
/// [`PdfError::Malformed`] for tokens that spell no object, and
/// [`PdfError::TooComplex`] past [`MAX_NESTING`] or the lexer's allowance.
pub(crate) fn object(lexer: &mut Lexer<'_>, depth: usize) -> Result<Object, PdfError> {
    let mut tokens = Tokens::new(lexer);
    let token = tokens.next()?.ok_or(PdfError::Malformed)?;
    value(&mut tokens, token, depth, Syntax::File)
}

/// The operand `token`, the first of its tokens, begins in a content stream,
/// which holds no references: an integer is never looked past.
pub(crate) fn operand(lexer: &mut Lexer<'_>, token: Token<'_>) -> Result<Object, PdfError> {
    value(&mut Tokens::new(lexer), token, 0, Syntax::Content)
}

/// Where an object is written: a content stream holds no references.
#[derive(Copy, Clone, Eq, PartialEq)]
enum Syntax {
    File,
    Content,
}

/// The tokens at a lexer: those read ahead looking for a reference are kept
/// for what follows rather than lexed again, and where nothing takes them
/// the lexer is left where the first of them began.
struct Tokens<'l, 'a> {
    lexer: &'l mut Lexer<'a>,
    /// Each with where it began, the next first.
    ahead: RingBuf<(usize, Token<'a>), 2>,
}

impl<'l, 'a> Tokens<'l, 'a> {
    fn new(lexer: &'l mut Lexer<'a>) -> Self {
        Self {
            lexer,
            ahead: RingBuf::new(),
        }
    }

    fn next(&mut self) -> Result<Option<Token<'a>>, PdfError> {
        match self.ahead.pop_front() {
            Some((_, token)) => Ok(Some(token)),
            None => self.lexer.next(),
        }
    }

    /// The token `offset` past the next, read ahead and kept: none at the
    /// end of the data, or where it cannot be lexed, which reading it says.
    ///
    /// # Errors
    ///
    /// A containment bound passed or the heap's refusal: what lies ahead is
    /// then unknown, not absent.
    fn peek(&mut self, offset: usize) -> Result<Option<&Token<'a>>, PdfError> {
        while self.ahead.len() <= offset {
            let at = self.lexer.at();
            let kept = match tolerated(self.lexer.next())? {
                Some(Some(token)) => self.ahead.try_push_back((at, token)).is_ok(),
                _ => false,
            };
            if !kept {
                self.lexer.seek(at);
                return Ok(None);
            }
        }
        Ok(self.ahead.get(offset).map(|(_, token)| token))
    }

    /// The reference `num` begins where the next tokens are a generation and
    /// `R`, which it takes.
    ///
    /// # Errors
    ///
    /// As [`peek`](Self::peek).
    fn reference(&mut self, num: i64) -> Result<Option<ObjRef>, PdfError> {
        let Ok(num) = u32::try_from(num) else {
            return Ok(None);
        };
        let Some(&Token::Integer(gen)) = self.peek(0)? else {
            return Ok(None);
        };
        let Ok(gen) = u16::try_from(gen) else {
            return Ok(None);
        };
        if !matches!(self.peek(1)?, Some(Token::Keyword(b"R"))) {
            return Ok(None);
        }
        self.ahead.clear();
        Ok(Some(ObjRef { num, gen }))
    }
}

impl Drop for Tokens<'_, '_> {
    fn drop(&mut self) {
        if let Some(&(at, _)) = self.ahead.front() {
            self.lexer.seek(at);
        }
    }
}

fn value(
    tokens: &mut Tokens<'_, '_>,
    token: Token<'_>,
    depth: usize,
    syntax: Syntax,
) -> Result<Object, PdfError> {
    if depth >= MAX_NESTING {
        return Err(PdfError::TooComplex);
    }
    Ok(match token {
        Token::Integer(value) if syntax == Syntax::File => match tokens.reference(value)? {
            Some(reference) => Object::Ref(reference),
            None => Object::Int(value),
        },
        Token::Integer(value) => Object::Int(value),
        Token::Real(value) => Object::Real(value),
        Token::String(bytes) => Object::String(bytes),
        Token::Name(bytes) => Object::Name(Name(bytes)),
        Token::ArrayOpen => {
            let mut items = Vec::new();
            loop {
                let token = tokens.next()?.ok_or(PdfError::Malformed)?;
                if token == Token::ArrayClose {
                    break;
                }
                try_push(&mut items, value(tokens, token, depth + 1, syntax)?)?;
            }
            Object::Array(items)
        }
        Token::DictOpen => Object::Dict(entries(tokens, depth, syntax)?),
        Token::Keyword(b"true") => Object::Bool(true),
        Token::Keyword(b"false") => Object::Bool(false),
        Token::Keyword(b"null") => Object::Null,
        _ => return Err(PdfError::Malformed),
    })
}

/// A dictionary's entries after its `<<`, through its `>>`.
pub(crate) fn dict_body(lexer: &mut Lexer<'_>, depth: usize) -> Result<Dict, PdfError> {
    entries(&mut Tokens::new(lexer), depth, Syntax::File)
}

fn entries(tokens: &mut Tokens<'_, '_>, depth: usize, syntax: Syntax) -> Result<Dict, PdfError> {
    let mut entries = Vec::new();
    loop {
        match tokens.next()?.ok_or(PdfError::Malformed)? {
            Token::DictClose => return Dict::from_written(entries),
            Token::Name(key) => {
                let token = tokens.next()?.ok_or(PdfError::Malformed)?;
                let value = value(tokens, token, depth + 1, syntax)?;
                try_push(&mut entries, (Name(key), value))?;
            }
            _ => return Err(PdfError::Malformed),
        }
    }
}

/// An indirect object's head and body at `lexer`: `num gen obj`, the object,
/// and its stream's dictionary and where the stream's bytes begin, which the
/// caller bounds by its length. The trailing `endobj` is the caller's to
/// skip, since a stream's end is known only once its length is.
pub(crate) fn indirect(lexer: &mut Lexer<'_>) -> Result<(ObjRef, Indirect), PdfError> {
    let reference = head(lexer)?;
    Ok((reference, body(lexer)?))
}

/// An indirect object's head at `lexer`, `num gen obj`: read apart from its
/// body, so a reader looking for one object rejects another's at its head.
pub(crate) fn head(lexer: &mut Lexer<'_>) -> Result<ObjRef, PdfError> {
    let (Some(Token::Integer(num)), Some(Token::Integer(gen)), Some(Token::Keyword(b"obj"))) =
        (lexer.next()?, lexer.next()?, lexer.next()?)
    else {
        return Err(PdfError::Malformed);
    };
    Ok(ObjRef {
        num: u32::try_from(num).map_err(|_| PdfError::Malformed)?,
        gen: u16::try_from(gen).map_err(|_| PdfError::Malformed)?,
    })
}

/// An indirect object's body at `lexer`, after its head.
pub(crate) fn body(lexer: &mut Lexer<'_>) -> Result<Indirect, PdfError> {
    let dict = match object(lexer, 0)? {
        Object::Dict(dict) => dict,
        other => return Ok(Indirect::Object(other)),
    };
    let after = lexer.at();
    match lexer.next() {
        Ok(Some(Token::Keyword(b"stream"))) => {
            let data = stream_start(lexer.data(), lexer.at());
            Ok(Indirect::Stream { dict, data })
        }
        // Past its allowance, what follows may be a stream it never read.
        Err(PdfError::TooComplex) => Err(PdfError::TooComplex),
        _ => {
            lexer.seek(after);
            Ok(Indirect::Object(Object::Dict(dict)))
        }
    }
}

/// What an indirect object turned out to be.
pub(crate) enum Indirect {
    /// An object other than a stream.
    Object(Object),
    /// A stream: its dictionary, and where its bytes begin.
    Stream { dict: Dict, data: usize },
}

/// Where a stream's bytes begin after its `stream` keyword, which ends at
/// `at`: past the end of line that must follow, a lone carriage return
/// tolerated.
pub(crate) fn stream_start(data: &[u8], at: usize) -> usize {
    match (data.get(at), data.get(at + 1)) {
        (Some(b'\r'), Some(b'\n')) => at + 2,
        (Some(b'\n' | b'\r'), _) => at + 1,
        _ => at,
    }
}

/// The span of a stream whose bytes begin at `start`, `length` long where its
/// dictionary says so and an `endstream` follows; otherwise up to the next
/// `endstream`, which every reader of damaged files falls back on.
pub(crate) fn stream_span(
    data: &[u8],
    start: usize,
    length: Option<usize>,
    ends: &Endstreams,
) -> Result<Option<(usize, usize)>, PdfError> {
    if let Some(end) = length.and_then(|length| start.checked_add(length)) {
        if end <= data.len() && ends_stream(data, end) {
            return Ok(Some((start, end)));
        }
    }
    let Some(mut end) = ends.at_or_after(data, start)? else {
        return Ok(None);
    };
    // The end of line before `endstream` is not part of the data.
    if end > start && data[end - 1] == b'\n' {
        end -= 1;
    }
    if end > start && data[end - 1] == b'\r' {
        end -= 1;
    }
    Ok(Some((start, end)))
}

/// Where each `endstream` keyword of a file lies, found the first time a
/// stream's length proves wrong: every later one then costs a search rather
/// than a scan, however many a damaged file holds.
#[derive(Default)]
pub(crate) struct Endstreams(OnceCell<Vec<usize>>);

impl Endstreams {
    /// The first `endstream` in `data` at or after `start`.
    pub(crate) fn at_or_after(&self, data: &[u8], start: usize) -> Result<Option<usize>, PdfError> {
        let offsets = if let Some(offsets) = self.0.get() {
            offsets
        } else {
            let mut found = Vec::new();
            let mut at = 0;
            while let Some(offset) = find(&data[at..], b"endstream") {
                try_push(&mut found, at + offset)?;
                at += offset + b"endstream".len();
            }
            self.0.get_or_init(|| found)
        };
        let next = offsets.partition_point(|&offset| offset < start);
        Ok(offsets.get(next).copied())
    }
}

/// Whether `endstream` follows offset `end`, after at most an end of line.
fn ends_stream(data: &[u8], end: usize) -> bool {
    let mut at = end;
    while data.get(at).is_some_and(|&byte| is_white(byte)) && at < end + 2 {
        at += 1;
    }
    data.get(at..at + 9) == Some(b"endstream")
}

/// The offset of the first `needle` in `haystack`.
pub(crate) fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The offset of the last `needle` in `haystack`.
pub(crate) fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}

/// The stream object `owner` is: dictionary `dict` over the file's bytes
/// from `start` to `end`.
pub(crate) fn stream_object(dict: Dict, (start, end): (usize, usize), owner: ObjRef) -> Object {
    Object::Stream(Stream {
        dict,
        data: start..end,
        owner,
    })
}

#[cfg(test)]
#[path = "parse_tests.rs"]
mod tests;
