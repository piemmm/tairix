//! One HTTP/1.1 client exchange (RFC 9112): a `POST` written whole, and the
//! response read whole once its server has closed the connection — the status
//! line, the header fields, and a body framed by `Content-Length`, the chunked
//! transfer coding, or the end of the stream.
//!
//! The reader is strict where leniency has been the root of request-smuggling
//! and response-splitting attacks: a bare line feed, a folded field, a space
//! before a field's colon, a `Content-Length` beside a `Transfer-Encoding`,
//! disagreeing lengths, or bytes after a framed body are all refused. Pure and
//! bounded: no socket, no clock, and nothing allocated but a chunked body.

use alloc::borrow::Cow;
use alloc::vec::Vec;
use core::fmt::{self, Write as _};

use tairix_inline::ArrayString;

use crate::addr::IpAddr;

/// The longest host name a request may name: a DNS name's limit.
pub const MAX_HOST_NAME: usize = 253;

/// The longest label a host name may hold.
const MAX_HOST_LABEL: usize = 63;

/// The most header fields a response may carry.
pub const MAX_FIELDS: usize = 64;

/// The longest line a response may carry, its status line included.
pub const MAX_LINE: usize = 4096;

/// The most interim (`1xx`) responses read past before the final one.
const MAX_INTERIM: usize = 4;

/// Why an exchange failed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HttpError {
    /// The response, or a request part, breaks the message grammar.
    Malformed,
    /// A line, the field count, the body or the request exceeds its bound.
    TooLarge,
    /// The response frames its body with a coding other than `chunked`.
    Unsupported,
    /// The server answered with a status other than `200 OK`.
    Status(u16),
    /// A chunked body could not be assembled for want of memory.
    OutOfMemory,
}

/// A host a URL names rather than addresses: RFC 3986's `reg-name` held to
/// its unreserved characters, in labels of at most 63, so it stands in a
/// `Host` field as written. A last label of digits alone is refused, since
/// that spells an address, never a name. Compared without case, as RFC 3986
/// compares a host.
#[derive(Clone, Debug)]
pub struct HostName(ArrayString<MAX_HOST_NAME>);

impl PartialEq for HostName {
    fn eq(&self, other: &Self) -> bool {
        self.as_str().eq_ignore_ascii_case(other.as_str())
    }
}

impl Eq for HostName {}

impl HostName {
    /// `text` as a host name, or `None` outside the grammar or past
    /// [`MAX_HOST_NAME`].
    #[must_use]
    pub fn new(text: &str) -> Option<Self> {
        let labels_hold = text.split('.').all(|label| {
            (1..=MAX_HOST_LABEL).contains(&label.len()) && label.bytes().all(is_label_byte)
        });
        let spells_address = text
            .rsplit('.')
            .next()
            .is_some_and(|last| last.bytes().all(|byte| byte.is_ascii_digit()));
        if !labels_hold || spells_address {
            return None;
        }
        ArrayString::try_from(text).ok().map(Self)
    }

    /// The name as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// Whether `byte` is one of RFC 3986's unreserved characters other than the
/// `.` that separates labels.
const fn is_label_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'~')
}

/// What a URL names its server by: an address, or a host by name.
#[allow(
    clippy::large_enum_variant,
    reason = "a name is held inline so reading one allocates nothing; the URLs holding one are \
              few, and bounded by whoever reads them"
)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Host {
    /// A literal address.
    Addr(IpAddr),
    /// A name.
    Name(HostName),
}

/// What a request's `Host` field names: the host and port of the URL it is
/// for, whatever address the connection carrying it was made to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Authority<'a> {
    /// The URL's host.
    pub host: &'a Host,
    /// The URL's port.
    pub port: u16,
}

impl fmt::Display for Authority<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.host {
            Host::Addr(IpAddr::V4(addr)) => write!(f, "{addr}")?,
            Host::Addr(IpAddr::V6(addr)) => write!(f, "[{addr}]")?,
            Host::Name(name) => f.write_str(name.as_str())?,
        }
        if self.port == 80 {
            Ok(())
        } else {
            write!(f, ":{}", self.port)
        }
    }
}

/// Write a `POST` of `body`, of `content_type`, to `target` at `authority`
/// into `out`, asking the server to close the connection once it has
/// answered, and return the request's length.
///
/// # Errors
///
/// [`HttpError::Malformed`] when `target` is not an absolute path of visible
/// ASCII or `content_type` holds anything but visible ASCII, and
/// [`HttpError::TooLarge`] when the request does not fit `out`.
pub fn write_post(
    out: &mut [u8],
    authority: Authority<'_>,
    target: &str,
    content_type: &str,
    body: &[u8],
) -> Result<usize, HttpError> {
    if !target.starts_with('/') || !is_visible(target) || !is_visible(content_type) {
        return Err(HttpError::Malformed);
    }
    let mut cursor = Cursor::new(out);
    write!(
        cursor,
        "POST {target} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: {content_type}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .map_err(|_| HttpError::TooLarge)?;
    cursor.put(body).map_err(|_| HttpError::TooLarge)?;
    Ok(cursor.len())
}

/// Whether `text` is all visible ASCII, so nothing in it can end a line or a
/// field.
fn is_visible(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_graphic())
}

