//! `QUERY_DIRECTORY` (MS-SMB2 §2.2.33, §2.2.34) and the one listing class
//! this client reads, `FileIdBothDirectoryInformation` (MS-FSCC §2.4.22).
//!
//! Entries are walked in place, nothing copied: an entry's structure is
//! checked as it is reached, and its name decoded only when asked for, so one
//! name the client cannot carry costs that entry, never the listing.

use alloc::string::String;

use crate::file::name::decode_name;
use crate::file::{output_buffer, write_output_buffer, FileAttributes, FileId, FileTimes};
use crate::header::{structure, HEADER_LEN};
use crate::wire::{utf16le, utf16le_len, Encoder, Reader};
use crate::SmbError;

/// `FileIdBothDirectoryInformation`'s class number.
const ID_BOTH_DIRECTORY: u8 = 0x25;

/// The pattern every listing is asked with: every entry.
const EVERYTHING: &str = "*";

/// The fixed part of a `QUERY_DIRECTORY` request's body, which its pattern
/// follows.
const REQUEST_FIXED: usize = 32;

/// The fixed part of one entry, which its name follows.
const ENTRY_FIXED: usize = 104;

/// The most output one listing response may carry: a bound on what a server
/// can make the client hold at once, and so on how many entries it holds.
pub const MAX_LISTING: usize = 256 << 10;

flags! {
    /// How a listing is asked for (MS-SMB2 §2.2.33).
    pub struct ListingFlags(u8) {
        /// Start again from the first entry.
        const RESTART_SCANS = 0x01;
        /// Return one entry.
        const RETURN_SINGLE_ENTRY = 0x02;
        /// Resume from the given entry index.
        const INDEX_SPECIFIED = 0x04;
        /// Start again, under a new pattern.
        const REOPEN = 0x10;
    }
}

/// A `QUERY_DIRECTORY` request for every entry of a directory.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct QueryDirectoryRequest {
    /// How to list.
    pub flags: ListingFlags,
    /// The entry index to resume from, with [`ListingFlags::INDEX_SPECIFIED`].
    pub file_index: u32,
    /// The directory's open.
    pub file_id: FileId,
    /// The most output the server may answer with.
    pub output_length: u32,
}

impl QueryDirectoryRequest {
    /// Append the request's body after its header. An output length past
    /// [`MAX_LISTING`] latches [`SmbError::TooLarge`].
    pub fn encode(&self, enc: &mut Encoder) {
        if usize::try_from(self.output_length).map_or(true, |len| len > MAX_LISTING) {
            enc.fail(SmbError::TooLarge);
        }
        enc.u16(33);
        enc.u8(ID_BOTH_DIRECTORY);
        enc.u8(self.flags.bits());
        enc.u32(self.file_index);
        enc.bytes(&self.file_id);
        let offset = enc.u16_of(HEADER_LEN + REQUEST_FIXED);
        enc.u16(offset);
        let len = enc.u16_of(utf16le_len(EVERYTHING));
        enc.u16(len);
        enc.u32(self.output_length);
        utf16le(EVERYTHING, enc);
    }

    /// The request in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not a well-formed request for this
    /// class.
    pub fn decode(message: &[u8]) -> Result<Self, SmbError> {
        let mut r = structure(message, 33)?;
        if r.u8()? != ID_BOTH_DIRECTORY {
            return Err(SmbError::Malformed);
        }
        let flags = ListingFlags::from_bits(r.u8()?);
        let file_index = r.u32()?;
        let file_id = r.array()?;
        r.skip(4)?;
        Ok(Self {
            flags,
            file_index,
            file_id,
            output_length: r.u32()?,
        })
    }
}

/// A `QUERY_DIRECTORY` response: the entries, borrowed from the message.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct QueryDirectoryResponse<'a> {
    /// The listing's raw output.
    pub output: &'a [u8],
}

impl<'a> QueryDirectoryResponse<'a> {
    /// Append the response's body after its header.
    pub fn encode(&self, enc: &mut Encoder) {
        write_output_buffer(enc, self.output);
    }

    /// The response in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not a well-formed response,
    /// [`SmbError::TooLarge`] past [`MAX_LISTING`].
    pub fn decode(message: &'a [u8]) -> Result<Self, SmbError> {
        Ok(Self {
            output: output_buffer(message, 9, MAX_LISTING)?,
        })
    }

    /// Its entries, in the server's order.
    #[must_use]
    pub const fn entries(&self) -> Entries<'a> {
        Entries {
            output: self.output,
            at: Some(0),
        }
    }
}

