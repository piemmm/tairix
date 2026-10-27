//! `CHANGE_NOTIFY` (MS-SMB2 §2.2.35, §2.2.36) and the change records it
//! answers with (MS-FSCC §2.7.1).
//!
//! A watch stays outstanding until something changes; the connection carries
//! its interim response and its cancellation. A record's path, relative to
//! the watched directory, is held to the same rules as any other path a
//! server names.

use crate::file::name::SharePath;
use crate::file::{output_buffer, write_output_buffer, FileId};
use crate::header::structure;
use crate::wire::{Encoder, Reader};
use crate::SmbError;

/// The most output one change response may carry, and so how many records.
pub const MAX_CHANGES: usize = 64 << 10;

/// The notify flag watching the whole subtree.
const WATCH_TREE: u16 = 0x0001;

flags! {
    /// What a watch reports (MS-SMB2 §2.2.35).
    pub struct NotifyFilter(u32) {
        /// A file's name.
        const FILE_NAME = 0x0000_0001;
        /// A directory's name.
        const DIR_NAME = 0x0000_0002;
        /// Attributes.
        const ATTRIBUTES = 0x0000_0004;
        /// Size.
        const SIZE = 0x0000_0008;
        /// The write time.
        const LAST_WRITE = 0x0000_0010;
        /// The access time.
        const LAST_ACCESS = 0x0000_0020;
        /// The creation time.
        const CREATION = 0x0000_0040;
        /// Extended attributes.
        const EA = 0x0000_0080;
        /// Security descriptors.
        const SECURITY = 0x0000_0100;
    }
}

/// A `CHANGE_NOTIFY` request.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ChangeNotifyRequest {
    /// Whether to watch the whole subtree, not just the directory's entries.
    pub watch_tree: bool,
    /// The most output the server may answer with.
    pub output_length: u32,
    /// The directory's open.
    pub file_id: FileId,
    /// What to report.
    pub filter: NotifyFilter,
}

impl ChangeNotifyRequest {
    /// Append the request's body after its header. An output length past
    /// [`MAX_CHANGES`] latches [`SmbError::TooLarge`].
    pub fn encode(&self, enc: &mut Encoder) {
        if usize::try_from(self.output_length).map_or(true, |len| len > MAX_CHANGES) {
            enc.fail(SmbError::TooLarge);
        }
        enc.u16(32);
        enc.u16(if self.watch_tree { WATCH_TREE } else { 0 });
        enc.u32(self.output_length);
        enc.bytes(&self.file_id);
        enc.u32(self.filter.bits());
        enc.u32(0);
    }

    /// The request in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not a well-formed request.
    pub fn decode(message: &[u8]) -> Result<Self, SmbError> {
        let mut r = structure(message, 32)?;
        let flags = r.u16()?;
        Ok(Self {
            watch_tree: flags & WATCH_TREE != 0,
            output_length: r.u32()?,
            file_id: r.array()?,
            filter: NotifyFilter::from_bits(r.u32()?),
        })
    }
}

/// A `CHANGE_NOTIFY` response: the records, borrowed from the message. A
/// response with `STATUS_NOTIFY_ENUM_DIR` carries none, and means more
/// changed than the server could say.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ChangeNotifyResponse<'a> {
    /// The raw records.
    pub output: &'a [u8],
}

impl<'a> ChangeNotifyResponse<'a> {
    /// Append the response's body after its header.
    pub fn encode(&self, enc: &mut Encoder) {
        write_output_buffer(enc, self.output);
    }

    /// The response in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not a well-formed response,
    /// [`SmbError::TooLarge`] past [`MAX_CHANGES`].
    pub fn decode(message: &'a [u8]) -> Result<Self, SmbError> {
        Ok(Self {
            output: output_buffer(message, 9, MAX_CHANGES)?,
        })
    }

    /// Its records, in the server's order.
    #[must_use]
    pub const fn changes(&self) -> Changes<'a> {
        Changes {
            output: self.output,
            at: Some(0),
        }
    }
}

/// What happened to a path (MS-FSCC §2.7.1).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Action {
    /// It was added.
    Added,
    /// It was removed.
    Removed,
    /// Its data or attributes changed.
    Modified,
    /// It was renamed from this name.
    RenamedOld,
    /// It was renamed to this name.
    RenamedNew,
}

impl Action {
    const fn code(self) -> u32 {
        match self {
            Self::Added => 1,
            Self::Removed => 2,
            Self::Modified => 3,
            Self::RenamedOld => 4,
            Self::RenamedNew => 5,
        }
    }

    /// The action of `code`, or `None` for a stream or object-id action, which
    /// names nothing a path reaches.
    const fn from_code(code: u32) -> Option<Self> {
        match code {
            1 => Some(Self::Added),
            2 => Some(Self::Removed),
            3 => Some(Self::Modified),
            4 => Some(Self::RenamedOld),
            5 => Some(Self::RenamedNew),
            _ => None,
        }
    }
}

/// One change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Change {
    /// What happened, or `None` for an action no path names.
    pub action: Option<Action>,
    /// What it happened to, relative to the watched directory.
    pub path: SharePath,
}

/// The records of a change response, each checked as it is reached.
#[derive(Clone, Debug)]
pub struct Changes<'a> {
    output: &'a [u8],
    at: Option<usize>,
}

impl Changes<'_> {
    fn record(&mut self, at: usize) -> Result<Change, SmbError> {
        let mut r = Reader::at(self.output, at)?;
        let next = usize::try_from(r.u32()?).map_err(|_| SmbError::Malformed)?;
        let action = Action::from_code(r.u32()?);
        let len = usize::try_from(r.u32()?).map_err(|_| SmbError::Malformed)?;
        let name = r.bytes(len)?;
        self.at = match next {
            0 => None,
            _ if !next.is_multiple_of(4) || next < 12 + len => return Err(SmbError::Malformed),
            _ => Some(at.checked_add(next).ok_or(SmbError::Malformed)?),
        };
        let path = SharePath::decode(name)?;
        if path.is_root() {
            return Err(SmbError::Malformed);
        }
        Ok(Change { action, path })
    }
}

impl Iterator for Changes<'_> {
    type Item = Result<Change, SmbError>;

    fn next(&mut self) -> Option<Self::Item> {
        let at = self.at.take()?;
        if self.output.is_empty() {
            return None;
        }
        let change = self.record(at);
        if matches!(change, Err(SmbError::Malformed)) {
            self.at = None;
        }
        Some(change)
    }
}

/// Write `changes` as one response's output into `enc`, positioned at the
/// output's start, each record four-byte aligned from there.
pub fn write_changes<'c>(enc: &mut Encoder, changes: impl IntoIterator<Item = &'c Change>) {
    let mut changes = changes.into_iter().peekable();
    while let Some(change) = changes.next() {
        let start = enc.position();
        enc.u32(0);
        enc.u32(change.action.map_or(0, Action::code));
        let len = enc.u32_of(change.path.utf16_len());
        enc.u32(len);
        change.path.encode(enc);
        if changes.peek().is_some() {
            enc.align(4);
            let next = enc.u32_of(enc.position() - start);
            enc.patch_u32(start, next);
        }
    }
}

#[cfg(test)]
#[path = "notify_tests.rs"]
mod tests;