/// A [`fmt::Write`] into a fixed buffer that fails rather than truncates.
pub(crate) struct Cursor<'a> {
    out: &'a mut [u8],
    len: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(out: &'a mut [u8]) -> Self {
        Self { out, len: 0 }
    }

    /// What has been written.
    pub(crate) const fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn put(&mut self, bytes: &[u8]) -> fmt::Result {
        let end = self.len.checked_add(bytes.len()).ok_or(fmt::Error)?;
        self.out
            .get_mut(self.len..end)
            .ok_or(fmt::Error)?
            .copy_from_slice(bytes);
        self.len = end;
        Ok(())
    }
}

impl fmt::Write for Cursor<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.put(text.as_bytes())
    }
}

/// The body of a `200 OK` response, read from the whole of what the server
/// sent before closing the connection, holding at most `max_body` bytes.
///
/// # Errors
///
/// [`HttpError::Status`] for any final status but `200`, and the other
/// [`HttpError`]s as the response breaks the grammar or a bound.
pub fn read_response(bytes: &[u8], max_body: usize) -> Result<Cow<'_, [u8]>, HttpError> {
    let mut rest = bytes;
    for _ in 0..=MAX_INTERIM {
        let (head, after) = split_head(rest)?;
        let response = Head::parse(head)?;
        if (100..200).contains(&response.status) {
            rest = after;
            continue;
        }
        if response.status != 200 {
            return Err(HttpError::Status(response.status));
        }
        return response.body(after, max_body);
    }
    Err(HttpError::Malformed)
}

/// `bytes` split after the blank line ending a response's head, the head
/// without it.
fn split_head(bytes: &[u8]) -> Result<(&[u8], &[u8]), HttpError> {
    let bound = (MAX_FIELDS + 2) * MAX_LINE;
    let window = &bytes[..bytes.len().min(bound)];
    match window.windows(4).position(|four| four == b"\r\n\r\n") {
        Some(at) => Ok((&bytes[..at], &bytes[at + 4..])),
        None if bytes.len() > bound => Err(HttpError::TooLarge),
        None => Err(HttpError::Malformed),
    }
}

/// How a response's body is framed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Framing {
    Length(usize),
    Chunked,
    Close,
}

/// A response's status and its body's framing.
struct Head {
    status: u16,
    framing: Framing,
}

impl Head {
    fn parse(head: &[u8]) -> Result<Self, HttpError> {
        let mut lines = Lines(head);
        let status = status(lines.next()?.ok_or(HttpError::Malformed)?)?;
        let (mut length, mut chunked, mut fields) = (None, false, 0);
        while let Some(line) = lines.next()? {
            fields += 1;
            if fields > MAX_FIELDS {
                return Err(HttpError::TooLarge);
            }
            let (name, value) = field(line)?;
            if name.eq_ignore_ascii_case(b"content-length") {
                let declared = content_length(value)?;
                if length.is_some_and(|seen| seen != declared) {
                    return Err(HttpError::Malformed);
                }
                length = Some(declared);
            } else if name.eq_ignore_ascii_case(b"transfer-encoding") {
                if chunked {
                    return Err(HttpError::Malformed);
                }
                if !value.eq_ignore_ascii_case(b"chunked") {
                    return Err(HttpError::Unsupported);
                }
                chunked = true;
            }
        }
        let framing = match (length, chunked) {
            (Some(_), true) => return Err(HttpError::Malformed),
            (Some(length), false) => Framing::Length(length),
            (None, true) => Framing::Chunked,
            (None, false) => Framing::Close,
        };
        Ok(Self { status, framing })
    }

    fn body(self, after: &[u8], max_body: usize) -> Result<Cow<'_, [u8]>, HttpError> {
        match self.framing {
            Framing::Length(length) if length > max_body => Err(HttpError::TooLarge),
            Framing::Length(length) if length == after.len() => Ok(Cow::Borrowed(after)),
            Framing::Length(_) => Err(HttpError::Malformed),
            Framing::Close if after.len() > max_body => Err(HttpError::TooLarge),
            Framing::Close => Ok(Cow::Borrowed(after)),
            Framing::Chunked => dechunk(after, max_body).map(Cow::Owned),
        }
    }
}

/// The lines of a head, each ended by CRLF; a bare CR or LF is refused.
struct Lines<'a>(&'a [u8]);

impl<'a> Lines<'a> {
    fn next(&mut self) -> Result<Option<&'a [u8]>, HttpError> {
        if self.0.is_empty() {
            return Ok(None);
        }
        let (line, rest) = match self.0.windows(2).position(|two| two == b"\r\n") {
            Some(at) => (&self.0[..at], &self.0[at + 2..]),
            None => (self.0, &[][..]),
        };
        if line.len() > MAX_LINE {
            return Err(HttpError::TooLarge);
        }
        if line.iter().any(|&byte| byte == b'\r' || byte == b'\n') {
            return Err(HttpError::Malformed);
        }
        self.0 = rest;
        Ok(Some(line))
    }
}