/// One listed entry.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Entry<'a> {
    /// The entry's position in the directory, where the server keeps one.
    pub file_index: u32,
    /// Its times.
    pub times: FileTimes,
    /// Its size.
    pub end_of_file: u64,
    /// The bytes allocated to it.
    pub allocation_size: u64,
    /// Its attributes.
    pub attributes: FileAttributes,
    /// Its reparse tag, when it is a reparse point.
    pub reparse_tag: Option<u32>,
    /// Its 64-bit file id, or zero where the server has none.
    pub file_id: u64,
    name: &'a [u8],
}

impl Entry<'_> {
    /// Whether the entry is `.` or `..`, which a listing names and a path
    /// never does.
    #[must_use]
    pub fn is_dot(&self) -> bool {
        matches!(self.name, [b'.', 0] | [b'.', 0, b'.', 0])
    }

    /// The entry's name, once both sides can carry it.
    ///
    /// # Errors
    ///
    /// As [`decode_name`]: [`SmbError::Name`] for a dot entry or one either
    /// side refuses.
    pub fn name(&self) -> Result<String, SmbError> {
        decode_name(self.name)
    }
}

/// The entries of a listing, each checked as it is reached.
#[derive(Clone, Debug)]
pub struct Entries<'a> {
    output: &'a [u8],
    at: Option<usize>,
}

impl<'a> Entries<'a> {
    fn entry(&mut self, at: usize) -> Result<Entry<'a>, SmbError> {
        let mut r = Reader::at(self.output, at)?;
        let next = usize::try_from(r.u32()?).map_err(|_| SmbError::Malformed)?;
        let file_index = r.u32()?;
        let times = FileTimes::read(&mut r)?;
        let end_of_file = r.u64()?;
        let allocation_size = r.u64()?;
        let attributes = FileAttributes::from_bits(r.u32()?);
        let name_len = usize::try_from(r.u32()?).map_err(|_| SmbError::Malformed)?;
        let ea_size = r.u32()?;
        r.skip(28)?;
        let file_id = r.u64()?;
        let name = r.bytes(name_len)?;
        self.at = match next {
            0 => None,
            _ if !next.is_multiple_of(8) || next < ENTRY_FIXED + name_len => {
                return Err(SmbError::Malformed);
            }
            _ => Some(at.checked_add(next).ok_or(SmbError::Malformed)?),
        };
        if name.is_empty() {
            return Err(SmbError::Malformed);
        }
        Ok(Entry {
            file_index,
            times,
            end_of_file,
            allocation_size,
            attributes,
            reparse_tag: attributes
                .contains(FileAttributes::REPARSE_POINT)
                .then_some(ea_size),
            file_id,
            name,
        })
    }
}

impl<'a> Iterator for Entries<'a> {
    type Item = Result<Entry<'a>, SmbError>;

    fn next(&mut self) -> Option<Self::Item> {
        let at = self.at.take()?;
        if self.output.is_empty() {
            return None;
        }
        let entry = self.entry(at);
        if entry.is_err() {
            self.at = None;
        }
        Some(entry)
    }
}

/// One entry as a server writes it: what [`Entries`] reads back.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntryRecord<'a> {
    /// The entry's position in the directory, or zero.
    pub file_index: u32,
    /// Its times.
    pub times: FileTimes,
    /// Its size.
    pub end_of_file: u64,
    /// The bytes allocated to it.
    pub allocation_size: u64,
    /// Its attributes.
    pub attributes: FileAttributes,
    /// Its reparse tag, or extended-attribute size.
    pub ea_size: u32,
    /// Its 64-bit file id.
    pub file_id: u64,
    /// Its name.
    pub name: &'a str,
}

/// Write `records` as one listing's output into `enc`, positioned at the
/// output's start, each entry eight-byte aligned from there.
pub fn write_entries<'r>(
    enc: &mut Encoder,
    records: impl IntoIterator<Item = &'r EntryRecord<'r>>,
) {
    let mut records = records.into_iter().peekable();
    while let Some(record) = records.next() {
        let start = enc.position();
        enc.u32(0);
        enc.u32(record.file_index);
        record.times.write(enc);
        enc.u64(record.end_of_file);
        enc.u64(record.allocation_size);
        enc.u32(record.attributes.bits());
        let len = enc.u32_of(utf16le_len(record.name));
        enc.u32(len);
        enc.u32(record.ea_size);
        enc.zeros(28);
        enc.u64(record.file_id);
        utf16le(record.name, enc);
        if records.peek().is_some() {
            enc.align(8);
            let next = enc.u32_of(enc.position() - start);
            enc.patch_u32(start, next);
        }
    }
}

#[cfg(test)]
#[path = "dir_tests.rs"]
mod tests;