/// The status code of `HTTP/1.x SP 3DIGIT SP reason`.
fn status(line: &[u8]) -> Result<u16, HttpError> {
    let rest = line
        .strip_prefix(b"HTTP/1.1 ")
        .or_else(|| line.strip_prefix(b"HTTP/1.0 "))
        .ok_or(HttpError::Malformed)?;
    let (code, reason) = rest.split_at_checked(3).ok_or(HttpError::Malformed)?;
    if !code.iter().all(u8::is_ascii_digit)
        || !(reason.is_empty() || reason.first() == Some(&b' '))
        || !reason
            .iter()
            .all(|&byte| byte == b'\t' || !byte.is_ascii_control())
    {
        return Err(HttpError::Malformed);
    }
    let status = code
        .iter()
        .fold(0u16, |status, digit| status * 10 + u16::from(digit - b'0'));
    if status < 100 {
        return Err(HttpError::Malformed);
    }
    Ok(status)
}

/// A field line split into its name and its value, optional whitespace
/// trimmed from the value.
fn field(line: &[u8]) -> Result<(&[u8], &[u8]), HttpError> {
    let colon = line
        .iter()
        .position(|&byte| byte == b':')
        .ok_or(HttpError::Malformed)?;
    let (name, value) = (&line[..colon], &line[colon + 1..]);
    if name.is_empty() || !name.iter().all(|&byte| is_token(byte)) {
        return Err(HttpError::Malformed);
    }
    if value
        .iter()
        .any(|&byte| byte != b'\t' && byte.is_ascii_control())
    {
        return Err(HttpError::Malformed);
    }
    Ok((name, value.trim_ascii()))
}

/// Whether `byte` is an RFC 9110 `tchar`.
fn is_token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

/// A `Content-Length` value: decimal digits alone.
fn content_length(value: &[u8]) -> Result<usize, HttpError> {
    if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
        return Err(HttpError::Malformed);
    }
    value.iter().try_fold(0usize, |length, digit| {
        length
            .checked_mul(10)
            .and_then(|length| length.checked_add(usize::from(digit - b'0')))
            .ok_or(HttpError::TooLarge)
    })
}

/// A chunked body decoded (RFC 9112 §7.1), its extensions and trailer fields
/// read past, refusing any byte after the last chunk's end.
fn dechunk(mut rest: &[u8], max_body: usize) -> Result<Vec<u8>, HttpError> {
    let mut body = Vec::new();
    loop {
        let (line, after) = chunk_line(rest)?;
        let size = chunk_size(line)?;
        if size == 0 {
            rest = after;
            break;
        }
        let total = body.len().checked_add(size).ok_or(HttpError::TooLarge)?;
        if total > max_body {
            return Err(HttpError::TooLarge);
        }
        let data = after.get(..size).ok_or(HttpError::Malformed)?;
        let after = after[size..]
            .strip_prefix(b"\r\n")
            .ok_or(HttpError::Malformed)?;
        body.try_reserve(size).map_err(|_| HttpError::OutOfMemory)?;
        body.extend_from_slice(data);
        rest = after;
    }
    let mut trailers = 0;
    loop {
        let (line, after) = chunk_line(rest)?;
        rest = after;
        if line.is_empty() {
            break;
        }
        trailers += 1;
        if trailers > MAX_FIELDS {
            return Err(HttpError::TooLarge);
        }
        field(line)?;
    }
    if rest.is_empty() {
        Ok(body)
    } else {
        Err(HttpError::Malformed)
    }
}

/// The line `rest` begins with, without its CRLF, and what follows it.
fn chunk_line(rest: &[u8]) -> Result<(&[u8], &[u8]), HttpError> {
    let window = &rest[..rest.len().min(MAX_LINE + 2)];
    let at = window
        .windows(2)
        .position(|two| two == b"\r\n")
        .ok_or(if rest.len() > MAX_LINE {
            HttpError::TooLarge
        } else {
            HttpError::Malformed
        })?;
    let line = &rest[..at];
    if line.iter().any(|&byte| byte == b'\r' || byte == b'\n') {
        return Err(HttpError::Malformed);
    }
    Ok((line, &rest[at + 2..]))
}

/// A chunk-size line's size, its extensions ignored.
fn chunk_size(line: &[u8]) -> Result<usize, HttpError> {
    let digits = line.split(|&byte| byte == b';').next().unwrap_or(line);
    let digits = digits.trim_ascii_end();
    if digits.is_empty() || digits.len() > 8 {
        return Err(HttpError::Malformed);
    }
    digits.iter().try_fold(0usize, |size, &digit| {
        let value = match digit {
            b'0'..=b'9' => digit - b'0',
            b'a'..=b'f' => digit - b'a' + 10,
            b'A'..=b'F' => digit - b'A' + 10,
            _ => return Err(HttpError::Malformed),
        };
        Ok(size * 16 + usize::from(value))
    })
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
